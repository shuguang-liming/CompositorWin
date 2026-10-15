"""Create and verify a new release ZIP with Python 3.11+ and deflate level 9.

The package manifest excludes itself. Its actual bytes are added to the verified
inventory, so every ZIP file, including package-files.json, is checked by SHA256.
Existing output files are retained. Failed partial archives remain as evidence.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import sys
import time
import traceback
import zipfile
import zlib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def absolute(path):
    # Inspect lexical paths before resolving anything that might be a junction.
    return Path(os.path.abspath(path))


def check_plain_path(path):
    for component in (*reversed(path.parents), path):
        try:
            attributes = component.lstat()
        except FileNotFoundError:
            continue
        require(not stat.S_ISLNK(attributes.st_mode) and
                not getattr(attributes, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT,
                f"Reparse points and symbolic links are excluded: {component}")


def require_new(path):
    check_plain_path(path)
    require(not os.path.lexists(path), f"Existing output is retained: {path}")


def member_name(value):
    require(isinstance(value, str) and value, "Expected a nonempty relative package path")
    name = value.replace("\\", "/")
    require(not name.startswith("/") and not re.match(r"^[A-Za-z]:", name), f"Rooted package path: {value}")
    for component in name.split("/"):
        require(component not in ("", ".", "..") and not re.search(r'[<>:"|?*\x00-\x1f]', component)
                and not component.endswith((".", " "))
                and not re.match(r"^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)", component, re.I),
                f"Unsafe package path component: {value}")
    return name


def stream_digest(stream):
    checksum, length = hashlib.sha256(), 0
    while data := stream.read(1024 * 1024):
        checksum.update(data)
        length += len(data)
    return {"bytes": length, "sha256": checksum.hexdigest()}


def file_identity(path):
    with path.open("rb") as stream:
        return {"path": str(absolute(path)), **stream_digest(stream)}


def inventory_package(package):
    check_plain_path(package)
    require(package.is_dir(), f"Missing package directory: {package}")
    actual, folded = {}, set()
    for directory, children, files in os.walk(package, followlinks=False):
        for name in (*children, *files):
            entry = Path(directory) / name
            check_plain_path(entry)
            require(entry.is_dir() or stat.S_ISREG(entry.stat().st_mode), f"Non-regular package entry: {entry}")
        for name in files:
            path = Path(directory) / name
            relative = member_name(path.relative_to(package).as_posix())
            require(relative.casefold() not in folded, f"Duplicate package path: {relative}")
            folded.add(relative.casefold())
            actual[relative] = {key: value for key, value in file_identity(path).items() if key != "path"}
    manifest_path = package / "package-files.json"
    require("package-files.json" in actual, "The package manifest is missing")
    raw_manifest = manifest_path.read_bytes()
    require(hashlib.sha256(raw_manifest).hexdigest() == actual["package-files.json"]["sha256"],
            "Package manifest changed while reading")
    manifest = json.loads(raw_manifest.decode("utf-8-sig"))
    require(isinstance(manifest, list) and manifest, "The package manifest must be a nonempty array")
    expected, folded = {}, set()
    for record in manifest:
        require(isinstance(record, dict), "Package manifest entries must be objects")
        relative = member_name(record["path"])
        require(relative.casefold() != "package-files.json", "The manifest must exclude its own self-referential hash")
        require(relative.casefold() not in folded, f"Duplicate manifest entry: {relative}")
        folded.add(relative.casefold())
        require(isinstance(record["bytes"], int) and not isinstance(record["bytes"], bool) and record["bytes"] >= 0
                and isinstance(record["sha256"], str) and re.fullmatch(r"[a-fA-F0-9]{64}", record["sha256"]),
                f"Invalid manifest size or SHA256: {relative}")
        expected[relative] = {"bytes": record["bytes"], "sha256": record["sha256"].lower()}
    expected["package-files.json"] = actual["package-files.json"]
    require(actual == expected, "Package files, lengths or hashes differ from the manifest (including its own bytes)")
    return dict(sorted(expected.items()))


def verify_archive(archive_path, expected):
    checked, folded = {}, set()
    directories = {parent.as_posix() for name in expected for parent in Path(name).parents if parent.as_posix() != "."}
    with zipfile.ZipFile(archive_path, "r") as archive:
        for entry in archive.infolist():
            normalized = entry.filename.replace("\\", "/")
            directory = normalized.endswith("/")
            name = member_name(normalized[:-1] if directory else normalized)
            require(name.casefold() not in folded, f"Duplicate ZIP path: {name}")
            folded.add(name.casefold())
            if directory:
                require(name in directories and entry.file_size == 0, f"Unexpected ZIP directory: {name}")
                continue
            require(name in expected, f"Unexpected ZIP file: {name}")
            with archive.open(entry, "r") as stream:
                observed = stream_digest(stream)
            require(entry.file_size == observed["bytes"] and observed == expected[name],
                    f"ZIP file differs from the package: {name}")
            checked[name] = {**observed, "compressedBytes": entry.compress_size, "compressionMethod": entry.compress_type}
    require(set(checked) == set(expected), "The ZIP omits package files")
    return checked


def publish_new(partial, output):
    # Exclusive creation avoids replacement even if another process races us.
    require_new(output)
    with partial.open("rb") as source, output.open("xb") as target:
        shutil.copyfileobj(source, target, length=1024 * 1024)
    require(file_identity(partial)["sha256"] == file_identity(output)["sha256"], "Published archive differs from its partial")
    partial.unlink()


def pack(package, output, report_path):
    require(sys.version_info >= (3, 11), "Packaging requires Python 3.11 or newer")
    package, output, report_path = map(absolute, (package, output, report_path))
    partial = output.with_suffix(output.suffix + ".partial")
    require(len({output, partial, report_path}) == 3, "Archive, partial and report paths must differ")
    require(all(not path.is_relative_to(package) for path in (output, partial, report_path)),
            "Archive and report outputs must stay outside the packaged directory")
    for path in (output, partial, report_path):
        require_new(path)
    expected = inventory_package(package)
    for path in (output, report_path):
        path.parent.mkdir(parents=True, exist_ok=True)
        check_plain_path(path.parent)
    report = {"schemaVersion": 1, "complete": False, "startedUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
              "packageDirectory": str(package), "partialArchive": str(partial), "packageFiles": expected,
              "manifestSelfHashIncluded": True,
              "compressor": {"implementation": "Python zipfile.ZIP_DEFLATED", "level": 9,
                             "python": sys.executable, "pythonVersion": platform.python_version(),
                             "zlibBuildVersion": zlib.ZLIB_VERSION, "zlibRuntimeVersion": zlib.ZLIB_RUNTIME_VERSION,
                             "timestampPolicy": "Clamp source dates to the ZIP-supported 1980-2107 range; file contents are unchanged."},
              "timingScope": "Compression seconds cover opening/writing/closing the ZIP; verification and publication are separate.",
              "scope": "Release packaging strategy; archive savings are not attributed to executable code optimization."}
    try:
        started = time.perf_counter()
        with zipfile.ZipFile(partial, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9,
                             allowZip64=True, strict_timestamps=False) as archive:
            for relative in expected:
                archive.write(package / relative, arcname=relative, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        report["compressionSeconds"] = time.perf_counter() - started
        started = time.perf_counter()
        report["zipFiles"] = verify_archive(partial, expected)
        require(inventory_package(package) == expected, "Package inputs changed during compression")
        report["verificationSeconds"] = time.perf_counter() - started
        started = time.perf_counter()
        publish_new(partial, output)
        report["publicationSeconds"] = time.perf_counter() - started
        report["archive"] = file_identity(output)
        report["complete"] = True
    except Exception:
        report["error"] = traceback.format_exc()
        if partial.is_file():
            report["failedPartial"] = file_identity(partial)
    report["endedUtc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    with report_path.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = pack(args.package_dir, args.output, args.report)
    print(json.dumps({"complete": report["complete"], "archive": report.get("archive"), "report": str(absolute(args.report))}))
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        traceback.print_exc()
        sys.exit(1)
