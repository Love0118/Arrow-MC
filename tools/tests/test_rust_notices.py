"""Regression tests for dependency notices and embedded-component provenance."""

import hashlib
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("arrow_rust_notices", ROOT / "tools" / "collect_rust_notices.py")
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)


class RustNoticeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "package"
        self.root.mkdir()
        self.package = {"name": "fixture", "version": "1.0.0", "license": "MIT",
                        "license_file": None, "manifest_path": str(self.root / "Cargo.toml")}

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)

    def test_case_independent_selection_preserves_notice_bytes(self):
        contents = {"license-mit": b"MIT\r\n\xff", "NOTICE": b"Additional attribution\r\n",
                    "UNLICENSE": b"Public domain option\n"}
        for name, content in contents.items():
            self.write(name, content)
        self.write("copyright.pm", b"implementation source must not be distributed")
        collected = collector.package_notices(self.package)
        self.assertEqual({name: content for name, content, _ in collected}, contents)
        self.assertEqual([name for name, _, _ in collected], sorted(contents))

    def test_embedded_notices_keep_distinct_paths_and_scopes(self):
        self.write("LICENSE", b"Wrapper license")
        self.write("embedded/LICENSE", b"Separate library license\r\n")
        record = {"sha256": hashlib.sha256(b"Separate library license\r\n").hexdigest(),
                  "license": "Apache-2.0", "scope": "Embedded library"}
        with patch.dict(collector.SUPPLEMENTAL_NOTICES, {"fixture": ("1.0.0", {"embedded/LICENSE": record})}):
            collected = {name: (content, notice) for name, content, notice in collector.package_notices(self.package)}
        self.assertEqual(set(collected), {"LICENSE", "embedded/LICENSE"})
        self.assertEqual(collected["embedded/LICENSE"][1], {"source_path": "embedded/LICENSE", **record})

    def test_unreviewed_embedded_notice_cannot_be_silently_omitted(self):
        self.write("LICENSE", b"Wrapper license")
        self.write("new_component/NOTICE", b"Required extra attribution")
        with self.assertRaisesRegex(ValueError, "Review nested license notice"):
            collector.package_notices(self.package)

    def test_audited_notice_version_and_content_must_match(self):
        self.write("AUTHORS", b"An original permission notice")
        record = {"sha256": hashlib.sha256(b"An original permission notice").hexdigest(),
                  "license": "MIT", "scope": "License stored in AUTHORS"}
        with patch.dict(collector.SUPPLEMENTAL_NOTICES, {"fixture": ("1.0.0", {"AUTHORS": record})}):
            self.assertEqual(collector.package_notices(self.package)[0][0], "AUTHORS")
            self.package["version"] = "2.0.0"
            with self.assertRaisesRegex(ValueError, "Review supplementary"):
                collector.package_notices(self.package)
            self.package["version"] = "1.0.0"
            self.write("AUTHORS", b"Changed permission notice")
            with self.assertRaisesRegex(ValueError, "Audited license notice changed"):
                collector.package_notices(self.package)

    def test_explicit_license_cannot_leave_package(self):
        (self.root.parent / "outside").write_bytes(b"Not from this dependency")
        self.package["license_file"] = "../outside"
        with self.assertRaises(ValueError):
            collector.package_notices(self.package)

    def test_missing_license_is_still_a_failure(self):
        self.write("README.md", b"A description is not a license")
        with self.assertRaisesRegex(ValueError, "Missing upstream license notice"):
            collector.package_notices(self.package)

    def test_cli_reports_cargo_stderr_and_cache_preparation(self):
        diagnostic = 'error: failed to download `r-efi v6.0.0`\nCaused by:\n  --offline was specified\n'
        failure = subprocess.CalledProcessError(101, ["cargo", "metadata"], stderr=diagnostic)
        with patch.object(sys, "argv", ["collect_rust_notices.py", "--check"]), \
                patch.object(collector.subprocess, "run", side_effect=failure), \
                self.assertRaises(SystemExit) as result:
            collector.main()
        message = str(result.exception)
        self.assertIn("exit code 101", message)
        self.assertIn("cargo fetch --locked", message)
        self.assertIn(diagnostic, message)


if __name__ == "__main__":
    unittest.main()
