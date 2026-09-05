import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import prepare_configuration_data as configuration


VERSION = "26.3-pre-2"
PROTOCOL = 1073742158


def checksum(data, algorithm="sha256"):
    return hashlib.new(algorithm, data).hexdigest()


class ConfigurationPreparationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name).resolve()
        self.repository = self.workspace / "implementation"
        self.repository.mkdir()
        self.root = self.workspace / "Decompile"
        self.artifacts = self.root / "artifacts" / VERSION
        self.artifacts.mkdir(parents=True)
        self.generator = self.repository / "ExportConfigurationData.java"
        self.generator.write_text("// Synthetic test helper", encoding="utf-8")
        self.lock_path = self.repository / "references.lock.json"
        for name, value in (("REPOSITORY", self.repository), ("LOCK_PATH", self.lock_path),
                            ("GENERATOR", self.generator)):
            patcher = patch.object(configuration, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.write_artifacts()

    def write_artifacts(self, protocol=PROTOCOL, relative=None):
        inner = io.BytesIO()
        with zipfile.ZipFile(inner, "w") as archive:
            archive.writestr("version.json", json.dumps({"id": VERSION, "protocol_version": protocol}))
        self.server_data = inner.getvalue()
        (self.artifacts / f"server-{VERSION}.jar").write_bytes(self.server_data)
        library_data = b"synthetic library whose contents must match the official table"
        library = self.artifacts / "libraries" / "example" / "library.jar"
        library.parent.mkdir(parents=True, exist_ok=True)
        library.write_bytes(library_data)
        bundle = self.artifacts / "server-bundler.jar"
        relative = relative or f"{VERSION}/server-{VERSION}.jar"
        with zipfile.ZipFile(bundle, "w") as archive:
            archive.writestr("META-INF/versions.list", f"{checksum(self.server_data)}\t{VERSION}\t{relative}")
            archive.writestr(f"META-INF/versions/{relative}", self.server_data)
            archive.writestr("META-INF/libraries.list", f"{checksum(library_data)}\texample:library:1\texample/library.jar")
            archive.writestr("META-INF/libraries/example/library.jar", library_data)
        metadata = {"id": VERSION, "downloads": {"server": {
            "sha1": configuration.digest_file(bundle, "sha1"), "size": bundle.stat().st_size}}}
        metadata_path = self.artifacts / "version-metadata.json"
        configuration.write_json(metadata_path, metadata)
        self.lock = {"minecraft": {"id": VERSION,
                                  "version_sha1": configuration.digest_file(metadata_path, "sha1")}}
        configuration.write_json(self.lock_path, self.lock)

    def write_export(self, root):
        packs = [{"namespace": "minecraft", "id": "core", "version": VERSION}]
        entries = root / "entries"
        entries.mkdir()
        data = b"\x0a\x00"
        (entries / "00000.nbt").write_bytes(data)
        source = {"sha256": checksum(self.server_data), "bytes": len(self.server_data)}
        files = {
            "registries.json": [{"id": "minecraft:worldgen/biome", "entries": [{
                "id": "minecraft:plains", "protocol_id": 0, "known_pack": packs[0],
                "network_nbt_file": "entries/00000.nbt", "bytes": len(data), "sha256": checksum(data)}]}],
            "tags.json": [{"id": "minecraft:worldgen/biome", "tags": [
                {"id": "minecraft:is_overworld", "members": [0]}]}],
            "static-domains.json": [{"id": "minecraft:block", "entries": [
                {"id": "minecraft:air", "protocol_id": 0}]}],
            "features.json": ["minecraft:vanilla"], "known-packs.json": packs,
            "export-metadata.json": {"minecraft_version": VERSION, "protocol": PROTOCOL,
                                     "source_jar": source, "selected_pack_ids": ["vanilla"],
                                     "known_packs": packs},
        }
        for name, value in files.items():
            configuration.write_json(root / name, value)

    def fake_java(self, command, **options):
        root = Path(command[-1])
        self.assertEqual(command[:3], ["test-java", "-Xmx2G", "-cp"])
        self.assertEqual(command[-2], str(self.generator))
        self.assertEqual(options, {"cwd": root.parent, "check": True})
        classpath = command[3].split(os.pathsep)
        self.assertEqual(classpath, [str(self.artifacts / f"server-{VERSION}.jar"),
                                     str(self.artifacts / "libraries/example/library.jar")])
        # Vanilla's logger writes relative to cwd even when the helper output differs.
        logs = root.parent / "logs"
        logs.mkdir()
        (logs / "latest.log").write_text("synthetic Java log", encoding="utf-8")
        self.write_export(root)
        return subprocess.CompletedProcess(command, 0)

    def prepare(self, **kwargs):
        return configuration.prepare(self.root, java="test-java", **kwargs)

    def test_prepares_verified_manifest_and_preserves_all_entry_data(self):
        with patch.object(configuration.subprocess, "run", side_effect=self.fake_java) as java:
            destination = self.prepare()
        java.assert_called_once()
        self.assertEqual(destination, self.root / "bootstrap" / VERSION)
        manifest = configuration.read_json(destination / "manifest.json")
        self.assertEqual(manifest["format_version"], 1)
        self.assertEqual(manifest["protocol"], PROTOCOL)
        self.assertEqual(manifest["configuration"], "vanilla-only")
        self.assertEqual(manifest["source_jar"], {"sha256": checksum(self.server_data),
                                                "bytes": len(self.server_data)})
        self.assertEqual([entry["path"] for entry in manifest["files"]],
                         sorted([*configuration.JSON_FILES, "entries/00000.nbt"]))
        for record in manifest["files"]:
            data = (destination / record["path"]).read_bytes()
            self.assertEqual((record["bytes"], record["sha256"]), (len(data), checksum(data)))
        provenance = manifest["provenance"]
        self.assertEqual(provenance["generator"]["sha256"], configuration.digest_file(self.generator))
        self.assertEqual(provenance["version_metadata"]["sha1"], self.lock["minecraft"]["version_sha1"])
        self.assertEqual(provenance["command"][0], "test-java")
        self.assertFalse(list(destination.parent.glob(".configuration-*")))
        self.assertFalse((destination / "eula.txt").exists())
        self.assertFalse((destination / "logs").exists())

    def test_output_boundaries_and_existing_data_are_rejected_before_java(self):
        destinations = [self.repository / "data", self.root, self.root / "bootstrap",
                        self.root / "sources" / VERSION, self.root / "artifacts" / VERSION,
                        self.root.parent / "Roadmap", self.root / "bootstrap" / ".." / "sources"]
        existing = self.root / "bootstrap" / "existing"
        existing.mkdir(parents=True)
        (existing / "keep.txt").write_text("retained", encoding="utf-8")
        destinations.append(existing)
        with patch.object(configuration.subprocess, "run") as java:
            for destination in destinations:
                with self.subTest(destination=destination), self.assertRaises(ValueError):
                    self.prepare(output=destination)
        java.assert_not_called()
        self.assertEqual((existing / "keep.txt").read_text(encoding="utf-8"), "retained")

    def test_reference_root_cannot_be_repository_or_its_ancestor(self):
        for root in (self.repository, self.workspace):
            with self.subTest(root=root), self.assertRaisesRegex(ValueError, "separate local reference"):
                configuration.local_output(root, VERSION, None)

    def test_bootstrap_symlink_redirect_is_rejected(self):
        target = self.workspace / "outside"
        target.mkdir()
        try:
            (self.root / "bootstrap").symlink_to(target, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"This account cannot create symlinks: {error}")
        with patch.object(configuration.subprocess, "run") as java:
            with self.assertRaisesRegex(ValueError, "symlink"):
                self.prepare()
        java.assert_not_called()
        self.assertEqual(list(target.iterdir()), [])

    def test_every_locked_artifact_is_verified_before_java(self):
        for relative in ("version-metadata.json", "server-bundler.jar", f"server-{VERSION}.jar",
                         "libraries/example/library.jar"):
            with self.subTest(relative=relative):
                self.write_artifacts()
                path = self.artifacts / relative
                path.write_bytes(path.read_bytes() + b"corrupt")
                with patch.object(configuration.subprocess, "run") as java:
                    with self.assertRaisesRegex(ValueError, "mismatch"):
                        self.prepare()
                java.assert_not_called()
                self.assertFalse((self.root / "bootstrap").exists())

    def test_version_mismatch_and_unsafe_bundle_member_are_rejected(self):
        with patch.object(configuration.subprocess, "run") as java:
            with self.assertRaisesRegex(ValueError, "references.lock"):
                self.prepare(version="26.4")
            self.write_artifacts(relative="../../escape.jar")
            with self.assertRaisesRegex(ValueError, "Unsafe bundled path"):
                self.prepare()
        java.assert_not_called()

    def test_failed_java_cleans_only_stage_and_preserves_previous_output(self):
        previous = self.root / "bootstrap" / "previous"
        previous.mkdir(parents=True)
        (previous / "keep.txt").write_text("keep", encoding="utf-8")

        def fail(command, **options):
            logs = Path(options["cwd"], "logs")
            logs.mkdir()
            (logs / "latest.log").write_text("failure log", encoding="utf-8")
            Path(command[-1], "partial.json").write_text("partial", encoding="utf-8")
            raise subprocess.CalledProcessError(1, command)

        with patch.object(configuration.subprocess, "run", side_effect=fail):
            with self.assertRaises(subprocess.CalledProcessError):
                self.prepare()
        self.assertEqual(list(previous.parent.iterdir()), [previous])
        self.assertEqual((previous / "keep.txt").read_text(encoding="utf-8"), "keep")

    def test_destination_created_during_java_execution_is_preserved(self):
        destination = self.root / "bootstrap" / VERSION

        def export(command, **options):
            self.fake_java(command, **options)
            destination.mkdir()
            (destination / "keep.txt").write_text("other writer", encoding="utf-8")

        with patch.object(configuration.subprocess, "run", side_effect=export):
            with self.assertRaisesRegex(ValueError, "already exists"):
                self.prepare()
        self.assertEqual(list(destination.parent.iterdir()), [destination])
        self.assertEqual((destination / "keep.txt").read_text(encoding="utf-8"), "other writer")

    def test_invalid_helper_metadata_and_entry_payloads_cannot_be_published(self):
        mutations = {
            "wrong protocol": lambda root: self.change_json(root, "export-metadata.json", lambda v: v.update(protocol=1)),
            "wrong source": lambda root: self.change_json(root, "export-metadata.json", lambda v: v["source_jar"].update(sha256="0" * 64)),
            "entry corruption": lambda root: (root / "entries/00000.nbt").write_bytes(b"corrupt"),
            "entry escape": lambda root: self.change_json(root, "registries.json", lambda v: v[0]["entries"][0].update(network_nbt_file="../secret.nbt")),
            "entry gap": lambda root: self.change_json(root, "registries.json", lambda v: v[0]["entries"][0].update(protocol_id=1)),
            "extra entry": lambda root: (root / "entries/00001.nbt").write_bytes(b"\x0a\x00"),
            "missing tags": lambda root: (root / "tags.json").unlink(),
            "unexpected file": lambda root: (root / "eula.txt").write_text("eula=true", encoding="utf-8"),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label):
                def export(command, **options):
                    self.fake_java(command, **options)
                    mutate(Path(command[-1]))
                with patch.object(configuration.subprocess, "run", side_effect=export):
                    with self.assertRaises(ValueError):
                        self.prepare()
                self.assertEqual(list((self.root / "bootstrap").iterdir()), [])

    def change_json(self, root, name, mutate):
        value = configuration.read_json(root / name)
        mutate(value)
        configuration.write_json(root / name, value)


if __name__ == "__main__":
    unittest.main()
