import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from prepare_minecraft import download, inspect_sources, select_version, unpack_bundle


class ReferencePreparationTests(unittest.TestCase):
    def test_latest_prefers_26_3_release_and_never_26_4(self):
        versions = [
            {"id": "26.3-pre-2", "type": "snapshot", "releaseTime": "2026-09-04"},
            {"id": "26.3", "type": "release", "releaseTime": "2026-09-10"},
            {"id": "26.3-snapshot-99", "type": "snapshot", "releaseTime": "2026-09-11"},
            {"id": "26.4", "type": "release", "releaseTime": "2026-12-01"},
        ]
        self.assertEqual(select_version({"versions": versions}, "latest")["id"], "26.3")
        self.assertEqual(select_version({"versions": versions}, "26.3-pre-2")["id"], "26.3-pre-2")

    def test_latest_snapshot_uses_release_time_instead_of_lexical_id(self):
        versions = [{"id": f"26.3-snapshot-{n}", "type": "snapshot", "releaseTime": date}
                    for n, date in [(9, "2026-08-17"), (10, "2026-08-25")]]
        self.assertEqual(select_version({"versions": versions}, "latest")["id"], "26.3-snapshot-10")
        with self.assertRaises(ValueError):
            select_version({"versions": versions}, "26.4")

    def test_hash_mismatch_cannot_replace_valid_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / "upstream", Path(directory) / "download"
            source.write_bytes(b"corrupted")
            destination.write_bytes(b"previous download")
            with self.assertRaisesRegex(ValueError, "Hash mismatch"):
                download(source.as_uri(), destination, "sha1", hashlib.sha1(b"expected").hexdigest())
            self.assertEqual(destination.read_bytes(), b"previous download")

    def test_valid_download_and_cache_reuse(self):
        with tempfile.TemporaryDirectory() as directory:
            source, destination = Path(directory) / "upstream", Path(directory) / "download"
            source.write_bytes(b"expected")
            digest = hashlib.sha1(b"expected").hexdigest()
            download(source.as_uri(), destination, "sha1", digest)
            download("invalid-url-is-never-requested", destination, "sha1", digest)
            self.assertEqual(destination.read_bytes(), b"expected")

    def test_bundle_rejects_path_traversal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / "server.jar"
            digest = hashlib.sha256(b"jar").hexdigest()
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("META-INF/versions.list", f"{digest}\tversion\t../../escape.jar")
                archive.writestr("META-INF/versions/../../escape.jar", b"jar")
            with self.assertRaisesRegex(ValueError, "Unsafe bundled path"):
                unpack_bundle(bundle, root / "artifacts")
            self.assertFalse((root / "escape.jar").exists())

    def test_missing_source_and_decompiler_failure_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jar = root / "server.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                archive.writestr("version.json", json.dumps({"id": "test"}))
                archive.writestr("Example.class", b"class")
                archive.writestr("Example$Inner.class", b"class")
            sources = root / "sources"
            sources.mkdir()
            with self.assertRaisesRegex(ValueError, "missing="):
                inspect_sources(jar, sources)
            (sources / "Example.java").write_text("// $VF: Couldn't be decompiled", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "failure_markers="):
                inspect_sources(jar, sources)
            (sources / "Example.java").write_text("class Example { class Inner {} }", encoding="utf-8")
            result = inspect_sources(jar, sources)
            self.assertEqual(result["class_count"], 2)
            self.assertEqual(result["java_file_count"], 1)


if __name__ == "__main__":
    unittest.main()
