"""Record PE imports of an EXE matching a completed All validation.

Inputs default to the current build validation directory. Each audit writes raw
PE evidence and a report copy into a fresh directory to retain earlier records.
"""
import argparse
from pathlib import Path
import datetime
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent
EXTERNAL_CRT = re.compile(
    r"^(?:(?:vcruntime|msvcp|msvcr|concrt)[^/\\]*|ucrtbased?|(?:api|ext)-ms-win-crt-[^/\\]*)\.dll$",
    re.I,
)

# These imports are documented as Windows 11-only entry points.  Keep this
# list intentionally small and explicit: ordinary Win10 APIs may be used by
# graphics/input backends and are accepted by the Windows 10 22H2 baseline.
WINDOWS_11_ONLY_IMPORTS = {
    "GetMachineTypeAttributes",
    "GetCurrentPackageInfo3",
    "GetCurrentPackagePath2",
    "GetKnownFolderPathEx",
    "GetWindowBand",
    "SetWindowBand",
}
WINDOWS_10_SUPPORTED_OS = "{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def identity(path):
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
            "modifiedUtc": datetime.datetime.fromtimestamp(path.stat().st_mtime, datetime.timezone.utc).isoformat()}


def report_path(path):
    return path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path)


def parse_dependents(text):
    parts = re.split(r"following delay load dependencies\s*:", text, maxsplit=1, flags=re.I)
    descriptor = r"^\s+([A-Za-z0-9_.-]+\.dll)\s*$"
    direct = re.findall(descriptor, parts[0], flags=re.M | re.I)
    delayed = re.findall(descriptor, parts[1], flags=re.M | re.I) if len(parts) == 2 else []
    require(direct, "No direct DLL import descriptors found")
    return direct, delayed


def require_static_crt(direct, delayed):
    forbidden = sorted({name for name in direct + delayed if EXTERNAL_CRT.fullmatch(name)}, key=str.lower)
    require(not forbidden, "Release requires an external CRT DLL: " + ", ".join(forbidden))


def parse_imported_symbols(text):
    """Read named imports after dumpbin's hexadecimal hint/ordinal column.

    /IMPORTS emits lines such as ``0 GetProcAddress`` and ``A0 NtWriteFile``.
    Matching a symbol alone would silently miss every ordinary dumpbin line.
    Require parsed names so an unexpected tool format fails the audit closed.
    """
    symbols = set(re.findall(
        r"^\s+[0-9A-Fa-f]+\s+([A-Za-z_?@][A-Za-z0-9_?@$]*)\s*$", text, re.M,
    ))
    require(symbols, "No named PE imports found in dumpbin /IMPORTS output")
    return symbols


def forbidden_windows11_imports(text):
    return sorted(WINDOWS_11_ONLY_IMPORTS & parse_imported_symbols(text))


def find_dumpbin(selected):
    if selected:
        return selected.resolve(strict=True)
    installed = shutil.which("dumpbin")
    if installed:
        return Path(installed).resolve(strict=True)
    vswhere = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    if vswhere.is_file():
        result = subprocess.run([str(vswhere), "-latest", "-products", "*", "-requires",
                                 "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-find",
                                 "VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe"],
                                capture_output=True, text=True, check=True)
        candidates = sorted(Path(line) for line in result.stdout.splitlines() if line.strip())
        if candidates:
            return candidates[-1].resolve(strict=True)
    raise ValueError("dumpbin.exe was not found; use --dumpbin PATH or a Visual Studio developer shell")


def find_mt(selected):
    """Find the Windows SDK manifest tool used by the PE resource gate."""
    if selected:
        return selected.resolve(strict=True)
    installed = shutil.which("mt") or shutil.which("mt.exe")
    if installed:
        return Path(installed).resolve(strict=True)
    candidates = []
    if os.environ.get("WindowsSdkVerBinPath"):
        candidates.append(Path(os.environ["WindowsSdkVerBinPath"]) / "x64" / "mt.exe")
    sdk = os.environ.get("WindowsSdkDir")
    if sdk:
        candidates.extend(Path(sdk).glob("bin/*/x64/mt.exe"))
    program_files_x86 = os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")
    candidates.extend(Path(program_files_x86).glob("Windows Kits/10/bin/*/x64/mt.exe"))
    existing = sorted((path for path in candidates if path.is_file()), key=lambda path: str(path))
    if existing:
        return existing[-1].resolve(strict=True)
    raise ValueError("mt.exe was not found; use --mt PATH or install the Windows SDK")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "build/rust/release/compositor.exe")
    parser.add_argument("--validation", type=Path, default=ROOT / "build/validation/current/build-all.json")
    parser.add_argument("--validation-log", type=Path, default=ROOT / "build/validation/current/all.log")
    parser.add_argument("--evidence-dir", type=Path, help="Fresh directory for raw evidence and an audit report copy")
    parser.add_argument("--dumpbin", type=Path, help="Optional dumpbin.exe path; otherwise detect the installed Build Tools")
    parser.add_argument("--mt", type=Path, help="Optional mt.exe path; otherwise detect the installed Windows SDK")
    return parser.parse_args(argv)


def verify_validation(validation, validation_log, exe):
    record = json.loads(validation.read_text(encoding="utf-8-sig"))
    require(record.get("action") == "All" and record.get("completed") is True,
            "Expected a completed All validation")
    steps = record.get("steps", [])
    require(len(steps) == 5, "Expected all five validation steps")
    log = validation_log.read_text(encoding="utf-8-sig")
    for step, gate in zip(steps, ("fmt", "check", "clippy", "test", "build")):
        tokens = shlex.split(step.get("command", ""), posix=False)
        require(type(step.get("exitCode")) is int and step["exitCode"] == 0
                and tokens[:2] == ["cargo", gate], f"Missing or failed validation step: {gate}")
        require(step["command"] in log, f"Validation log does not contain the {gate} command")
        if gate == "fmt":
            require("--check" in tokens, "Formatting was not checked")
        else:
            require("--locked" in tokens, f"Validation step was not locked: {gate}")
        if gate in ("check", "clippy"):
            require("--all-targets" in tokens, f"Validation did not cover all targets: {gate}")
        if gate == "clippy":
            require("--" in tokens and tokens[tokens.index("--") + 1:] == ["-D", "warnings"],
                    "Clippy did not deny warnings")
        if gate == "build":
            require("--release" in tokens and "--bin" in tokens
                    and tokens[tokens.index("--bin") + 1:tokens.index("--bin") + 2] == ["compositor"],
                    "Validation did not build the compositor Release binary")
    ended = datetime.datetime.fromisoformat(record["endedUtc"].replace("Z", "+00:00"))
    started = datetime.datetime.fromisoformat(record["startedUtc"].replace("Z", "+00:00"))
    require(started.tzinfo is not None and ended.tzinfo is not None and ended >= started,
            "Validation timestamps must be ordered and timezone-aware")
    target = (Path(record["targetDirectory"]).resolve(strict=True) / "release/compositor.exe").resolve(strict=True)
    target_identity, exe_identity = identity(target), identity(exe)
    require(target_identity["bytes"] == exe_identity["bytes"] and target_identity["sha256"] == exe_identity["sha256"],
            "Selected EXE differs from the current validated target")
    require(target.stat().st_mtime <= ended.timestamp() + 1,
            "Target EXE was modified after the validation completed")
    hash_fields = []
    for field in ("executable", "inputExecutable"):
        entry = record.get(field)
        if isinstance(entry, dict) and "sha256" in entry:
            require(entry["sha256"].lower() == exe_identity["sha256"], f"Validation {field} SHA256 mismatch")
            for size in ("bytes", "length"):
                require(size not in entry or entry[size] == exe_identity["bytes"], f"Validation {field} size mismatch")
            hash_fields.append(field)
    if "executableSha256" in record:
        require(record["executableSha256"].lower() == exe_identity["sha256"], "Validation executableSha256 mismatch")
        hash_fields.append("executableSha256")
    return {"completedVerified": True, "allFiveStepsPassed": True, "recordedCommandsPresentInLog": True,
            "executableMatchesCurrentValidatedTarget": True, "targetMtimeNoLaterThanValidationEnd": True,
            "mtimeToleranceSeconds": 1, "validatedTargetExecutable": target_identity,
            "validationExecutableHashFields": hash_fields,
            "hashMatchScope": "Selected EXE equals the current targetDirectory/release/compositor.exe by bytes and SHA256; target mtime is no later than validation completion plus one second.",
            "limitations": ["Legacy All records have no build-time EXE hash or cryptographic link to the text log. Current-target equality and mtime do not retroactively establish build-time hash or source-to-binary provenance."]}, target, exe_identity


def main(argv=None):
    args = parse_args(argv)
    dumpbin = find_dumpbin(args.dumpbin)
    mt = find_mt(args.mt)
    exe, validation, validation_log = (path.resolve(strict=True) for path in
                                      (args.exe, args.validation, args.validation_log))
    validation_identity, log_identity = identity(validation), identity(validation_log)
    verification, target, exe_identity = verify_validation(validation, validation_log, exe)
    target_identity = identity(target)
    previous_path = ROOT / "runtime-imports.json"
    previous_identity, tool_identity = identity(previous_path), identity(dumpbin)
    evidence_dir = args.evidence_dir or ROOT / "build/validation" / (
        "runtime-audit-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))
    evidence_dir = evidence_dir.resolve()
    evidence_dir.mkdir(parents=True, exist_ok=False)
    evidence, evidence_files = {}, {}
    for key, switch in (("dependents", "/DEPENDENTS"), ("headers", "/HEADERS"),
                        ("imports", "/IMPORTS"), ("vcruntime140Imports", "/IMPORTS:VCRUNTIME140.dll")):
        output = subprocess.run([str(dumpbin), switch, str(exe)], capture_output=True, check=True)
        evidence[key] = output.stdout.decode("utf-8", "replace")
        path = evidence_dir / f"pe-{key}.txt"
        with path.open("x", encoding="utf-8", newline="\n") as stream:
            stream.write(evidence[key])
        evidence_files[key] = identity(path)
    descriptors, delayed_descriptors = parse_dependents(evidence["dependents"])
    require_static_crt(descriptors, delayed_descriptors)
    imported_symbols = parse_imported_symbols(evidence["imports"])
    forbidden_imports = forbidden_windows11_imports(evidence["imports"])
    require(not forbidden_imports,
            "Release imports Windows 11-only APIs: " + ", ".join(forbidden_imports))
    headers = evidence["headers"]
    for required in ("PE32+", "machine (x64)", "Windows GUI", "High Entropy Virtual Addresses", "Dynamic base", "NX compatible"):
        require(required in headers, f"PE header validation failed: {required}")
    require(identity(exe) == exe_identity and identity(target) == target_identity,
            "EXE or target changed during dumpbin")
    require(identity(validation) == validation_identity and identity(validation_log) == log_identity,
            "Validation record/log changed during dumpbin")
    require(identity(previous_path) == previous_identity and identity(dumpbin) == tool_identity,
            "Previous audit or dumpbin changed during audit")
    manifest_output = evidence_dir / "app-manifest.xml"
    manifest_result = subprocess.run(
        [str(mt), f"-inputresource:{exe};#1", f"-out:{manifest_output}"],
        capture_output=True,
    )
    require(manifest_result.returncode == 0 and manifest_output.is_file(),
            "The Release executable has no readable RT_MANIFEST resource")
    manifest_raw = manifest_output.read_bytes()
    try:
        manifest_text = manifest_raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        manifest_text = manifest_raw.decode("utf-16")
    manifest_lower = manifest_text.lower()
    require(WINDOWS_10_SUPPORTED_OS.lower() in manifest_lower,
            "The Release manifest does not declare Windows 10/11 compatibility")
    require('level="asInvoker"'.lower() in manifest_lower,
            "The Release manifest must request asInvoker execution")
    require("permonitorv2" in manifest_lower,
            "The Release manifest must declare PerMonitorV2 DPI awareness")
    manifest_evidence = identity(manifest_output)
    evidence_files["manifest"] = manifest_evidence
    evidence["manifest"] = manifest_text
    verification["executableAndValidationStableAcrossDumpbin"] = True
    result = {
        "schemaVersion": 1,
        "scope": "Direct and delay-loaded PE imports of this optimized Windows x64 Release; dynamically loaded drivers and WIC codecs are not exhaustively enumerated.",
        "uiLanguage": "zh-CN", "checkedUtc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "executable": {"path": str(exe), "length": exe_identity["bytes"],
                       "modifiedUtc": datetime.datetime.fromtimestamp(exe.stat().st_mtime, datetime.timezone.utc).isoformat(),
                       "sha256": exe_identity["sha256"]},
        "auditTool": {**tool_identity, "versionEvidence": headers.splitlines()[0]},
        "manifestTool": identity(mt),
        "pe": {"format": "PE32+", "machine": "x64", "subsystem": "Windows GUI",
               "highEntropyVirtualAddresses": True, "dynamicBase": True, "nxCompatible": True},
        "directImportDescriptorCount": len(descriptors), "directImportDescriptors": descriptors,
        "delayImportDescriptorCount": len(delayed_descriptors), "delayImportDescriptors": delayed_descriptors,
        "windows10ImportPolicy": {
            "target": "Windows 10 22H2 x64 and Windows 11 x64",
            "parsedNamedImportCount": len(imported_symbols),
            "symbolEvidence": "dumpbin /IMPORTS hexadecimal hint followed by exact symbol name",
            "forbiddenWindows11OnlyImports": sorted(WINDOWS_11_ONLY_IMPORTS),
            "forbiddenImportsFound": forbidden_imports,
            "passed": not forbidden_imports,
        },
        "caseInsensitiveDistinctDllCount": len({name.lower() for name in descriptors + delayed_descriptors}),
        "caseInsensitiveDistinctDlls": sorted({name.lower() for name in descriptors + delayed_descriptors}),
        "staticCrtVerified": True,
        "manifest": {
            "windows10CompatibilityId": WINDOWS_10_SUPPORTED_OS,
            "perMonitorV2": True,
            "requestedExecutionLevel": "asInvoker",
            "sha256": manifest_evidence["sha256"],
        },
        "requirements": {
            "targetWindows": "Windows 10/11 x64",
            "graphics": "A graphics driver supporting Direct3D 12 or Vulkan; backend dependencies can load dynamically.",
            "cRuntime": "The MSVC and Universal C runtimes are statically linked. No separate Visual C++ Redistributable installation is required.",
            "runtimePolicy": "Direct and delay-loaded VCRUNTIME, MSVCP, MSVCR, CONCRT, UCRTBASE and api/ext-ms-win-crt DLL imports are rejected.",
            "heic": "A compatible installed Windows HEIF WIC codec is needed to decode HEIC/HEIF.",
            "separateOnnxRuntimeDllRequired": False,
            "hostRuntimeVersionMeaning": "Observed installed host versions only; they are not declared minimum supported versions.",
            "bundledSystemDlls": False,
        },
        "buildValidation": report_path(validation),
        "buildValidationLog": report_path(validation_log), "buildValidationRecordIdentity": validation_identity,
        "buildValidationLogIdentity": log_identity, "buildValidationVerification": verification,
        "previousChineseAudit": report_path(previous_path), "previousChineseAuditIdentity": previous_identity,
        "rawEvidence": evidence,
        "rawEvidenceFiles": evidence_files, "evidenceDirectory": str(evidence_dir),
    }
    text = json.dumps(result, indent=2) + "\n"
    with (evidence_dir / "runtime-imports.json").open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(text)
    (ROOT / "runtime-imports.json").write_text(text, encoding="utf-8")
    print(json.dumps({"bytes": exe_identity["bytes"], "sha256": exe_identity["sha256"],
                      "importDescriptors": descriptors, "evidenceDirectory": str(evidence_dir)}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
