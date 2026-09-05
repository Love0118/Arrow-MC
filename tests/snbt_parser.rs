use arrow_mc::nbt::{Compound, CompoundEntry, NbtString, Tag};
use arrow_mc::snbt::{ErrorKind, Limits, parse, parse_compound_utf16, parse_prefix, parse_utf16};

fn read(input: &str) -> Tag {
    parse(input, Limits::default()).unwrap()
}
fn units(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

#[test]
fn integers_preserve_explicit_width_unsigned_bits_and_widen_after_conversion() {
    for (input, expected) in [
        ("255ub", Tag::Byte(-1)),
        ("65535us", Tag::Short(-1)),
        ("4294967295ui", Tag::Int(-1)),
        ("18446744073709551615ul", Tag::Long(-1)),
        ("0xff", Tag::Int(255)),
        ("0xff b", Tag::Byte(-1)),
        ("-0x1si", Tag::Int(-1)),
        ("0b", Tag::Byte(0)),
        ("0 x FF", Tag::Int(255)),
        ("1 u b", Tag::Byte(1)),
        ("[I;255ub,65535us]", Tag::IntArray(vec![-1, -1])),
        (
            "[L;0xffffffff,0xffffffffui]",
            Tag::LongArray(vec![4294967295, -1]),
        ),
    ] {
        assert_eq!(read(input), expected, "{input}");
    }
    for input in [
        "-0ub",
        "256ub",
        "0x100000000",
        "01",
        "1_",
        "[B;1s]",
        "[I;1L]",
    ] {
        assert!(parse(input, Limits::default()).is_err(), "{input}");
    }
}

#[test]
fn whole_and_argument_parsing_keep_the_actual_utf16_boundaries() {
    let (tag, consumed) = parse_prefix(&units("  1foo"), Limits::default()).unwrap();
    assert_eq!((tag, consumed), (Tag::Float(1.0), 4));
    assert_eq!(
        parse("  1foo", Limits::default()).unwrap_err().offset_utf16,
        4
    );
    assert_eq!(
        parse("{\"😀\":1}x", Limits::default())
            .unwrap_err()
            .offset_utf16,
        8
    );
    assert_eq!(
        parse_compound_utf16(&units("[]"), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::ExpectedCompound
    );
    assert_eq!(
        parse("1_ .2", Limits::default()).unwrap_err().offset_utf16,
        3
    );
}

#[test]
fn optional_builtin_suffix_preserves_observed_argument_rollback() {
    for input in ["bool(1", "bool(1,", "bool(1]", "bool(1,,)", "bool(bool(1"] {
        assert_eq!(
            parse_prefix(&units(input), Limits::default()),
            Ok((Tag::Byte(1), 4)),
            "{input}"
        );
        assert_eq!(
            parse(input, Limits::default()).unwrap_err().offset_utf16,
            4,
            "{input}"
        );
    }
    assert_eq!(
        parse_prefix(&units("uuid('1-1-1-1-1'"), Limits::default()),
        Ok((Tag::IntArray(vec![1, 65537, 65536, 1]), 4))
    );
    for input in ["foo(1", "bool(1,2", "bool()", "Bool(1)"] {
        assert!(parse_prefix(&units(input), Limits::default()).is_err());
    }
}

#[test]
fn java_whitespace_and_escape_token_boundaries_are_distinct() {
    for whitespace in ['\t', '\u{1c}', '\u{2003}'] {
        assert_eq!(read(&format!("1{whitespace}b")), Tag::Byte(1));
    }
    for whitespace in ['\u{85}', '\u{a0}', '\u{2007}', '\u{202f}', '\u{feff}'] {
        assert_eq!(
            parse(&format!("1{whitespace}b"), Limits::default())
                .unwrap_err()
                .offset_utf16,
            1
        );
    }
    assert_eq!(read("\"\\ u0041\""), Tag::String("A".into()));
    assert!(parse("\"\\u 0041\"", Limits::default()).is_err());
    assert_eq!(
        read("\"\\s\\x00\\uD800\\U0001F600\""),
        Tag::String(NbtString::from_utf16(vec![32, 0, 0xd800, 0xd83d, 0xde00]))
    );
}

#[test]
fn full_java_character_names_use_canonical_and_algorithmic_name_rules() {
    for (name, expected) in [
        ("LATIN CAPITAL LETTER A", vec![65]),
        ("BELL", vec![0xd83d, 0xdd14]),
        ("HIGH SURROGATES D800", vec![0xd800]),
        ("HANGUL SYLLABLES AC00", vec![0xac00]),
        ("CJK UNIFIED IDEOGRAPHS 4E00", vec![0x4e00]),
    ] {
        assert_eq!(
            read(&format!("\"\\N{{{name}}}\"")),
            Tag::String(NbtString::from_utf16(expected))
        );
    }
    for name in [
        "CJK UNIFIED IDEOGRAPH-4E00",
        "BASIC LATIN 0041",
        "HANGUL SYLLABLE GA",
        "NUL",
    ] {
        assert!(
            parse(&format!("\"\\N{{{name}}}\""), Limits::default()).is_err(),
            "{name}"
        );
    }
}

#[test]
fn compound_batch_build_retains_last_duplicate_without_reordering_utf16_keys() {
    let actual = read("{z:1,a:1,z:2,a:2,z:3}");
    let expected = Compound::from_entries(vec![
        CompoundEntry::new("a".into(), Tag::Int(2)),
        CompoundEntry::new("z".into(), Tag::Int(3)),
    ])
    .unwrap();
    assert_eq!(actual, Tag::Compound(expected));
    assert!(Compound::from_entries(vec![CompoundEntry::new("a".into(), Tag::End)]).is_ok());
}

#[test]
fn utf16_input_and_decoded_allocations_have_separate_preadmission_limits() {
    let limits = Limits {
        input_units: 3,
        ..Limits::default()
    };
    assert_eq!(
        parse("\"😀\"", limits).unwrap_err().kind,
        ErrorKind::InputLimit
    );
    let limits = Limits {
        allocation_bytes: 0,
        ..Limits::default()
    };
    assert_eq!(parse_utf16(&units("7"), limits), Ok(Tag::Int(7)));
    assert_eq!(
        parse("7", limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(
        parse_utf16(&units("\"x\""), limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(
        parse_utf16(&units("1.0"), limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(
        parse_utf16(&units("[1]"), limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
}

#[test]
fn parse_512_lists_on_the_default_test_thread_stack() {
    let input = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    let value = read(&input);
    assert_eq!(unwrap_nested(value, 512), Tag::Int(0));
    let excessive = format!("[{}]", input);
    assert_eq!(
        parse(&excessive, Limits::default()).unwrap_err().kind,
        ErrorKind::DepthLimit
    );
    let zero = Limits {
        max_depth: 0,
        ..Limits::default()
    };
    assert!(parse("7", zero).is_ok());
    for input in ["[]", "{}", "[B;]", "bool(1)"] {
        assert_eq!(parse(input, zero).unwrap_err().kind, ErrorKind::DepthLimit);
    }
}

#[test]
fn parse_512_compounds_on_the_default_test_thread_stack() {
    let compound = format!("{}0{}", "{a:".repeat(512), "}".repeat(512));
    let value = read(&compound);
    assert_eq!(unwrap_nested(value, 512), Tag::Int(0));
    assert_eq!(
        parse(&format!("{{a:{compound}}}"), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn parse_512_builtin_calls_on_the_default_test_thread_stack() {
    let calls = format!("{}1{}", "bool(".repeat(512), ")".repeat(512));
    assert_eq!(read(&calls), Tag::Byte(1));
    assert_eq!(
        parse(&format!("bool({calls})"), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
}

fn unwrap_nested(mut value: Tag, depth: usize) -> Tag {
    for _ in 0..depth {
        value = match value {
            Tag::List(mut children) => {
                assert_eq!(children.len(), 1);
                children.pop().unwrap()
            }
            Tag::Compound(mut compound) => {
                assert_eq!(compound.entries().len(), 1);
                compound.insert("a".into(), Tag::Int(0)).unwrap().unwrap()
            }
            _ => panic!("missing nested container"),
        };
    }
    value
}

#[test]
fn drop_512_containers_without_involving_a_parser_or_writer() {
    let mut list = Tag::Int(0);
    let mut compound = Tag::Int(0);
    for _ in 0..512 {
        list = Tag::List(vec![list]);
        compound = Tag::Compound(
            Compound::from_entries(vec![CompoundEntry::new("a".into(), compound)]).unwrap(),
        );
    }
    drop(list);
    drop(compound);
}

#[test]
fn broad_compounds_are_built_in_one_sort_and_fail_under_a_small_budget() {
    let input = format!(
        "{{{}}}",
        (0..4096)
            .rev()
            .map(|index| format!("key{index:04}:{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let Tag::Compound(compound) = read(&input) else {
        panic!()
    };
    assert_eq!(compound.entries().len(), 4096);
    assert_eq!(compound.entries()[0].name, NbtString::from("key0000"));
    assert_eq!(compound.entries()[4095].value, Tag::Int(4095));
    let limits = Limits {
        allocation_bytes: 1024,
        ..Limits::default()
    };
    assert_eq!(
        parse_utf16(&units(&input), limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(
        parse_utf16(
            &units("bool(\"0123456789\""),
            Limits {
                allocation_bytes: 1,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::AllocationBudget
    );
}

#[test]
fn uuid_matches_java_permissive_segments_and_unicode_hex_digits() {
    assert_eq!(
        read("uuid('+1-1-1-1-1')"),
        Tag::IntArray(vec![1, 65537, 65536, 1])
    );
    assert_eq!(
        read("uuid('100000000-1-1-1-1')"),
        Tag::IntArray(vec![0, 65537, 65536, 1])
    );
    assert_eq!(
        read("uuid('１-١-１-١-１')"),
        Tag::IntArray(vec![1, 65537, 65536, 1])
    );
    for input in ["uuid('bad')", "uuid('1--1-1-1')", "uuid('1-1-1-1-1-1')"] {
        assert!(parse(input, Limits::default()).is_err());
    }
}
