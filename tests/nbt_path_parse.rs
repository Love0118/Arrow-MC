use arrow_mc::nbt::path::{Error, ErrorKind, Limits, Node, Path};
use arrow_mc::nbt::{NbtString, Tag};

fn units(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

fn hex_units(input: &str) -> Vec<u16> {
    assert_eq!(input.len() % 4, 0);
    (0..input.len())
        .step_by(4)
        .map(|offset| u16::from_str_radix(&input[offset..offset + 4], 16).unwrap())
        .collect()
}

fn argument_units(error: &Error, input: &[u16]) -> Option<Vec<u16>> {
    let mut output = Vec::new();
    error
        .write_argument(input, &mut output, usize::MAX)
        .unwrap()
        .then_some(output)
}

#[test]
fn official_path_parse_observations_preserve_cursor_spelling_and_diagnostics() {
    let mut count = 0;
    for line in include_str!("fixtures/nbt_path.tsv").lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        if columns[1] != "parse" {
            continue;
        }
        count += 1;
        let id = columns[0];
        let input = hex_units(columns[2]);
        let start = if columns[3] == "-" {
            0
        } else {
            columns[3].parse().unwrap()
        };
        let result = Path::parse_utf16(&input, start, Limits::default());
        if columns[14] == "1" {
            let (path, cursor) = result.unwrap_or_else(|error| panic!("{id}: {error:?}"));
            assert_eq!(cursor, columns[15].parse::<usize>().unwrap(), "{id}");
            assert_eq!(path.as_string().as_utf16(), hex_units(columns[16]), "{id}");
            continue;
        }
        let error = result.unwrap_err();
        if columns[25] != "-" {
            // A lone '[' throws an unchecked Java exception. Arrow exposes an
            // explicit invalid-path error instead of panicking at this boundary.
            assert_eq!(columns[25], "StringIndexOutOfBoundsException", "{id}");
            assert_eq!(error.kind, ErrorKind::InvalidPath, "{id}");
            continue;
        }
        assert_eq!(
            error.cursor,
            Some(columns[22].parse().unwrap()),
            "{id}: {error:?}"
        );
        if columns[23] != "-" {
            assert_eq!(error.key, columns[23], "{id}");
        }
        if columns[24] != "-" {
            let expected = if columns[24].is_empty() {
                None
            } else {
                Some(hex_units(columns[24].strip_prefix("s:").unwrap()))
            };
            assert_eq!(argument_units(&error, &input), expected, "{id}");
        }
    }
    assert!(
        count >= 52,
        "the baseline parser observations must be present"
    );
}

#[test]
fn all_six_nodes_keep_keys_indices_and_compound_predicates() {
    let input = r#"{root:1}.a.'x.y'[0001][][{a:1}].b{k:2}"#;
    let (path, end) = Path::parse(input, Limits::default()).unwrap();
    assert_eq!(end, input.len());
    assert_eq!(path.node_count(), 7);
    let nodes = path.nodes();
    assert!(matches!(&nodes[0], Node::MatchRoot(Tag::Compound(c))
        if c.get(&NbtString::from("root")) == Some(&Tag::Int(1))));
    assert!(matches!(&nodes[1], Node::Child(key) if key == &NbtString::from("a")));
    assert!(matches!(&nodes[2], Node::Child(key) if key == &NbtString::from("x.y")));
    assert!(matches!(&nodes[3], Node::Index(1)));
    assert!(matches!(&nodes[4], Node::All));
    assert!(matches!(&nodes[5], Node::MatchElement(Tag::Compound(c))
        if c.get(&NbtString::from("a")) == Some(&Tag::Int(1))));
    assert!(
        matches!(&nodes[6], Node::MatchChild { name, pattern: Tag::Compound(c) }
        if name == &NbtString::from("b") && c.get(&NbtString::from("k")) == Some(&Tag::Int(2)))
    );
}

#[test]
fn path_quotes_are_brigadier_quotes_but_predicates_use_snbt_escapes() {
    for (source, decoded) in [
        (r#""a\\b""#, "a\\b"),
        (r#""a\"b""#, "a\"b"),
        (r#"'a\'b'"#, "a'b"),
        ("\"line\nfeed\"", "line\nfeed"),
    ] {
        let (path, _) = Path::parse(source, Limits::default()).unwrap();
        assert!(
            matches!(&path.nodes()[0], Node::Child(name)
            if name.as_utf16() == units(decoded)),
            "{source}"
        );
    }
    for source in [r#""\n""#, r#""\u0041""#, r#""\N{SPACE}""#, r#"'\"'"#] {
        assert_eq!(
            Path::parse(source, Limits::default()).unwrap_err().kind,
            ErrorKind::InvalidQuotedEscape,
            "{source}"
        );
    }
    let (path, _) = Path::parse(r#"a[{x:"\u0041"}]"#, Limits::default()).unwrap();
    assert!(
        matches!(&path.nodes()[1], Node::MatchElement(Tag::Compound(c))
        if c.get(&NbtString::from("x")) == Some(&Tag::String(NbtString::from("A"))))
    );
}

#[test]
fn unclosed_quotes_and_invalid_indices_report_absolute_utf16_spans() {
    for source in ["\"a", "'a", "\"a\\"] {
        let error = Path::parse(source, Limits::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnclosedQuote);
        assert_eq!(error.cursor, Some(source.len()));
        assert_eq!(error.key, "parsing.quote.expected.end");
    }
    let input = units("😀 xx[2147483648] tail");
    let error = Path::parse_utf16(&input, 3, Limits::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidIndex);
    assert_eq!(error.cursor, Some(6));
    assert_eq!(argument_units(&error, &input), Some(units("2147483648")));
    let input = units("😀 xx[{x:256b}] tail");
    let error = Path::parse_utf16(&input, 3, Limits::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Snbt);
    assert_eq!(
        argument_units(&error, &input),
        Some(units("Value out of range. Value:\"256\" Radix:10"))
    );
    assert!(error.cursor.is_some_and(|cursor| cursor >= 9));
}

#[test]
fn utf16_input_preserves_isolated_surrogates_and_exact_start_and_suffix() {
    let input = [0x78, 0x20, 0x22, 0xd800, 0x22, 0x2e, 0x61, 0x20, 0x7a];
    let (path, end) = Path::parse_utf16(&input, 2, Limits::default()).unwrap();
    assert_eq!(end, 7);
    assert_eq!(path.as_string().as_utf16(), &input[2..7]);
    assert!(matches!(&path.nodes()[0], Node::Child(name) if name.as_utf16() == [0xd800]));
    assert!(matches!(&path.nodes()[1], Node::Child(name) if name == &NbtString::from("a")));
}

#[test]
fn arrow_admission_limits_do_not_replace_vanilla_path_grammar() {
    let (path, cursor) = Path::parse(" ", Limits::default()).unwrap();
    assert_eq!(path.node_count(), 0);
    assert_eq!(cursor, 0);
    let source = std::iter::repeat_n("a", 513).collect::<Vec<_>>().join(".");
    assert_eq!(
        Path::parse(&source, Limits::default())
            .unwrap()
            .0
            .node_count(),
        513
    );
    let limits = Limits {
        node_count: 512,
        ..Limits::default()
    };
    assert_eq!(
        Path::parse(&source, limits).unwrap_err().kind,
        ErrorKind::NodeLimit
    );
    let limits = Limits {
        input_units: 1,
        ..Limits::default()
    };
    assert_eq!(
        Path::parse("a suffix", limits).unwrap_err().kind,
        ErrorKind::InputLimit
    );
    let limits = Limits {
        allocation_bytes: 0,
        ..Limits::default()
    };
    assert_eq!(
        Path::parse_utf16(&units("a"), 0, limits).unwrap_err().kind,
        ErrorKind::AllocationBudget
    );
    let limits = Limits {
        work_units: 0,
        ..Limits::default()
    };
    assert_eq!(
        Path::parse_utf16(&units("a"), 0, limits).unwrap_err().kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(
        Path::parse_utf16(&[], 1, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidPath
    );
}

#[test]
fn deep_snbt_predicates_keep_the_existing_snbt_stack_policy() {
    let input = format!("a{{x:{}1{}}}", "[".repeat(511), "]".repeat(511));
    let (path, consumed) = Path::parse(&input, Limits::default()).unwrap();
    assert_eq!(consumed, input.len());
    assert_eq!(path.node_count(), 1);
    let input = format!("a{{x:{}1{}}}", "[".repeat(512), "]".repeat(512));
    assert_eq!(
        Path::parse(&input, Limits::default()).unwrap_err().kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn arbitrarily_many_leading_zeroes_follow_decimal_integer_value() {
    let input = format!("[-{}2147483648]", "0".repeat(1024));
    let (path, _) = Path::parse(&input, Limits::default()).unwrap();
    assert!(matches!(path.nodes(), [Node::Index(i32::MIN)]));
    let input = format!("[{}2147483648]", "0".repeat(1024));
    assert_eq!(
        Path::parse(&input, Limits::default()).unwrap_err().kind,
        ErrorKind::InvalidIndex
    );
}
