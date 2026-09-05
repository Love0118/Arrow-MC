//! Frozen observations from the actual locked Java parser and compact visitor.
//! No Mojang implementation or registry data is included in the fixture.

use arrow_mc::nbt::{Compound, NbtString, Tag};
use arrow_mc::snbt::{self, ErrorKind, Limits};
use std::fmt::Write as _;

fn units(hex: &str) -> Vec<u16> {
    assert_eq!(hex.len() % 4, 0);
    (0..hex.len())
        .step_by(4)
        .map(|i| u16::from_str_radix(&hex[i..i + 4], 16).unwrap())
        .collect()
}

fn unit_hex(value: &[u16]) -> String {
    let mut text = String::new();
    for unit in value {
        write!(text, "{unit:04x}").unwrap();
    }
    text
}

fn typed_tree(tag: &Tag) -> String {
    match tag {
        Tag::End => "0".into(),
        Tag::Byte(v) => format!("1:{v}"),
        Tag::Short(v) => format!("2:{v}"),
        Tag::Int(v) => format!("3:{v}"),
        Tag::Long(v) => format!("4:{v}"),
        Tag::Float(v) => format!("5:{}", v.to_bits()),
        Tag::Double(v) => format!("6:{}", v.to_bits()),
        Tag::String(v) => format!("8:{}", unit_hex(v.as_utf16())),
        Tag::List(v) => format!(
            "9:[{}]",
            v.iter().map(typed_tree).collect::<Vec<_>>().join(",")
        ),
        Tag::Compound(v) => format!(
            "10:{{{}}}",
            v.entries()
                .iter()
                .map(|entry| {
                    format!(
                        "{}={}",
                        unit_hex(entry.name.as_utf16()),
                        typed_tree(&entry.value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
        Tag::ByteArray(v) => format!(
            "7:[{}]",
            v.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Tag::IntArray(v) => format!(
            "11:[{}]",
            v.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Tag::LongArray(v) => format!(
            "12:[{}]",
            v.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

fn cases() -> impl Iterator<Item = Vec<&'static str>> {
    include_str!("fixtures/snbt.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 10);
            fields
        })
}

fn constructed(name: &str) -> Tag {
    match name {
        "end" => Tag::End,
        "float_nan" => Tag::Float(f32::NAN),
        "float_positive_infinity" => Tag::Float(f32::INFINITY),
        "double_negative_infinity" => Tag::Double(f64::NEG_INFINITY),
        "float_negative_zero" => Tag::Float(-0.0),
        "double_negative_zero" => Tag::Double(-0.0),
        "empty_key" => {
            let mut value = Compound::new();
            value.insert(NbtString::from(""), Tag::Int(1)).unwrap();
            Tag::Compound(value)
        }
        _ => panic!("unknown fixture constructor {name}"),
    }
}

#[test]
fn official_parser_values_rejections_and_utf16_cursors() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for fields in cases() {
        let [
            id,
            mode,
            start,
            input,
            outcome,
            cursor,
            tree,
            _,
            translation_key,
            argument,
        ] = fields[..]
        else {
            unreachable!()
        };
        if mode.starts_with("construct:") || mode.starts_with("depth:") {
            continue;
        }
        checked += 1;
        let input = units(input);
        let start: usize = start.parse().unwrap();
        let parsed = match mode {
            "argument" => snbt::parse_prefix(&input[start..], Limits::default())
                .map(|(tag, consumed)| (tag, consumed + start)),
            "compound" => snbt::parse_compound_utf16(&input, Limits::default())
                .map(|tag| (Tag::Compound(tag), input.len())),
            "fully" => {
                snbt::parse_utf16(&input[start..], Limits::default()).map(|tag| (tag, input.len()))
            }
            _ => panic!("unknown fixture mode {mode}"),
        };
        match (outcome, parsed) {
            ("ok", Ok((tag, consumed))) => {
                let actual = typed_tree(&tag);
                if actual != tree
                    || (!cursor.is_empty() && consumed != cursor.parse::<usize>().unwrap())
                {
                    failures.push(format!("{id}: got tree={actual}, cursor={consumed}; expected tree={tree}, cursor={cursor}"));
                }
            }
            ("error", Err(error)) => {
                let absolute = error.offset_utf16 + start;
                if !cursor.is_empty() && absolute != cursor.parse::<usize>().unwrap() {
                    failures.push(format!(
                        "{id}: got error {error:?} absolute={absolute}; expected cursor={cursor}"
                    ));
                }
                if error.translation_key() != Some(translation_key) {
                    failures.push(format!(
                        "{id}: got translation key {:?}; expected {translation_key}",
                        error.translation_key()
                    ));
                }
                if let Some(diagnostic) = error.diagnostic {
                    let mut output = Vec::new();
                    match diagnostic.write_argument(&input[start..], &mut output, 65536) {
                        Ok(present) => {
                            let actual = if present {
                                unit_hex(&output)
                            } else {
                                "-".into()
                            };
                            if actual != argument {
                                failures.push(format!(
                                    "{id}: got diagnostic argument {actual}; expected {argument}"
                                ));
                            }
                        }
                        Err(error) => {
                            failures.push(format!("{id}: diagnostic rendering failed: {error:?}"))
                        }
                    }
                }
            }
            (_, actual) => failures.push(format!("{id}: expected {outcome}, got {actual:?}")),
        }
    }
    assert_eq!(checked, 7005);
    assert!(
        failures.is_empty(),
        "{} parser mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn official_compact_writer_output() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for fields in cases() {
        let [id, mode, start, input, outcome, _, _, canonical, _, _] = fields[..] else {
            unreachable!()
        };
        if outcome != "ok" || mode.starts_with("depth:") {
            continue;
        }
        checked += 1;
        let input = units(input);
        let start: usize = start.parse().unwrap();
        let value = if let Some(constructor) = mode.strip_prefix("construct:") {
            Ok(constructed(constructor))
        } else if mode == "argument" {
            snbt::parse_prefix(&input[start..], Limits::default()).map(|(tag, _)| tag)
        } else {
            snbt::parse_utf16(&input[start..], Limits::default())
        };
        match value {
            Ok(value) => {
                let mut output = Vec::new();
                match snbt::write(&value, &mut output, Limits::default()) {
                    Ok(()) if unit_hex(&output) == canonical => {}
                    result => failures.push(format!(
                        "{id}: write={result:?}, actual={}, expected={canonical}",
                        unit_hex(&output)
                    )),
                }
            }
            Err(error) => failures.push(format!("{id}: prerequisite parse failed: {error:?}")),
        }
    }
    assert_eq!(checked, 2063);
    assert!(
        failures.is_empty(),
        "{} writer mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn explicit_arrow_depth_policy_is_not_a_vanilla_grammar_limit() {
    let mut checked = 0;
    for fields in cases() {
        let Some(depth) = fields[1].strip_prefix("depth:") else {
            continue;
        };
        checked += 1;
        assert_eq!(fields[4], "ok", "official grammar accepts these depths");
        let depth: usize = depth.parse().unwrap();
        let input = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let parsed = snbt::parse(&input, Limits::default());
        if depth > 512 {
            assert_eq!(parsed.unwrap_err().kind, ErrorKind::DepthLimit);
        } else {
            assert!(parsed.is_ok(), "depth {depth}: {parsed:?}");
        }
    }
    assert_eq!(checked, 6);
}
