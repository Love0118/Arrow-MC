"""Inventory every locked Java source, bundled resource, registry entry and packet.

Writes local Roadmap/catalog artifacts. Coverage is discovery evidence, not a claim
that any Vanilla functionality has been implemented. No game server is launched.
"""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess
import zipfile

from prepare_minecraft import DECOMPILE, LOCK_PATH, REPOSITORY, digest_file, write_json

ROADMAP = REPOSITORY.parent / "Roadmap"
REPORT_FILES = ("registries.json", "packets.json", "blocks.json", "commands.json", "datapack.json")


def report_hashes(reports):
    return {name: digest_file(reports / name, "sha256") for name in REPORT_FILES}


def validate_report_provenance(reports, version, server_sha256):
    provenance = json.loads((reports.parent / "report-provenance.json").read_text(encoding="utf-8"))
    if (provenance.get("minecraft_version") != version
            or provenance.get("server_sha256") != server_sha256
            or provenance.get("reports") != report_hashes(reports)):
        raise ValueError("Official report provenance mismatch; regenerate with --refresh-reports")


def source_area(relative):
    """Discovery buckets; semantic prerequisites live in the reviewed catalog."""
    path = relative.removesuffix(".java")
    if path.startswith("net/minecraft/"):
        path = path.removeprefix("net/minecraft/")
    else:
        return "external-support"
    for prefixes, area in (
        (("nbt/", "core/component/", "resources/", "core/registries/", "core/"), "data-foundations"),
        (("network/", "server/network/", "server/players/"), "network-session"),
        (("world/item/", "world/inventory/"), "items-components-inventory"),
        (("world/entity/", "world/effect/", "world/damagesource/"), "entities-ai-player"),
        (("world/ticks/",), "scheduled-ticks"),
        (("world/level/block/", "world/level/material/", "world/level/redstone/"), "blocks-fluids-redstone"),
        (("world/level/levelgen/", "world/level/biome/", "world/level/dimension/"), "worldgen-biomes-dimensions"),
        (("world/level/storage/", "world/level/chunk/", "world/level/lighting/", "server/level/"), "chunks-storage-light"),
        (("world/level/", "world/phys/"), "world-simulation"),
        (("commands/", "server/commands/", "advancements/", "stats/", "world/scores/", "server/dialog/"), "commands-progression"),
        (("data/", "gametest/", "test/", "util/datafix/"), "data-generation-migration-tests"),
        (("server/",), "server-runtime-admin"),
        (("util/", "tags/", "recipebook/", "sounds/", "world/", "locale/"), "shared-rules-support"),
    ):
        if path.startswith(prefixes):
            return area
    return "common-bootstrap-support"


def resource_area(relative):
    parts = relative.split("/")
    if len(parts) >= 4 and parts[0] == "data":
        depth = 4 if parts[2] == "worldgen" and len(parts) >= 5 else 3
        return "/".join(parts[:depth])
    if parts[0] == "assets" and len(parts) >= 4:
        return "/".join(parts[:3])
    return parts[0] if len(parts) > 1 else "jar-root-metadata"


def flatten_packets(packets):
    return [
        {"state": state, "direction": direction, "id": name, "protocol_id": details["protocol_id"]}
        for state, directions in sorted(packets.items())
        for direction, entries in sorted(directions.items())
        for name, details in sorted(entries.items())
    ]


def build_inventory(sources, server_jar, reports, version):
    source_files = []
    for path in sorted(sources.rglob("*.java")):
        relative = path.relative_to(sources).as_posix()
        source_files.append({"path": relative, "area": source_area(relative),
                             "sha256": digest_file(path, "sha256")})
    resources = []
    with zipfile.ZipFile(server_jar) as archive:
        class_files = [name for name in archive.namelist() if name.endswith(".class")]
        expected_sources = {name[:-6] + ".java" for name in class_files if "$" not in name and name != "module-info.class"}
        actual_sources = {entry["path"] for entry in source_files}
        if expected_sources != actual_sources:
            raise ValueError(f"Source coverage mismatch: missing={sorted(expected_sources-actual_sources)}, extra={sorted(actual_sources-expected_sources)}")
        embedded_version = json.loads(archive.read("version.json"))["id"]
        if embedded_version != version:
            raise ValueError("The server JAR is not the locked version")
        for name in sorted(archive.namelist()):
            if name.endswith(("/", ".class")):
                continue
            data = archive.read(name)
            resources.append({"path": name, "area": resource_area(name), "bytes": len(data),
                              "sha256": hashlib.sha256(data).hexdigest()})
    server_hash = digest_file(server_jar, "sha256")
    validate_report_provenance(reports, version, server_hash)
    registries_raw = json.loads((reports / "registries.json").read_text(encoding="utf-8"))
    registries = [
        {"id": name, "protocol_id": registry["protocol_id"], "default": registry.get("default"),
         "entries": [{"id": entry, "protocol_id": value["protocol_id"]}
                     for entry, value in sorted(registry["entries"].items())]}
        for name, registry in sorted(registries_raw.items())
    ]
    packets = flatten_packets(json.loads((reports / "packets.json").read_text(encoding="utf-8")))
    return {
        "schema_version": 1, "minecraft_version": version,
        "meaning": "Discovery coverage only. Presence never means implemented, correct or tested.",
        "server_sha256": server_hash, "official_report_hashes": report_hashes(reports),
        "counts": {"class_files": len(class_files), "java_files": len(source_files),
                   "bundled_resources": len(resources), "builtin_registries": len(registries),
                   "builtin_registry_entries": sum(len(r["entries"]) for r in registries), "packets": len(packets)},
        "source_areas": dict(sorted(Counter(entry["area"] for entry in source_files).items())),
        "resource_areas": dict(sorted(Counter(entry["area"] for entry in resources).items())),
        "java_sources": source_files, "bundled_resources": resources,
        "builtin_registries": registries, "packets": packets,
    }


def render_summary(inventory):
    counts = inventory["counts"]
    lines = ["# Vanilla 원문·데이터 전체 발견 목록", "",
             f"기준: `{inventory['minecraft_version']}`. `tools/generate_vanilla_inventory.py`로 생성했습니다.", "",
             "이 목록은 누락을 찾기 위한 입력 목록입니다. **파일/등록 항목 존재는 구현 완료나 동등성 검증을 뜻하지 않습니다.**",
             "세부 항목은 `vanilla-inventory.json`, 의미별 의존성과 완료 조건은 `data-items.md`, `world-ticks.md`, `server-gameplay.md`에 있습니다.", "",
             f"클래스 {counts['class_files']:,}개 → 최상위 Java {counts['java_files']:,}개 대조; JAR 리소스 {counts['bundled_resources']:,}개.",
             f"공식 실행 보고서: built-in registry {counts['builtin_registries']:,}개, 등록 항목 {counts['builtin_registry_entries']:,}개, packet {counts['packets']:,}개.", "",
             "동적 datapack 항목은 registry dump에 모두 포함되지 않으므로 JAR resource 목록도 별도로 보존합니다.",
             "bootstrap/support/data-generator 파일도 제거하지 않고 분류합니다. 분류는 발견 편의를 위한 것이며 구현 순서 자체가 아닙니다.", "",
             "## Java 발견 영역", "", "| 영역 | 파일 수 |", "| --- | ---: |"]
    lines += [f"| {area} | {count:,} |" for area, count in inventory["source_areas"].items()]
    lines += ["", "## Built-in registry 전체", "", "| Registry | 등록 항목 수 |", "| --- | ---: |"]
    lines += [f"| `{r['id']}` | {len(r['entries']):,} |" for r in inventory["builtin_registries"]]
    lines += ["", "## JAR 리소스 영역 전체", "", "| 영역 | 파일 수 |", "| --- | ---: |"]
    lines += [f"| `{area}` | {count:,} |" for area, count in inventory["resource_areas"].items()]
    lines += ["", "## 사용 규칙", "",
              "- 각 기능 착수 시 해당 원문·registry·resource 항목을 의미별 catalog ID와 구현/테스트 근거에 연결합니다.",
              "- component·item·block·entity는 대표 몇 개의 테스트를 전체 구현 근거로 사용하지 않습니다.",
              "- 버전 변경 시 다시 생성하고 이전 manifest와 diff하여 추가/삭제/변경된 항목을 로드맵에 반영합니다.",
              "- `--check`는 입력 목록이 최신인지 검사하며, 전체 서버 구현 완료 여부를 검사하는 명령이 아닙니다.", ""]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh-reports", action="store_true", help="Run official data generator --reports; does not start a game server")
    parser.add_argument("--check", action="store_true", help="Compare generated inventory to existing local artifacts")
    args = parser.parse_args()
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    version = lock["minecraft"]["id"]
    artifacts = DECOMPILE / "artifacts" / version
    server = artifacts / "versions" / version / f"server-{version}.jar"
    sources = DECOMPILE / "sources" / version
    report_root = ROADMAP / "research" / f"vanilla-reports-{version}"
    metadata_path = artifacts / "version-metadata.json"
    if digest_file(metadata_path, "sha1") != lock["minecraft"]["version_sha1"]:
        raise ValueError("Version metadata differs from the reference lock")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    bundle = artifacts / "server-bundler.jar"
    if digest_file(bundle, "sha1") != metadata["downloads"]["server"]["sha1"]:
        raise ValueError("Official server bundler checksum mismatch")
    with zipfile.ZipFile(bundle) as archive:
        rows = archive.read("META-INF/versions.list").decode().splitlines()
        if len(rows) != 1 or rows[0].split("\t")[1] != version:
            raise ValueError("Server bundler version differs from the reference lock")
        if digest_file(server, "sha256") != rows[0].split("\t")[0]:
            raise ValueError("Inner server JAR checksum mismatch")
    if args.refresh_reports:
        libraries = sorted((artifacts / "libraries").rglob("*.jar"))
        # A platform separator belongs to Java's classpath, not to shell syntax.
        import os
        classpath = os.pathsep.join(str(path) for path in [server, *libraries])
        report_root.mkdir(parents=True, exist_ok=True)
        with (report_root / "generator.log").open("w", encoding="utf-8") as log:
            subprocess.run(["java", "-Xmx4G", "-cp", classpath, "net.minecraft.data.Main",
                            "--reports", "--output", str(report_root)], cwd=report_root,
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        write_json(report_root / "report-provenance.json", {
            "minecraft_version": version,
            "server_sha256": digest_file(server, "sha256"),
            "generator": "net.minecraft.data.Main --reports",
            "reports": report_hashes(report_root / "reports"),
        })
    inventory = build_inventory(sources, server, report_root / "reports", version)
    catalog = ROADMAP / "catalog"
    target = catalog / "vanilla-inventory.json"
    summary = catalog / "VANILLA-INVENTORY.md"
    markdown = render_summary(inventory)
    if args.check:
        if json.loads(target.read_text(encoding="utf-8")) != inventory or summary.read_text(encoding="utf-8") != markdown:
            raise SystemExit("Inventory is stale; regenerate and review baseline changes")
    else:
        write_json(target, inventory)
        summary.write_text(markdown, encoding="utf-8")
    print(json.dumps(inventory["counts"]))
    print("PASS: inventory matches locked inputs" if args.check else target)


if __name__ == "__main__":
    main()
