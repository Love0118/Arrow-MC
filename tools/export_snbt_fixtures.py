"""Freeze independently selected official-JVM SNBT cases as dependency-free TSV.

The local research JSON contains synthetic inputs and actual Java outcomes, not
copied implementation. UTF-16 hex preserves unpaired surrogates on every host.
"""

import argparse
import hashlib
import json
from pathlib import Path

from prepare_minecraft import LOCK_PATH, REPOSITORY

FIXTURES = REPOSITORY / "tests" / "fixtures" / "snbt.tsv"


def utf16_hex(value):
    return value.encode("utf-16-be", errors="surrogatepass").hex()


def tree_text(tree):
    tag = tree["tag_id"]
    if tag == 0:
        return "0"
    if tag in (1, 2, 3, 4):
        value = tree["value"]
        if tag != 3:
            value = value[:-1]
        return f"{tag}:{value}"
    if tag in (5, 6):
        return f"{tag}:{tree['raw_bits']}"
    if tag == 8:
        return "8:" + "".join(f"{unit:04x}" for unit in tree["utf16_units"])
    if tag == 9:
        return "9:[" + ",".join(map(tree_text, tree["values"])) + "]"
    if tag == 10:
        # Java sorts keys lexicographically by UTF-16 units, not Unicode scalar.
        entries = sorted(tree["entries"], key=lambda e: utf16_hex(e["key"]))
        return "10:{" + ",".join(utf16_hex(e["key"]) + "=" + tree_text(e["value"]) for e in entries) + "}"
    if tag in (7, 11, 12):
        return f"{tag}:[" + ",".join(str(v) for v in tree["values"]) + "]"
    raise ValueError(f"Unexpected tag type {tag}")


def export(data, source_hash, version):
    if data["minecraft_version"] != version:
        raise ValueError("SNBT oracle fixture version differs from reference lock")
    if len({case["id"] for case in data["cases"]}) != len(data["cases"]):
        raise ValueError("Duplicate fixture IDs")
    lines = [
        "# Synthetic cases observed through official Minecraft Java APIs; no JAR/source included.",
        f"# minecraft_version={version}; java_version={data['java_version']}; research_sha256={source_hash}",
        "# id\tmode\tstart_utf16\tinput_utf16_hex\toutcome\tcursor\ttyped_tree\tcompact_snbt_utf16_hex\terror_translation_key\terror_argument_utf16_hex_or_dash",
    ]
    for case in data["cases"]:
        if "runtime_error" in case:
            raise ValueError(f"Unresolved oracle failure: {case['id']}")
        mode = case.get("mode", "fully")
        if "construct" in case:
            mode = "construct:" + case["construct"]
        elif "depth" in case:
            mode = "depth:" + str(case["depth"])
        input_value = case.get("input", "")
        tree = tree_text(case["tree"]) if "tree" in case else ""
        cursor = case.get("cursor", "") if case["ok"] else case.get("error_cursor", "")
        # Compound-only parse uses a separate Java reader; a recorded success
        # cursor from the original harness is unrelated to the parsed value.
        if mode == "compound" and case["ok"]:
            cursor = ""
        arguments = case.get("translation_args", [])
        if len(arguments) > 1:
            raise ValueError(f"New multi-argument diagnostic requires fixture schema update: {case['id']}")
        fields = [case["id"], mode, str(case.get("start_cursor", 0)), utf16_hex(input_value),
                  "ok" if case["ok"] else "error", str(cursor), tree,
                  utf16_hex(case.get("canonical_snbt", "")), case.get("translation_key", ""),
                  utf16_hex(str(arguments[0])) if arguments else "-"]
        if any("\t" in value or "\n" in value for value in fields):
            raise ValueError(f"Unsafe TSV field: {case['id']}")
        lines.append("\t".join(fields))
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    version = json.loads(LOCK_PATH.read_text(encoding="utf-8"))["minecraft"]["id"]
    roadmap = REPOSITORY.parent / "Roadmap"
    sources = [roadmap / "reviews" / "snbt-all-diagnostics.json",
               roadmap / "reviews" / "snbt-numeric-diagnostics.json"]
    combined = {"minecraft_version": version, "java_version": "25.0.3", "cases": []}
    hashes = []
    for source in sources:
        raw = source.read_bytes()
        data = json.loads(raw)
        if data["minecraft_version"] != version or data["java_version"] != combined["java_version"]:
            raise ValueError(f"Oracle version mismatch: {source}")
        combined["cases"].extend(data["cases"])
        hashes.append(hashlib.sha256(raw).hexdigest())
    source_hash = hashlib.sha256("\n".join(hashes).encode("ascii")).hexdigest()
    rendered = export(combined, source_hash, version)
    if args.check:
        if FIXTURES.read_text(encoding="utf-8") != rendered:
            raise SystemExit("SNBT fixture export is stale; regenerate and review")
    else:
        FIXTURES.parent.mkdir(parents=True, exist_ok=True)
        FIXTURES.write_text(rendered, encoding="utf-8")
    print(f"{'Verified' if args.check else 'Exported'} {len(combined['cases'])} SNBT oracle cases")


if __name__ == "__main__":
    main()
