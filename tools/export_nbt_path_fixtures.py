"""Freeze synthetic official-JVM NBT path observations in dependency-free TSV.

Keep Java-only alias and unchecked-exception observations visible. The Rust
tests must explicitly distinguish those object/API boundaries, not erase them.
"""

import argparse
import hashlib
import json

from export_snbt_fixtures import utf16_hex
from prepare_minecraft import LOCK_PATH, REPOSITORY

FIXTURES = REPOSITORY / "tests" / "fixtures" / "nbt_path.tsv"

# A dash is absent, an empty field is a present empty value. Text uses UTF-16
# hex so lone surrogates, newlines, tabs and a literal dash remain unambiguous.
COLUMNS = (
    ("id", "raw"), ("op", "raw"), ("path", "text"),
    ("start_cursor", "number"), ("path_nodes", "number"),
    ("root", "text"), ("value", "text"), ("value_depth", "number"),
    ("values", "inputs"), ("index", "number"),
    ("expected", "text"), ("actual", "text"), ("partial", "bool"),
    ("start_depth", "number"), ("ok", "bool"), ("cursor", "number"),
    ("parsed_path", "text"), ("selected", "selected"),
    ("count", "number"), ("changed", "number"), ("match", "bool"),
    ("too_deep", "bool"), ("error_cursor", "number"),
    ("translation_key", "raw"), ("translation_args", "args"),
    ("runtime_error", "raw"), ("supplier_calls", "number"),
    ("root_after", "text"), ("root_changed", "bool"),
    ("root_too_deep", "bool"), ("same_supplier", "bool"),
    ("mutate_selected", "bool"), ("same_reference", "bool"),
    ("source_aliased", "bool"), ("supplied_after", "text"),
    ("message", "text"), ("context", "text"),
    ("root_construct", "raw"), ("value_construct", "raw"),
    ("expected_construct", "raw"), ("actual_construct", "raw"),
    ("binary_original_tag_id", "number"), ("binary_original_snbt", "text"),
    ("binary_encoded_hex", "raw"), ("binary_decoded_tag_id", "number"),
    ("binary_decoded_snbt", "text"), ("binary_meaning_equal", "bool"),
    ("binary_remaining_bytes", "number"), ("binary_decode_error", "raw"),
    ("binary_encode_error", "raw"),
)


def encode(value, kind):
    if kind == "raw":
        if not isinstance(value, str) or not value or value == "-" or any(c in value for c in "\r\n\t"):
            raise ValueError("Invalid raw field")
        return value
    if kind == "text":
        if not isinstance(value, str):
            raise ValueError("Expected text")
        return utf16_hex(value)
    if kind == "number":
        if type(value) is not int:
            raise ValueError("Expected integer")
        return str(value)
    if kind == "bool":
        if type(value) is not bool:
            raise ValueError("Expected boolean")
        return "1" if value else "0"
    if kind == "inputs":
        inputs = []
        for item in value:
            if isinstance(item, str):
                inputs.append("s:" + utf16_hex(item))
            elif isinstance(item, dict) and set(item) == {"construct"}:
                inputs.append("c:" + encode(item["construct"], "raw"))
            else:
                raise ValueError("Unrepresented constructed value")
        return ";".join(inputs)
    if kind == "selected":
        return ";".join(f"{encode(item['tag_id'], 'number')}:{encode(item['snbt'], 'text')}" for item in value)
    if kind == "args":
        arguments = []
        for item in value:
            if type(item) is bool:
                arguments.append("b:" + encode(item, "bool"))
            elif type(item) is int:
                arguments.append("n:" + str(item))
            elif isinstance(item, str):
                arguments.append("s:" + utf16_hex(item))
            else:
                raise ValueError("Unrepresented diagnostic argument type")
        return ";".join(arguments)
    raise ValueError(f"Unknown field encoding {kind}")


def export(data, source_hash, version):
    if data["minecraft_version"] != version or data["java_version"] != "25.0.3":
        raise ValueError("NBT path oracle version differs from the locked baseline")
    cases = data["cases"]
    if len({case["id"] for case in cases}) != len(cases):
        raise ValueError("Duplicate fixture IDs")
    names = {name for name, _ in COLUMNS}
    lines = [
        "# Synthetic public-API observations, not copied Minecraft implementation or game data.",
        f"# minecraft_version={version}; java_version={data['java_version']}; research_sha256={source_hash}",
        "# " + "\t".join(name for name, _ in COLUMNS),
    ]
    for original in cases:
        original = dict(original)
        if "binary_observation" in original:
            binary = original.pop("binary_observation")
            original.update(("binary_" + key, value) for key, value in binary.items())
        unknown = set(original) - names
        if unknown:
            raise ValueError(f"Unrepresented observation fields: {sorted(unknown)}")
        case = {"op": "parse", **original}
        if "ok" not in case or "supplier_calls" not in case:
            raise ValueError("Incomplete JVM observation")
        fields = ["-" if name not in case or case[name] is None else encode(case[name], kind)
                  for name, kind in COLUMNS]
        lines.append("\t".join(fields))
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    version = json.loads(LOCK_PATH.read_text(encoding="utf-8"))["minecraft"]["id"]
    roadmap = REPOSITORY.parent / "Roadmap"
    sources = [roadmap / "research" / "nbt-path-fixtures.json",
               roadmap / "reviews" / "nbt-path-review-results.json",
               roadmap / "reviews" / "nbt-end-path-results.json"]
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
            raise SystemExit("NBT path fixture export is stale; regenerate and review")
    else:
        FIXTURES.parent.mkdir(parents=True, exist_ok=True)
        FIXTURES.write_text(rendered, encoding="utf-8", newline="\n")
    print(f"{'Verified' if args.check else 'Exported'} {len(combined['cases'])} NBT path observations")


if __name__ == "__main__":
    main()
