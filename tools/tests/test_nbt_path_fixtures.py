from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from export_nbt_path_fixtures import COLUMNS, encode, export


class NbtPathFixtureExportTests(unittest.TestCase):
    def test_unicode_empty_null_and_primitive_type_are_distinct(self):
        self.assertEqual(encode("\ud800\t-", "text"), "d8000009002d")
        self.assertEqual(encode([], "args"), "")
        self.assertEqual(encode(["", 1, "1", False], "args"), "s:;n:1;s:0031;b:0")
        self.assertEqual(encode([], "inputs"), "")
        self.assertEqual(encode(["", {"construct": "end"}, "END"], "inputs"), "s:;c:end;s:0045004e0044")
        case = {"id": "test", "path": "", "context": None, "ok": True, "supplier_calls": 0}
        data = {"minecraft_version": "test", "java_version": "25.0.3", "cases": [case]}
        fields = export(data, "hash", "test").splitlines()[-1].split("\t")
        row = dict(zip((name for name, _ in COLUMNS), fields, strict=True))
        self.assertEqual(row["path"], "")
        self.assertEqual(row["context"], "-")
        self.assertEqual(row["op"], "parse")

    def test_incomplete_duplicate_wrong_version_and_unknown_fields_fail(self):
        case = {"id": "test", "ok": True, "supplier_calls": 0}
        data = {"minecraft_version": "test", "java_version": "25.0.3", "cases": [case]}
        with self.assertRaisesRegex(ValueError, "version"):
            export(data, "hash", "other")
        data["cases"] = [case, case]
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            export(data, "hash", "test")
        data["cases"] = [{**case, "new_observation": 1}]
        with self.assertRaisesRegex(ValueError, "Unrepresented"):
            export(data, "hash", "test")
        data["cases"] = [{"id": "test", "ok": True}]
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            export(data, "hash", "test")

    def test_java_runtime_failure_and_alias_observations_are_not_dropped(self):
        case = {"id": "java-boundary", "ok": False, "supplier_calls": 2,
                "runtime_error": "ArrayIndexOutOfBoundsException", "same_reference": True,
                "selected": [{"tag_id": 1, "snbt": "1b"}]}
        data = {"minecraft_version": "test", "java_version": "25.0.3", "cases": [case]}
        rendered = export(data, "hash", "test")
        row = dict(zip((name for name, _ in COLUMNS), rendered.splitlines()[-1].split("\t"), strict=True))
        self.assertEqual(row["runtime_error"], "ArrayIndexOutOfBoundsException")
        self.assertEqual(row["same_reference"], "1")
        self.assertEqual(row["selected"], "1:00310062")


if __name__ == "__main__":
    unittest.main()
