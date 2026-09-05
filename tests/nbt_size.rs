//! Packet admission must measure the existing binary writer, without temporary
//! serialization buffers. These cases compare size/error boundaries to writing.
use arrow_mc::nbt::{Compound, Error, Limits, NbtString, Tag, network_encoded_len, write_network};

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    let mut result = Compound::new();
    for (name, value) in entries {
        result.insert(name.into(), value).unwrap();
    }
    Tag::Compound(result)
}

fn written_len(tag: &Tag, limits: Limits) -> Result<usize, Error> {
    let mut bytes = vec![42];
    let result = write_network(tag, &mut bytes, limits).map(|()| bytes.len() - 1);
    if result.is_err() {
        assert_eq!(bytes, [42]);
    }
    result
}

#[test]
fn all_tag_kinds_and_optional_compound_roots_have_exact_sizes() {
    for (value, expected) in [
        (Tag::End, 1),
        (Tag::Byte(-1), 2),
        (Tag::Short(-2), 3),
        (Tag::Int(-3), 5),
        (Tag::Long(-4), 9),
        (Tag::Float(f32::NAN), 5),
        (Tag::Double(-0.0), 9),
        (Tag::ByteArray(vec![1, 2, 3]), 8),
        (Tag::IntArray(vec![1, 2, 3]), 17),
        (Tag::LongArray(vec![1, 2, 3]), 29),
        (Tag::String("abc".into()), 6),
        (Tag::List(vec![]), 6),
        (Tag::List(vec![Tag::Int(7), Tag::Int(8)]), 14),
        (compound(vec![]), 2),
        (compound(vec![("x", Tag::Int(7))]), 10),
    ] {
        assert_eq!(network_encoded_len(&value, Limits::default()), Ok(expected));
        assert_eq!(written_len(&value, Limits::default()), Ok(expected));
    }
}

#[test]
fn mixed_list_wrappers_and_real_empty_key_compounds_match_actual_bytes() {
    let values = [
        Tag::List(vec![Tag::Int(7), Tag::String("x".into())]),
        Tag::List(vec![compound(vec![("", Tag::Int(7))])]),
        Tag::List(vec![compound(vec![("x", Tag::Int(7))])]),
        Tag::List(vec![
            Tag::List(vec![Tag::Byte(1)]),
            compound(vec![("", Tag::String("x".into()))]),
        ]),
        compound(vec![(
            "string",
            Tag::String(NbtString::from_utf16(vec![
                0, 0x7f, 0x80, 0x7ff, 0x800, 0xd800, 0xd83d, 0xde00,
            ])),
        )]),
    ];
    for value in values {
        let expected = written_len(&value, Limits::default()).unwrap();
        for output_bytes in 0..=expected + 1 {
            for max_depth in [0, 1, 2, 3, 512] {
                let limits = Limits {
                    output_bytes,
                    max_depth,
                    ..Limits::default()
                };
                assert_eq!(
                    network_encoded_len(&value, limits),
                    written_len(&value, limits),
                    "limit={output_bytes}, depth={max_depth}"
                );
            }
        }
    }
}

#[test]
fn invalid_runtime_values_preserve_writer_error_precedence() {
    let values = [
        Tag::List(vec![Tag::Int(1), Tag::End]),
        compound(vec![("a", Tag::Byte(1)), ("b", Tag::End)]),
        Tag::String(NbtString::from_utf16(vec![0; 32768])),
        compound(vec![(
            "a",
            Tag::String(NbtString::from_utf16(vec![0xd800; 21846])),
        )]),
    ];
    for value in values {
        for output_bytes in [0, 1, 2, 5, 65538] {
            for max_depth in [0, 1, 512, 513] {
                let limits = Limits {
                    output_bytes,
                    max_depth,
                    ..Limits::default()
                };
                assert_eq!(
                    network_encoded_len(&value, limits),
                    written_len(&value, limits)
                );
            }
        }
    }
}

#[test]
fn strings_obey_modified_utf8_byte_limit_and_ignore_decode_only_budgets() {
    let exact = Tag::String(NbtString::from_utf16(vec![65; 65535]));
    let limits = Limits {
        vanilla_quota_bytes: 0,
        allocation_bytes: 0,
        output_bytes: usize::MAX,
        ..Limits::default()
    };
    assert_eq!(network_encoded_len(&exact, limits), Ok(65538));
    assert_eq!(
        network_encoded_len(&exact, limits),
        written_len(&exact, limits)
    );
    let over = Tag::String(NbtString::from_utf16(vec![65; 65536]));
    assert_eq!(
        network_encoded_len(&over, limits),
        Err(Error::StringTooLong)
    );
}

#[test]
fn fixed_stack_handles_512_container_depth_and_rejects_513() {
    let mut list = Tag::Int(7);
    for _ in 0..512 {
        list = Tag::List(vec![list]);
    }
    assert_eq!(
        network_encoded_len(&list, Limits::default()),
        Ok(1 + 512 * 5 + 4)
    );
    assert_eq!(
        network_encoded_len(&list, Limits::default()),
        written_len(&list, Limits::default())
    );
    let excessive = Tag::List(vec![list]);
    assert_eq!(
        network_encoded_len(&excessive, Limits::default()),
        Err(Error::DepthLimit)
    );
    excessive.drop_iterative();

    let mut value = Tag::Int(7);
    for _ in 0..512 {
        value = compound(vec![("x", value)]);
    }
    assert_eq!(
        network_encoded_len(&value, Limits::default()),
        Ok(1 + 512 * 5 + 4)
    );
    assert_eq!(
        network_encoded_len(&value, Limits::default()),
        written_len(&value, Limits::default())
    );
    value.drop_iterative();

    let mut value = Tag::Int(7);
    for _ in 0..256 {
        value = Tag::List(vec![Tag::Byte(1), value]);
    }
    assert_eq!(
        network_encoded_len(&value, Limits::default()),
        written_len(&value, Limits::default())
    );
    assert_eq!(
        network_encoded_len(
            &value,
            Limits {
                max_depth: 511,
                ..Limits::default()
            }
        ),
        Err(Error::DepthLimit)
    );
    value.drop_iterative();
}

#[test]
fn wide_byte_array_trees_match_actual_writer_size() {
    let values: Vec<_> = (0..1024)
        .map(|index| {
            compound(vec![
                ("index", Tag::Int(index)),
                ("bytes", Tag::ByteArray(vec![1; 64])),
            ])
        })
        .collect();
    let tag = Tag::List(values);
    assert_eq!(
        network_encoded_len(&tag, Limits::default()),
        written_len(&tag, Limits::default())
    );
}
