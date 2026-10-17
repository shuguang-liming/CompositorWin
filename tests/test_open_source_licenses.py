"""Byte preservation and fail-closed checks for embedded license packaging."""
import contextlib
import argparse
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("embedded_licenses", ROOT / "generate-open-source-licenses.py")
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


class EmbeddedLicenseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.resource, cls.index, cls.index_raw, cls.originals = GENERATOR.generate(ROOT / "LICENSES.md")

    def setUp(self):
        staging = ROOT / "build/temp/license-generator-tests"
        staging.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=staging)
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def run_cli(self, *arguments):
        return subprocess.run([sys.executable, "-B", "-X", "utf8", str(ROOT / "generate-open-source-licenses.py"), *map(str, arguments)], cwd=ROOT, capture_output=True, encoding="utf-8")

    def test_all_originals_and_compiler_notices_are_byte_exact(self):
        index, bundle = GENERATOR.unpack(self.resource)
        self.assertEqual(index, self.index_raw)
        self.assertEqual(bundle, (ROOT / "LICENSES.md").read_bytes())
        for source in self.index["originals"]:
            restored = bundle[source["offset"]:source["offset"] + source["bytes"]]
            self.assertEqual(restored, self.originals[source["path"]])
            self.assertEqual(hashlib.sha256(restored).hexdigest(), source["sha256"])
        self.assertIn("installer-licenses/NSIS-3.11-COPYING.txt", self.originals)

    def test_current_application_row_does_not_repeat_historical_self_notice(self):
        application = self.index["components"][0]
        package = GENERATOR.tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
        self.assertEqual(application["name"], "CompositorWin")
        self.assertEqual(application["version"], package["version"])
        self.assertEqual(application["license"], "Apache-2.0")
        self.assertEqual(application["repository"], "https://github.com/shuguang-liming/CompositorWin")
        self.assertEqual(self.originals["CompositorWin-LICENSE.md"], (ROOT / "LICENSE.md").read_bytes())
        self.assertEqual([self.index["originals"][source]["path"] for source in application["originals"]], ["CompositorWin-LICENSE.md"])
        original = self.index["components"][1]
        self.assertEqual(original["license"], "MIT")
        self.assertEqual([self.index["originals"][source]["path"] for source in original["originals"]], ["LICENSE"])
        self.assertIn(b"Copyright (c) 2026 Wonder Assembly LLC", self.originals["LICENSE"])
        self.assertFalse(any(component["name"] == "compositor-windows" for component in self.index["components"]))
        self.assertIn(b'"name": "compositor-windows"', self.originals["THIRD-PARTY-NOTICES.json"])

    def test_different_bundled_application_license_is_rejected(self):
        summary, originals = GENERATOR.BUNDLE.verify_bundle(ROOT / "LICENSES.md")
        originals["CompositorWin-LICENSE.md"] += b"changed"
        with patch.object(GENERATOR.BUNDLE, "verify_bundle", return_value=(summary, originals)), patch.object(GENERATOR, "source_ranges", return_value=[{"path": name, "offset": 0, "bytes": len(text), "sha256": hashlib.sha256(text).hexdigest()} for name, text in originals.items()]):
            with self.assertRaisesRegex(ValueError, "bundled application license differs"):
                GENERATOR.generate(ROOT / "LICENSES.md")

    def test_recreated_bundle_keeps_application_and_upstream_originals(self):
        license_directory = self.directory / "third-party-licenses"
        license_directory.mkdir()
        for name, raw in self.originals.items():
            if name.startswith("third-party-licenses/"):
                (license_directory / name.removeprefix("third-party-licenses/")).write_bytes(raw)
        notices = self.directory / "THIRD-PARTY-NOTICES.json"
        notices.write_bytes(self.originals["THIRD-PARTY-NOTICES.json"])
        output = self.directory / "LICENSES.md"
        GENERATOR.BUNDLE.create_bundle(argparse.Namespace(source_bundle=ROOT / "LICENSES.md", notices=notices, license_dir=license_directory, output=output))
        _, restored = GENERATOR.BUNDLE.verify_bundle(output)
        self.assertEqual(restored, self.originals)
        _, index, _, _ = GENERATOR.generate(output)
        self.assertEqual(index["components"][0]["license"], "Apache-2.0")
        self.assertEqual(index["components"][1]["license"], "MIT")

    def test_every_dependency_points_to_its_actual_license_originals(self):
        for component in self.index["components"][4:]:
            prefix = f"third-party-licenses/{component['name']}-{component['version']}-"
            sources = [self.index["originals"][source]["path"] for source in component["originals"]]
            self.assertTrue(sources)
            self.assertTrue(all(source.startswith(prefix) for source in sources), component["name"])
            self.assertNotIn("THIRD-PARTY-NOTICES.json", sources)
        egui = next(component for component in self.index["components"] if component["name"] == "egui")
        texts = [self.originals[self.index["originals"][source]["path"]] for source in egui["originals"]]
        self.assertTrue(any(b"Emil Ernerfeldt" in text and b"Permission is hereby granted" in text for text in texts))

    def test_missing_license_text_is_rejected_instead_of_using_metadata(self):
        summary, originals = GENERATOR.BUNDLE.verify_bundle(ROOT / "LICENSES.md")
        originals = {name: text for name, text in originals.items() if not name.startswith("third-party-licenses/egui-0.33.3-")}
        with patch.object(GENERATOR.BUNDLE, "verify_bundle", return_value=(summary, originals)), patch.object(GENERATOR, "source_ranges", return_value=[{"path": name, "offset": 0, "bytes": len(text), "sha256": hashlib.sha256(text).hexdigest()} for name, text in originals.items()]):
            with self.assertRaisesRegex(ValueError, "Missing actual license original for egui 0.33.3"):
                GENERATOR.generate(ROOT / "LICENSES.md")

    def test_spdx_with_exception_keeps_the_actual_spdx_exception_link(self):
        self.assertEqual(GENERATOR.spdx_links("Apache-2.0 WITH LLVM-exception OR MIT"), [
            {"name": "Apache-2.0", "url": "https://spdx.org/licenses/Apache-2.0.html"},
            {"name": "LLVM-exception", "url": "https://spdx.org/licenses/LLVM-exception.html"},
            {"name": "MIT", "url": "https://spdx.org/licenses/MIT.html"},
        ])

    def test_manifest_declaration_alone_does_not_count_as_license_text(self):
        summary, originals = GENERATOR.BUNDLE.verify_bundle(ROOT / "LICENSES.md")
        originals = {name: text for name, text in originals.items() if not name.startswith("third-party-licenses/egui-0.33.3-")}
        originals["third-party-licenses/egui-0.33.3-Cargo.toml"] = b'license = "MIT OR Apache-2.0"\n'
        with patch.object(GENERATOR.BUNDLE, "verify_bundle", return_value=(summary, originals)), patch.object(GENERATOR, "source_ranges", return_value=[{"path": name, "offset": 0, "bytes": len(text), "sha256": hashlib.sha256(text).hexdigest()} for name, text in originals.items()]):
            with self.assertRaisesRegex(ValueError, "Missing actual license text for egui 0.33.3"):
                GENERATOR.generate(ROOT / "LICENSES.md")

    def test_executable_requires_exact_embedded_resource(self):
        executable = self.directory / "synthetic-resource-container.exe"
        executable.write_bytes(b"MZ-QA-container\0" + self.resource + b"trailing-container-data")
        result = self.run_cli("--check", "--verify-exe", executable)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["executableOffset"], 16)
        damaged = bytearray(executable.read_bytes())
        damaged[100] ^= 1
        executable.write_bytes(damaged)
        self.assertNotEqual(self.run_cli("--check", "--verify-exe", executable).returncode, 0)
        executable.write_bytes(b"MZ-QA-container\0" + self.resource[:-1])
        self.assertNotEqual(self.run_cli("--check", "--verify-exe", executable).returncode, 0)

    def test_small_entry_point_retains_existing_files_and_detects_changes(self):
        target = self.directory / "LICENSES.md"
        created = self.run_cli("--check", "--write-package-index", target)
        self.assertEqual(created.returncode, 0, created.stderr)
        original = target.read_bytes()
        self.assertLess(len(original), 2048)
        self.assertIn("CompositorWin 主程序：Apache-2.0".encode(), original)
        self.assertIn("原 Compositor 项目：MIT".encode(), original)
        self.assertEqual(self.run_cli("--check", "--verify-package-index", target).returncode, 0)
        self.assertNotEqual(self.run_cli("--check", "--write-package-index", target).returncode, 0)
        self.assertEqual(target.read_bytes(), original)
        target.write_bytes(original + b"unrecorded change")
        self.assertNotEqual(self.run_cli("--check", "--verify-package-index", target).returncode, 0)

    def test_different_nsis_copying_is_rejected(self):
        copying = self.directory / "COPYING"
        copying.write_bytes(self.originals["installer-licenses/NSIS-3.11-COPYING.txt"])
        self.assertEqual(self.run_cli("--check", "--nsis-copying", copying).returncode, 0)
        copying.write_bytes(copying.read_bytes() + b"changed")
        self.assertNotEqual(self.run_cli("--check", "--nsis-copying", copying).returncode, 0)

    def test_check_never_repairs_a_modified_resource(self):
        asset = self.directory / "resource.bin"
        asset.write_bytes(self.resource[:-1])
        before = asset.read_bytes()
        with patch.object(GENERATOR, "ASSET", asset), patch.object(sys, "argv", ["generator", "--check"]), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, "stale"):
                GENERATOR.main()
        self.assertEqual(asset.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
