//! Synthetic observations through the locked official NBT path/predicate APIs.
//! Runtime-object alias differences remain explicit fixture cases.

use arrow_mc::nbt::path::{self, Argument, Path};
use arrow_mc::nbt::predicate::{CompareBudget, CompareLimits};
use arrow_mc::nbt::{self, Compound, Tag};
use arrow_mc::snbt;
use std::fmt::Write as _;

const FIELDS: &[&str] = &[
    "id",
    "op",
    "path",
    "start_cursor",
    "path_nodes",
    "root",
    "value",
    "value_depth",
    "values",
    "index",
    "expected",
    "actual",
    "partial",
    "start_depth",
    "ok",
    "cursor",
    "parsed_path",
    "selected",
    "count",
    "changed",
    "match",
    "too_deep",
    "error_cursor",
    "translation_key",
    "translation_args",
    "runtime_error",
    "supplier_calls",
    "root_after",
    "root_changed",
    "root_too_deep",
    "same_supplier",
    "mutate_selected",
    "same_reference",
    "source_aliased",
    "supplied_after",
    "message",
    "context",
    "root_construct",
    "value_construct",
    "expected_construct",
    "actual_construct",
    "binary_original_tag_id",
    "binary_original_snbt",
    "binary_encoded_hex",
    "binary_decoded_tag_id",
    "binary_decoded_snbt",
    "binary_meaning_equal",
    "binary_remaining_bytes",
    "binary_decode_error",
    "binary_encode_error",
];

struct Case(Vec<&'static str>);

impl Case {
    fn field(&self, name: &str) -> Option<&'static str> {
        let index = FIELDS.iter().position(|&field| field == name).unwrap();
        let value = self.0[index];
        (value != "-").then_some(value)
    }

    fn id(&self) -> &'static str {
        self.field("id").unwrap()
    }

    fn boolean(&self, name: &str) -> Option<bool> {
        self.field(name).map(|value| match value {
            "0" => false,
            "1" => true,
            _ => panic!("{}: invalid boolean {name}", self.id()),
        })
    }

    fn number(&self, name: &str) -> Option<i64> {
        self.field(name).map(|text| text.parse().unwrap())
    }

    fn tag(&self, name: &str) -> Option<Tag> {
        if let Some(constructor) = self.field(&format!("{name}_construct")) {
            return Some(constructed(constructor));
        }
        if name == "value"
            && let Some(depth) = self.number("value_depth")
        {
            let mut value = Tag::Int(1);
            for _ in 0..depth {
                value = Tag::List(vec![value]);
            }
            return Some(value);
        }
        self.field(name).map(|hex| {
            snbt::parse_utf16(&units(hex), snbt::Limits::default())
                .unwrap_or_else(|error| panic!("{}: fixture {name}: {error:?}", self.id()))
        })
    }
}

fn hex(units: &[u16]) -> String {
    let mut output = String::new();
    for unit in units {
        write!(output, "{unit:04x}").unwrap();
    }
    output
}

fn canonical(tag: &Tag) -> String {
    let mut output = Vec::new();
    snbt::write(tag, &mut output, snbt::Limits::default()).unwrap();
    hex(&output)
}

fn selected<'a>(tags: impl Iterator<Item = &'a Tag>) -> String {
    tags.map(|tag| format!("{}:{}", tag.id(), canonical(tag)))
        .collect::<Vec<_>>()
        .join(";")
}

fn check_error(case: &Case, error: &path::Error, input: &[u16]) {
    assert_eq!(case.boolean("ok"), Some(false), "{}: {error:?}", case.id());
    if let Some(runtime) = case.field("runtime_error") {
        assert!(matches!(
            runtime,
            "ArrayIndexOutOfBoundsException" | "StringIndexOutOfBoundsException"
        ));
        // Java unchecked failures are explicit Rust errors, never successful no-ops.
        assert_eq!(error.kind, path::ErrorKind::InvalidPath, "{}", case.id());
        assert_eq!(
            error.translation_key(),
            Some("arguments.nbtpath.node.invalid")
        );
        return;
    }
    assert_eq!(
        error.cursor.map(|n| n as i64).unwrap_or(-1),
        case.number("error_cursor").unwrap_or(-1),
        "{}",
        case.id()
    );
    assert_eq!(
        error.translation_key(),
        case.field("translation_key"),
        "{}",
        case.id()
    );
    let mut output = Vec::new();
    let present = error.write_argument(input, &mut output, 1 << 20).unwrap();
    let actual = if !present {
        String::new()
    } else {
        let numeric = matches!(error.argument, Argument::Index(_))
            || matches!(&error.argument, Argument::Snbt { diagnostic, .. } if matches!(diagnostic.argument, snbt::DiagnosticArgument::HexWidth(_)));
        if numeric {
            format!("n:{}", String::from_utf16(&output).unwrap())
        } else {
            format!("s:{}", hex(&output))
        }
    };
    assert_eq!(
        actual,
        case.field("translation_args").unwrap_or(""),
        "{}: argument",
        case.id()
    );
}

fn mutate_first<'a>(mut tags: impl Iterator<Item = &'a mut Tag>) {
    if let Some(Tag::Compound(value)) = tags.next() {
        value.insert("probe".into(), Tag::Int(7)).unwrap();
    }
}

fn compound(entries: &[(&str, Tag)]) -> Tag {
    let mut value = Compound::new();
    for (key, tag) in entries {
        value.insert((*key).into(), tag.clone()).unwrap();
    }
    Tag::Compound(value)
}

fn constructed(name: &str) -> Tag {
    match name {
        "end" => Tag::End,
        "list_end" => Tag::List(vec![Tag::End]),
        "list_end_int" => Tag::List(vec![Tag::End, Tag::Int(7)]),
        "list_int_end" => Tag::List(vec![Tag::Int(7), Tag::End]),
        "list_end_string" => Tag::List(vec![Tag::End, Tag::String("x".into())]),
        "list_string_end" => Tag::List(vec![Tag::String("x".into()), Tag::End]),
        "compound_end" => compound(&[("a", Tag::End)]),
        "compound_end_int" => compound(&[("a", Tag::End), ("b", Tag::Int(7))]),
        "compound_int_end" => compound(&[("a", Tag::Int(7)), ("b", Tag::End)]),
        "root_list_end" => compound(&[("a", Tag::List(vec![Tag::End]))]),
        "root_mixed_end" => compound(&[("a", Tag::List(vec![Tag::Int(1), Tag::End, Tag::Int(2)]))]),
        _ => panic!("unknown fixture constructor {name}"),
    }
}

fn units(hex: &str) -> Vec<u16> {
    assert_eq!(hex.len() % 4, 0);
    (0..hex.len())
        .step_by(4)
        .map(|index| u16::from_str_radix(&hex[index..index + 4], 16).unwrap())
        .collect()
}

fn cases() -> impl Iterator<Item = Case> {
    include_str!("fixtures/nbt_path.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let values: Vec<_> = line.split('\t').collect();
            assert_eq!(values.len(), FIELDS.len());
            Case(values)
        })
}

#[test]
fn official_path_parsing_selection_and_partial_mutation() {
    let limits = path::Limits::default();
    let mut checked = 0;
    let mut observed_java_failures = 0;
    let mut owned_alias_boundary = 0;
    let mut immutable_identity_boundary = 0;
    let mut failures = Vec::new();
    for case in
        cases().filter(|case| !matches!(case.field("op"), Some("compare" | "too_deep" | "binary")))
    {
        checked += 1;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let input = if let Some(count) = case.number("path_nodes") {
                std::iter::repeat_n("a", count as usize)
                    .collect::<Vec<_>>()
                    .join(".")
                    .encode_utf16()
                    .collect()
            } else {
                units(case.field("path").unwrap())
            };
            let (path, cursor) = match Path::parse_utf16(
                &input,
                case.number("start_cursor").unwrap_or(0) as usize,
                limits,
            ) {
                Ok(value) => value,
                Err(error) => {
                    observed_java_failures += usize::from(case.field("runtime_error").is_some());
                    check_error(&case, &error, &input);
                    return;
                }
            };
            assert_eq!(
                cursor as i64,
                case.number("cursor").unwrap(),
                "{}",
                case.id()
            );
            assert_eq!(
                hex(path.as_string().as_utf16()),
                case.field("parsed_path").unwrap(),
                "{}",
                case.id()
            );
            let op = case.field("op").unwrap();
            if op == "parse" {
                assert_eq!(case.boolean("ok"), Some(true), "{}", case.id());
                return;
            }
            let mut root = case
                .tag("root")
                .unwrap_or_else(|| Tag::Compound(Compound::new()));
            let supplied = case
                .tag("value")
                .unwrap_or_else(|| Tag::Compound(Compound::new()));
            let mut calls = 0;
            let mut selected_text = None;
            let mut changed = None;
            let mut count = None;
            let mut same_reference = None;
            let mut source_aliased = None;
            let java_shared_factory = case.boolean("same_supplier") == Some(true);
            let result: Result<(), path::Error> = (|| {
                match op {
                    "get" => {
                        let values = path.get(&root, limits)?;
                        selected_text = Some(selected(values.iter().map(|value| value.as_tag())));
                    }
                    "count" => count = Some(path.count_matching(&root, limits)? as i64),
                    "set" => changed = Some(path.set(&mut root, &supplied, limits)? as i64),
                    "remove" => changed = Some(path.remove(&mut root, limits)? as i64),
                    "insert" => {
                        let values = match case.field("values") {
                            None => vec![supplied.clone()],
                            Some("") => vec![],
                            Some(encoded) => encoded
                                .split(';')
                                .map(|value| {
                                    if let Some(name) = value.strip_prefix("c:") {
                                        constructed(name)
                                    } else {
                                        snbt::parse_utf16(
                                            &units(value.strip_prefix("s:").unwrap()),
                                            snbt::Limits::default(),
                                        )
                                        .unwrap()
                                    }
                                })
                                .collect(),
                        };
                        changed = Some(path.insert(
                            &mut root,
                            case.number("index").unwrap() as i32,
                            &values,
                            limits,
                        )? as i64);
                    }
                    "create" => {
                        let mut factory = || {
                            calls += 1;
                            supplied.clone()
                        };
                        let mut values = path.get_or_create(&mut root, &mut factory, limits)?;
                        selected_text = Some(selected(values.iter().map(|value| value.as_tag())));
                        same_reference = Some(
                            values.len() > 1
                                && std::ptr::eq(values[0].as_tag(), values[1].as_tag()),
                        );
                        if case.boolean("mutate_selected") == Some(true) {
                            mutate_first(values.iter_mut().map(|value| value.as_tag_mut()));
                        }
                    }
                    "set_alias" => {
                        changed = Some(path.set(&mut root, &supplied, limits)? as i64);
                        let mut factory = || panic!("existing set targets must not call factory");
                        let mut values = path.get_or_create(&mut root, &mut factory, limits)?;
                        same_reference = Some(
                            values.len() > 1
                                && std::ptr::eq(values[0].as_tag(), values[1].as_tag()),
                        );
                        source_aliased =
                            Some(!values.is_empty() && std::ptr::eq(values[0].as_tag(), &supplied));
                        mutate_first(values.iter_mut().map(|value| value.as_tag_mut()));
                    }
                    _ => panic!("unknown operation {op}"),
                }
                Ok(())
            })();
            match result {
                Ok(()) => assert_eq!(case.boolean("ok"), Some(true), "{}", case.id()),
                Err(error) => {
                    observed_java_failures += usize::from(case.field("runtime_error").is_some());
                    check_error(&case, &error, path.as_string().as_utf16());
                }
            }
            assert_eq!(
                selected_text.as_deref(),
                case.field("selected"),
                "{}",
                case.id()
            );
            assert_eq!(changed, case.number("changed"), "{}", case.id());
            assert_eq!(count, case.number("count"), "{}", case.id());
            assert_eq!(
                calls,
                case.number("supplier_calls").unwrap(),
                "{}",
                case.id()
            );
            assert_eq!(
                source_aliased,
                case.boolean("source_aliased"),
                "{}",
                case.id()
            );
            if java_shared_factory {
                // Owned factory values intentionally do not reproduce Java's
                // mutable one-object alias across two branches.
                owned_alias_boundary += 1;
                assert_eq!(case.id(), "alias-002");
                assert_eq!(case.boolean("same_reference"), Some(true));
                assert_eq!(same_reference, Some(false));
                assert_eq!(
                    canonical(&supplied),
                    canonical(&snbt::parse("{v:1}", snbt::Limits::default()).unwrap())
                );
                assert_eq!(
                    canonical(&root),
                    canonical(
                        &snbt::parse("{a:[{x:{probe:7,v:1}},{x:{v:1}}]}", snbt::Limits::default())
                            .unwrap()
                    )
                );
            } else {
                if case.boolean("same_reference") == Some(true) {
                    // Java scalar copies/cache entries may share immutable objects.
                    // Rust stores independent values; identity has no mutable effect.
                    immutable_identity_boundary += 1;
                    assert_eq!(same_reference, Some(false));
                    for value in case.field("selected").unwrap().split(';').take(2) {
                        let id: u8 = value.split(':').next().unwrap().parse().unwrap();
                        assert!(matches!(id, 0..=6 | 8));
                    }
                } else {
                    assert_eq!(
                        same_reference,
                        case.boolean("same_reference"),
                        "{}",
                        case.id()
                    );
                }
                if let Some(expected) = case.field("supplied_after") {
                    assert_eq!(canonical(&supplied), expected, "{}", case.id());
                }
                if let Some(expected) = case.field("root_after") {
                    assert_eq!(canonical(&root), expected, "{}: root after", case.id());
                }
            }
            if let Some(expected) = case.boolean("root_changed") {
                assert_eq!(
                    !matches!(&root, Tag::Compound(value) if value.entries().is_empty()),
                    expected,
                    "{}",
                    case.id()
                );
            }
            if let Some(expected) = case.boolean("root_too_deep") {
                assert_eq!(
                    path::is_too_deep(&root, 0, limits).unwrap(),
                    expected,
                    "{}",
                    case.id()
                );
            }
        }));
        if outcome.is_err() {
            failures.push(case.id());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatched path cases: {}",
        failures.len(),
        failures.join(", ")
    );
    assert_eq!(checked, 2900);
    assert_eq!(observed_java_failures, 11);
    assert_eq!(owned_alias_boundary, 1);
    assert_eq!(immutable_identity_boundary, 20);
}

#[test]
fn official_source_depth_rule_is_distinct_from_path_admission() {
    let mut checked = 0;
    for case in cases().filter(|case| case.field("op") == Some("too_deep")) {
        checked += 1;
        let value = case.tag("value").unwrap();
        assert_eq!(
            path::is_too_deep(
                &value,
                case.number("start_depth").unwrap() as usize,
                path::Limits::default()
            )
            .unwrap(),
            case.boolean("too_deep").unwrap(),
            "{}",
            case.id()
        );
    }
    assert_eq!(checked, 10);
}

#[test]
fn official_partial_predicate_values() {
    let mut checked = 0;
    for case in cases().filter(|case| case.field("op") == Some("compare")) {
        checked += 1;
        let expected = case.tag("expected");
        let actual = case.tag("actual");
        let mut budget = CompareBudget::new(CompareLimits::default());
        let matched = budget
            .compare(
                expected.as_ref(),
                actual.as_ref(),
                case.boolean("partial").unwrap_or(true),
            )
            .unwrap();
        assert_eq!(case.boolean("ok"), Some(true), "{}", case.id());
        assert_eq!(Some(matched), case.boolean("match"), "{}", case.id());
    }
    assert_eq!(checked, 129);
}

#[test]
fn end_runtime_values_and_binary_validation_are_separate_contracts() {
    let mut checked = 0;
    for case in cases().filter(|case| case.field("op") == Some("binary")) {
        checked += 1;
        let value = case.tag("value").unwrap();
        assert_eq!(
            Some(value.id().to_string().as_str()),
            case.field("binary_original_tag_id")
        );
        let mut output = vec![0x55];
        let result = nbt::write_network(&value, &mut output, nbt::Limits::default());
        if matches!(value, Tag::End) {
            result.unwrap();
            assert_eq!(output, [0x55, 0]);
            assert_eq!(case.field("binary_encoded_hex"), Some("00"));
            assert_eq!(case.boolean("binary_meaning_equal"), Some(true));
        } else {
            // Vanilla produces bytes for these runtime End-containing values,
            // but the observations show lost meaning, trailing bytes or decode
            // failures. Arrow explicitly rejects such binary output. This does
            // not assert byte parity for malformed constructed containers.
            assert!(case.field("binary_encoded_hex").is_some(), "{}", case.id());
            assert!(
                case.field("binary_decode_error").is_some()
                    || case.boolean("binary_meaning_equal") == Some(false)
                    || case.field("binary_remaining_bytes") != Some("0")
            );
            assert_eq!(result, Err(nbt::Error::UnexpectedEnd), "{}", case.id());
            assert_eq!(output, [0x55]);
        }
    }
    assert_eq!(checked, 11);
}
