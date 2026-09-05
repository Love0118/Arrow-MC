"""Prepare the official server as a local, versioned research reference.

Python 3.12+, Java matching the selected server (currently 25), no Python packages.
Run from any directory; paths derive from this script's repository, never cwd.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
from urllib.request import Request, urlopen
import zipfile

REPOSITORY = Path(__file__).resolve().parents[1]
DECOMPILE = REPOSITORY.parent / "Decompile"
LOCK_PATH = REPOSITORY / "references.lock.json"
FAILURE_MARKERS = ("$VF:", "Couldn't be decompiled", "could not be decompiled")


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def digest_file(path, algorithm):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def download(url, destination, algorithm, expected):
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() and digest_file(destination, algorithm) == expected:
        return
    partial = destination.with_suffix(destination.suffix + ".part")
    request = Request(url, headers={"User-Agent": "Arrow-MC-reference-setup"})
    with urlopen(request, timeout=60) as response, partial.open("wb") as output:
        shutil.copyfileobj(response, output)
    if digest_file(partial, algorithm) != expected:
        raise ValueError(f"Hash mismatch: {url}; rejected file retained at {partial}")
    partial.replace(destination)


def select_version(manifest, requested):
    candidates = [v for v in manifest["versions"] if re.fullmatch(r"26\.3(?:[-.].+)?", v["id"])]
    if requested == "latest":
        releases = [v for v in candidates if v["type"] == "release"]
        candidates = releases or candidates
    else:
        candidates = [v for v in candidates if v["id"] == requested]
    if not candidates:
        raise ValueError(f"No official 26.3 version matches {requested!r}")
    return max(candidates, key=lambda v: v["releaseTime"])


def unpack_bundle(bundle, artifacts):
    server = None
    libraries = []
    with zipfile.ZipFile(bundle) as archive:
        for table in ("versions", "libraries"):
            rows = archive.read(f"META-INF/{table}.list").decode().splitlines()
            if table == "versions" and len(rows) != 1:
                raise ValueError("Expected exactly one inner server JAR")
            for row in rows:
                expected, _, relative = row.split("\t")
                data = archive.read(f"META-INF/{table}/{relative}")
                if hashlib.sha256(data).hexdigest() != expected:
                    raise ValueError(f"Bundled SHA-256 mismatch: {relative}")
                # Preserve library hierarchy and prevent extraction outside artifacts.
                parent = artifacts / table
                destination = (parent / relative).resolve()
                if not destination.is_relative_to(parent.resolve()):
                    raise ValueError(f"Unsafe bundled path: {relative}")
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(data)
                if table == "versions":
                    server = destination
                else:
                    libraries.append(destination)
    return server, libraries


def inspect_sources(server, sources):
    with zipfile.ZipFile(server) as archive:
        classes = [n for n in archive.namelist() if n.endswith(".class")]
        game_version = json.loads(archive.read("version.json"))
    # Inner/anonymous classes are normally folded into their enclosing Java file.
    expected = {n[:-6] + ".java" for n in classes if "$" not in n and n != "module-info.class"}
    actual = {p.relative_to(sources).as_posix() for p in sources.rglob("*.java")}
    missing = sorted(expected - actual)
    failures = []
    for relative in sorted(actual):
        text = (sources / relative).read_text(encoding="utf-8")
        if any(marker in text for marker in FAILURE_MARKERS):
            failures.append(relative)
    if missing or failures:
        raise ValueError(f"Incomplete decompilation: missing={missing}, failure_markers={failures}")
    return {
        "game_version": game_version,
        "class_count": len(classes),
        "top_level_class_count": len(expected),
        "java_file_count": len(actual),
        "missing_top_level_sources": missing,
        "decompiler_failure_files": failures,
        "named_server_class_present": "net/minecraft/server/MinecraftServer.class" in classes,
        "server_mappings_present": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", help="Official 26.3 version or 'latest'; default: locked version")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--verify-existing", action="store_true", help="Verify and reuse local decompiled sources")
    args = parser.parse_args()
    if args.threads < 1:
        parser.error("--threads must be positive")
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    minecraft = lock["minecraft"]
    if args.version:
        with urlopen(minecraft["manifest_url"], timeout=60) as response:
            selected = select_version(json.load(response), args.version)
        minecraft.update(id=selected["id"], type=selected["type"],
                         version_url=selected["url"], version_sha1=selected["sha1"])
    version = minecraft["id"]
    if not re.fullmatch(r"26\.3(?:[-.][A-Za-z0-9.-]+)?", version):
        raise ValueError(f"Invalid reference version: {version}")
    artifacts = DECOMPILE / "artifacts" / version
    reports = DECOMPILE / "reports" / version
    sources = DECOMPILE / "sources" / version
    reports.mkdir(parents=True, exist_ok=True)
    metadata_path = artifacts / "version-metadata.json"
    download(minecraft["version_url"], metadata_path, "sha1", minecraft["version_sha1"])
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if metadata["id"] != version:
        raise ValueError("Version metadata does not match the selected version")
    server_download = metadata["downloads"]["server"]
    bundle = artifacts / "server-bundler.jar"
    download(server_download["url"], bundle, "sha1", server_download["sha1"])
    if bundle.stat().st_size != server_download["size"]:
        raise ValueError("Server download size mismatch")
    server, libraries = unpack_bundle(bundle, artifacts)
    vineflower = lock["vineflower"]
    decompiler = DECOMPILE / "tools" / f"vineflower-{vineflower['version']}.jar"
    download(vineflower["url"], decompiler, "sha256", vineflower["sha256"])
    java_version = subprocess.run(["java", "-version"], capture_output=True, text=True, check=True)
    if not re.search(r'version "' + str(metadata["javaVersion"]["majorVersion"]) + r'[.\"]', java_version.stderr):
        raise ValueError(f"Use Java {metadata['javaVersion']['majorVersion']} for this reference")
    if not args.verify_existing:
        if sources.exists() and any(sources.iterdir()):
            raise ValueError(f"Sources already exist: {sources}. Use --verify-existing or archive them before rebuilding.")
        sources.mkdir(parents=True, exist_ok=True)
        command = ["java", "-Xmx6G", "-jar", str(decompiler), "--folder", "--log-level=WARN",
                   f"--thread-count={args.threads}"]
        command += [f"--add-external={p}" for p in libraries]
        command += [str(server), str(sources)]
        with (reports / "decompiler.log").open("w", encoding="utf-8") as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
    result = inspect_sources(server, sources)
    result["server_mappings_present"] = "server_mappings" in metadata["downloads"]
    javap = subprocess.run(["javap", "-p", "-classpath", str(server),
                            "net.minecraft.world.entity.ai.goal.GoalSelector"],
                           capture_output=True, text=True, check=True)
    (reports / "GoalSelector.javap.txt").write_text(javap.stdout, encoding="utf-8")
    readable = all(name in javap.stdout for name in ("addGoal", "tickRunningGoals", "availableGoals"))
    if not result["named_server_class_present"] or not readable:
        raise ValueError("Expected readable Minecraft class/member names; inspect obfuscation before proceeding")
    result.update(obfuscation="readable class, method and field names verified",
                  server_download=server_download,
                  inner_server_sha256=digest_file(server, "sha256"),
                  vineflower=vineflower, java_runtime=java_version.stderr.strip(),
                  library_count=len(libraries), sources=str(sources),
                  verification_mode="existing sources" if args.verify_existing else "fresh decompilation")
    write_json(reports / "provenance.json", result)
    if args.version:
        write_json(LOCK_PATH, lock)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
