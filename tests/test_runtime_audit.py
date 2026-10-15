"""Windows import gate regressions; runnable with the Python standard library."""
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("audit_runtime", ROOT / "audit-runtime.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


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
        record = json.loads((ROOT / "runtime-imports.json").read_text(encoding="utf-8-sig"))
        imported = AUDIT.parse_imported_symbols(record["rawEvidence"]["imports"])
        self.assertIn("GetProcAddress", imported)
        self.assertEqual(AUDIT.forbidden_windows11_imports(record["rawEvidence"]["imports"]), [])


if __name__ == "__main__":
    unittest.main()
