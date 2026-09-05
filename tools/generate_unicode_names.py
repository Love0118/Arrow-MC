"""Generate Java 25-compatible Unicode 16 name data from licensed UCD files.

Generation is offline by default. --download restores only hash-pinned inputs;
--check verifies that checked-in outputs match the local, verified inputs.
Neither JDK source/tables nor JVM oracle output is an input to this generator.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import struct
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
INPUT_DIRECTORY = ROOT / "third_party" / "unicode"
OUTPUT_DIRECTORY = ROOT / "src" / "unicode_names" / "data"
MAX_NAME_BYTES = 128


@dataclass
class NameData:
    names: dict[int, str]
    ranges: list[tuple[int, int, str]]
    uppercase_ascii: dict[int, bytes]
    decimal_starts: list[int]

    def name(self, code_point: int) -> str | None:
        if code_point in self.names:
            return self.names[code_point]
        for start, end, prefix in self.ranges:
            if start <= code_point <= end:
                return f"{prefix} {code_point:X}"
        return None


def load_sources(directory: Path = INPUT_DIRECTORY, download: bool = False) -> dict[str, str]:
    manifest = json.loads((directory / "sources.json").read_text(encoding="utf-8"))
    if manifest["unicode_version"] != "16.0.0":
        raise ValueError("the Java 25 name contract requires Unicode 16.0.0")
    sources = {}
    for entry in manifest["sources"]:
        path = directory / entry["path"]
        if download:
            with urllib.request.urlopen(entry["url"], timeout=60) as response:
                content = response.read()
            if hashlib.sha256(content).hexdigest() != entry["sha256"]:
                raise ValueError(f"download hash mismatch: {entry['url']}")
            path.parent.mkdir(parents=True, exist_ok=True)
            if not path.exists() or path.read_bytes() != content:
                path.write_bytes(content)
        content = path.read_bytes()
        if hashlib.sha256(content).hexdigest() != entry["sha256"]:
            raise ValueError(f"input hash mismatch: {path}")
        sources[path.name] = content.decode("utf-8")
    return sources


def rows(text: str):
    for line in text.splitlines():
        content = line.split("#", 1)[0].strip()
        if content:
            yield [field.strip() for field in content.split(";")]


def build_name_data(sources: dict[str, str]) -> NameData:
    blocks = []
    for bounds, name in rows(sources["Blocks.txt"]):
        start, end = (int(value, 16) for value in bounds.split(".."))
        # Blocks containing unnamed assigned Unicode 16 points use the UCD name
        # with hyphens replaced by spaces. The complete JVM comparison checks
        # these actual fallback ranges; unused block aliases are not needed.
        java_name = name.upper().replace("-", " ")
        blocks.append((start, end, java_name))

    aliases = {(int(code, 16), alias): kind for code, alias, kind in rows(sources["NameAliases.txt"])}
    names = {}
    unnamed_ranges = []
    simple_upper = {}
    decimals = {}
    range_start = None
    for fields in rows(sources["UnicodeData.txt"]):
        code_point = int(fields[0], 16)
        name = fields[1]
        if name.endswith(", First>"):
            range_start = code_point
        elif name.endswith(", Last>"):
            if range_start is None:
                raise ValueError("range end without start")
            unnamed_ranges.append((range_start, code_point))
            range_start = None
        elif name == "<control>":
            if code_point == 7:
                # UCD's old BELL control name collides with the emoji's modern
                # name. Java exposes the Unicode alias BEL for this control.
                if aliases.get((7, "BEL")) != "abbreviation":
                    raise ValueError("Unicode BEL alias is missing")
                names[code_point] = "BEL"
            elif fields[10]:
                names[code_point] = fields[10]
            else:
                figments = [alias for (code, alias), kind in aliases.items() if code == code_point and kind == "figment"]
                if len(figments) == 1:
                    names[code_point] = figments[0]
                elif figments:
                    raise ValueError("ambiguous control figment alias")
                else:
                    unnamed_ranges.append((code_point, code_point))
        elif name.startswith("<"):
            raise ValueError(f"unexpected Unicode name placeholder: {name}")
        else:
            names[code_point] = name
        if fields[12]:
            simple_upper[code_point] = [int(fields[12], 16)]
        if fields[6] and code_point <= 0xFFFF:
            decimals[code_point] = int(fields[6])
    if range_start is not None:
        raise ValueError("unterminated Unicode range")

    for fields in rows(sources["SpecialCasing.txt"]):
        # Locale.ROOT uppercase uses unconditional mappings. Conditional entries
        # with root contexts only have non-ASCII output and cannot form a name.
        if len(fields) == 4 or not fields[4]:
            simple_upper[int(fields[0], 16)] = [int(value, 16) for value in fields[3].split()]
        elif not any(language in fields[4].split() for language in ("tr", "az", "lt")):
            if all(int(value, 16) < 128 for value in fields[3].split()):
                raise ValueError("contextual ASCII uppercase requires explicit support")
    uppercase_ascii = {
        code_point: bytes(mapping)
        for code_point, mapping in simple_upper.items()
        if code_point >= 128 and mapping and all(value < 128 for value in mapping)
    }
    if any(code_point > 0xFFFF or len(mapping) > 3 for code_point, mapping in uppercase_ascii.items()):
        raise ValueError("uppercase mapping exceeds the compact UTF-16 record format")

    ranges = []
    for start, end in unnamed_ranges:
        block = next((block for block in blocks if block[0] <= start <= block[1]), None)
        if block is None or end > block[1]:
            raise ValueError(f"unnamed range has no single Unicode block: {start:X}..{end:X}")
        ranges.append((start, end, block[2]))
    ranges.sort()

    decimal_starts = sorted(code_point for code_point, digit in decimals.items() if digit == 0)
    if len(decimals) != len(decimal_starts) * 10:
        raise ValueError("decimal digits are not contiguous blocks of ten")
    for start in decimal_starts:
        if any(decimals.get(start + digit) != digit for digit in range(10)):
            raise ValueError("decimal digits are not contiguous blocks of ten")

    return NameData(names, ranges, uppercase_ascii, decimal_starts)


def generate_tables(data: NameData) -> dict[str, bytes]:
    names_blob = bytearray()
    name_records = bytearray()
    names = sorted((name.encode("ascii"), code_point) for code_point, name in data.names.items())
    if len({name for name, _ in names}) != len(names):
        raise ValueError("duplicate canonical names")
    for name, code_point in names:
        if len(name) > MAX_NAME_BYTES:
            raise ValueError("canonical name exceeds normalization buffer")
        name_records.extend(struct.pack("<II", code_point, len(names_blob)))
        names_blob.extend(name)
    name_records.extend(struct.pack("<II", 0, len(names_blob)))

    prefixes = bytearray()
    range_records = bytearray()
    for start, end, prefix in data.ranges:
        encoded = prefix.encode("ascii")
        if len(encoded) + 7 > MAX_NAME_BYTES:
            raise ValueError("algorithmic name exceeds normalization buffer")
        range_records.extend(struct.pack("<IIIH", start, end, len(prefixes), len(encoded)))
        prefixes.extend(encoded)
    uppercase = bytearray()
    for code_point, mapping in sorted(data.uppercase_ascii.items()):
        uppercase.extend(struct.pack("<HB3s", code_point, len(mapping), mapping))
    digits = b"".join(struct.pack("<H", start) for start in data.decimal_starts)
    tables = {
        "names.bin": bytes(names_blob),
        "name_records.bin": bytes(name_records),
        "range_prefixes.bin": bytes(prefixes),
        "range_records.bin": bytes(range_records),
        "uppercase_ascii.bin": bytes(uppercase),
        "decimal_starts.bin": digits,
    }
    metadata = {
        "unicode_version": "16.0.0",
        "format_version": 1,
        "named_code_points": len(data.names),
        "algorithmic_ranges": len(data.ranges),
        "algorithmic_code_points": sum(end - start + 1 for start, end, _ in data.ranges),
        "uppercase_ascii_mappings": len(data.uppercase_ascii),
        "bmp_decimal_ranges": len(data.decimal_starts),
        "maximum_explicit_name_bytes": max(len(name) for name, _ in names),
        "binary_bytes": sum(len(content) for content in tables.values()),
        "files": {
            name: {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
            for name, content in tables.items()
        },
    }
    tables["metadata.json"] = (json.dumps(metadata, indent=2) + "\n").encode("utf-8")
    return tables


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if generated files differ")
    parser.add_argument("--download", action="store_true", help="restore inputs from verified official URLs")
    args = parser.parse_args()
    tables = generate_tables(build_name_data(load_sources(download=args.download)))
    OUTPUT_DIRECTORY.mkdir(parents=True, exist_ok=True)
    for name, content in tables.items():
        path = OUTPUT_DIRECTORY / name
        matches = path.exists() and path.read_bytes() == content
        if args.check and not matches:
            raise SystemExit(f"generated Unicode data differs: {path}")
        if not args.check and not matches:
            path.write_bytes(content)
    print(tables["metadata.json"].decode("utf-8"), end="")


if __name__ == "__main__":
    main()
