"""Generate and audit the application's compressed, byte-exact license resource."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct
import tomllib
from urllib.parse import quote, urlsplit
import zlib

ROOT = Path(__file__).resolve().parent
ASSET = ROOT / "assets/open-source-licenses.bin"
MAGIC = b"COMPOSITOR-OPEN-SOURCE-LICENSES\0v1\0"
MAX_PART_BYTES = 16 * 1024 * 1024
SPEC = importlib.util.spec_from_file_location("license_bundle", ROOT / "package-licenses.py")
BUNDLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUNDLE)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_ranges(raw, originals):
    ranges, cursor = [], 0
    while (start := raw.find(BUNDLE.SECTION, cursor)) >= 0:
        start += len(BUNDLE.SECTION)
        end = raw.index(b" -->\n", start)
        record = json.loads(raw[start:end])
        begin = end + len(b" -->\n") + len(f"{record['fence']}text\n".encode())
        finish = begin + record["bytes"]
        require(raw[begin:finish] == originals[record["path"]], "Original range differs")
        ranges.append({"path": record["path"], "offset": begin, "bytes": record["bytes"], "sha256": record["sha256"]})
        cursor = finish + len(f"\n{record['fence']}\n".encode())
    require(len(ranges) == len(originals), "Original range count differs")
    return ranges


def https_url(value):
    parsed = urlsplit(value)
    require(parsed.scheme == "https" and parsed.netloc and not parsed.username, f"Expected an HTTPS license link: {value}")
    return value


def component(name, version, license_expression, repository, sources, license_links):
    require(bool(license_links), f"No online license link for {name}")
    return {"name": name, "version": version, "license": license_expression,
            "repository": repository or "", "url": https_url(license_links[0]["url"]),
            "licenseLinks": license_links, "originals": sources}


def spdx_links(expression):
    tokens = re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*", expression)
    links, seen = [], set()
    for token in tokens:
        if token in {"AND", "OR", "WITH"}:
            continue
        if token not in seen:
            seen.add(token)
            # SPDX publishes both license and exception pages under /licenses/.
            links.append({"name": token, "url": https_url(f"https://spdx.org/licenses/{quote(token, safe='-.+')}.html")})
    return links


def generate(source_bundle):
    summary, originals = BUNDLE.verify_bundle(source_bundle)
    raw = source_bundle.read_bytes()
    require(len(raw) <= MAX_PART_BYTES, "The license source exceeds the bounded embedded format")
    ranges = source_ranges(raw, originals)
    identities = {entry["path"]: index for index, entry in enumerate(ranges)}
    package = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    version = package["version"]
    components = [
        component("Compositor / CompositorWin", version, "MIT", "https://github.com/robbietilton/Compositor", [identities["LICENSE"]], [{"name": "MIT", "url": "https://spdx.org/licenses/MIT.html"}]),
        component("U²-Netp model", "", "Apache-2.0", "https://github.com/xuebinqin/U-2-Net", [identities["LICENSE-U2NET.txt"]], [{"name": "Apache-2.0", "url": "https://spdx.org/licenses/Apache-2.0.html"}]),
    ]
    nsis = "installer-licenses/NSIS-3.11-COPYING.txt"
    if nsis in identities:
        components.append(component("NSIS", "3.11", "zlib/libpng, bzip2, CPL-1.0 and included notices", "https://nsis.sourceforge.io/", [identities[nsis]], [{"name": "NSIS licenses", "url": "https://nsis.sourceforge.io/License"}]))
    for notice in BUNDLE.dependency_notices(originals["THIRD-PARTY-NOTICES.json"]):
        # The original bundle also preserves an older self-package notice.
        # Keep its bytes intact, while the active application row uses Cargo.
        if notice["name"] == package["name"]:
            continue
        prefix = f"third-party-licenses/{notice['name']}-{notice['version']}-"
        sources = [index for path, index in identities.items() if path.startswith(prefix)]
        require(bool(sources), f"Missing actual license original for {notice['name']} {notice['version']}; metadata is not a license text")
        require(any(re.search(r"licen[sc]e|copying|copyright|notice|(?:^|[-_])(?:OFL|UFL)(?:[.-]|$)", ranges[source]["path"][len(prefix):], re.I) for source in sources),
                f"Missing actual license text for {notice['name']} {notice['version']}; declarations alone do not suffice")
        links = spdx_links(notice["license"])
        components.append(component(notice["name"], notice["version"], notice["license"], notice["repository"], sources, links))
    index = {"formatVersion": 1, "bundleBytes": len(raw), "bundleSha256": digest(raw),
             "dependencyNotices": summary["dependencyNotices"], "licenseTexts": summary["licenseTexts"],
             "components": components, "originals": ranges}
    index_raw = (json.dumps(index, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
    require(len(index_raw) <= MAX_PART_BYTES, "The license index exceeds the bounded embedded format")
    index_compressed, raw_compressed = zlib.compress(index_raw, 9), zlib.compress(raw, 9)
    header = struct.pack("<IIII", len(index_compressed), len(index_raw), len(raw_compressed), len(raw))
    return MAGIC + header + index_compressed + raw_compressed, index, index_raw, originals


def unpack(resource):
    require(resource.startswith(MAGIC), "Missing embedded license resource identity")
    start = len(MAGIC)
    a, b, c, d = struct.unpack_from("<IIII", resource, start)
    start += 16
    require(max(b, d) <= MAX_PART_BYTES and len(resource) == start + a + c, "Invalid embedded resource lengths")
    index_raw = zlib.decompress(resource[start:start + a])
    bundle_raw = zlib.decompress(resource[start + a:])
    require(len(index_raw) == b and len(bundle_raw) == d, "Embedded decoded lengths differ")
    index = json.loads(index_raw)
    require(index["bundleBytes"] == d and index["bundleSha256"] == digest(bundle_raw), "Embedded source identity differs")
    return index_raw, bundle_raw


def package_index(index):
    return (
        "# 开源软件许可证\n\n"
        "在 CompositorWin 中打开“帮助 → 开源软件许可证”。点击组件名称可在线查看许可证，"
        "选择“随附原文”可离线查看该组件的完整许可和版权声明。\n\n"
        "完整原文已压缩内嵌在 compositor.exe 中，按需解压，不需要另外附带大段文本。"
        "压缩不改变原始字节或许可条款；各组件仍分别适用自己的许可证。\n\n"
        "- 应用：MIT，https://spdx.org/licenses/MIT.html\n"
        "- 原项目：https://github.com/robbietilton/Compositor\n"
        "- U²-Net 模型：Apache-2.0，https://spdx.org/licenses/Apache-2.0.html\n"
        "- 模型项目：https://github.com/xuebinqin/U-2-Net\n"
        "- NSIS：https://nsis.sourceforge.io/License\n\n"
        f"随附 {index['dependencyNotices']} 项组件声明和 {index['licenseTexts']} 份许可原文。\n"
        f"完整原文 SHA256：`{index['bundleSha256']}`。\n"
    ).encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Read-only exact regeneration check; do not write the resource")
    parser.add_argument("--verify-exe", type=Path, help="Require the executable to contain the exact checked resource bytes")
    parser.add_argument("--nsis-copying", type=Path, help="Require the bundle to preserve this compiler's exact COPYING bytes")
    parser.add_argument("--write-package-index", type=Path, help="Exclusively create the small release LICENSES.md entry point")
    parser.add_argument("--verify-package-index", type=Path, help="Read-only exact check of the small release LICENSES.md entry point")
    args = parser.parse_args()
    expected, index, index_raw, originals = generate(ROOT / "LICENSES.md")
    verify_only = args.check or args.verify_exe is not None or args.verify_package_index is not None
    if verify_only:
        require(ASSET.read_bytes() == expected, "Embedded licenses are stale; run generate-open-source-licenses.py")
    else:
        ASSET.write_bytes(expected)
    restored_index, restored_bundle = unpack(ASSET.read_bytes())
    require(restored_index == index_raw and restored_bundle == (ROOT / "LICENSES.md").read_bytes(), "Embedded originals differ byte-for-byte")
    if args.nsis_copying is not None:
        require(originals.get("installer-licenses/NSIS-3.11-COPYING.txt") == args.nsis_copying.read_bytes(), "NSIS compiler COPYING differs from the embedded original")
    executable_offset = None
    if args.verify_exe is not None:
        executable = args.verify_exe.read_bytes()
        executable_offset = executable.find(expected)
        require(executable_offset >= 0, "The executable does not contain the exact verified license resource")
        recovered_index, recovered_bundle = unpack(executable[executable_offset:executable_offset + len(expected)])
        require(recovered_index == index_raw and recovered_bundle == restored_bundle, "Executable license metadata or originals differ")
    small_index = package_index(index)
    if args.write_package_index is not None:
        with args.write_package_index.open("xb") as stream:
            stream.write(small_index)
    if args.verify_package_index is not None:
        require(args.verify_package_index.read_bytes() == small_index, "Packaged license entry point differs")
    print(json.dumps({"status": "passed", "dependencyNotices": index["dependencyNotices"], "licenseTexts": index["licenseTexts"],
                      "bundleBytes": index["bundleBytes"], "bundleSha256": index["bundleSha256"], "indexSha256": digest(index_raw),
                      "resourceBytes": len(expected), "resourceSha256": digest(expected), "packageIndexBytes": len(small_index),
                      "executableOffset": executable_offset}, ensure_ascii=False))


if __name__ == "__main__":
    main()
