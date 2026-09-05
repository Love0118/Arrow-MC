use arrow_mc::snbt::{Diagnostic, DiagnosticArgument, ErrorKind, Limits, parse_utf16};

fn units(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

#[test]
fn exact_translation_keys_and_single_arguments_follow_first_farthest_failure() {
    for (input, key, argument) in [
        (
            "128b",
            "snbt.parser.number_parse_failure",
            "Value out of range. Value:\"128\" Radix:10",
        ),
        (
            "-129b",
            "snbt.parser.number_parse_failure",
            "Value out of range. Value:\"-129\" Radix:10",
        ),
        (
            "256ub",
            "snbt.parser.number_parse_failure",
            "out of range: 256",
        ),
        (
            "18446744073709551616000ul",
            "snbt.parser.number_parse_failure",
            "String value 18446744073709551616000 exceeds range of unsigned long.",
        ),
        ("32768s", "argument.literal.incorrect", "b|B"),
        ("foo(1)", "snbt.parser.no_such_operation", "foo/1"),
        ("bool()", "snbt.parser.no_such_operation", "bool/0"),
        ("\"\\u0\"", "snbt.parser.expected_hex_escape", "4"),
        (
            "\"\\U00110000\"",
            "snbt.parser.invalid_codepoint",
            "U+00110000",
        ),
        ("[", "argument.literal.incorrect", "B"),
    ] {
        let input = units(input);
        let failure = parse_utf16(&input, Limits::default()).unwrap_err();
        assert_eq!(failure.translation_key(), Some(key));
        let mut output = Vec::new();
        assert!(
            failure
                .diagnostic
                .unwrap()
                .write_argument(&input, &mut output, 1024)
                .unwrap()
        );
        assert_eq!(output, units(argument));
    }
}

#[test]
fn absent_arguments_and_resource_failures_do_not_invent_translation_parameters() {
    let input = units("bool('true')");
    let failure = parse_utf16(&input, Limits::default()).unwrap_err();
    let mut output = units("prefix");
    assert!(
        !failure
            .diagnostic
            .unwrap()
            .write_argument(&input, &mut output, 0)
            .unwrap()
    );
    assert_eq!(output, units("prefix"));
    let failure = parse_utf16(
        &units("[1]"),
        Limits {
            allocation_bytes: 0,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::AllocationBudget);
    assert_eq!(failure.diagnostic, None);
    assert_eq!(failure.translation_key(), None);
}

#[test]
fn diagnostic_argument_is_bounded_transactional_and_validates_source_spans() {
    let input = units("0xFF sb");
    let diagnostic = parse_utf16(&input, Limits::default())
        .unwrap_err()
        .diagnostic
        .unwrap();
    let mut expected = Vec::new();
    diagnostic
        .write_argument(&input, &mut expected, 1024)
        .unwrap();
    for limit in 0..expected.len() {
        let mut output = units("unchanged");
        assert_eq!(
            diagnostic
                .write_argument(&input, &mut output, limit)
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(output, units("unchanged"));
    }
    let invalid = Diagnostic {
        key: "snbt.parser.no_such_operation",
        argument: DiagnosticArgument::Operation {
            name_start: 4,
            name_end: 100,
            arity: 1,
        },
    };
    let mut output = units("prefix");
    assert_eq!(
        invalid
            .write_argument(&input, &mut output, 1024)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidDiagnostic
    );
    assert_eq!(output, units("prefix"));
    for input in ["_", "_1", "1_"] {
        let input = units(input);
        let invalid = Diagnostic {
            key: "snbt.parser.number_parse_failure",
            argument: DiagnosticArgument::Number {
                digits_start: 0,
                digits_end: input.len(),
                radix: 10,
                width: 8,
                unsigned: true,
                negative: false,
            },
        };
        assert_eq!(
            invalid
                .write_argument(&input, &mut output, 1024)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidDiagnostic
        );
        assert_eq!(output, units("prefix"));
    }
}
