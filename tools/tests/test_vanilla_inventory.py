import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from generate_vanilla_inventory import (
    REPORT_FILES, build_inventory, flatten_packets, report_hashes,
    resource_area, source_area, validate_report_provenance,
)


class VanillaInventoryTests(unittest.TestCase):
    def test_source_buckets_keep_special_domains_distinct(self):
        self.assertEqual(source_area("net/minecraft/core/component/DataComponents.java"), "data-foundations")
        self.assertEqual(source_area("net/minecraft/world/ticks/LevelTicks.java"), "scheduled-ticks")
        self.assertEqual(source_area("net/minecraft/world/level/redstone/NeighborUpdater.java"), "blocks-fluids-redstone")
        self.assertEqual(source_area("net/minecraft/world/entity/ai/goal/target/TargetGoal.java"), "entities-ai-player")
        self.assertEqual(source_area("net/minecraft/server/network/Listener.java"), "network-session")

    def test_dynamic_resources_and_packets_preserve_state_and_identity(self):
        self.assertEqual(resource_area("data/minecraft/worldgen/biome/plains.json"), "data/minecraft/worldgen/biome")
        self.assertEqual(resource_area("data/minecraft/recipe/test.json"), "data/minecraft/recipe")
        self.assertEqual(flatten_packets({"login": {"clientbound": {"minecraft:hello": {"protocol_id": 1}}}}),
                         [{"state": "login", "direction": "clientbound", "id": "minecraft:hello", "protocol_id": 1}])

    def test_missing_java_source_is_a_failure_not_coverage_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = root / "sources"
            sources.mkdir()
            jar = root / "server.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                archive.writestr("Example.class", b"not-used")
                archive.writestr("version.json", json.dumps({"id": "test"}))
            with self.assertRaisesRegex(ValueError, "Source coverage mismatch"):
                build_inventory(sources, jar, root / "reports", "test")

    def test_reports_must_match_version_jar_and_generated_content(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            reports = root / "reports"
            reports.mkdir()
            for name in REPORT_FILES:
                (reports / name).write_text("{}", encoding="utf-8")
            provenance = {"minecraft_version": "test", "server_sha256": "trusted",
                          "reports": report_hashes(reports)}
            (root / "report-provenance.json").write_text(json.dumps(provenance), encoding="utf-8")
            validate_report_provenance(reports, "test", "trusted")
            for version, checksum in (("other", "trusted"), ("test", "wrong")):
                with self.assertRaisesRegex(ValueError, "provenance mismatch"):
                    validate_report_provenance(reports, version, checksum)
            (reports / "packets.json").write_text('{"changed":true}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "provenance mismatch"):
                validate_report_provenance(reports, "test", "trusted")


if __name__ == "__main__":
    unittest.main()
