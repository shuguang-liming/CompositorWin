"""Windows import gate regressions; runnable with the Python standard library."""
from pathlib import Path
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
import zipfile
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
def load_project_tool(name, root=ROOT):
    # Test the same readable source and in-memory loader used by project.ps1.
    script = (root / "project.ps1").read_text(encoding="utf-8-sig")
    bootstrap = re.search(r"(?ms)^# <PROJECT-PYTHON-BOOTSTRAP>\n\$script:ProjectPythonBootstrap = @'\n(.*?)\n'@\n# </PROJECT-PYTHON-BOOTSTRAP>", script).group(1)
    loader = types.ModuleType("project_loader")
    with patch.object(sys, "argv", ["project-loader", str(root)]):
        exec(compile(bootstrap, str(root / "project.ps1") + "::bootstrap", "exec"), loader.__dict__)
    return loader.project_module(name)


AUDIT = load_project_tool("audit-runtime")


def shell_environment():
    # Each PowerShell must load its own built-ins, rather than inherited PS7 modules.
    return {key: value for key, value in os.environ.items() if key.casefold() != "psmodulepath"}


class ImportGateTests(unittest.TestCase):
    def test_real_dumpbin_hint_columns_are_parsed(self):
        text = """  Section contains the following imports:
    KERNEL32.dll
              1421AD000 Import Address Table
                     0 time date stamp
                           0 GetProcAddress
                          A0 NtWriteFile
                         12c NtCreateNamedPipeFile
        """
        self.assertEqual(AUDIT.parse_imported_symbols(text), {
            "GetProcAddress", "NtWriteFile", "NtCreateNamedPipeFile",
        })

    def test_every_forbidden_symbol_with_real_dumpbin_prefix_is_rejected(self):
        for symbol in AUDIT.WINDOWS_11_ONLY_IMPORTS:
            for hint in ("0", "A0", "12c", "FFFF"):
                with self.subTest(symbol=symbol, hint=hint):
                    self.assertEqual(
                        AUDIT.forbidden_windows11_imports(f"    {hint} {symbol}\r\n"),
                        [symbol],
                    )

    def test_symbol_names_are_exact_and_not_substring_matches(self):
        text = "\n".join(
            f"    0 {symbol}Extra" for symbol in AUDIT.WINDOWS_11_ONLY_IMPORTS
        )
        self.assertEqual(AUDIT.forbidden_windows11_imports(text), [])

    def test_unexpected_dumpbin_format_fails_closed(self):
        for text in ("", "GetWindowBand", "    GetWindowBand\n", "error: tool failed"):
            with self.subTest(text=text):
                with self.assertRaisesRegex(ValueError, "No named PE imports"):
                    AUDIT.forbidden_windows11_imports(text)

    def test_external_crt_is_rejected_in_both_import_tables(self):
        for direct, delayed in ((["KERNEL32.dll", "VCRUNTIME140.dll"], []),
                                (["KERNEL32.dll"], ["api-ms-win-crt-runtime-l1-1-0.dll"])):
            with self.subTest(direct=direct, delayed=delayed):
                with self.assertRaisesRegex(ValueError, "external CRT DLL"):
                    AUDIT.require_static_crt(direct, delayed)

    def test_shipped_import_evidence_uses_supported_parser_format(self):
        import json
        record = json.loads((ROOT / "packaging/runtime-imports.json").read_text(encoding="utf-8-sig"))
        imported = AUDIT.parse_imported_symbols(record["rawEvidence"]["imports"])
        self.assertIn("GetProcAddress", imported)
        self.assertEqual(AUDIT.forbidden_windows11_imports(record["rawEvidence"]["imports"]), [])


@unittest.skipUnless(os.name == "nt", "PowerShell entry point requires Windows")
class UnifiedEntryPointTests(unittest.TestCase):
    def test_verify_package_uses_target_repository_source_and_license_identity(self):
        shells = list(dict.fromkeys(shell for shell in (shutil.which("pwsh"), shutil.which("powershell")) if shell))
        if not shells:
            self.skipTest("PowerShell is not installed")
        staging = ROOT / "build/temp/entrypoint-tests"
        staging.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=staging) as temporary:
            target = Path(temporary) / "另一个 源码根"
            package = target / "dist/Compositor-QA"
            package.mkdir(parents=True)
            (target / "assets").mkdir()
            for name in ("LICENSE.md", "LICENSES.md"):
                shutil.copyfile(ROOT / name, target / name)
            cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
            cargo = re.sub(r'(?m)^version = "[^"]+"$', 'version = "8.7.6"', cargo, count=1)
            (target / "Cargo.toml").write_text(cargo, encoding="utf-8")
            # Distinguish the target tool's source as well as its document inputs.
            script = (ROOT / "project.ps1").read_text(encoding="utf-8-sig")
            marker = 'print(json.dumps({"status": "passed", "dependencyNotices":'
            self.assertEqual(script.count(marker), 1)
            script = script.replace(marker, 'print(json.dumps({"qaSource": "target-repository", "status": "passed", "dependencyNotices":')
            (target / "project.ps1").write_text(script, encoding="utf-8-sig")
            generator = load_project_tool("generate-open-source-licenses", target)
            resource, index, _, _ = generator.generate(target / "LICENSES.md")
            self.assertEqual(index["components"][0]["version"], "8.7.6")
            self.assertNotEqual(resource, (ROOT / "assets/open-source-licenses.bin").read_bytes())
            (target / "assets/open-source-licenses.bin").write_bytes(resource)
            executable = b"MZ-synthetic-resource-container-not-executed\0" + resource
            (package / "compositor.exe").write_bytes(executable)
            release = target / "build/rust/release/compositor.exe"
            release.parent.mkdir(parents=True)
            release.write_bytes(executable)
            executable_hash = hashlib.sha256(executable).hexdigest()
            runtime = {"executable": {"sha256": executable_hash}, "staticCrtVerified": True,
                       "requirements": {"targetWindows": "Windows 10/11 x64"},
                       "manifest": {"windows10CompatibilityId": "{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}",
                                    "perMonitorV2": True, "requestedExecutionLevel": "asInvoker"},
                       "windows10ImportPolicy": {"passed": True, "forbiddenImportsFound": []}}
            (package / "runtime-imports.json").write_text(json.dumps(runtime), encoding="utf-8")
            (package / "LICENSES.md").write_bytes(generator.package_index(index))
            manifest = [{"path": path.name, "bytes": path.stat().st_size,
                         "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in package.iterdir()]
            (package / "package-files.json").write_text(json.dumps(manifest), encoding="utf-8")
            with zipfile.ZipFile(target / "dist/Compositor-QA-x64.zip", "w") as archive:
                for path in package.iterdir():
                    archive.write(path, path.name)
            for number, shell in enumerate(shells):
                with self.subTest(shell=shell):
                    report = f"build/verify-target-{number}.json"
                    result = subprocess.run([shell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                                             str(ROOT / "project.ps1"), "VerifyPackage", "-PythonPath", sys.executable,
                                             "-RepositoryRoot", str(target), "-PackageName", "Compositor-QA", "-ReportPath", report],
                                            cwd=ROOT, env=shell_environment(), capture_output=True, encoding="utf-8", errors="replace")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    verified = json.loads((target / report).read_text(encoding="utf-8-sig"))
                    self.assertEqual(verified["status"], "passed")
                    self.assertEqual(verified["embeddedLicenses"]["qaSource"], "target-repository")
                    self.assertEqual(verified["embeddedLicenses"]["resourceSha256"], hashlib.sha256(resource).hexdigest())

    def test_python_options_and_unicode_paths_survive_both_powershells(self):
        shells = [shutil.which(name) for name in ("pwsh", "powershell")]
        shells = list(dict.fromkeys(shell for shell in shells if shell))
        if not shells:
            self.skipTest("PowerShell is not installed")
        staging = ROOT / "build/temp/entrypoint-tests"
        staging.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=staging) as temporary:
            directory = Path(temporary)
            package = directory / "中文 空格 package"
            package.mkdir()
            content = "中文文件名和空格\n".encode("utf-8")
            (package / "中文 image.txt").write_bytes(content)
            (package / "package-files.json").write_text(json.dumps([{
                "path": "中文 image.txt", "bytes": len(content),
                "sha256": hashlib.sha256(content).hexdigest(),
            }], ensure_ascii=False), encoding="utf-8")
            for number, shell in enumerate(shells):
                with self.subTest(shell=shell):
                    archive = directory / f"{number} 中文 空格.zip"
                    report = directory / f"{number} report.json"
                    command = [shell, "-NoProfile", "-ExecutionPolicy", "Bypass",
                               "-File", str(ROOT / "project.ps1"), "PackageZip",
                               "-PythonPath", sys.executable, "--package-dir", str(package),
                               "--output", str(archive), "--report", str(report)]
                    result = subprocess.run(command, cwd=ROOT, env=shell_environment(), capture_output=True,
                                            encoding="utf-8", errors="replace")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(json.loads(report.read_text(encoding="utf-8"))["complete"])
                    with zipfile.ZipFile(archive) as zipped:
                        self.assertEqual(zipped.read("中文 image.txt"), content)
                    before = archive.read_bytes()
                    repeated = subprocess.run(command, cwd=ROOT, env=shell_environment(), capture_output=True)
                    self.assertEqual(repeated.returncode, 1)
                    self.assertEqual(archive.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
