import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import prepare_block_state_data as block_states
import prepare_configuration_data as configuration

VERSION = "26.3-pre-2"
PROTOCOL = 1073742158


class BlockStatePreparationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.workspace = Path(temporary.name).resolve()
        self.repository = self.workspace / "implementation"
        self.repository.mkdir()
        self.root = self.workspace / "Decompile"
        self.config_root = self.root / "bootstrap" / VERSION
        self.config_root.mkdir(parents=True)
        self.lock_path = self.repository / "references.lock.json"
        self.lock = {"minecraft": {"id": VERSION}}
        configuration.write_json(self.lock_path, self.lock)
        self.generator = self.repository / "ExportBlockStateData.java"
        self.generator.write_text("// independently written synthetic test helper", encoding="utf-8")
        self.server = self.root / "server.jar"
        self.server.write_bytes(b"synthetic verified server artifact")
        self.source_jar = {"sha256": configuration.digest_file(self.server),
                           "bytes": self.server.stat().st_size}
        for module, name, value in ((configuration, "REPOSITORY", self.repository),
                                    (block_states, "LOCK_PATH", self.lock_path),
                                    (block_states, "GENERATOR", self.generator)):
            patcher = patch.object(module, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        artifact_patch = patch.object(block_states, "verified_artifacts", return_value=(
            self.server, [], PROTOCOL, {"server_bundle": {"sha1": "test", "bytes": 1}}))
        self.artifacts = artifact_patch.start()
        self.addCleanup(artifact_patch.stop)
        self.write_configuration()

    def write_configuration(self):
        for name in configuration.JSON_FILES:
            configuration.write_json(self.config_root / name, [])
        entries = self.config_root / "entries"
        entries.mkdir(exist_ok=True)
        for index in range(2):
            (entries / f"{index:05}.nbt").write_bytes(b"\x0a\x00")
        configuration.write_json(self.config_root / "registries.json", [{
            "id": "minecraft:worldgen/biome", "entries": [
                {"id": "minecraft:zeta", "protocol_id": 0},
                {"id": "minecraft:alpha", "protocol_id": 1}]}])
        # Static numeric order intentionally differs from Java block export order.
        configuration.write_json(self.config_root / "static-domains.json", [
            {"id": "minecraft:block", "entries": [
                {"id": "minecraft:example", "protocol_id": 0},
                {"id": "minecraft:air", "protocol_id": 1}]},
            {"id": "minecraft:block_entity_type", "entries": [
                {"id": "minecraft:furnace", "protocol_id": 0},
                {"id": "minecraft:chest", "protocol_id": 1},
                {"id": "minecraft:sign", "protocol_id": 2}]}])
        configuration.write_json(self.config_root / "tags.json", [{
            "id": "minecraft:block", "tags": [
                {"id": "minecraft:blocks_motion_in_heightmap", "members": [0]},
                {"id": "minecraft:blocks_motion_in_heightmap_no_leaves", "members": [0]}]}])
        self.manifest = {
            "format_version": 1, "minecraft_version": VERSION, "protocol": PROTOCOL,
            "configuration": "vanilla-only", "source_jar": self.source_jar,
            "selected_packs": block_states.selected_packs(VERSION, self.source_jar), "files": [],
        }
        self.trust_configuration(refresh_descriptors=True)

    def trust_configuration(self, refresh_descriptors=False):
        if refresh_descriptors:
            self.manifest["files"] = [configuration.file_record(self.config_root, path)
                                      for path in sorted(self.config_root.rglob("*"))
                                      if path.is_file() and path.name != "manifest.json"]
        configuration.write_json(self.config_root / "manifest.json", self.manifest)
        self.trusted_digest = configuration.digest_file(self.config_root / "manifest.json")

    def write_blocks(self, root):
        data = {
            "state_count": 5, "state_flags": [1, 0, 2, 3, 2], "blocks": [
                {"id": "minecraft:air", "default_state": 0, "properties": [], "states": [0]},
                {"id": "minecraft:example", "default_state": 4, "properties": [
                    {"name": "facing", "values": ["south", "north"], "default_index": 1},
                    {"name": "waterlogged", "values": ["true", "false"], "default_index": 0}],
                 "states": [3, 1, 4, 2]}],
        }
        configuration.write_json(root / "blocks.json", data)
        configuration.write_json(root / "export-metadata.json", {
            "minecraft_version": VERSION, "protocol": PROTOCOL, "source_jar": self.source_jar,
            "block_count": 2, "state_count": 5,
        })

    def fake_java(self, command, **options):
        root = Path(command[-1])
        self.assertEqual(command[:3], ["test-java", "-Xmx2G", "-cp"])
        self.assertEqual(command[3:5], [str(self.server), str(self.generator)])
        self.assertEqual(options, {"cwd": root.parent, "check": True})
        logs = root.parent / "logs"
        logs.mkdir()
        (logs / "latest.log").write_text("test Java logs", encoding="utf-8")
        self.write_blocks(root)
        return subprocess.CompletedProcess(command, 0)

    def prepare(self, **options):
        return block_states.prepare(options.pop("configuration_manifest_sha256", self.trusted_digest),
                                    self.root, java="test-java", **options)

    def change_json(self, root, name, mutation):
        value = configuration.read_json(root / name)
        mutation(value)
        configuration.write_json(root / name, value)

    def test_preserves_biome_order_binds_trusted_identity_and_hashes_every_output(self):
        with patch.object(block_states.subprocess, "run", side_effect=self.fake_java) as java:
            output = self.prepare()
        java.assert_called_once()
        self.artifacts.assert_called_once_with(self.root, VERSION, self.lock)
        self.assertEqual(output, self.root / "bootstrap" / (VERSION + "-block-states-v2"))
        self.assertEqual(configuration.read_json(output / "biomes.json"), [
            {"id": "minecraft:zeta", "protocol_id": 0}, {"id": "minecraft:alpha", "protocol_id": 1}])
        manifest = configuration.read_json(output / "manifest.json")
        self.assertEqual(manifest["format_version"], 2)
        self.assertEqual(manifest["configuration_manifest_sha256"], self.trusted_digest)
        self.assertEqual(manifest["selected_packs"], self.manifest["selected_packs"])
        self.assertEqual(manifest["source_jar"], self.source_jar)
        self.assertEqual([record["path"] for record in manifest["files"]], sorted(block_states.JSON_FILES))
        for record in manifest["files"]:
            self.assertEqual(record, configuration.file_record(output, output / record["path"]))
        self.assertEqual(manifest["provenance"]["generator"]["sha256"],
                         configuration.digest_file(self.generator))
        self.assertFalse((output / "logs").exists())
        self.assertFalse(list(output.parent.glob(".block-states-*")))
        blocks = configuration.read_json(output / "blocks.json")
        self.assertEqual(len((output / "blocks.json").read_text(encoding="utf-8").splitlines()), 1)
        self.assertEqual([block["heightmap_tags"] for block in blocks["blocks"]], [0, 3])
        self.assertEqual(blocks["state_count"], 5)
        self.assertEqual(blocks["state_flags"], [1, 0, 2, 3, 2])
        self.assertEqual(blocks["blocks"][1]["states"], [3, 1, 4, 2])
        self.assertEqual(blocks["blocks"][1]["default_state"], 4)
        self.assertEqual(configuration.read_json(output / "block-entity-types.json"), [
            {"id": "minecraft:furnace", "protocol_id": 0},
            {"id": "minecraft:chest", "protocol_id": 1},
            {"id": "minecraft:sign", "protocol_id": 2}])

    def test_missing_wrong_or_malformed_external_digest_is_rejected_before_java(self):
        for digest in (None, "", "trusted", "0" * 64, self.trusted_digest.upper()):
            with self.subTest(digest=digest), patch.object(block_states.subprocess, "run") as java:
                with self.assertRaisesRegex(ValueError, "digest|SHA256"):
                    self.prepare(configuration_manifest_sha256=digest)
                java.assert_not_called()

    def test_manifest_tampering_is_not_accepted_by_recomputing_its_own_digest(self):
        self.manifest["protocol"] = 1
        configuration.write_json(self.config_root / "manifest.json", self.manifest)
        with patch.object(block_states.subprocess, "run") as java:
            with self.assertRaisesRegex(ValueError, "trusted digest"):
                self.prepare()
        java.assert_not_called()

    def test_trusted_manifest_must_still_match_pinned_identity(self):
        mutations = [lambda m: m.update(format_version=True), lambda m: m.update(minecraft_version="26.4"),
                     lambda m: m.update(protocol=1), lambda m: m.update(configuration="custom"),
                     lambda m: m.update(source_jar={"sha256": "0" * 64, "bytes": 1}),
                     lambda m: m.update(selected_packs=[]),
                     lambda m: m["selected_packs"][0].update(sha256="0" * 64)]
        for mutation in mutations:
            self.write_configuration()
            mutation(self.manifest)
            self.trust_configuration()
            with patch.object(block_states.subprocess, "run") as java:
                with self.assertRaisesRegex(ValueError, "identity"):
                    self.prepare()
                java.assert_not_called()

    def test_configuration_file_corruption_missing_and_extra_data_are_rejected(self):
        cases = [lambda: (self.config_root / "entries/00000.nbt").write_bytes(b"corrupt"),
                 lambda: (self.config_root / "tags.json").unlink(),
                 lambda: (self.config_root / "unexpected.txt").write_text("extra", encoding="utf-8")]
        for mutation in cases:
            self.write_configuration()
            mutation()
            with patch.object(block_states.subprocess, "run") as java:
                with self.assertRaisesRegex(ValueError, "digest|Missing|Unexpected"):
                    self.prepare()
                java.assert_not_called()

    def test_manifest_descriptor_admission_rejects_unsafe_duplicate_or_false_size(self):
        mutations = [lambda m: m["files"][0].update(path="../outside.json"),
                     lambda m: m["files"].append(m["files"][0].copy()),
                     lambda m: m["files"][0].update(bytes=True),
                     lambda m: m["files"][0].update(bytes=m["files"][0]["bytes"] + 1),
                     lambda m: m["files"].pop()]
        for mutation in mutations:
            self.write_configuration()
            mutation(self.manifest)
            self.trust_configuration()
            with patch.object(block_states.subprocess, "run") as java:
                with self.assertRaises(ValueError):
                    self.prepare()
                java.assert_not_called()

    def test_biome_registry_requires_unique_contiguous_ids_in_source_order(self):
        mutations = [lambda r: r[0]["entries"][0].update(protocol_id=1),
                     lambda r: r[0]["entries"][0].update(protocol_id=False),
                     lambda r: r[0]["entries"][1].update(id="minecraft:zeta"),
                     lambda r: r[0].update(entries=[]), lambda r: r.append(r[0].copy())]
        for mutation in mutations:
            self.write_configuration()
            self.change_json(self.config_root, "registries.json", mutation)
            self.trust_configuration(refresh_descriptors=True)
            with patch.object(block_states.subprocess, "run") as java:
                with self.assertRaisesRegex(ValueError, "biome|Biome"):
                    self.prepare()
                java.assert_not_called()

    def test_domains_and_tags_are_parsed_from_the_exact_authenticated_bytes(self):
        original_read = Path.read_bytes
        mutations = {
            "registries.json": lambda data: data[0]["entries"][0].update(id="minecraft:untrusted"),
            "static-domains.json": lambda data: data[1]["entries"][0].update(id="minecraft:untrusted"),
            "tags.json": lambda data: data[0]["tags"][0].update(members=[1]),
        }
        replacements = []

        def replacing_read(path):
            contents = original_read(path)
            if path.parent == self.config_root and path.name in mutations:
                replacements.append(path.name)
                self.change_json(self.config_root, path.name, mutations[path.name])
            return contents

        with patch.object(Path, "read_bytes", replacing_read), \
                patch.object(block_states.subprocess, "run", side_effect=self.fake_java):
            output = self.prepare()
        biomes = configuration.read_json(output / "biomes.json")
        self.assertCountEqual(replacements, list(mutations))
        self.assertEqual(configuration.read_json(self.config_root / "registries.json")[0]["entries"][0]["id"],
                         "minecraft:untrusted")
        self.assertEqual(configuration.read_json(self.config_root / "static-domains.json")[1]["entries"][0]["id"],
                         "minecraft:untrusted")
        self.assertEqual(configuration.read_json(self.config_root / "tags.json")[0]["tags"][0]["members"], [1])
        self.assertEqual(biomes[0], {"id": "minecraft:zeta", "protocol_id": 0})
        self.assertEqual(configuration.read_json(output / "block-entity-types.json")[0]["id"], "minecraft:furnace")
        self.assertEqual([block["heightmap_tags"] for block in configuration.read_json(output / "blocks.json")["blocks"]],
                         [0, 3])

    def test_heightmap_tag_bits_resolve_static_ids_independently_of_export_order(self):
        for flags in range(4):
            with self.subTest(flags=flags):
                self.write_configuration()
                for bit in range(2):
                    self.change_json(self.config_root, "tags.json", lambda data:
                                     data[0]["tags"][bit].update(members=[0] if flags & (1 << bit) else []))
                self.trust_configuration(refresh_descriptors=True)
                output = self.root / "bootstrap" / f"tag-bits-{flags}"
                with patch.object(block_states.subprocess, "run", side_effect=self.fake_java):
                    self.prepare(output=output)
                blocks = configuration.read_json(output / "blocks.json")
                self.assertEqual([(block["id"], block["heightmap_tags"]) for block in blocks["blocks"]],
                                 [("minecraft:air", 0), ("minecraft:example", flags)])
                self.assertEqual(blocks["state_flags"], [1, 0, 2, 3, 2])
                self.assertEqual(blocks["blocks"][1]["states"], [3, 1, 4, 2])

    def test_required_static_domains_reject_missing_names_duplicates_and_invalid_ids(self):
        mutations = [lambda data: data.pop(0), lambda data: data.pop(1),
                     lambda data: data[0].update(id="minecraft:wrong"),
                     lambda data: data[1].update(entries=[]),
                     lambda data: data.append(data[0].copy()),
                     lambda data: data[0]["entries"][0].pop("id"),
                     lambda data: data[0]["entries"][1].update(protocol_id=0),
                     lambda data: data[1]["entries"][0].update(protocol_id=False),
                     lambda data: data[1]["entries"][1].update(id="minecraft:furnace"),
                     lambda data: data[1]["entries"][2].update(protocol_id=3)]
        for index, mutation in enumerate(mutations):
            with self.subTest(case=index):
                self.write_configuration()
                self.change_json(self.config_root, "static-domains.json", mutation)
                self.trust_configuration(refresh_descriptors=True)
                with patch.object(block_states.subprocess, "run") as java:
                    with self.assertRaisesRegex(ValueError, "domain|Registry"):
                        self.prepare()
                    java.assert_not_called()

    def test_required_heightmap_tags_reject_missing_wrong_domain_and_invalid_members(self):
        mutations = [lambda data: data.clear(), lambda data: data.append(data[0].copy()),
                     lambda data: data[0].update(id="minecraft:biome"),
                     lambda data: data[0]["tags"].pop(),
                     lambda data: data[0]["tags"][0].update(id="minecraft:wrong"),
                     lambda data: data[0]["tags"].append(data[0]["tags"][0].copy()),
                     lambda data: data[0]["tags"][0].update(members=[2]),
                     lambda data: data[0]["tags"][0].update(members=[-1]),
                     lambda data: data[0]["tags"][0].update(members=[False]),
                     lambda data: data[0]["tags"][0].update(members=["minecraft:example"]),
                     lambda data: data[0]["tags"][0].update(members=[0, 0]),
                     lambda data: data[0]["tags"][0].pop("members")]
        for index, mutation in enumerate(mutations):
            with self.subTest(case=index):
                self.write_configuration()
                self.change_json(self.config_root, "tags.json", mutation)
                self.trust_configuration(refresh_descriptors=True)
                with patch.object(block_states.subprocess, "run") as java:
                    with self.assertRaisesRegex(ValueError, "tag"):
                        self.prepare()
                    java.assert_not_called()

    def test_exported_block_names_must_equal_the_authenticated_static_domain(self):
        for extra in (False, True):
            with self.subTest(extra=extra):
                self.write_configuration()
                mutation = (lambda data: data[0]["entries"].append({"id": "minecraft:extra", "protocol_id": 2})) \
                    if extra else (lambda data: data[0]["entries"][0].update(id="minecraft:other"))
                self.change_json(self.config_root, "static-domains.json", mutation)
                self.trust_configuration(refresh_descriptors=True)
                with patch.object(block_states.subprocess, "run", side_effect=self.fake_java):
                    with self.assertRaisesRegex(ValueError, "Exported block names"):
                        self.prepare()
                self.assertEqual(list(self.config_root.parent.iterdir()), [self.config_root])

    def test_schema_v2_default_preserves_existing_v1_bundle(self):
        previous = self.config_root.parent / (VERSION + "-block-states")
        previous.mkdir()
        (previous / "manifest.json").write_text("preserved v1", encoding="utf-8")
        with patch.object(block_states.subprocess, "run", side_effect=self.fake_java):
            output = self.prepare()
        self.assertEqual(configuration.read_json(output / "manifest.json")["format_version"], 2)
        self.assertEqual((previous / "manifest.json").read_text(encoding="utf-8"), "preserved v1")

    def test_output_and_configuration_boundaries_preserve_existing_files(self):
        for output in (self.repository / "bulk", self.root / "sources" / VERSION,
                       self.root / "bootstrap", self.config_root, self.config_root / "nested"):
            with self.subTest(output=output), patch.object(block_states.subprocess, "run") as java:
                with self.assertRaises(ValueError):
                    self.prepare(output=output)
                java.assert_not_called()
        for config_root in (self.root, self.repository, self.root / "bootstrap"):
            with self.subTest(config_root=config_root), self.assertRaises(ValueError):
                self.prepare(configuration_root=config_root)
        self.assertTrue((self.config_root / "manifest.json").is_file())

    def test_invalid_export_counts_flags_properties_and_global_states_are_rejected(self):
        mutations = {
            "state count": lambda d: d.update(state_count=4),
            "unknown flag": lambda d: d["state_flags"].__setitem__(0, 4),
            "boolean flag": lambda d: d["state_flags"].__setitem__(0, True),
            "default mapping": lambda d: d["blocks"][1].update(default_state=3),
            "state overlap": lambda d: d["blocks"][1]["states"].__setitem__(0, 0),
            "negative ID": lambda d: d["blocks"][1]["states"].__setitem__(0, -1),
            "state gap": lambda d: d["blocks"].pop(),
            "property order": lambda d: d["blocks"][1]["properties"].reverse(),
            "repeated value": lambda d: d["blocks"][1]["properties"][0].update(values=["north", "north"]),
            "default index": lambda d: d["blocks"][1]["properties"][0].update(default_index=2),
            "property product": lambda d: d["blocks"][1]["properties"][0]["values"].append("east"),
            "block ID": lambda d: d["blocks"][1].update(id="minecraft:air"),
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label):
                def export(command, **options):
                    self.fake_java(command, **options)
                    self.change_json(Path(command[-1]), "blocks.json", mutation)
                with patch.object(block_states.subprocess, "run", side_effect=export):
                    with self.assertRaises(ValueError):
                        self.prepare()
                self.assertEqual(list(self.config_root.parent.iterdir()), [self.config_root])

    def test_export_metadata_must_match_source_and_counts(self):
        for field, value in (("minecraft_version", "26.4"), ("protocol", 1), ("block_count", 3),
                             ("state_count", 4), ("source_jar", {"sha256": "0" * 64, "bytes": 1})):
            with self.subTest(field=field):
                def export(command, **options):
                    self.fake_java(command, **options)
                    self.change_json(Path(command[-1]), "export-metadata.json", lambda m: m.update({field: value}))
                with patch.object(block_states.subprocess, "run", side_effect=export):
                    with self.assertRaises(ValueError):
                        self.prepare()

    def test_failed_java_and_concurrent_destination_cleanup_preserves_source(self):
        def fail(command, **options):
            self.fake_java(command, **options)
            raise subprocess.CalledProcessError(1, command)
        with patch.object(block_states.subprocess, "run", side_effect=fail):
            with self.assertRaises(subprocess.CalledProcessError):
                self.prepare()
        self.assertEqual(list(self.config_root.parent.iterdir()), [self.config_root])
        destination = self.config_root.parent / (VERSION + "-block-states-v2")
        def concurrent(command, **options):
            self.fake_java(command, **options)
            destination.mkdir()
            (destination / "keep.txt").write_text("other writer", encoding="utf-8")
        with patch.object(block_states.subprocess, "run", side_effect=concurrent):
            with self.assertRaisesRegex(ValueError, "already exists"):
                self.prepare()
        self.assertEqual((destination / "keep.txt").read_text(encoding="utf-8"), "other writer")
        self.assertEqual(configuration.digest_file(self.config_root / "manifest.json"), self.trusted_digest)
        self.assertFalse(list(destination.parent.glob(".block-states-*")))

    def test_cli_requires_external_trust_and_prints_new_manifest_digest_outside_bundle(self):
        with patch.object(sys, "argv", ["prepare_block_state_data.py"]), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                block_states.main()
        self.assertEqual(error.exception.code, 2)
        arguments = ["prepare_block_state_data.py", "--decompile-root", str(self.root),
                     "--java", "test-java", "--configuration-manifest-sha256", self.trusted_digest]
        stdout = io.StringIO()
        with patch.object(sys, "argv", arguments), contextlib.redirect_stdout(stdout), \
                patch.object(block_states.subprocess, "run", side_effect=self.fake_java):
            block_states.main()
        output = self.config_root.parent / (VERSION + "-block-states-v2")
        digest = configuration.digest_file(output / "manifest.json")
        self.assertIn(f"Trusted block-state manifest SHA256: {digest}", stdout.getvalue())
        self.assertEqual({path.name for path in output.iterdir()}, {*block_states.JSON_FILES, "manifest.json"})


if __name__ == "__main__":
    unittest.main()
