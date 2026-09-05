//! Manual binary fixtures and independently obtained 26.3-pre-2 JVM vectors.
//! These assertions cover wire meaning, not only this codec's own round trips.
use arrow_mc::nbt::{
    Compound, Error, Limits, NamedTag, NbtString, Tag, read_named, read_network, write_named,
    write_network,
};

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn decode(hex: &str) -> Tag {
    let input = bytes(hex);
    let mut remaining = input.as_slice();
    let result = read_network(&mut remaining, Limits::default()).unwrap();
    assert!(remaining.is_empty());
    result
}

fn encode(tag: &Tag) -> Vec<u8> {
    let mut output = Vec::new();
    write_network(tag, &mut output, Limits::default()).unwrap();
    output
}

fn compound(entries: &[(&str, Tag)]) -> Compound {
    let mut result = Compound::new();
    for (key, value) in entries {
        result.insert((*key).into(), value.clone()).unwrap();
    }
    result
}

#[test]
fn all_thirteen_tag_ids_have_manual_big_endian_fixtures() {
    let fixtures = [
        ("00", Tag::End),
        ("01ff", Tag::Byte(-1)),
        ("028000", Tag::Short(i16::MIN)),
        ("0301020304", Tag::Int(0x01020304)),
        ("048000000000000000", Tag::Long(i64::MIN)),
        ("053fc00000", Tag::Float(1.5)),
        ("06c002000000000000", Tag::Double(-2.25)),
        ("0700000003007fff", Tag::ByteArray(vec![0, 127, -1])),
        ("0800026869", Tag::String("hi".into())),
        (
            "09020000000200018000",
            Tag::List(vec![Tag::Short(1), Tag::Short(i16::MIN)]),
        ),
        (
            "0a01000178ff00",
            Tag::Compound(compound(&[("x", Tag::Byte(-1))])),
        ),
        (
            "0b000000020102030480000000",
            Tag::IntArray(vec![0x01020304, i32::MIN]),
        ),
        (
            "0c0000000200000000000000018000000000000000",
            Tag::LongArray(vec![1, i64::MIN]),
        ),
    ];
    for (hex, tag) in fixtures {
        assert_eq!(decode(hex), tag, "decode {hex}");
        assert_eq!(encode(&tag), bytes(hex), "encode {hex}");
    }
}

#[test]
fn disk_name_and_network_root_are_distinct_and_prefix_consuming() {
    let source = bytes("0a0003726f6f010001780100ff");
    let mut input = source.as_slice();
    let named = read_named(&mut input, Limits::default()).unwrap();
    assert_eq!(named.name, NbtString::from("roo"));
    assert_eq!(named.tag, Tag::Compound(compound(&[("x", Tag::Byte(1))])));
    assert_eq!(input, &[0xff]);
    let mut output = vec![0x42];
    write_named(&named, &mut output, Limits::default()).unwrap();
    assert_eq!(&output[1..], &source[..source.len() - 1]);
    assert_eq!(encode(&named.tag), bytes("0a010001780100"));
    assert_eq!(
        read_named(&mut &[0u8][..], Limits::default()).unwrap(),
        NamedTag {
            name: NbtString::default(),
            tag: Tag::End
        }
    );
}

#[test]
fn modern_lists_wrap_mixed_values_and_escape_real_empty_key_compounds() {
    let mixed = Tag::List(vec![Tag::Int(7), Tag::String("x".into())]);
    let mixed_hex = "090a00000002030000000000070008000000017800";
    assert_eq!(decode(mixed_hex), mixed);
    assert_eq!(encode(&mixed), bytes(mixed_hex));

    let escaped = Tag::List(vec![Tag::Compound(compound(&[("", Tag::Int(7))]))]);
    let escaped_hex = "090a000000010a0000030000000000070000";
    assert_eq!(decode(escaped_hex), escaped);
    assert_eq!(encode(&escaped), bytes(escaped_hex));

    let ordinary = Tag::List(vec![Tag::Compound(compound(&[("x", Tag::Int(7))]))]);
    let ordinary_hex = "090a00000001030001780000000700";
    assert_eq!(decode(ordinary_hex), ordinary);
    assert_eq!(encode(&ordinary), bytes(ordinary_hex));
    assert_eq!(
        decode("090a000000010300000000000700"),
        Tag::List(vec![Tag::Int(7)])
    );
    assert_eq!(
        decode("090a0000000100"),
        Tag::List(vec![Tag::Compound(Compound::new())])
    );
}

#[test]
fn modified_utf8_preserves_utf16_nul_supplementary_and_isolated_surrogates() {
    for (hex, units) in [
        ("080002c080", vec![0]),
        ("080006eda0bdedb880", vec![0xd83d, 0xde00]),
        ("080003eda080", vec![0xd800]),
        ("080003edb080", vec![0xdc00]),
        ("0800067fc280e0a080", vec![0x7f, 0x80, 0x800]),
    ] {
        let tag = Tag::String(NbtString::from_utf16(units));
        assert_eq!(decode(hex), tag);
        assert_eq!(encode(&tag), bytes(hex));
    }
    assert!(NbtString::from_utf16(vec![0xd800]).to_string().is_err());
}

#[test]
fn java_read_utf_tolerates_noncanonical_encodings_but_rejects_bad_sequences() {
    assert_eq!(
        decode("08000100"),
        Tag::String(NbtString::from_utf16(vec![0]))
    );
    assert_eq!(decode("080002c181"), Tag::String("A".into()));
    assert_eq!(
        decode("080003e08080"),
        Tag::String(NbtString::from_utf16(vec![0]))
    );
    for hex in [
        "080004f09f9880",
        "08000180",
        "080001c0",
        "080002c041",
        "080002e080",
        "080003e08041",
        "080001ff",
    ] {
        assert_eq!(
            read_network(&mut bytes(hex).as_slice(), Limits::default()),
            Err(Error::InvalidModifiedUtf8),
            "{hex}"
        );
    }
}

#[test]
fn names_use_modified_utf8_too_and_duplicates_keep_the_last_value() {
    let decoded = decode("0a03000178000000010300017800000002010002c080ff00");
    let Tag::Compound(value) = decoded else {
        panic!()
    };
    assert_eq!(value.entries().len(), 2);
    assert_eq!(value.get(&"x".into()), Some(&Tag::Int(2)));
    assert_eq!(
        value.get(&NbtString::from_utf16(vec![0])),
        Some(&Tag::Byte(-1))
    );
    // Sorting the map is deterministic and does not retain duplicate wire keys.
    assert_eq!(
        encode(&Tag::Compound(value)),
        bytes("0a010002c080ff030001780000000200")
    );
}

#[test]
fn three_interleaved_duplicate_values_keep_the_final_input_value() {
    let decoded = decode(
        "0a0300017800000001030001790000000a03000178000000020300017900000014030001780000000300",
    );
    assert_eq!(
        decoded,
        Tag::Compound(compound(&[("x", Tag::Int(3)), ("y", Tag::Int(20))]))
    );
    assert_eq!(
        encode(&decoded),
        bytes("0a0300017800000003030001790000001400")
    );
}

#[test]
fn floating_point_decode_and_write_match_java_zero_and_nan_rules() {
    assert_eq!(decode("0580000000"), Tag::Float(0.0));
    assert_eq!(decode("068000000000000000"), Tag::Double(0.0));
    assert_eq!(encode(&Tag::Float(-0.0)), bytes("0580000000"));
    assert_eq!(encode(&Tag::Double(-0.0)), bytes("068000000000000000"));
    let Tag::Float(value) = decode("057f800001") else {
        panic!()
    };
    assert_eq!(value.to_bits(), 0x7f800001);
    assert_eq!(encode(&Tag::Float(value)), bytes("057fc00000"));
    let Tag::Double(value) = decode("06fff8123456789abc") else {
        panic!()
    };
    assert_eq!(value.to_bits(), 0xfff8123456789abc);
    assert_eq!(encode(&Tag::Double(value)), bytes("067ff8000000000000"));
    assert_eq!(Tag::Float(f32::NAN), Tag::Float(f32::from_bits(0xffc12345)));
    assert_eq!(
        Tag::Double(f64::NAN),
        Tag::Double(f64::from_bits(0xfff8123456789abc))
    );
    assert_ne!(Tag::Float(0.0), Tag::Float(-0.0));
    assert_ne!(Tag::Double(0.0), Tag::Double(-0.0));
}

#[test]
fn unknown_empty_list_type_is_accepted_and_canonicalized() {
    for hex in ["090000000000", "090100000000", "09ff00000000"] {
        let tag = decode(hex);
        assert_eq!(tag, Tag::List(vec![]));
        assert_eq!(encode(&tag), bytes("090000000000"));
    }
    assert_eq!(
        read_network(&mut bytes("09ff00000001ff").as_slice(), Limits::default()),
        Err(Error::UnknownTag(255))
    );
}

#[test]
fn invalid_end_unknown_tags_and_negative_lengths_are_rejected() {
    for id in [13, 127, 128, 255] {
        assert_eq!(
            read_network(&mut &[id][..], Limits::default()),
            Err(Error::UnknownTag(id))
        );
    }
    for hex in ["07ffffffff", "0901ffffffff", "0bffffffff", "0cffffffff"] {
        assert_eq!(
            read_network(&mut bytes(hex).as_slice(), Limits::default()),
            Err(Error::NegativeLength(-1))
        );
    }
    assert_eq!(
        read_network(&mut bytes("090000000001").as_slice(), Limits::default()),
        Err(Error::UnexpectedEnd)
    );
    assert_eq!(
        write_network(
            &Tag::List(vec![Tag::End]),
            &mut Vec::new(),
            Limits::default()
        ),
        Err(Error::UnexpectedEnd)
    );
    let mut runtime = Compound::new();
    assert_eq!(runtime.insert("bad".into(), Tag::End), Ok(None));
    assert_eq!(
        write_network(&Tag::Compound(runtime), &mut Vec::new(), Limits::default()),
        Err(Error::UnexpectedEnd)
    );
}

#[test]
fn every_truncation_of_a_nested_fixture_fails_without_advancing_the_input() {
    let full = bytes("0a090001780a0000000203000000000007000800000001780000");
    assert!(read_network(&mut full.as_slice(), Limits::default()).is_ok());
    for length in 0..full.len() {
        let original = &full[..length];
        let mut input = original;
        assert!(
            read_network(&mut input, Limits::default()).is_err(),
            "prefix {length}"
        );
        assert_eq!(input, original);
    }
}

#[test]
fn quota_and_decoded_allocation_are_separate_and_checked_before_array_allocation() {
    let mut limits = Limits {
        vanilla_quota_bytes: 12,
        allocation_bytes: 0,
        ..Limits::default()
    };
    assert_eq!(
        read_network(&mut bytes("0300000007").as_slice(), limits),
        Ok(Tag::Int(7))
    );
    limits.vanilla_quota_bytes = 11;
    assert_eq!(
        read_network(&mut bytes("0300000007").as_slice(), limits),
        Err(Error::VanillaQuotaExceeded)
    );
    limits.vanilla_quota_bytes = usize::MAX;
    assert_eq!(
        read_network(&mut bytes("070000000100").as_slice(), limits),
        Err(Error::AllocationBudgetExceeded)
    );
    limits.allocation_bytes = usize::MAX;
    assert_eq!(
        read_network(&mut bytes("0b7fffffff").as_slice(), limits),
        Err(Error::Truncated)
    );
    limits = Limits {
        vanilla_quota_bytes: 92,
        ..Limits::default()
    };
    // 36 list + 8 child slots + 2*(24 array + 0 elements).
    assert_eq!(
        read_network(
            &mut bytes("0907000000020000000000000000").as_slice(),
            limits
        ),
        Ok(Tag::List(vec![
            Tag::ByteArray(vec![]),
            Tag::ByteArray(vec![])
        ]))
    );
    limits.vanilla_quota_bytes -= 1;
    assert_eq!(
        read_network(
            &mut bytes("0907000000020000000000000000").as_slice(),
            limits
        ),
        Err(Error::VanillaQuotaExceeded)
    );
}

#[test]
fn duplicate_compound_keys_charge_all_read_values_but_one_map_entry() {
    // Compound48 + 2*(key28+2 + Int12) + distinct map entry36 =168.
    let fixture = bytes("0a0300017800000001030001780000000200");
    let mut limits = Limits {
        vanilla_quota_bytes: 168,
        ..Limits::default()
    };
    assert!(read_network(&mut fixture.as_slice(), limits).is_ok());
    limits.vanilla_quota_bytes -= 1;
    assert_eq!(
        read_network(&mut fixture.as_slice(), limits),
        Err(Error::VanillaQuotaExceeded)
    );
}

#[test]
fn container_depth_and_wire_wrappers_count_towards_the_limit() {
    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    assert!(read_network(&mut bytes("0a00").as_slice(), limits).is_ok());
    assert_eq!(
        read_network(&mut bytes("0a0a0001780000").as_slice(), limits),
        Err(Error::DepthLimit)
    );
    assert!(write_network(&Tag::List(vec![Tag::Int(1)]), &mut Vec::new(), limits).is_ok());
    let mixed = Tag::List(vec![Tag::Int(1), Tag::Byte(2)]);
    assert_eq!(
        write_network(&mixed, &mut Vec::new(), limits),
        Err(Error::DepthLimit)
    );
    assert!(
        write_network(
            &mixed,
            &mut Vec::new(),
            Limits {
                max_depth: 2,
                ..limits
            }
        )
        .is_ok()
    );
    let zero = Limits {
        max_depth: 0,
        ..limits
    };
    assert!(read_network(&mut &[0][..], zero).is_ok());
    assert!(read_network(&mut &[1, 1][..], zero).is_ok());
    assert_eq!(
        read_network(&mut &[10, 0][..], zero),
        Err(Error::DepthLimit)
    );
}

#[test]
fn vanilla_512_container_boundary_is_supported() {
    fn nesting(depth: usize) -> Vec<u8> {
        let mut value = vec![10];
        for _ in 1..depth {
            value.extend_from_slice(&[10, 0, 0]);
        }
        value.resize(value.len() + depth, 0);
        value
    }
    let input = nesting(512);
    let value = read_network(&mut input.as_slice(), Limits::default()).unwrap();
    assert_eq!(encode(&value), input);
    assert_eq!(
        read_network(&mut nesting(513).as_slice(), Limits::default()),
        Err(Error::DepthLimit)
    );
}

#[test]
fn mixed_list_wrappers_reach_512_and_fail_513_with_transactional_cleanup() {
    // Each list has mixed types, so its nested list element is wrapped in one
    // extra compound. 256 logical lists therefore require 512 wire containers.
    fn mixed(depth: usize) -> Tag {
        let mut value = Tag::Int(7);
        for _ in 0..depth {
            value = Tag::List(vec![Tag::Byte(1), value]);
        }
        value
    }
    let value = mixed(256);
    let encoded = encode(&value);
    assert_eq!(
        read_network(&mut encoded.as_slice(), Limits::default()),
        Ok(value)
    );
    let mut output = vec![0x42];
    assert_eq!(
        write_network(&mixed(257), &mut output, Limits::default()),
        Err(Error::DepthLimit)
    );
    assert_eq!(output, [0x42]);
}

#[test]
fn impossible_typed_list_lengths_fail_before_decoded_allocation() {
    let limits = Limits {
        vanilla_quota_bytes: usize::MAX,
        allocation_bytes: 0,
        ..Limits::default()
    };
    // Enough bytes to pass a one-byte lower bound, but not two Long payloads.
    assert_eq!(
        read_network(&mut bytes("0904000000020000").as_slice(), limits),
        Err(Error::Truncated)
    );
}

#[test]
fn writer_length_budget_and_string_errors_preserve_existing_bytes() {
    let mut output = vec![1, 2, 3];
    let tiny = Limits {
        output_bytes: 4,
        ..Limits::default()
    };
    assert_eq!(
        write_network(&Tag::Int(1), &mut output, tiny),
        Err(Error::OutputLimit)
    );
    assert_eq!(output, [1, 2, 3]);
    let exact = Tag::String(NbtString::from_utf16(vec![1; 65535]));
    assert_eq!(encode(&exact).len(), 65538);
    let too_long = Tag::String(NbtString::from_utf16(vec![0; 32768]));
    assert_eq!(
        write_network(&too_long, &mut output, Limits::default()),
        Err(Error::StringTooLong)
    );
    assert_eq!(output, [1, 2, 3]);
    let end = NamedTag {
        name: "name".into(),
        tag: Tag::End,
    };
    assert_eq!(
        write_named(&end, &mut output, Limits::default()),
        Err(Error::NamedEnd)
    );
    assert_eq!(output, [1, 2, 3]);
}
