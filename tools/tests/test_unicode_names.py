"""Unicode data reproducibility and optional complete Java 25 comparison.

Set ARROW_MC_UNICODE_JAVA_ORACLE=1 to run the JVM/Rust comparison. It compiles a
temporary, independent batch driver against the exact Rust module. Java output
is temporary verification evidence only, never a generator input.
"""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("arrow_unicode_generator", ROOT / "tools" / "generate_unicode_names.py")
generator = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = generator
SPEC.loader.exec_module(generator)


JAVA_ORACLE = r"""
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.Locale;

class UnicodeNameOracle {
    public static void main(String[] args) throws Exception {
        if (args[0].equals("names")) {
            try (var output = Files.newBufferedWriter(Path.of(args[1]), StandardCharsets.US_ASCII)) {
                for (int cp = 0; cp <= Character.MAX_CODE_POINT; cp++) {
                    String name = Character.getName(cp);
                    if (name != null) {
                        if (Character.codePointOf(name) != cp) throw new AssertionError(cp);
                        output.write(Integer.toHexString(cp) + "\t" + name + "\n");
                    }
                }
            }
        } else if (args[0].equals("lookup")) {
            try (var input = new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[1]))));
                 var output = new DataOutputStream(new BufferedOutputStream(Files.newOutputStream(Path.of(args[2]))))) {
                int count = input.readInt();
                for (int index = 0; index < count; index++) {
                    char[] chars = new char[input.readInt()];
                    for (int i = 0; i < chars.length; i++) chars[i] = input.readChar();
                    try { output.writeInt(Character.codePointOf(new String(chars))); }
                    catch (IllegalArgumentException error) { output.writeInt(-1); }
                }
            }
        } else if (args[0].equals("digits")) {
            try (var output = new DataOutputStream(Files.newOutputStream(Path.of(args[1])))) {
                for (int unit = 0; unit <= 0xFFFF; unit++) output.writeInt(Character.digit((char) unit, 16));
            }
        } else if (args[0].equals("uppercase")) {
            try (var output = Files.newBufferedWriter(Path.of(args[1]), StandardCharsets.US_ASCII)) {
                for (int cp = 128; cp <= Character.MAX_CODE_POINT; cp++) {
                    String upper = new String(Character.toChars(cp)).toUpperCase(Locale.ROOT);
                    if (upper.chars().allMatch(value -> value < 128)) {
                        output.write(Integer.toHexString(cp) + "\t" + upper + "\n");
                    }
                }
            }
        } else throw new IllegalArgumentException(args[0]);
    }
}
"""

RUST_DRIVER = r"""
#[path = MODULE_PATH]
mod unicode_names;
use std::{env, fs, io::{self, Write}};

fn main() {
    let args: Vec<_> = env::args().collect();
    let mut output = io::BufWriter::new(io::stdout().lock());
    if args[1] == "digits" {
        for unit in 0..=u16::MAX {
            let value = unicode_names::hex_digit_utf16(unit).map_or(-1, i32::from);
            output.write_all(&value.to_be_bytes()).unwrap();
        }
    } else {
        let input = fs::read(&args[1]).unwrap();
        let mut position = 0;
        let count = take_u32(&input, &mut position);
        for _ in 0..count {
            let length = take_u32(&input, &mut position) as usize;
            let units: Vec<_> = input[position..position + length * 2].chunks_exact(2)
                .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]])).collect();
            position += length * 2;
            let value = unicode_names::lookup_utf16(&units).map_or(-1, |value| value as i32);
            output.write_all(&value.to_be_bytes()).unwrap();
        }
        assert_eq!(position, input.len());
    }
}

fn take_u32(input: &[u8], position: &mut usize) -> u32 {
    let value = u32::from_be_bytes(input[*position..*position + 4].try_into().unwrap());
    *position += 4;
    value
}
"""


class UnicodeNameDataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.sources = generator.load_sources()
        cls.data = generator.build_name_data(cls.sources)

    def test_hashes_and_reproducible_binary_tables(self):
        tables = generator.generate_tables(self.data)
        for name, content in tables.items():
            with self.subTest(name=name):
                self.assertEqual(content, (generator.OUTPUT_DIRECTORY / name).read_bytes())
        self.assertEqual(tables, generator.generate_tables(self.data))

    def test_complete_assigned_unicode16_coverage(self):
        self.assertEqual(len(self.data.names), 40_077)
        self.assertEqual(sum(end - start + 1 for start, end, _ in self.data.ranges), 254_502)
        occupied = set(self.data.names)
        for start, end, _ in self.data.ranges:
            points = set(range(start, end + 1))
            self.assertTrue(occupied.isdisjoint(points))
            occupied.update(points)
        self.assertEqual(len(occupied), 294_579)

    def test_binary_record_order_and_offsets(self):
        tables = generator.generate_tables(self.data)
        records = list(struct.iter_unpack("<II", tables["name_records.bin"]))
        previous_name = b""
        for (code_point, offset), (_, next_offset) in zip(records, records[1:]):
            name = tables["names.bin"][offset:next_offset]
            self.assertLess(previous_name, name)
            self.assertEqual(name.decode("ascii"), self.data.names[code_point])
            previous_name = name
        self.assertEqual(records[-1][1], len(tables["names.bin"]))

    def test_java_semantic_choices_are_explicit(self):
        self.assertEqual(self.data.names[7], "BEL")
        self.assertEqual(self.data.names[0x1F514], "BELL")
        self.assertEqual(self.data.names[0x80], "PADDING CHARACTER")
        self.assertEqual(self.data.name(0x84), "LATIN 1 SUPPLEMENT 84")
        self.assertEqual(self.data.name(0xAC00), "HANGUL SYLLABLES AC00")
        self.assertEqual(self.data.name(0x4E00), "CJK UNIFIED IDEOGRAPHS 4E00")
        self.assertIsNone(self.data.name(0x378))
        self.assertEqual(len(self.data.uppercase_ascii), 10)
        self.assertEqual(self.data.uppercase_ascii[0xFB03], b"FFI")

    def test_corrupted_inputs_fail_before_generation(self):
        with tempfile.TemporaryDirectory(prefix="arrow-unicode-hashes-") as directory:
            path = Path(directory)
            source = {"unicode_version": "16.0.0", "sources": [{"path": "test.txt", "sha256": "0" * 64}]}
            (path / "sources.json").write_text(json.dumps(source), encoding="utf-8")
            (path / "test.txt").write_bytes(b"wrong input")
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                generator.load_sources(path)

    @unittest.skipUnless(os.environ.get("ARROW_MC_UNICODE_JAVA_ORACLE") == "1", "set ARROW_MC_UNICODE_JAVA_ORACLE=1 for exhaustive Java25/Rust validation")
    def test_exhaustive_java25_names_normalization_aliases_and_bmp_digits(self):
        with tempfile.TemporaryDirectory(prefix="arrow-unicode-oracle-") as directory:
            directory = Path(directory)
            java_source = directory / "UnicodeNameOracle.java"
            java_source.write_text(JAVA_ORACLE, encoding="utf-8")
            rust_source = directory / "driver.rs"
            rust_source.write_text(RUST_DRIVER.replace("MODULE_PATH", json.dumps((ROOT / "src/unicode_names/mod.rs").as_posix())), encoding="utf-8")
            executable = directory / ("driver.exe" if os.name == "nt" else "driver")
            subprocess.run(["rustc", "--edition=2024", "-O", str(rust_source), "-o", str(executable)], check=True, cwd=ROOT)

            version = subprocess.run(["java", "-version"], capture_output=True, text=True, check=True).stderr
            self.assertIn('version "25.', version, "the reference must be Java 25")

            def java(mode, *paths):
                subprocess.run(["java", str(java_source), mode, *(str(path) for path in paths)], check=True, cwd=ROOT)

            java_names = directory / "java-names.tsv"
            java("names", java_names)
            observed = {int(code, 16): name for code, name in (line.split("\t", 1) for line in java_names.read_text(encoding="ascii").splitlines())}
            for code_point in range(0x110000):
                self.assertEqual(self.data.name(code_point), observed.get(code_point), f"getName U+{code_point:04X}")

            uppercase_path = directory / "uppercase.tsv"
            java("uppercase", uppercase_path)
            observed_uppercase = {int(code, 16): upper.encode("ascii") for code, upper in (line.split("\t", 1) for line in uppercase_path.read_text(encoding="ascii").splitlines())}
            self.assertEqual(self.data.uppercase_ascii, observed_uppercase)

            inputs = []
            for name in observed.values():
                inputs.extend((name, "\0\t" + name.lower() + " \r\n"))
            inputs.extend(alias for _, alias, _ in generator.rows(self.sources["NameAliases.txt"]))
            # Test every BMP unit as an input name, including lone surrogates and
            # trim-only inputs; supplement with prefixes/suffixes around names.
            inputs.extend(chr(unit) for unit in range(0x10000))
            for unit in range(0x10000):
                inputs.extend((chr(unit) + "SPACE", "SPACE" + chr(unit)))
            for start, end, prefix in self.data.ranges:
                for code_point in (start - 1, start, end, end + 1):
                    for suffix in (f"{code_point:X}", f"0{code_point:X}", f"+{code_point:X}", f"{code_point:X}X"):
                        inputs.append(prefix + " " + suffix)
            for code_point, replacement in self.data.uppercase_ascii.items():
                text = replacement.decode("ascii")
                name = next(name for name in observed.values() if text in name)
                inputs.append(name.replace(text, chr(code_point)))
            inputs.extend((
                "HANGUL SYLLABLE GA", "CJK UNIFIED IDEOGRAPH-4E00", "BASIC LATIN 41",
                "LATIN_CAPITAL_LETTER_A", "LATIN  CAPITAL LETTER A", "\u00A0SPACE", "SPACE\u3000",
                "NULL" + " " * 1000, " " * 1000 + "NULL", "A" * 129, "\uFB03" * 128,
            ))
            query_path = directory / "queries.bin"
            with query_path.open("wb") as query:
                query.write(struct.pack(">I", len(inputs)))
                for name in inputs:
                    encoded = name.encode("utf-16-be", errors="surrogatepass")
                    query.write(struct.pack(">I", len(encoded) // 2))
                    query.write(encoded)
            expected_path = directory / "expected.bin"
            java("lookup", query_path, expected_path)
            expected = expected_path.read_bytes()
            actual = subprocess.run([str(executable), str(query_path)], check=True, capture_output=True).stdout
            self.assertEqual(len(expected), len(inputs) * 4)
            self.assertEqual(len(actual), len(expected))
            if actual != expected:
                for index, (left, right) in enumerate(zip(struct.iter_unpack(">i", actual), struct.iter_unpack(">i", expected))):
                    self.assertEqual(left, right, f"codePointOf {inputs[index]!r}")

            digits_path = directory / "digits.bin"
            java("digits", digits_path)
            actual_digits = subprocess.run([str(executable), "digits"], check=True, capture_output=True).stdout
            self.assertEqual(actual_digits, digits_path.read_bytes(), "Character.digit(char,16) all BMP units")
            print(json.dumps({
                "java": version.splitlines()[0],
                "getName_code_points_checked": 0x110000,
                "canonical_names": len(observed),
                "codePointOf_queries": len(inputs),
                "bmp_hex_digits_checked": 0x10000,
                "all_non_ascii_uppercase_checked": 0x110000 - 128,
                "mismatches": 0,
            }, indent=2))


if __name__ == "__main__":
    unittest.main()
