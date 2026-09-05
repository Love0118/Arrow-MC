use arrow_mc::wire::{
    BufferTooSmall, DecodeError, read_varint, read_varlong, varint_len, varlong_len, write_varint,
    write_varlong,
};

#[test]
fn varint_canonical_fixtures() {
    let cases: &[(i32, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (255, &[0xff, 0x01]),
        (300, &[0xac, 0x02]),
        (16_383, &[0xff, 0x7f]),
        (16_384, &[0x80, 0x80, 0x01]),
        (2_097_151, &[0xff, 0xff, 0x7f]),
        (2_097_152, &[0x80, 0x80, 0x80, 0x01]),
        (268_435_455, &[0xff, 0xff, 0xff, 0x7f]),
        (268_435_456, &[0x80, 0x80, 0x80, 0x80, 0x01]),
        (i32::MAX, &[0xff, 0xff, 0xff, 0xff, 0x07]),
        (i32::MIN, &[0x80, 0x80, 0x80, 0x80, 0x08]),
        (-2, &[0xfe, 0xff, 0xff, 0xff, 0x0f]),
        (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
    ];
    for &(value, encoded) in cases {
        assert_eq!(read_varint(encoded), Ok((value, encoded.len())));
        assert_eq!(varint_len(value), encoded.len());
        let mut output = [0xcc; 7];
        let length = write_varint(value, &mut output).unwrap();
        assert_eq!(&output[..length], encoded);
        assert!(output[length..].iter().all(|&byte| byte == 0xcc));
    }
}

#[test]
fn varlong_canonical_fixtures() {
    let cases: &[(i64, &[u8])] = &[
        (0, &[0x00]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (16_383, &[0xff, 0x7f]),
        (16_384, &[0x80, 0x80, 0x01]),
        (2_097_151, &[0xff, 0xff, 0x7f]),
        (2_097_152, &[0x80, 0x80, 0x80, 0x01]),
        (268_435_455, &[0xff, 0xff, 0xff, 0x7f]),
        (268_435_456, &[0x80, 0x80, 0x80, 0x80, 0x01]),
        (34_359_738_367, &[0xff, 0xff, 0xff, 0xff, 0x7f]),
        (34_359_738_368, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]),
        (4_398_046_511_103, &[0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]),
        (
            4_398_046_511_104,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        ),
        (
            562_949_953_421_311,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ),
        (
            562_949_953_421_312,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        ),
        (
            72_057_594_037_927_935,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ),
        (
            72_057_594_037_927_936,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        ),
        (
            i64::MAX,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ),
        (
            i64::MIN,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        ),
        (
            -1,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
        ),
    ];
    for &(value, encoded) in cases {
        assert_eq!(read_varlong(encoded), Ok((value, encoded.len())));
        assert_eq!(varlong_len(value), encoded.len());
        let mut output = [0xcc; 12];
        let length = write_varlong(value, &mut output).unwrap();
        assert_eq!(&output[..length], encoded);
        assert!(output[length..].iter().all(|&byte| byte == 0xcc));
    }
}

#[test]
fn noncanonical_encodings_match_java_truncation() {
    assert_eq!(read_varint(&[0x80, 0x00]), Ok((0, 2)));
    assert_eq!(read_varlong(&[0x81, 0x00]), Ok((1, 2)));
    // Java's fixed-width shift discards payload bits above the integer width.
    for terminal in 0..=127 {
        assert_eq!(
            read_varint(&[0x80, 0x80, 0x80, 0x80, terminal]),
            Ok(((((terminal & 15) as u32) << 28) as i32, 5)),
        );
        assert_eq!(
            read_varlong(&[
                0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, terminal
            ]),
            Ok((if terminal & 1 == 0 { 0 } else { i64::MIN }, 10)),
        );
    }
}

#[test]
fn incomplete_and_too_long_are_distinct_at_java_boundary() {
    for length in 0..=5 {
        assert_eq!(
            read_varint(&[0x80; 5][..length]),
            Err(DecodeError::Incomplete)
        );
    }
    for length in 0..=10 {
        assert_eq!(
            read_varlong(&[0x80; 10][..length]),
            Err(DecodeError::Incomplete)
        );
    }
    assert_eq!(read_varint(&[0x80; 6]), Err(DecodeError::TooLong));
    assert_eq!(
        read_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0]),
        Err(DecodeError::TooLong)
    );
    assert_eq!(read_varlong(&[0x80; 11]), Err(DecodeError::TooLong));
    assert_eq!(
        read_varlong(&[
            0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0
        ]),
        Err(DecodeError::TooLong)
    );
}

#[test]
fn reader_stops_at_first_terminator() {
    assert_eq!(
        read_varint(&[0xac, 0x02, 0xff, 0xff, 0xff, 0xff]),
        Ok((300, 2))
    );
    assert_eq!(
        read_varlong(&[0xac, 0x02, 0xff, 0xff, 0xff, 0xff]),
        Ok((300, 2))
    );
}

#[test]
fn short_output_is_unchanged() {
    for available in 0..5 {
        let mut output = [0xcc; 5];
        assert_eq!(
            write_varint(-1, &mut output[..available]),
            Err(BufferTooSmall {
                required: 5,
                available
            }),
        );
        assert_eq!(output, [0xcc; 5]);
    }
    for available in 0..10 {
        let mut output = [0xcc; 10];
        assert_eq!(
            write_varlong(-1, &mut output[..available]),
            Err(BufferTooSmall {
                required: 10,
                available
            }),
        );
        assert_eq!(output, [0xcc; 10]);
    }
    assert_eq!(
        write_varint(0, &mut []),
        Err(BufferTooSmall {
            required: 1,
            available: 0
        })
    );
    assert_eq!(
        write_varlong(0, &mut []),
        Err(BufferTooSmall {
            required: 1,
            available: 0
        })
    );
}
