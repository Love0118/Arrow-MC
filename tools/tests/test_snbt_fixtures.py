from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from export_snbt_fixtures import export, tree_text, utf16_hex


class SnbtFixtureExportTests(unittest.TestCase):
    def test_utf16_surrogates_and_long_precision_are_retained(self):
        self.assertEqual(utf16_hex("\ud800\0\U0001f600"), "d8000000d83dde00")
        self.assertEqual(tree_text({"tag_id": 4, "value": "9223372036854775807L"}), "4:9223372036854775807")
        self.assertEqual(tree_text({"tag_id": 6, "raw_bits": "18444492273895866368"}), "6:18444492273895866368")

    def test_binary_array_type_is_distinct_from_logical_list(self):
        self.assertEqual(tree_text({"tag_id": 7, "values": [-1, 2]}), "7:[-1,2]")
        self.assertEqual(tree_text({"tag_id": 9, "values": [{"tag_id": 1, "value": "-1b"}]}), "9:[1:-1]")

    def test_mismatched_version_and_duplicate_ids_are_rejected(self):
        data = {"minecraft_version": "test", "java_version": "25", "cases": [{"id": "one", "ok": True, "input": "{}"}]}
        with self.assertRaisesRegex(ValueError, "version differs"):
            export(data, "hash", "other")
        data["cases"] *= 2
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            export(data, "hash", "test")

    def test_absent_empty_and_multiple_error_arguments_remain_distinct(self):
        case = {"id": "error", "ok": False, "input": "", "translation_key": "key"}
        data = {"minecraft_version": "test", "java_version": "25", "cases": [case]}
        self.assertEqual(export(data, "hash", "test").splitlines()[-1].split("\t")[-1], "-")
        case["translation_args"] = [""]
        self.assertEqual(export(data, "hash", "test").splitlines()[-1].split("\t")[-1], "")
        case["translation_args"] = ["one", "two"]
        with self.assertRaisesRegex(ValueError, "multi-argument"):
            export(data, "hash", "test")


if __name__ == "__main__":
    unittest.main()
