"""Prepare local Vanilla block states bound to an independently trusted configuration.

The configuration manifest SHA256 must come from a trusted record outside its bundle.
No artifacts are downloaded and the dedicated server is never launched.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import zipfile

import prepare_configuration_data as configuration
from prepare_configuration_data import (DECOMPILE, LOCK_PATH, REPOSITORY, digest_file,
                                        file_record, local_output, read_json,
                                        verified_artifacts, write_json)

GENERATOR = REPOSITORY / "tools" / "oracles" / "ExportBlockStateData.java"
JSON_FILES = ("blocks.json", "biomes.json", "block-entity-types.json", "export-metadata.json")
HEIGHTMAP_TAGS = ("minecraft:blocks_motion_in_heightmap",
                 "minecraft:blocks_motion_in_heightmap_no_leaves")
SHA256 = re.compile(r"[0-9a-f]{64}")
IDENTIFIER = re.compile(r"[a-z0-9_.-]+:[a-z0-9/._-]+")


def selected_packs(version, source_jar):
    return [{"id": "vanilla", "version": version, "hash_kind": "source_jar_sha256",
             "sha256": source_jar["sha256"]}]


def ordered_domain(domains, registry_id):
    if not isinstance(domains, list):
        raise ValueError(f"Invalid registry array for {registry_id}")
    matching = [domain for domain in domains
                if isinstance(domain, dict) and domain.get("id") == registry_id]
    if len(matching) != 1 or not isinstance(matching[0].get("entries"), list) or not matching[0]["entries"]:
        raise ValueError(f"Expected exactly one nonempty configuration domain: {registry_id}")
    entries = []
    names = set()
    for index, entry in enumerate(matching[0]["entries"]):
        if (not isinstance(entry, dict) or not isinstance(entry.get("id"), str)
                or not IDENTIFIER.fullmatch(entry["id"]) or entry["id"] in names
                or type(entry.get("protocol_id")) is not int or entry["protocol_id"] != index):
            raise ValueError(f"Registry IDs must be unique, contiguous and ordered: {registry_id}")
        names.add(entry["id"])
        entries.append({"id": entry["id"], "protocol_id": index})
    return entries


def heightmap_membership(tags, blocks):
    if not isinstance(tags, list):
        raise ValueError("Invalid configuration tag array")
    matching = [domain for domain in tags
                if isinstance(domain, dict) and domain.get("id") == "minecraft:block"]
    if len(matching) != 1 or not isinstance(matching[0].get("tags"), list):
        raise ValueError("Expected exactly one block tag domain")
    flags = {block["id"]: 0 for block in blocks}
    for bit, tag_id in enumerate(HEIGHTMAP_TAGS):
        matches = [tag for tag in matching[0]["tags"]
                   if isinstance(tag, dict) and tag.get("id") == tag_id]
        if len(matches) != 1 or not isinstance(matches[0].get("members"), list):
            raise ValueError(f"Missing or duplicate heightmap tag: {tag_id}")
        members = matches[0]["members"]
        if (any(type(member) is not int or not 0 <= member < len(blocks) for member in members)
                or len(set(members)) != len(members)):
            raise ValueError(f"Invalid block-domain members in heightmap tag: {tag_id}")
        for member in members:
            flags[blocks[member]["id"]] |= 1 << bit
    return flags


def verified_configuration(root, manifest_sha256, version, protocol, source_jar):
    """Authenticate the manifest before trusting any descriptor or registry order."""
    if not isinstance(manifest_sha256, str) or not SHA256.fullmatch(manifest_sha256):
        raise ValueError("Provide an independently trusted lowercase configuration manifest SHA256")
    manifest_path = root / "manifest.json"
    if manifest_path.resolve() != manifest_path or not manifest_path.is_file():
        raise ValueError("Unsafe or missing configuration manifest")
    contents = manifest_path.read_bytes()
    if hashlib.sha256(contents).hexdigest() != manifest_sha256:
        raise ValueError("Configuration manifest SHA256 differs from the trusted digest")
    manifest = json.loads(contents)
    if (not isinstance(manifest, dict)
            or type(manifest.get("format_version")) is not int or manifest["format_version"] != 1
            or manifest.get("minecraft_version") != version
            or type(manifest.get("protocol")) is not int or manifest["protocol"] != protocol
            or manifest.get("configuration") != "vanilla-only"
            or manifest.get("source_jar") != source_jar
            or manifest.get("selected_packs") != selected_packs(version, source_jar)):
        raise ValueError("Configuration manifest identity differs from the pinned Vanilla source")
    descriptors = manifest.get("files")
    if not isinstance(descriptors, list) or not descriptors:
        raise ValueError("Missing configuration file descriptors")
    expected = {}
    for record in descriptors:
        if not isinstance(record, dict) or set(record) != {"path", "bytes", "sha256"}:
            raise ValueError("Invalid configuration file descriptor")
        relative = record["path"]
        if (not isinstance(relative, str)
                or relative not in configuration.JSON_FILES
                and not re.fullmatch(r"entries/[0-9]+\.nbt", relative)):
            raise ValueError("Unsafe configuration file descriptor path")
        if (relative in expected or type(record["bytes"]) is not int or record["bytes"] <= 0
                or not isinstance(record["sha256"], str) or not SHA256.fullmatch(record["sha256"])):
            raise ValueError("Duplicate or invalid configuration file descriptor")
        expected[relative] = record
    if not all(name in expected for name in configuration.JSON_FILES):
        raise ValueError("Missing required configuration descriptor")
    found = set()
    captured = {}
    for path in root.rglob("*"):
        if path.is_symlink() or path.resolve() != path:
            raise ValueError("Configuration files must not redirect through symlinks")
        if path.is_dir():
            if path != root / "entries":
                raise ValueError("Unexpected configuration directory")
            continue
        relative = path.relative_to(root).as_posix()
        if relative == "manifest.json":
            continue
        if not path.is_file() or relative not in expected:
            raise ValueError("Unexpected configuration file")
        if relative in ("registries.json", "static-domains.json", "tags.json"):
            # Parse the same bytes we authenticate, even if another process replaces the file.
            contents = path.read_bytes()
            captured[relative] = contents
            actual = {"path": relative, "bytes": len(contents),
                      "sha256": hashlib.sha256(contents).hexdigest()}
        else:
            actual = file_record(root, path)
        if actual != expected[relative]:
            raise ValueError(f"Configuration file digest or size mismatch: {relative}")
        found.add(relative)
    if found != set(expected):
        raise ValueError("Missing configuration file")
    biomes = ordered_domain(json.loads(captured["registries.json"]), "minecraft:worldgen/biome")
    static_domains = json.loads(captured["static-domains.json"])
    blocks = ordered_domain(static_domains, "minecraft:block")
    block_entities = ordered_domain(static_domains, "minecraft:block_entity_type")
    heightmap_tags = heightmap_membership(json.loads(captured["tags.json"]), blocks)
    return biomes, heightmap_tags, block_entities


def validate_export(root, version, protocol, source_jar):
    for path in root.iterdir():
        if (path.is_symlink() or path.resolve() != path or not path.is_file()
                or path.name not in ("blocks.json", "export-metadata.json")):
            raise ValueError("Unexpected block-state export file")
    metadata = read_json(root / "export-metadata.json")
    if (not isinstance(metadata, dict) or metadata.get("minecraft_version") != version
            or type(metadata.get("protocol")) is not int or metadata["protocol"] != protocol
            or metadata.get("source_jar") != source_jar):
        raise ValueError("Block-state export metadata differs from the pinned source")
    data = read_json(root / "blocks.json")
    if not isinstance(data, dict) or set(data) != {"state_count", "state_flags", "blocks"}:
        raise ValueError("Invalid block-state export schema")
    count = data["state_count"]
    flags = data["state_flags"]
    blocks = data["blocks"]
    if (type(count) is not int or count <= 0 or not isinstance(flags, list) or len(flags) != count
            or any(type(flag) is not int or not 0 <= flag <= 3 for flag in flags)
            or not isinstance(blocks, list) or not blocks
            or type(metadata.get("block_count")) is not int or metadata["block_count"] != len(blocks)
            or type(metadata.get("state_count")) is not int or metadata["state_count"] != count):
        raise ValueError("Block/state counts or state flags are inconsistent")
    block_ids = set()
    state_ids = set()
    for block in blocks:
        if (not isinstance(block, dict) or set(block) != {"id", "default_state", "properties", "states"}
                or not isinstance(block["id"], str) or not IDENTIFIER.fullmatch(block["id"])
                or block["id"] in block_ids or not isinstance(block["properties"], list)
                or not isinstance(block["states"], list) or type(block["default_state"]) is not int):
            raise ValueError("Invalid or duplicate block definition")
        block_ids.add(block["id"])
        previous_name = ""
        combinations = 1
        default_index = 0
        for prop in block["properties"]:
            if (not isinstance(prop, dict) or set(prop) != {"name", "values", "default_index"}
                    or not isinstance(prop["name"], str) or not prop["name"]
                    or prop["name"] <= previous_name or not isinstance(prop["values"], list)
                    or not prop["values"]
                    or any(not isinstance(value, str) or not value for value in prop["values"])
                    or len(set(prop["values"])) != len(prop["values"])
                    or type(prop["default_index"]) is not int
                    or not 0 <= prop["default_index"] < len(prop["values"])):
                raise ValueError("Invalid block property domain, ordering or default")
            previous_name = prop["name"]
            combinations *= len(prop["values"])
            default_index = default_index * len(prop["values"]) + prop["default_index"]
            if combinations > count:
                raise ValueError("Block property combinations exceed the state count")
        states = block["states"]
        if (len(states) != combinations
                or any(type(state) is not int or not 0 <= state < count for state in states)
                or len(set(states)) != len(states) or state_ids.intersection(states)
                or states[default_index] != block["default_state"]):
            raise ValueError("Block state mapping, ownership or default is inconsistent")
        state_ids.update(states)
    if len(state_ids) != count:
        raise ValueError("Block-state export does not cover every global state ID")
    return data


def prepare(configuration_manifest_sha256, decompile_root=DECOMPILE, version=None,
            java="java", output=None, configuration_root=None):
    lock = read_json(LOCK_PATH)
    version = version or lock["minecraft"]["id"]
    if version != lock["minecraft"]["id"]:
        raise ValueError("Requested version must match references.lock.json")
    root, bootstrap, destination = local_output(decompile_root, version + "-block-states-v2", output)
    config_root = Path(configuration_root).absolute() if configuration_root else bootstrap / version
    if (config_root.resolve() != config_root or not config_root.is_dir()
            or config_root == bootstrap or not config_root.is_relative_to(bootstrap)
            or config_root.is_relative_to(destination) or destination.is_relative_to(config_root)):
        raise ValueError("Configuration must be a separate directory below Decompile/bootstrap")
    server, libraries, protocol, provenance = verified_artifacts(root, version, lock)
    source_jar = {"sha256": digest_file(server), "bytes": server.stat().st_size}
    biomes, heightmap_tags, block_entities = verified_configuration(
        config_root, configuration_manifest_sha256, version, protocol, source_jar)
    provenance["generator"] = {"path": "tools/oracles/ExportBlockStateData.java",
                               "sha256": digest_file(GENERATOR)}
    provenance["preparer"] = {"path": "tools/prepare_block_state_data.py",
                              "sha256": digest_file(Path(__file__))}
    stage = Path(tempfile.mkdtemp(prefix=".block-states-", dir=bootstrap)).resolve()
    try:
        export = stage / "export"
        export.mkdir()
        command = [java, "-Xmx2G", "-cp", os.pathsep.join(map(str, [server, *libraries])),
                   str(GENERATOR), str(export)]
        provenance["command"] = command
        subprocess.run(command, cwd=stage, check=True)
        blocks = validate_export(export, version, protocol, source_jar)
        if {block["id"] for block in blocks["blocks"]} != set(heightmap_tags):
            raise ValueError("Exported block names differ from the authenticated static block domain")
        for block in blocks["blocks"]:
            block["heightmap_tags"] = heightmap_tags[block["id"]]
        # Compact the large state arrays so consumer admission is charged for useful data.
        (export / "blocks.json").write_text(
            json.dumps(blocks, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
        write_json(export / "biomes.json", biomes)
        write_json(export / "block-entity-types.json", block_entities)
        manifest = {
            "format_version": 2, "minecraft_version": version, "protocol": protocol,
            "source_jar": source_jar, "selected_packs": selected_packs(version, source_jar),
            "configuration_manifest_sha256": configuration_manifest_sha256,
            "files": [file_record(export, export / name) for name in sorted(JSON_FILES)],
            "provenance": provenance,
        }
        write_json(export / "manifest.json", manifest)
        local_output(root, version + "-block-states-v2", destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        export.rename(destination)
        return destination
    finally:
        if stage.exists():
            if (stage.parent != bootstrap or stage.resolve() != stage
                    or not stage.name.startswith(".block-states-") or stage.is_symlink()):
                raise ValueError("Refusing to remove an unsafe block-state staging path")
            shutil.rmtree(stage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--configuration-manifest-sha256", required=True,
                        help="Previously trusted digest from outside the configuration bundle")
    parser.add_argument("--configuration-root", type=Path,
                        help="Local configuration bundle; defaults to bootstrap/<version>")
    parser.add_argument("--decompile-root", type=Path, default=DECOMPILE)
    parser.add_argument("--version", help="Must match references.lock.json")
    parser.add_argument("--java", default="java")
    parser.add_argument("--output", type=Path, help="New directory below Decompile/bootstrap")
    args = parser.parse_args()
    try:
        destination = prepare(args.configuration_manifest_sha256, args.decompile_root, args.version,
                              args.java, args.output, args.configuration_root)
    except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Block-state preparation failed: {error}\n")
    print(f"Prepared local Vanilla block states: {destination}")
    print(f"Trusted block-state manifest SHA256: {digest_file(destination / 'manifest.json')}")


if __name__ == "__main__":
    main()
