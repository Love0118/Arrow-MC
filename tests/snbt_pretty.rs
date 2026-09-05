//! Synthetic formatting observations from the pinned official Java visitor.
//! The ignored oracle rechecks them through public APIs; no upstream body or
//! generated Minecraft content is included here.

use arrow_mc::nbt::{self, Compound, NbtString, Tag};
use arrow_mc::snbt::{self, ErrorKind, Limits};

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    let mut value = Compound::new();
    for (key, tag) in entries {
        value.insert(key.into(), tag).unwrap();
    }
    Tag::Compound(value)
}

fn ints(values: &[i32]) -> Tag {
    Tag::List(values.iter().copied().map(Tag::Int).collect())
}

fn ordered_fields() -> Tag {
    compound(vec![
        ("z", Tag::Int(8)),
        ("palettes", Tag::Int(6)),
        ("palette", Tag::Int(5)),
        ("entities", Tag::Int(4)),
        ("data", Tag::Int(3)),
        ("size", Tag::Int(2)),
        ("author", Tag::Int(1)),
        ("DataVersion", Tag::Int(0)),
        ("A", Tag::Int(7)),
    ])
}

fn data_entry() -> Tag {
    compound(vec![
        ("extra", Tag::Int(5)),
        (
            "nbt",
            compound(vec![("Z", Tag::Int(1)), ("A", ints(&[2, 3]))]),
        ),
        ("state", Tag::Int(4)),
        ("pos", ints(&[0, 1, 2])),
    ])
}

fn entity_entry() -> Tag {
    compound(vec![
        ("z", Tag::Int(3)),
        ("pos", Tag::List(vec![Tag::Double(0.5), Tag::Double(1.5)])),
        ("blockPos", ints(&[0, 1])),
    ])
}

struct Fixture {
    name: &'static str,
    tag: Tag,
    expected: Vec<u16>,
}

fn fixture(name: &'static str, tag: Tag, expected: &str) -> Fixture {
    Fixture {
        name,
        tag,
        expected: expected.encode_utf16().collect(),
    }
}

fn fixtures() -> Vec<Fixture> {
    let mut values = vec![
        fixture("end", Tag::End, ""),
        fixture("byte", Tag::Byte(i8::MIN), "-128b"),
        fixture("short", Tag::Short(i16::MIN), "-32768s"),
        fixture("int", Tag::Int(i32::MIN), "-2147483648"),
        fixture("long", Tag::Long(i64::MIN), "-9223372036854775808L"),
        fixture("float_zero", Tag::Float(-0.0), "-0.0f"),
        fixture("double_zero", Tag::Double(-0.0), "-0.0d"),
        fixture("float_nan", Tag::Float(f32::NAN), "NaNf"),
        fixture(
            "double_infinity",
            Tag::Double(f64::NEG_INFINITY),
            "-Infinityd",
        ),
        fixture("float_subnormal", Tag::Float(f32::from_bits(1)), "1.4E-45f"),
        fixture(
            "double_subnormal",
            Tag::Double(f64::from_bits(1)),
            "4.9E-324d",
        ),
        fixture("empty_string", Tag::String("".into()), "\"\""),
        fixture(
            "quoted_string",
            Tag::String("a\"b'c\\\n".into()),
            "'a\"b\\'c\\\\\\n'",
        ),
        fixture(
            "byte_array",
            Tag::ByteArray(vec![-128, 0, 127]),
            "[B; -128B, 0B, 127B]",
        ),
        fixture(
            "int_array",
            Tag::IntArray(vec![i32::MIN, i32::MAX]),
            "[I; -2147483648, 2147483647]",
        ),
        fixture(
            "long_array",
            Tag::LongArray(vec![i64::MIN, i64::MAX]),
            "[L; -9223372036854775808L, 9223372036854775807L]",
        ),
        fixture("empty_byte_array", Tag::ByteArray(vec![]), "[B;]"),
        fixture("empty_int_array", Tag::IntArray(vec![]), "[I;]"),
        fixture("empty_long_array", Tag::LongArray(vec![]), "[L;]"),
        fixture("empty_list", Tag::List(vec![]), "[]"),
        fixture("empty_compound", compound(vec![]), "{}"),
        fixture("root_list", ints(&[1, 2]), "[\n    1,\n    2\n]"),
        fixture(
            "mixed_list",
            Tag::List(vec![
                Tag::Int(1),
                Tag::String("two".into()),
                compound(vec![("x", Tag::Int(3))]),
            ]),
            "[\n    1,\n    \"two\",\n    {\n        x: 3\n    }\n]",
        ),
        fixture(
            "pretty_key_grammar",
            compound(vec![
                ("z", Tag::Int(1)),
                ("true", Tag::Int(1)),
                ("FALSE", Tag::Int(1)),
                ("9foo", Tag::Int(1)),
                ("", Tag::Int(1)),
                ("_a+9-.", Tag::Int(1)),
                ("a:b", Tag::Int(1)),
                (".a", Tag::Int(1)),
                ("-a", Tag::Int(1)),
            ]),
            "{\n    \"\": 1,\n    -a: 1,\n    .a: 1,\n    9foo: 1,\n    FALSE: 1,\n    _a+9-.: 1,\n    \"a:b\": 1,\n    true: 1,\n    z: 1\n}",
        ),
        fixture(
            "root_key_priority",
            ordered_fields(),
            "{\n    DataVersion: 0,\n    author: 1,\n    size: 2,\n    data: 3,\n    entities: 4,\n    palette: 5,\n    palettes: 6,\n    A: 7,\n    z: 8\n}",
        ),
        fixture(
            "nested_ordinary_compound",
            compound(vec![("outer", ordered_fields())]),
            "{\n    outer: {\n        A: 7,\n        DataVersion: 0,\n        author: 1,\n        data: 3,\n        entities: 4,\n        palette: 5,\n        palettes: 6,\n        size: 2,\n        z: 8\n    }\n}",
        ),
        fixture(
            "size_disables_descendant_indentation",
            compound(vec![(
                "size",
                Tag::List(vec![
                    ints(&[]),
                    ints(&[1, 2]),
                    compound(vec![("x", ints(&[3, 4]))]),
                ]),
            )]),
            "{\n    size: [[], [1, 2], {x: [3, 4]}]\n}",
        ),
        fixture(
            "data_priority_and_sibling_indentation",
            compound(vec![
                ("data", Tag::List(vec![data_entry(), data_entry()])),
                ("after", ints(&[7, 8])),
            ]),
            "{\n    data: [\n        {pos: [0, 1, 2], state: 4, nbt: {A: [2, 3], Z: 1}, extra: 5},\n        {pos: [0, 1, 2], state: 4, nbt: {A: [2, 3], Z: 1}, extra: 5}\n    ],\n    after: [\n        7,\n        8\n    ]\n}",
        ),
        fixture(
            "entity_key_priority",
            compound(vec![("entities", Tag::List(vec![entity_entry()]))]),
            "{\n    entities: [\n        {blockPos: [0, 1], pos: [0.5d, 1.5d], z: 3}\n    ]\n}",
        ),
        fixture(
            "palette_compounds_are_inline",
            compound(vec![(
                "palette",
                Tag::List(vec![compound(vec![
                    ("z", Tag::Int(0)),
                    ("Name", Tag::String("a".into())),
                    (
                        "Properties",
                        compound(vec![("axis", Tag::String("y".into()))]),
                    ),
                ])]),
            )]),
            "{\n    palette: [\n        {Name: \"a\", Properties: {axis: \"y\"}, z: 0}\n    ]\n}",
        ),
        fixture(
            "palettes_do_not_share_palette_rule",
            compound(vec![(
                "palettes",
                Tag::List(vec![Tag::List(vec![compound(vec![(
                    "Name",
                    Tag::String("a".into()),
                )])])]),
            )]),
            "{\n    palettes: [\n        [\n            {\n                Name: \"a\"\n            }\n        ]\n    ]\n}",
        ),
        fixture(
            "data_joined_path_key_collision",
            compound(vec![("data.[]", data_entry())]),
            "{\n    \"data.[]\": {pos: [0, 1, 2], state: 4, nbt: {A: [2, 3], Z: 1}, extra: 5}\n}",
        ),
        fixture(
            "entity_joined_path_key_collision",
            compound(vec![("entities.[]", entity_entry())]),
            "{\n    \"entities.[]\": {blockPos: [0, 1], pos: [0.5d, 1.5d], z: 3}\n}",
        ),
        fixture(
            "palette_joined_path_key_collision",
            compound(vec![("palette.[]", compound(vec![("z", ints(&[1, 2]))]))]),
            "{\n    \"palette.[]\": {z: [1, 2]}\n}",
        ),
        fixture(
            "size_joined_path_near_miss",
            compound(vec![("size.[]", ints(&[1, 2]))]),
            "{\n    \"size.[]\": [\n        1,\n        2\n    ]\n}",
        ),
        fixture(
            "data_compound_is_not_list_entry",
            compound(vec![(
                "data",
                compound(vec![
                    ("state", Tag::Int(1)),
                    ("pos", Tag::Int(2)),
                    ("nbt", Tag::Int(3)),
                ]),
            )]),
            "{\n    data: {\n        nbt: 3,\n        pos: 2,\n        state: 1\n    }\n}",
        ),
    ];
    let units = vec![0xd800, 0xdc00, 0xdfff, 0x7f, 0x85, 0x2028];
    values.push(Fixture {
        name: "utf16_string_units",
        tag: Tag::String(NbtString::from_utf16(units.clone())),
        expected: [vec![0x22], units, vec![0x22]].concat(),
    });
    let mut keyed = Compound::new();
    keyed
        .insert(NbtString::from_utf16(vec![0xe000]), Tag::Int(2))
        .unwrap();
    keyed
        .insert(NbtString::from_utf16(vec![0xd800, 0xdc00]), Tag::Int(1))
        .unwrap();
    let mut expected: Vec<u16> = "{\n    \"".encode_utf16().collect();
    expected.extend([0xd800, 0xdc00]);
    expected.extend("\": 1,\n    \"".encode_utf16());
    expected.push(0xe000);
    expected.extend("\": 2\n}".encode_utf16());
    values.push(Fixture {
        name: "keys_sort_as_java_utf16",
        tag: Tag::Compound(keyed),
        expected,
    });
    values
}

#[test]
fn frozen_official_pretty_printer_observations() {
    for Fixture {
        name,
        tag,
        expected,
    } in fixtures()
    {
        let mut actual = Vec::new();
        snbt::write_pretty(&tag, &mut actual, Limits::default()).unwrap();
        assert_eq!(actual, expected, "fixture {name}");
    }
}

#[test]
fn pretty_output_limits_preserve_prefix_at_every_cutoff() {
    let case = fixtures()
        .into_iter()
        .find(|case| case.name == "data_priority_and_sibling_indentation")
        .unwrap();
    let prefix = vec![0xd800, 0x2a];
    for output_units in 0..case.expected.len() {
        let mut output = prefix.clone();
        let error = snbt::write_pretty(
            &case.tag,
            &mut output,
            Limits {
                output_units,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::OutputLimit, "limit {output_units}");
        assert_eq!(output, prefix, "limit {output_units}");
    }
    let mut output = prefix.clone();
    snbt::write_pretty(
        &case.tag,
        &mut output,
        Limits {
            output_units: case.expected.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(output, [prefix, case.expected].concat());
}

#[test]
fn pretty_end_empty_containers_and_invalid_limits() {
    let mut output = vec![0xdfff];
    snbt::write_pretty(
        &Tag::End,
        &mut output,
        Limits {
            output_units: 0,
            max_depth: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(output, [0xdfff]);
    for tag in [Tag::List(vec![]), compound(vec![])] {
        assert_eq!(
            snbt::write_pretty(
                &tag,
                &mut output,
                Limits {
                    max_depth: 0,
                    ..Limits::default()
                }
            )
            .unwrap_err()
            .kind,
            ErrorKind::DepthLimit
        );
        assert_eq!(output, [0xdfff]);
    }
    assert_eq!(
        snbt::write_pretty(
            &Tag::End,
            &mut output,
            Limits {
                max_depth: 513,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InvalidLimits
    );
    assert_eq!(output, [0xdfff]);
}

#[test]
fn pretty_depth_512_is_supported_and_513_rolls_back() {
    let mut tag = Tag::Int(1);
    for _ in 0..512 {
        tag = Tag::List(vec![tag]);
    }
    let mut output = Vec::new();
    snbt::write_pretty(&tag, &mut output, Limits::default()).unwrap();
    assert_eq!(output.len(), 1_050_625);
    let before = output.clone();
    tag = Tag::List(vec![tag]);
    assert_eq!(
        snbt::write_pretty(&tag, &mut output, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
    assert_eq!(output, before);
}

#[test]
#[ignore = "requires the locked local official JAR and Java 25; verifies all synthetic pretty fixtures"]
fn official_java_pretty_printer_oracle() {
    use std::fmt::Write as _;
    use std::process::Command;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let artifacts = root
        .parent()
        .unwrap()
        .join("Decompile/artifacts/26.3-pre-2");
    let server = artifacts.join("server-26.3-pre-2.jar");
    assert!(
        server.is_file(),
        "prepare the locked local references first"
    );
    let classpath = std::env::join_paths([server, artifacts.join("libraries/*")]).unwrap();
    let scratch = std::env::temp_dir().join(format!("arrow-pretty-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let source = scratch.join("ArrowPrettyOracle.java");
    let input_path = scratch.join("input.tsv");
    std::fs::write(&source, r#"
import java.io.*;
import java.nio.file.*;
import java.util.HexFormat;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.*;

public class ArrowPrettyOracle {
    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 25) throw new IllegalStateException("Java 25 required");
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2"))
            throw new IllegalStateException("locked Minecraft version required");
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] fields = line.split("\t");
            byte[] bytes = HexFormat.of().parseHex(fields[1]);
            Tag tag = NbtIo.readAnyTag(new DataInputStream(new ByteArrayInputStream(bytes)), NbtAccounter.unlimitedHeap());
            // Binary decoding interns zero; construct the signed-zero probes directly.
            if (fields[0].equals("float_zero")) tag = new FloatTag(-0.0f);
            if (fields[0].equals("double_zero")) tag = new DoubleTag(-0.0d);
            String result = new SnbtPrinterTagVisitor().visit(tag);
            StringBuilder hex = new StringBuilder();
            for (int i = 0; i < result.length(); i++) hex.append(String.format("%04x", (int)result.charAt(i)));
            System.out.println(fields[0] + "\t" + hex);
        }
    }
}
"#).unwrap();
    let cases = fixtures();
    let mut input = String::new();
    for case in &cases {
        let mut bytes = Vec::new();
        nbt::write_network(&case.tag, &mut bytes, nbt::Limits::default()).unwrap();
        write!(input, "{}\t", case.name).unwrap();
        for byte in bytes {
            write!(input, "{byte:02x}").unwrap();
        }
        input.push('\n');
    }
    std::fs::write(&input_path, input).unwrap();
    let result = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(&source)
        .arg(&input_path)
        .current_dir(&scratch)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().filter(|line| line.contains('\t')).collect();
    assert_eq!(lines.len(), cases.len(), "{stdout}");
    for (line, case) in lines.into_iter().zip(&cases) {
        let (name, hex) = line.split_once('\t').unwrap();
        assert_eq!(name, case.name);
        assert_eq!(hex.len() % 4, 0);
        let actual: Vec<u16> = (0..hex.len())
            .step_by(4)
            .map(|i| u16::from_str_radix(&hex[i..i + 4], 16).unwrap())
            .collect();
        assert_eq!(actual, case.expected, "official fixture {}", case.name);
        let mut rust = Vec::new();
        snbt::write_pretty(&case.tag, &mut rust, Limits::default()).unwrap();
        assert_eq!(rust, actual, "live differential {}", case.name);
    }
    eprintln!(
        "Official Java 25 / Minecraft 26.3-pre-2 pretty corpus: {} fixtures matched",
        cases.len()
    );
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(input_path).unwrap();
    // Java logging may add files; remove only this test's exact owned files.
    let _ = std::fs::remove_dir(scratch);
}
