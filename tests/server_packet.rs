use arrow_mc::server::packet::{PacketError, PacketReader, PacketWriter};
use arrow_mc::wire::DecodeError;

#[test]
fn scalar_fields_match_fixed_network_bytes() {
    let expected = [
        0xac, 0x02, // VarInt 300
        0x01, 0x00, 0x80, 0xff, // booleans, signed and unsigned bytes
        0x80, 0x00, 0xff, 0xff, // signed and unsigned shorts
        0x80, 0x00, 0x00, 0x01, // int
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, // long
        0x80, 0x00, 0x00, 0x00, // float negative zero
        0x7f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // double infinity
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, // UUID
    ];
    let uuid = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let mut writer = PacketWriter::new(expected.len());
    writer.varint(300).unwrap();
    writer.boolean(true).unwrap();
    writer.boolean(false).unwrap();
    writer.byte(i8::MIN).unwrap();
    writer.unsigned_byte(u8::MAX).unwrap();
    writer.short(i16::MIN).unwrap();
    writer.unsigned_short(u16::MAX).unwrap();
    writer.int(i32::MIN + 1).unwrap();
    writer.long(-2).unwrap();
    writer.float(-0.0).unwrap();
    writer.double(f64::INFINITY).unwrap();
    writer.uuid(uuid).unwrap();
    assert_eq!(writer.as_bytes(), expected);
    let bytes = writer.into_bytes();
    let mut reader = PacketReader::new(&bytes);
    assert_eq!(reader.position(), 0);
    assert_eq!(reader.remaining(), expected.len());
    assert_eq!(reader.varint().unwrap(), 300);
    assert!(reader.boolean().unwrap());
    assert!(!reader.boolean().unwrap());
    assert_eq!(reader.byte().unwrap(), i8::MIN);
    assert_eq!(reader.unsigned_byte().unwrap(), u8::MAX);
    assert_eq!(reader.short().unwrap(), i16::MIN);
    assert_eq!(reader.unsigned_short().unwrap(), u16::MAX);
    assert_eq!(reader.int().unwrap(), i32::MIN + 1);
    assert_eq!(reader.long().unwrap(), -2);
    assert_eq!(reader.float().unwrap().to_bits(), (-0.0f32).to_bits());
    assert_eq!(reader.double().unwrap(), f64::INFINITY);
    assert_eq!(reader.uuid().unwrap(), uuid);
    assert_eq!(reader.remaining(), 0);
    assert_eq!(reader.position(), expected.len());
    reader.finish().unwrap();
}

#[test]
fn noncanonical_booleans_varints_and_float_bits_are_preserved() {
    for byte in 0..=255 {
        assert_eq!(PacketReader::new(&[byte]).boolean().unwrap(), byte != 0);
    }
    for (bytes, expected) in [
        (&[0x80, 0x00][..], 0),
        (&[0xff, 0xff, 0xff, 0xff, 0x7f], -1),
        (&[0xff, 0xff, 0xff, 0xff, 0x0f], -1),
    ] {
        assert_eq!(PacketReader::new(bytes).varint().unwrap(), expected);
    }
    assert_eq!(
        PacketReader::new(&[0x80; 5]).varint(),
        Err(PacketError::VarInt(DecodeError::Incomplete))
    );
    assert_eq!(
        PacketReader::new(&[0x80; 6]).varint(),
        Err(PacketError::VarInt(DecodeError::TooLong))
    );
    for bits in [0x7fc0_4321u32, 0xff80_0000, 0x0000_0001] {
        let bytes = bits.to_be_bytes();
        assert_eq!(PacketReader::new(&bytes).float().unwrap().to_bits(), bits);
        let mut writer = PacketWriter::new(4);
        writer.float(f32::from_bits(bits)).unwrap();
        assert_eq!(writer.as_bytes(), bytes);
    }
    let bits = 0x7ff8_1234_5678_9abcu64;
    let bytes = bits.to_be_bytes();
    assert_eq!(PacketReader::new(&bytes).double().unwrap().to_bits(), bits);
}

#[test]
fn truncated_fields_and_trailing_data_do_not_hide_input() {
    for length in 0..16 {
        let bytes = vec![0; length];
        let mut reader = PacketReader::new(&bytes);
        assert!(matches!(
            reader.uuid(),
            Err(PacketError::UnexpectedEnd { needed: 16, .. })
        ));
        assert_eq!(reader.position(), 0);
        assert_eq!(reader.remaining(), length);
    }
    let mut reader = PacketReader::new(&[1, 2, 3]);
    assert_eq!(reader.unsigned_byte().unwrap(), 1);
    assert_eq!(reader.finish(), Err(PacketError::TrailingBytes(2)));
    assert!(reader.int().is_err());
    assert_eq!(reader.position(), 1);
    assert_eq!(reader.short().unwrap(), 0x0203);
    reader.finish().unwrap();
}

fn string_wire(bytes: &[u8]) -> Vec<u8> {
    let mut writer = PacketWriter::new(bytes.len() + 5);
    writer.bytes(bytes, bytes.len()).unwrap();
    writer.into_bytes()
}

#[test]
fn java_replacement_decoding_and_utf16_limits_match_observed_cases() {
    // Results independently observed through pinned Utf8String.read on Java 25.
    for (bytes, expected) in [
        (&b"hello"[..], "hello"),
        (&[0xed, 0xa0, 0x80], "\u{fffd}"),
        (&[0xed, 0xbf, 0xbf], "\u{fffd}"),
        (&[0xed, 0xa0], "\u{fffd}"),
        (&[0xed, 0xa0, b'a'], "\u{fffd}a"),
        (&[0xe0, 0x80, 0xaf], "\u{fffd}\u{fffd}\u{fffd}"),
        (&[0xc0, 0xaf], "\u{fffd}\u{fffd}"),
        (&[0xe1, 0x80], "\u{fffd}"),
        (&[0xf0, 0x90, 0x80], "\u{fffd}"),
        (&[0xe1, 0x80, b'a'], "\u{fffd}a"),
        (&[0xf0, 0x90, 0x80, b'a'], "\u{fffd}a"),
        (
            &[0xf4, 0x90, 0x80, 0x80],
            "\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
        ),
        (&[0xf0, 0x9f, 0x98, 0x80], "😀"),
    ] {
        let wire = string_wire(bytes);
        let limit = expected.encode_utf16().count();
        let mut reader = PacketReader::new(&wire);
        assert_eq!(reader.utf(limit).unwrap(), expected, "{bytes:?}");
        reader.finish().unwrap();
        let mut reader = PacketReader::new(&wire);
        assert!(reader.utf(limit - 1).is_err(), "{bytes:?}");
        assert_eq!(reader.position(), 0);
    }
    assert_eq!(PacketReader::new(&[0]).utf(0).unwrap(), "");
    assert_eq!(
        PacketReader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]).utf(10),
        Err(PacketError::NegativeLength(-1))
    );
    assert!(matches!(
        PacketReader::new(&[4]).utf(1),
        Err(PacketError::LengthLimit {
            kind: "encoded UTF-8",
            ..
        })
    ));
    assert!(matches!(
        PacketReader::new(&[3, b'a']).utf(1),
        Err(PacketError::UnexpectedEnd { .. })
    ));
    assert_eq!(
        PacketReader::new(&[0]).utf(usize::MAX),
        Err(PacketError::LengthOverflow)
    );
    let mut writer = PacketWriter::new(64);
    writer.utf("😀가", 3).unwrap();
    assert_eq!(
        writer.as_bytes(),
        [7, 0xf0, 0x9f, 0x98, 0x80, 0xea, 0xb0, 0x80]
    );
    assert!(writer.utf("😀가", 2).is_err());
}

#[test]
fn byte_arrays_borrow_input_and_enforce_declared_and_remainder_bounds() {
    let bytes = [3, 5, 6, 7, 8, 9];
    let mut reader = PacketReader::new(&bytes);
    assert!(reader.bytes(2).is_err());
    assert_eq!(reader.position(), 0);
    let field = reader.bytes(3).unwrap();
    assert_eq!(field, [5, 6, 7]);
    assert_eq!(field.as_ptr(), bytes[1..].as_ptr());
    assert!(reader.remaining_bytes(1).is_err());
    assert_eq!(reader.position(), 4);
    assert_eq!(reader.remaining_bytes(2).unwrap(), [8, 9]);
    reader.finish().unwrap();
    assert!(reader.remaining_bytes(0).unwrap().is_empty());
    assert!(PacketReader::new(&[2, 1]).bytes(2).is_err());
    assert!(PacketReader::new(&[0]).bytes(0).unwrap().is_empty());
}

#[test]
fn identifiers_follow_java_parse_including_empty_paths() {
    for (input, expected) in [
        ("", "minecraft:"),
        (":", "minecraft:"),
        ("stone", "minecraft:stone"),
        (":stone", "minecraft:stone"),
        ("custom:", "custom:"),
        (".:a", ".:a"),
        ("...:a", "...:a"),
        ("minecraft:..", "minecraft:.."),
        ("namespace:path/./../x", "namespace:path/./../x"),
    ] {
        let encoded = string_wire(input.as_bytes());
        let mut reader = PacketReader::new(&encoded);
        assert_eq!(reader.identifier().unwrap(), expected);
        reader.finish().unwrap();
        let mut writer = PacketWriter::new(100);
        writer.identifier(input).unwrap();
        assert_eq!(writer.as_bytes(), string_wire(expected.as_bytes()));
    }
    for value in [
        "..:a",
        "a:b:c",
        "Minecraft:stone",
        "a:Stone",
        "a:b\\c",
        "a b",
        "가",
    ] {
        let encoded = string_wire(value.as_bytes());
        let mut reader = PacketReader::new(&encoded);
        assert_eq!(
            reader.identifier(),
            Err(PacketError::InvalidIdentifier),
            "{value}"
        );
        assert_eq!(reader.position(), 0);
        let mut writer = PacketWriter::new(100);
        assert_eq!(
            writer.identifier(value),
            Err(PacketError::InvalidIdentifier)
        );
        assert!(writer.as_bytes().is_empty());
    }
    // The input limit applies before default-namespace expansion, as in Java.
    let maximum = "x".repeat(32767);
    assert_eq!(
        PacketReader::new(&string_wire(maximum.as_bytes()))
            .identifier()
            .unwrap()
            .len(),
        32777
    );
    assert!(PacketWriter::new(40000).identifier(&maximum).is_err());
}

#[test]
fn writer_limits_are_cumulative_and_failed_fields_leave_output_unchanged() {
    let mut writer = PacketWriter::new(4);
    writer.raw(&[42]).unwrap();
    assert!(writer.int(1).is_err());
    assert!(writer.bytes(&[1, 2, 3], 3).is_err());
    assert!(writer.utf("abc", 3).is_err());
    assert!(writer.identifier("").is_err());
    assert_eq!(writer.as_bytes(), [42]);
    writer.bytes(&[1, 2], 2).unwrap();
    assert_eq!(writer.as_bytes(), [42, 2, 1, 2]);
    assert!(writer.boolean(false).is_err());
    writer.raw(&[]).unwrap();
    let mut writer = PacketWriter::new(0);
    writer.raw(&[]).unwrap();
    assert!(writer.bytes(&[], 0).is_err());
    assert!(writer.varint(0).is_err());
    assert!(writer.as_bytes().is_empty());
}

#[test]
fn borrowed_utf_comparison_validates_skipped_and_mismatching_fields() {
    // The encoded surrogate is a single Java replacement between valid chunks.
    let mut wire = string_wire(&[b'a', 0xed, 0xa0, 0x80, 0xf0, 0x9f, 0x98, 0x80]);
    wire.push(42);
    for (expected, matches) in [
        (Some("a\u{fffd}😀"), true),
        (Some("a\u{fffd}😀!"), false),
        (Some("a\u{fffd}"), false),
        (Some("z\u{fffd}😀"), false),
        (Some("a\u{fffd}\u{fffd}\u{fffd}😀"), false),
        (Some(""), false),
        (None, false),
    ] {
        let mut reader = PacketReader::new(&wire);
        assert_eq!(reader.utf_equals(expected, 4).unwrap(), matches);
        assert_eq!(reader.unsigned_byte().unwrap(), 42);
        reader.finish().unwrap();
    }
    assert!(PacketReader::new(&[0]).utf_equals(Some(""), 0).unwrap());
    assert!(!PacketReader::new(&[0]).utf_equals(None, 0).unwrap());
    for expected in [Some("a\u{fffd}😀"), Some("different"), None] {
        let mut reader = PacketReader::new(&wire);
        assert!(matches!(
            reader.utf_equals(expected, 3),
            Err(PacketError::LengthLimit { kind: "UTF-16", .. })
        ));
        assert_eq!(reader.position(), 0);
    }
    for wire in [
        vec![4],                            // Encoded byte limit, checked before truncation.
        vec![3, b'a'],                      // Truncated payload.
        vec![0xff, 0xff, 0xff, 0xff, 0x0f], // Negative length.
        vec![0x80; 6],                      // Oversized VarInt.
    ] {
        for expected in [Some("different"), None] {
            let mut reader = PacketReader::new(&wire);
            assert!(reader.utf_equals(expected, 1).is_err());
            assert_eq!(reader.position(), 0);
        }
    }
}

#[test]
fn utf_comparison_byte_bound_preserves_replacement_and_longer_field_limits() {
    for (bytes, limit, expected, matches) in [
        (&b""[..], 0, Some(""), true),
        (&b"abc"[..], 3, Some("abc"), true),
        (&b"abc"[..], 3, Some("ab"), false),
        (&[0x80, 0x80][..], 2, None, false),
        (&[0x80, 0x80][..], 2, Some("aa"), false),
        (&[0x80, 0x80][..], 2, Some("\u{fffd}\u{fffd}"), true),
        (&[0xed, 0xa0, 0x80][..], 3, Some("abc"), false),
        (&[0xed, 0xa0, 0x80][..], 3, Some("\u{fffd}"), true),
        (&[0xc2, 0xa2][..], 1, None, false),
        (&[0xc2, 0xa2][..], 1, Some("¢"), true),
    ] {
        let mut wire = string_wire(bytes);
        wire.push(42);
        let mut reader = PacketReader::new(&wire);
        assert_eq!(reader.utf_equals(expected, limit).unwrap(), matches);
        assert_eq!(reader.unsigned_byte().unwrap(), 42);
        reader.finish().unwrap();
    }
    // These complete payloads fit the encoded-byte limit, but not UTF-16 units.
    for bytes in [&b"ab"[..], &[0x80, 0x80][..]] {
        let wire = string_wire(bytes);
        for expected in [None, Some("a"), Some("\u{fffd}")] {
            let mut reader = PacketReader::new(&wire);
            assert!(matches!(
                reader.utf_equals(expected, 1),
                Err(PacketError::LengthLimit { kind: "UTF-16", .. })
            ));
            assert_eq!(reader.position(), 0);
        }
    }
}
