"""Combine release license originals and dependency notices into one Markdown file."""
import argparse
import hashlib
import json
from pathlib import Path
import re


SUMMARY = b"<!-- LICENSES-SUMMARY "
SECTION = b"<!-- LICENSES-SOURCE "


def require(condition, message):
    if not condition:
        raise ValueError(message)


def dependency_notices(raw):
    notices = json.loads(raw.decode("utf-8-sig"))
    require(isinstance(notices, list) and notices, "Dependency notices are missing")
    identities = set()
    for notice in notices:
        require(isinstance(notice, dict), "Invalid dependency notice")
        identity = (notice.get("name"), notice.get("version"))
        require(all(isinstance(value, str) and value for value in identity), "Missing component identity")
        require(identity not in identities, f"Duplicate dependency identity: {identity}")
        identities.add(identity)
    return notices


def append_source(output, name, raw):
    raw.decode("utf-8")  # Fail rather than silently alter an unsupported original.
    runs = [len(match[0]) for match in re.finditer(rb"`+", raw)]
    fence = "`" * max(3, max(runs, default=0) + 1)
    record = {"path": name, "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(), "fence": fence}
    output.extend(f"\n## {name}\n\n".encode("utf-8"))
    output.extend(SECTION + json.dumps(record, ensure_ascii=False).encode("utf-8") + b" -->\n")
    output.extend(f"{fence}text\n".encode("ascii"))
    output.extend(raw)
    output.extend(f"\n{fence}\n".encode("ascii"))


def verify_bundle(path):
    data = path.read_bytes()
    data.decode("utf-8")
    summary_start = data.index(SUMMARY) + len(SUMMARY)
    summary_end = data.index(b" -->\n", summary_start)
    summary = json.loads(data[summary_start:summary_end])
    require(summary.get("schemaVersion") == 1, "Unsupported license bundle")
    originals, cursor = {}, summary_end
    while (start := data.find(SECTION, cursor)) >= 0:
        start += len(SECTION)
        end = data.index(b" -->\n", start)
        record = json.loads(data[start:end])
        name = record["path"]
        require(name not in originals, f"Duplicate bundled source: {name}")
        opening = f"{record['fence']}text\n".encode("ascii")
        begin = end + len(b" -->\n")
        require(data[begin:begin + len(opening)] == opening, f"Missing text fence: {name}")
        begin += len(opening)
        finish = begin + record["bytes"]
        raw = data[begin:finish]
        require(len(raw) == record["bytes"] and hashlib.sha256(raw).hexdigest() == record["sha256"],
                f"Bundled source size or hash differs: {name}")
        closing = f"\n{record['fence']}\n".encode("ascii")
        require(data[finish:finish + len(closing)] == closing, f"Missing closing fence: {name}")
        originals[name] = raw
        cursor = finish + len(closing)
    require({"LICENSE", "LICENSE-U2NET.txt", "THIRD-PARTY-NOTICES.json"} <= originals.keys(),
            "Application, model or dependency notices are missing")
    require(len(originals) == summary["licenseTexts"] + 1, "License text count differs")
    require(any(name.startswith("third-party-licenses/") for name in originals), "Third-party texts are missing")
    notices = dependency_notices(originals["THIRD-PARTY-NOTICES.json"])
    require(len(notices) == summary["dependencyNotices"], "Dependency notice count differs")
    return summary, originals


def create_bundle(args):
    _, source_originals = verify_bundle(args.source_bundle)
    files = sorted(args.license_dir.iterdir(), key=lambda path: path.name)
    require(files and all(path.is_file() and not path.is_symlink() for path in files),
            "Expected a nonempty directory of license files")
    originals = {
        "LICENSE": source_originals["LICENSE"],
        "LICENSE-U2NET.txt": source_originals["LICENSE-U2NET.txt"],
        "THIRD-PARTY-NOTICES.json": args.notices.read_bytes(),
        "CompositorWin-LICENSE.md": (Path(__file__).resolve().parent / "LICENSE.md").read_bytes(),
    }
    # Installer stubs/plugins accompany the app, outside the Cargo dependency graph.
    originals.update({name: raw for name, raw in source_originals.items()
                      if name.startswith("installer-licenses/")})
    originals.update({f"third-party-licenses/{path.name}": path.read_bytes() for path in files})
    notices = dependency_notices(originals["THIRD-PARTY-NOTICES.json"])
    summary = {"schemaVersion": 1, "dependencyNotices": len(notices), "licenseTexts": len(originals) - 1}
    header = (
        "# Compositor Windows 许可证与第三方声明\n\n"
        "本文件汇集应用、模型、第三方依赖和字体的原始许可证、版权声明及组件来源。"
        "各组件适用各自许可证；合并文件不改变任何许可条款。\n\n"
        "CompositorWin 主程序及 Windows 移植新增代码采用 Apache License 2.0，"
        "完整条款同时见根目录 LICENSE.md 和下方 CompositorWin-LICENSE.md。"
        "原 Compositor 项目（Copyright (c) 2026 Wonder Assembly LLC）保留 MIT 许可，"
        "其原文见下方 LICENSE；模型、依赖与字体分别保留各自许可。\n\n"
        "下方原文保留原始字节和换行，每段标注原文件名、大小及 SHA256。"
        "使用编辑器搜索组件名或文件名即可定位。\n\n"
        "原项目：https://github.com/robbietilton/Compositor\n\n"
        "模型来源：https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2netp.onnx\n\n"
        "原模型项目：https://github.com/xuebinqin/U-2-Net\n\n"
    )
    output = bytearray(header.encode("utf-8"))
    output.extend(SUMMARY + json.dumps(summary).encode("ascii") + b" -->\n")
    for name, raw in originals.items():
        append_source(output, name, raw)
    with args.output.open("xb") as stream:
        stream.write(output)
    observed, restored = verify_bundle(args.output)
    require(restored == originals, "The combined file differs from the source originals")
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("--source-bundle", type=Path, required=True)
    create.add_argument("--notices", type=Path, required=True)
    create.add_argument("--license-dir", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("path", type=Path)
    args = parser.parse_args()
    summary = create_bundle(args) if args.command == "create" else verify_bundle(args.path)[0]
    print(json.dumps({"status": "passed", **summary}))


if __name__ == "__main__":
    main()
