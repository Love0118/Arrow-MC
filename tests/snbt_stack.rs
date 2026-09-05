//! Default-stack writer regressions, independent of parsing and recursive drop.
//! Each test name isolates one format, container shape and depth boundary.

use arrow_mc::nbt::{Compound, NbtString, Tag};
use arrow_mc::snbt::{self, Error, ErrorKind, Limits};

#[derive(Clone, Copy)]
enum Shape {
    List,
    Compound,
}

#[derive(Clone, Copy)]
enum Format {
    Compact,
    Pretty,
}

fn nested(mut value: Tag, shape: Shape, depth: usize) -> Tag {
    for _ in 0..depth {
        value = match shape {
            Shape::List => Tag::List(vec![value]),
            Shape::Compound => {
                let mut compound = Compound::new();
                compound.insert("x".into(), value).unwrap();
                Tag::Compound(compound)
            }
        };
    }
    value
}

// The writer is the stack boundary under test. Releasing one owned child per
// iteration avoids confusing a recursive Tag destructor failure with writing.
fn release_chain(mut value: Tag) {
    loop {
        value = match value {
            Tag::List(mut children) => children.pop().unwrap(),
            Tag::Compound(mut compound) => {
                compound.insert("x".into(), Tag::Int(0)).unwrap().unwrap()
            }
            _ => return,
        };
    }
}

fn write(format: Format, tag: &Tag, output: &mut Vec<u16>, limits: Limits) -> Result<(), Error> {
    match format {
        Format::Compact => snbt::write(tag, output, limits),
        Format::Pretty => snbt::write_pretty(tag, output, limits),
    }
}

fn append_ascii(output: &mut Vec<u16>, text: &str) {
    output.extend(text.bytes().map(u16::from));
}

fn expected_nesting(shape: Shape, format: Format, depth: usize, leaf: &[u16]) -> Vec<u16> {
    let mut output = Vec::new();
    for level in 0..depth {
        append_ascii(
            &mut output,
            match shape {
                Shape::List => "[",
                Shape::Compound => "{",
            },
        );
        if matches!(format, Format::Pretty) {
            output.push(u16::from(b'\n'));
            output.extend(std::iter::repeat_n(u16::from(b' '), 4 * (level + 1)));
        }
        if matches!(shape, Shape::Compound) {
            append_ascii(
                &mut output,
                if matches!(format, Format::Pretty) {
                    "x: "
                } else {
                    "x:"
                },
            );
        }
    }
    output.extend_from_slice(leaf);
    for level in (0..depth).rev() {
        if matches!(format, Format::Pretty) {
            output.push(u16::from(b'\n'));
            output.extend(std::iter::repeat_n(u16::from(b' '), 4 * level));
        }
        output.push(u16::from(match shape {
            Shape::List => b']',
            Shape::Compound => b'}',
        }));
    }
    output
}

fn verify_512(shape: Shape, format: Format) {
    let numeric_leaves = [
        (Tag::Int(i32::MIN), "-2147483648"),
        (Tag::Long(i64::MIN), "-9223372036854775808L"),
        (Tag::Float(f32::from_bits(1)), "1.4E-45f"),
        (Tag::Double(f64::MAX), "1.7976931348623157E308d"),
    ];
    let mut leaves: Vec<_> = numeric_leaves
        .into_iter()
        .map(|(tag, spelling)| (tag, spelling.encode_utf16().collect::<Vec<_>>()))
        .collect();
    leaves.push((
        Tag::String(NbtString::from_utf16(vec![0xd800, 0x0a, 0x22, 0x61, 0x27])),
        vec![0x27, 0xd800, 0x5c, 0x6e, 0x22, 0x61, 0x5c, 0x27, 0x27],
    ));
    for (leaf, spelling) in leaves {
        let tag = nested(leaf, shape, 512);
        let mut output = vec![0xdc00, 0x2a];
        let result = write(format, &tag, &mut output, Limits::default());
        // Teardown and assertion happen after writing, without cloning or
        // formatting the nested Tag even when the returned result is an error.
        release_chain(tag);
        result.unwrap();
        assert_eq!(&output[..2], &[0xdc00, 0x2a]);
        assert_eq!(
            &output[2..],
            expected_nesting(shape, format, 512, &spelling)
        );
    }
}

fn verify_513(shape: Shape, format: Format, expected_offset: usize) {
    let tag = nested(Tag::Int(7), shape, 513);
    let mut output = vec![0xd800, 0xdc00, 0xffff];
    let result = write(format, &tag, &mut output, Limits::default());
    release_chain(tag);
    assert_eq!(
        result,
        Err(Error {
            offset_utf16: expected_offset,
            kind: ErrorKind::DepthLimit,
            diagnostic: None,
        })
    );
    assert_eq!(output, [0xd800, 0xdc00, 0xffff]);
}

fn verify_leaf_output_limit(shape: Shape, format: Format, leaf_offset: usize) {
    let tag = nested(Tag::String("abc".into()), shape, 512);
    let mut output = vec![0xdfff, 0x2a];
    // Admit the opening quote and "ab", then fail on the final string unit
    // while every recursive container frame is still active.
    let cutoff = leaf_offset + 3;
    let result = write(
        format,
        &tag,
        &mut output,
        Limits {
            output_units: cutoff,
            ..Limits::default()
        },
    );
    release_chain(tag);
    assert_eq!(
        result,
        Err(Error {
            offset_utf16: cutoff,
            kind: ErrorKind::OutputLimit,
            diagnostic: None,
        })
    );
    assert_eq!(output, [0xdfff, 0x2a]);
}

#[test]
fn compact_list_512_default_stack() {
    verify_512(Shape::List, Format::Compact);
}

#[test]
fn compact_compound_512_default_stack() {
    verify_512(Shape::Compound, Format::Compact);
}

#[test]
fn pretty_list_512_default_stack() {
    verify_512(Shape::List, Format::Pretty);
}

#[test]
fn pretty_compound_512_default_stack() {
    verify_512(Shape::Compound, Format::Pretty);
}

#[test]
fn compact_list_513_reports_depth_and_rolls_back() {
    verify_513(Shape::List, Format::Compact, 512);
}

#[test]
fn compact_compound_513_reports_depth_and_rolls_back() {
    verify_513(Shape::Compound, Format::Compact, 1536);
}

#[test]
fn pretty_list_513_reports_depth_and_rolls_back() {
    // 512 opening brackets/newlines plus 4 * (1 + ... + 512) spaces.
    verify_513(Shape::List, Format::Pretty, 526_336);
}

#[test]
fn pretty_compound_513_reports_depth_and_rolls_back() {
    // The same prefix plus the three units in "x: " for every compound.
    verify_513(Shape::Compound, Format::Pretty, 527_872);
}

#[test]
fn compact_list_512_output_limit_unwinds_and_preserves_prefix() {
    verify_leaf_output_limit(Shape::List, Format::Compact, 512);
}

#[test]
fn compact_compound_512_output_limit_unwinds_and_preserves_prefix() {
    verify_leaf_output_limit(Shape::Compound, Format::Compact, 1536);
}

#[test]
fn pretty_list_512_output_limit_unwinds_and_preserves_prefix() {
    verify_leaf_output_limit(Shape::List, Format::Pretty, 526_336);
}

#[test]
fn pretty_compound_512_output_limit_unwinds_and_preserves_prefix() {
    verify_leaf_output_limit(Shape::Compound, Format::Pretty, 527_872);
}
