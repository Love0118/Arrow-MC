use arrow_mc::nbt::{Compound, NbtString, Tag};
use arrow_mc::snbt::{self, ErrorKind, Limits};

fn render(tag: &Tag) -> String {
    let mut output = Vec::new();
    snbt::write(tag, &mut output, Limits::default()).unwrap();
    String::from_utf16(&output).unwrap()
}

#[test]
fn compact_tag_forms() {
    for (tag, expected) in [
        (Tag::End, "END"),
        (Tag::Byte(i8::MIN), "-128b"),
        (Tag::Short(i16::MIN), "-32768s"),
        (Tag::Int(i32::MIN), "-2147483648"),
        (Tag::Long(i64::MIN), "-9223372036854775808L"),
        (Tag::ByteArray(vec![-128, 0, 127]), "[B;-128B,0B,127B]"),
        (
            Tag::IntArray(vec![i32::MIN, i32::MAX]),
            "[I;-2147483648,2147483647]",
        ),
        (
            Tag::LongArray(vec![i64::MIN, i64::MAX]),
            "[L;-9223372036854775808L,9223372036854775807L]",
        ),
        (Tag::ByteArray(vec![]), "[B;]"),
        (Tag::IntArray(vec![]), "[I;]"),
        (Tag::LongArray(vec![]), "[L;]"),
        (
            Tag::List(vec![Tag::Int(1), Tag::String("two".into())]),
            "[1,\"two\"]",
        ),
    ] {
        assert_eq!(render(&tag), expected);
    }
}

#[test]
fn quote_selection_controls_and_utf16() {
    assert_eq!(render(&Tag::String("".into())), "\"\"");
    assert_eq!(render(&Tag::String("a\"b'c".into())), "'a\"b\\'c'");
    assert_eq!(render(&Tag::String("a'b\"c".into())), "\"a'b\\\"c\"");
    assert_eq!(
        render(&Tag::String("\\\u{8}\t\n\u{c}\r\0\u{b}\u{1f}".into())),
        "\"\\\\\\b\\t\\n\\f\\r\\x00\\x0B\\x1F\""
    );
    let units = vec![0xd800, 0xdc00, 0xdfff, 0x7f, 0x85, 0x2028];
    let mut output = Vec::new();
    snbt::write(
        &Tag::String(NbtString::from_utf16(units.clone())),
        &mut output,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(output, [vec![0x22], units, vec![0x22]].concat());
}

#[test]
fn keys_sort_by_utf16_and_quote_ambiguous_names() {
    let mut compound = Compound::new();
    for name in [
        "z", "true", "FALSE", "9foo", "", "_a+9-.", "a:b", ".a", "-a",
    ] {
        compound.insert(name.into(), Tag::Int(1)).unwrap();
    }
    assert_eq!(
        render(&Tag::Compound(compound)),
        "{\"\":1,\"-a\":1,.a:1,\"9foo\":1,\"FALSE\":1,_a+9-.:1,\"a:b\":1,\"true\":1,z:1}"
    );
}

#[test]
fn java_numeric_boundary_spellings() {
    for (tag, expected) in [
        (Tag::Float(0.0), "0.0f"),
        (Tag::Float(-0.0), "-0.0f"),
        (Tag::Float(f32::from_bits(1)), "1.4E-45f"),
        (Tag::Float(f32::from_bits(2)), "2.8E-45f"),
        (Tag::Float(f32::MIN_POSITIVE), "1.1754944E-38f"),
        (Tag::Float(f32::MAX), "3.4028235E38f"),
        (Tag::Double(f64::from_bits(1)), "4.9E-324d"),
        (Tag::Double(f64::from_bits(2)), "9.9E-324d"),
        (Tag::Double(f64::MIN_POSITIVE), "2.2250738585072014E-308d"),
        (Tag::Double(f64::MAX), "1.7976931348623157E308d"),
        (Tag::Double(0.001), "0.001d"),
        (Tag::Double(0.0001), "1.0E-4d"),
        (Tag::Double(9999999.0), "9999999.0d"),
        (Tag::Double(10000000.0), "1.0E7d"),
        (Tag::Double(1e23), "1.0E23d"),
        (Tag::Double(-0.0), "-0.0d"),
        (Tag::Float(f32::NAN), "NaNf"),
        (Tag::Double(f64::from_bits(u64::MAX)), "NaNd"),
        (Tag::Float(f32::INFINITY), "Infinityf"),
        (Tag::Double(f64::NEG_INFINITY), "-Infinityd"),
    ] {
        assert_eq!(render(&tag), expected, "{tag:?}");
    }
}

#[test]
fn output_limits_are_per_call_and_transactional() {
    let mut output = vec![0xd800, 42];
    let prefix = output.clone();
    let tag = Tag::String("abc".into());
    let error = snbt::write(
        &tag,
        &mut output,
        Limits {
            output_units: 4,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    assert_eq!(output, prefix);
    snbt::write(
        &tag,
        &mut output,
        Limits {
            output_units: 5,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(&output[..prefix.len()], &prefix);
    assert_eq!(
        &output[prefix.len()..],
        &"\"abc\"".encode_utf16().collect::<Vec<_>>()
    );
    let before = output.clone();
    assert_eq!(
        snbt::write(
            &tag,
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
    assert_eq!(output, before);
}

#[test]
fn nested_writer_limit_and_default_stack() {
    let mut tag = Tag::Int(1);
    for _ in 0..512 {
        tag = Tag::List(vec![tag]);
    }
    let mut output = Vec::new();
    snbt::write(&tag, &mut output, Limits::default()).unwrap();
    assert_eq!(output.len(), 1025);
    let prefix = output.clone();
    assert_eq!(
        snbt::write(
            &tag,
            &mut output,
            Limits {
                max_depth: 511,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
    assert_eq!(output, prefix);
}

// The Java source here is an independently written API caller, not upstream
// implementation. It is compiled only for this explicitly requested oracle.
#[test]
#[ignore = "requires installed Java 25; runs live canonical float differential corpus"]
fn java25_float_oracle() {
    use std::fmt::Write as _;
    use std::process::Command;
    let scratch =
        std::env::temp_dir().join(format!("arrow-snbt-float-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let source = scratch.join("ArrowFloatOracle.java");
    let input_path = scratch.join("input.tsv");
    std::fs::write(
        &source,
        r#"
import java.nio.file.*;
public class ArrowFloatOracle {
    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 25) throw new IllegalStateException("Java 25 required");
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] parts = line.split(" ");
            long bits = Long.parseUnsignedLong(parts[1], 16);
            String value = parts[0].equals("f") ? Float.toString(Float.intBitsToFloat((int)bits))
                : Double.toString(Double.longBitsToDouble(bits));
            System.out.println(line + " " + value);
        }
    }
}
"#,
    )
    .unwrap();
    let mut cases = Vec::new();
    let mut add_f32 = |bits: u32| {
        for delta in -2_i64..=2 {
            if let Ok(bits) = u32::try_from(i64::from(bits) + delta) {
                cases.push((b'f', u64::from(bits)));
                cases.push((b'f', u64::from(bits ^ (1 << 31))));
            }
        }
    };
    for exponent in 0..=255 {
        add_f32(exponent << 23);
    }
    for exponent in -45..=38 {
        add_f32(format!("1e{exponent}").parse::<f32>().unwrap().to_bits());
    }
    for bits in 0..256 {
        add_f32(bits);
    }
    for bits in [u32::MAX, 0x7f800000, 0x7fc00000, 0x7f7fffff] {
        add_f32(bits);
    }
    let mut add_f64 = |bits: u64| {
        for delta in -2_i128..=2 {
            if let Ok(bits) = u64::try_from(i128::from(bits) + delta) {
                cases.push((b'd', bits));
                cases.push((b'd', bits ^ (1 << 63)));
            }
        }
    };
    for exponent in 0..=2047 {
        add_f64(exponent << 52);
    }
    for exponent in -324..=308 {
        add_f64(format!("1e{exponent}").parse::<f64>().unwrap().to_bits());
    }
    for bits in 0..256 {
        add_f64(bits);
    }
    for bits in [
        u64::MAX,
        0x7ff0000000000000,
        0x7ff8000000000000,
        0x7fefffffffffffff,
    ] {
        add_f64(bits);
    }
    let mut state = 0x7e3a_919c_a8df_445d_u64;
    for _ in 0..20000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        cases.push((b'f', u64::from(state as u32)));
        cases.push((b'd', state));
    }
    cases.sort_unstable();
    cases.dedup();
    let mut input = String::new();
    for &(kind, bits) in &cases {
        writeln!(input, "{} {bits:x}", char::from(kind)).unwrap();
    }
    std::fs::write(&input_path, input).unwrap();
    let result = Command::new("java")
        .arg(&source)
        .arg(&input_path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert_eq!(stdout.lines().count(), cases.len());
    let started = std::time::Instant::now();
    let mut units = Vec::with_capacity(48);
    for (line, &(kind, bits)) in stdout.lines().zip(&cases) {
        let expected = line.rsplit_once(' ').unwrap().1;
        let tag = if kind == b'f' {
            Tag::Float(f32::from_bits(bits as u32))
        } else {
            Tag::Double(f64::from_bits(bits))
        };
        units.clear();
        snbt::write(&tag, &mut units, Limits::default()).unwrap();
        let actual = String::from_utf16(&units[..units.len() - 1]).unwrap();
        assert_eq!(actual, expected, "kind={} bits={bits:x}", char::from(kind));
    }
    eprintln!(
        "Java 25 canonical float corpus: {} values matched; Rust comparison {:?}",
        cases.len(),
        started.elapsed()
    );
    // Remove only the two exact files created by this test; no recursive delete.
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(input_path).unwrap();
    std::fs::remove_dir(scratch).unwrap();
}
