"""Export the pinned Vanilla configuration into the local Decompile reference.

This invokes a Java API oracle, not the dedicated server entry point. It neither
downloads artifacts nor accepts an EULA. Bulk output must stay in Decompile/bootstrap.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tempfile
import zipfile

REPOSITORY = Path(__file__).resolve().parents[1]
DECOMPILE = REPOSITORY.parent / "Decompile"
LOCK_PATH = REPOSITORY / "references.lock.json"
GENERATOR = REPOSITORY / "tools" / "oracles" / "ExportConfigurationData.java"
JSON_FILES = ("registries.json", "tags.json", "static-domains.json", "known-packs.json",
              "features.json", "export-metadata.json")


def digest_file(path, algorithm="sha256"):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def local_output(decompile_root, version, output):
    """Resolve all paths before creating a directory or invoking the oracle."""
    root = Path(decompile_root).resolve(strict=True)
    repository = REPOSITORY.resolve()
    if not root.is_dir() or root.is_relative_to(repository) or repository.is_relative_to(root):
        raise ValueError("Decompile root must be a separate local reference directory")
    bootstrap = root / "bootstrap"
    if bootstrap.resolve() != bootstrap:
        raise ValueError("Decompile/bootstrap must not redirect through a symlink")
    destination = Path(output).absolute() if output is not None else bootstrap / version
    # Reject redirects even when their final location remains within bootstrap.
    if destination.resolve() != destination:
        raise ValueError("Output must not contain symlinks or parent path traversal")
    if destination == bootstrap or not destination.is_relative_to(bootstrap):
        raise ValueError("Output must be a new directory below Decompile/bootstrap")
    if destination.exists() or destination.is_symlink():
        raise ValueError(f"Output already exists: {destination}; choose a different --output")
    return root, bootstrap, destination


def artifact_path(artifacts, category, relative):
    name = PurePosixPath(relative)
    if (not relative or "\\" in relative or name.is_absolute()
            or any(part in ("", ".", "..") for part in relative.split("/"))):
        raise ValueError(f"Unsafe bundled path: {relative}")
    candidate = artifacts / category / relative
    if candidate.resolve() != candidate or not candidate.resolve().is_relative_to(artifacts):
        raise ValueError(f"Unsafe bundled path: {relative}")
    return candidate


def verified_artifacts(root, version, lock):
    """Follow the lock's SHA1 through the official bundle's SHA256 tables."""
    minecraft = lock["minecraft"]
    if version != minecraft["id"] or not re.fullmatch(r"26\.3(?:[-.][A-Za-z0-9.-]+)?", version):
        raise ValueError("Requested version must match references.lock.json")
    artifacts = root / "artifacts" / version
    if artifacts.resolve() != artifacts:
        raise ValueError("Reference artifacts must not redirect through a symlink")
    metadata_path = artifacts / "version-metadata.json"
    if digest_file(metadata_path, "sha1") != minecraft["version_sha1"]:
        raise ValueError("Locked version metadata SHA1 mismatch")
    metadata = read_json(metadata_path)
    if metadata["id"] != version:
        raise ValueError("Version metadata ID mismatch")
    download = metadata["downloads"]["server"]
    bundle = artifacts / "server-bundler.jar"
    if bundle.stat().st_size != download["size"] or digest_file(bundle, "sha1") != download["sha1"]:
        raise ValueError("Official server bundle SHA1 or size mismatch")
    libraries = []
    with zipfile.ZipFile(bundle) as archive:
        versions = archive.read("META-INF/versions.list").decode("utf-8").splitlines()
        if len(versions) != 1:
            raise ValueError("Expected exactly one inner server JAR")
        expected, bundle_version, relative = versions[0].split("\t")
        if bundle_version != version:
            raise ValueError("Bundled server version mismatch")
        artifact_path(artifacts, "versions", relative)
        bundled_server = archive.read(f"META-INF/versions/{relative}")
        if hashlib.sha256(bundled_server).hexdigest() != expected:
            raise ValueError("Bundled server SHA256 mismatch")
        server = artifacts / f"server-{version}.jar"
        if server.is_symlink() or digest_file(server) != expected:
            raise ValueError("Extracted server SHA256 mismatch")
        seen = set()
        for row in archive.read("META-INF/libraries.list").decode("utf-8").splitlines():
            checksum, _, relative = row.split("\t")
            library = artifact_path(artifacts, "libraries", relative)
            if library in seen or library.suffix != ".jar":
                raise ValueError("Duplicate or non-JAR bundled library")
            seen.add(library)
            if digest_file(library) != checksum:
                raise ValueError(f"Bundled library SHA256 mismatch: {relative}")
            libraries.append(library)
    with zipfile.ZipFile(server) as archive:
        inner_version = json.loads(archive.read("version.json"))
    protocol = inner_version["protocol_version"]
    if inner_version["id"] != version or type(protocol) is not int or not 0 <= protocol <= 0x7FFFFFFF:
        raise ValueError("Inner server version or protocol mismatch")
    return server, libraries, protocol, {
        "version_metadata": {"sha1": minecraft["version_sha1"]},
        "server_bundle": {"sha1": download["sha1"], "bytes": download["size"]},
    }


def file_record(root, path):
    return {"path": path.relative_to(root).as_posix(), "bytes": path.stat().st_size,
            "sha256": digest_file(path)}


def validate_export(root, version, protocol, source_jar):
    """Reject incomplete, redirected, or inconsistent helper output before publishing."""
    records = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink() or path.resolve() != path:
            raise ValueError("Export must not contain symlinks")
        if path.is_file():
            relative = path.relative_to(root).as_posix()
            if relative not in JSON_FILES and not re.fullmatch(r"entries/[0-9]+\.nbt", relative):
                raise ValueError(f"Unexpected export file: {relative}")
            records[relative] = file_record(root, path)
        elif path != root / "entries":
            raise ValueError(f"Unexpected export directory: {path}")
    if not all(name in records for name in JSON_FILES):
        raise ValueError("Missing required export JSON file")
    metadata = read_json(root / "export-metadata.json")
    if (metadata.get("minecraft_version") != version or metadata.get("protocol") != protocol
            or metadata.get("selected_pack_ids") != ["vanilla"]
            or metadata.get("source_jar") != source_jar):
        raise ValueError("Export metadata does not match the pinned Vanilla source")
    packs = read_json(root / "known-packs.json")
    if (not isinstance(packs, list) or not packs or metadata.get("known_packs") != packs
            or any(not isinstance(pack, dict) or set(pack) != {"namespace", "id", "version"}
                   or any(not isinstance(value, str) or not value for value in pack.values())
                   for pack in packs)):
        raise ValueError("Invalid known-pack metadata")
    features = read_json(root / "features.json")
    if (not isinstance(features, list) or not features
            or any(not isinstance(feature, str) or not feature for feature in features)
            or len(set(features)) != len(features)):
        raise ValueError("Invalid enabled feature list")
    registries = read_json(root / "registries.json")
    if not isinstance(registries, list) or not registries:
        raise ValueError("Missing configuration registries")
    referenced = set()
    registry_ids = set()
    for registry in registries:
        registry_id = registry["id"]
        if registry_id in registry_ids or not isinstance(registry_id, str) or not registry_id:
            raise ValueError("Duplicate or invalid configuration registry ID")
        registry_ids.add(registry_id)
        entry_ids = set()
        for index, entry in enumerate(registry["entries"]):
            entry_id = entry["id"]
            if entry_id in entry_ids or not isinstance(entry_id, str) or not entry_id:
                raise ValueError("Duplicate or invalid registry entry ID")
            entry_ids.add(entry_id)
            if type(entry["protocol_id"]) is not int or entry["protocol_id"] != index:
                raise ValueError("Registry protocol IDs must be contiguous and ordered")
            pack = entry["known_pack"]
            if pack is not None and pack not in packs:
                raise ValueError("Registry entry refers to an unknown pack")
            relative = entry["network_nbt_file"]
            if not isinstance(relative, str) or not re.fullmatch(r"entries/[0-9]+\.nbt", relative):
                raise ValueError("Unsafe registry entry NBT path")
            if relative in referenced or relative not in records:
                raise ValueError("Duplicate or missing registry entry NBT file")
            referenced.add(relative)
            record = records[relative]
            if (type(entry["bytes"]) is not int or entry["bytes"] <= 0
                    or record["bytes"] != entry["bytes"] or record["sha256"] != entry["sha256"]):
                raise ValueError("Registry entry NBT digest or size mismatch")
    if referenced != {name for name in records if name.startswith("entries/")}:
        raise ValueError("Unreferenced registry entry NBT file")
    # The Rust consumer validates domain identities and tag members in detail.
    for name in ("tags.json", "static-domains.json"):
        if not isinstance(read_json(root / name), list):
            raise ValueError(f"Expected an array in {name}")
    return [records[name] for name in sorted(records)]


def prepare(decompile_root=DECOMPILE, version=None, java="java", output=None):
    lock = read_json(LOCK_PATH)
    version = version or lock["minecraft"]["id"]
    if version != lock["minecraft"]["id"]:
        raise ValueError("Requested version must match references.lock.json")
    root, bootstrap, destination = local_output(decompile_root, version, output)
    server, libraries, protocol, provenance = verified_artifacts(root, version, lock)
    source_jar = {"sha256": digest_file(server), "bytes": server.stat().st_size}
    provenance["generator"] = {"path": "tools/oracles/ExportConfigurationData.java",
                               "sha256": digest_file(GENERATOR)}
    bootstrap.mkdir(exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=".configuration-", dir=bootstrap)).resolve()
    try:
        # Java logging may create files in cwd; keep them outside the exported bundle.
        export = stage / "export"
        export.mkdir()
        command = [java, "-Xmx2G", "-cp", os.pathsep.join(map(str, [server, *libraries])),
                   str(GENERATOR), str(export)]
        provenance["command"] = command
        subprocess.run(command, cwd=stage, check=True)
        files = validate_export(export, version, protocol, source_jar)
        manifest = {
            "format_version": 1, "minecraft_version": version, "protocol": protocol,
            "configuration": "vanilla-only", "source_jar": source_jar,
            "selected_packs": [{"id": "vanilla", "version": version,
                                "hash_kind": "source_jar_sha256", "sha256": source_jar["sha256"]}],
            "files": files, "provenance": provenance,
        }
        write_json(export / "manifest.json", manifest)
        # Recheck immediately before publishing; never merge with an existing directory.
        local_output(root, version, destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        export.rename(destination)
        return destination
    finally:
        if stage.exists():
            if (stage.parent != bootstrap or stage.resolve() != stage
                    or not stage.name.startswith(".configuration-") or stage.is_symlink()):
                raise ValueError("Refusing to remove an unsafe staging path")
            shutil.rmtree(stage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--decompile-root", type=Path, default=DECOMPILE)
    parser.add_argument("--version", help="Must match references.lock.json")
    parser.add_argument("--java", default="java", help="Java executable matching the pinned server")
    parser.add_argument("--output", type=Path, help="New directory below Decompile/bootstrap")
    args = parser.parse_args()
    try:
        destination = prepare(args.decompile_root, args.version, args.java, args.output)
    except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Configuration preparation failed: {error}\n")
    print(f"Prepared local Vanilla configuration: {destination}")
    print(f"Trusted manifest SHA-256: {digest_file(destination / 'manifest.json')}")


if __name__ == "__main__":
    main()
