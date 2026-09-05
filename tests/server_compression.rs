use arrow_mc::server::compression::{
    CompressionError, CompressionLimits, CompressionScratch, CompressionState,
    MAX_FRAME_BODY_BYTES, MAX_UNCOMPRESSED_BYTES,
};

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut length = body.len();
    let mut output = Vec::new();
    loop {
        let byte = (length & 127) as u8;
        length >>= 7;
        output.push(byte | if length == 0 { 0 } else { 128 });
        if length == 0 {
            break;
        }
    }
    output.extend_from_slice(body);
    output
}

fn decode(threshold: i32, body: &[u8]) -> Result<Vec<u8>, CompressionError> {
    let mut scratch = CompressionScratch::default();
    let encoded = frame(body);
    let mut input = encoded.as_slice();
    let mut output = Vec::new();
    let mut allocation = 32 * 1024 * 1024;
    CompressionState::new(threshold).decode_frame(
        &mut input,
        &mut scratch,
        &mut output,
        CompressionLimits::default(),
        &mut allocation,
    )?;
    assert!(input.is_empty());
    Ok(output)
}

#[test]
fn raw_threshold_and_declared_prefix_follow_actual_java() {
    assert_eq!(decode(1, &[0, 1, 2, 3]).unwrap(), [1, 2, 3]);
    assert!(decode(0, &[0]).unwrap().is_empty());
    assert_eq!(decode(-1, &[0, 1, 2]).unwrap(), [0, 1, 2]);
    let stream = "789c4b4c4a4e494d0300081e0256"; // actual Java zlib("abcdef")
    for length in [1, 5, 6] {
        let mut body = vec![length];
        body.extend(bytes(stream));
        assert_eq!(decode(0, &body).unwrap(), &b"abcdef"[..length as usize]);
    }
    assert!(matches!(
        decode(7, &bytes(&format!("06{stream}"))),
        Err(CompressionError::BelowThreshold)
    ));
    assert!(matches!(
        decode(0, &bytes(&format!("07{stream}"))),
        Err(CompressionError::LengthMismatch)
    ));
    assert!(matches!(
        decode(0, &bytes("ffffffff0f")),
        Err(CompressionError::NegativeDataLength)
    ));
}

#[test]
fn java_truncation_trailing_and_checksum_semantics_are_preserved() {
    assert_eq!(decode(0, &bytes("02789c737402")).unwrap(), b"AB");
    assert_eq!(
        decode(0, &bytes("02789c7374020000c600840102ff")).unwrap(),
        b"AB"
    );
    assert_eq!(decode(0, &bytes("01789c7374020000c60085")).unwrap(), b"A");
    assert!(matches!(
        decode(0, &bytes("02789c7374020000c60085")),
        Err(CompressionError::InvalidZlib)
    ));
    assert!(decode(0, &bytes("02789c")).is_err());
}

#[test]
fn encoder_resets_between_packets_and_never_falls_back_for_expansion() {
    let mut scratch = CompressionScratch::default();
    for threshold in [-1, 0, 1, 128, 256] {
        let state = CompressionState::new(threshold);
        for length in [1, 127, 128, 255, 256, 257, 4096, 65536] {
            let packet: Vec<_> = (0..length).map(|index| (index * 37) as u8).collect();
            let mut encoded = Vec::new();
            let mut allocation = 32 * 1024 * 1024;
            state
                .encode_frame(
                    &packet,
                    &mut scratch,
                    &mut encoded,
                    CompressionLimits::default(),
                    &mut allocation,
                )
                .unwrap();
            let header = encoded.iter().position(|byte| byte & 128 == 0).unwrap() + 1;
            if threshold >= 0 && length >= threshold as usize {
                assert_ne!(encoded[header], 0);
            }
            let mut input = encoded.as_slice();
            let mut decoded = Vec::new();
            state
                .decode_frame(
                    &mut input,
                    &mut scratch,
                    &mut decoded,
                    CompressionLimits::default(),
                    &mut allocation,
                )
                .unwrap();
            assert!(input.is_empty());
            assert_eq!(decoded, packet);
        }
    }
}

#[test]
fn frame_limits_allocation_and_errors_preserve_existing_buffers() {
    let state = CompressionState::new(0);
    let mut scratch = CompressionScratch::default();
    let mut output = vec![7, 8];
    let mut allocation = 0;
    let encoded = frame(&bytes("80808004789c"));
    let mut input = encoded.as_slice();
    assert!(matches!(
        state.decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::AllocationLimit)
    ));
    assert_eq!(input, encoded);
    assert_eq!(output, [7, 8]);
    let encoded = frame(&bytes("81808004789c"));
    let mut input = encoded.as_slice();
    assert!(matches!(
        state.decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::DecompressedTooLarge)
    ));
    assert_eq!(allocation, 0);
    for encoded in [
        vec![],
        vec![0],
        vec![0x80, 0x80, 0x80],
        vec![2, 0],
        vec![0xff, 0xff, 0x7f],
    ] {
        let mut input = encoded.as_slice();
        assert!(
            state
                .decode_frame(
                    &mut input,
                    &mut scratch,
                    &mut output,
                    CompressionLimits::default(),
                    &mut allocation
                )
                .is_err()
        );
        assert_eq!(input, encoded);
        assert_eq!(output, [7, 8]);
    }
    assert!(matches!(
        state.encode_frame(
            &vec![1; MAX_UNCOMPRESSED_BYTES + 1],
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::DecompressedTooLarge)
    ));
    let raw = CompressionState::new(-1);
    assert!(matches!(
        raw.encode_frame(
            &vec![1; MAX_FRAME_BODY_BYTES + 1],
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::FrameTooLarge)
    ));
    assert_eq!(output, [7, 8]);
}

#[test]
fn decoder_consumes_one_frame_and_preserves_coalesced_followup() {
    let state = CompressionState::new(256);
    let data = [frame(&[0, 1]), frame(&[0, 2])].concat();
    let mut input = data.as_slice();
    let mut scratch = CompressionScratch::default();
    let mut output = vec![];
    let mut allocation = 100;
    state
        .decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert_eq!(output, [1]);
    assert_eq!(input, &[2, 0, 2]);
    state
        .decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert_eq!(output, [1, 2]);
    assert!(input.is_empty());
}

#[test]
fn empty_compression_envelopes_and_failed_scratch_reuse_are_explicit() {
    let mut scratch = CompressionScratch::default();
    let mut allocation = 1_000_000;
    let mut output = Vec::new();
    CompressionState::new(0)
        .encode_frame(
            &[],
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    // Exact Java encoder corner: zero DataLength makes the zlib-empty bytes a
    // raw payload to the decoder. Empty input is not a real packet with an ID.
    assert_eq!(output, bytes("0900789c030000000001"));
    output.clear();
    CompressionState::new(1)
        .encode_frame(
            &[],
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert_eq!(output, [1, 0]);
    let state = CompressionState::new(0);
    output.clear();
    output.push(9);
    let limits = CompressionLimits {
        max_frame_body_bytes: 8,
        ..CompressionLimits::default()
    };
    assert!(matches!(
        state.encode_frame(
            b"0123456789abcdef",
            &mut scratch,
            &mut output,
            limits,
            &mut allocation
        ),
        Err(CompressionError::FrameTooLarge)
    ));
    assert_eq!(output, [9]);
    let damaged = frame(&bytes("02789c7374020000c60085"));
    let mut input = damaged.as_slice();
    assert!(matches!(
        state.decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::InvalidZlib)
    ));
    assert_eq!(output, [9]);
    assert_eq!(input, damaged);
    let valid = frame(&bytes("02789c7374020000c60084"));
    let mut input = valid.as_slice();
    state
        .decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert_eq!(output, [9, b'A', b'B']);
    let mut encoded = Vec::new();
    state
        .encode_frame(
            b"AB",
            &mut scratch,
            &mut encoded,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    let mut input = encoded.as_slice();
    output.clear();
    state
        .decode_frame(
            &mut input,
            &mut scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert_eq!(output, b"AB");
}

#[test]
fn every_frozen_java_boundary_case_matches() {
    let content = JAVA_BOUNDARIES;
    let mut mismatches = Vec::new();
    let mut count = 0;
    for line in content.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let actual = decode(fields[1].parse().unwrap(), &bytes(fields[2]));
        let matches = if fields[3] == "OK" {
            actual
                .as_ref()
                .is_ok_and(|result| *result == bytes(fields[4]))
        } else {
            actual.is_err()
        };
        if !matches {
            mismatches.push(format!(
                "{} Java {} {} Rust {:?}",
                fields[0], fields[3], fields[4], actual
            ));
        }
        count += 1;
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    eprintln!("Matched {count} independent actual-Java compression boundary cases");
}

// Small independent JVM observations from CompressionBoundaryReview.java.
const JAVA_BOUNDARIES: &str = r#"decl-0--1	0	ffffffff0f789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of -1 is below server threshold of 0
decl-0-0	0	00789c4b4c4a4e494d0300081e0256	OK	789c4b4c4a4e494d0300081e0256
decl-0-1	0	01789c4b4c4a4e494d0300081e0256	OK	61
decl-0-5	0	05789c4b4c4a4e494d0300081e0256	OK	6162636465
decl-0-6	0	06789c4b4c4a4e494d0300081e0256	OK	616263646566
decl-0-7	0	07789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
decl-0-8388609	0	81808004789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 8388609 is larger than protocol maximum of 8388608
decl-1--1	1	ffffffff0f789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of -1 is below server threshold of 1
decl-1-0	1	00789c4b4c4a4e494d0300081e0256	OK	789c4b4c4a4e494d0300081e0256
decl-1-1	1	01789c4b4c4a4e494d0300081e0256	OK	61
decl-1-5	1	05789c4b4c4a4e494d0300081e0256	OK	6162636465
decl-1-6	1	06789c4b4c4a4e494d0300081e0256	OK	616263646566
decl-1-7	1	07789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
decl-1-8388609	1	81808004789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 8388609 is larger than protocol maximum of 8388608
decl-6--1	6	ffffffff0f789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of -1 is below server threshold of 6
decl-6-0	6	00789c4b4c4a4e494d0300081e0256	OK	789c4b4c4a4e494d0300081e0256
decl-6-1	6	01789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 1 is below server threshold of 6
decl-6-5	6	05789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 5 is below server threshold of 6
decl-6-6	6	06789c4b4c4a4e494d0300081e0256	OK	616263646566
decl-6-7	6	07789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
decl-6-8388609	6	81808004789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 8388609 is larger than protocol maximum of 8388608
decl-7--1	7	ffffffff0f789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of -1 is below server threshold of 7
decl-7-0	7	00789c4b4c4a4e494d0300081e0256	OK	789c4b4c4a4e494d0300081e0256
decl-7-1	7	01789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 1 is below server threshold of 7
decl-7-5	7	05789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 5 is below server threshold of 7
decl-7-6	7	06789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 6 is below server threshold of 7
decl-7-7	7	07789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
decl-7-8388609	7	81808004789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - size of 8388609 is larger than protocol maximum of 8388608
truncate-0-1	0	01	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 1
truncate-0-5	0	05	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 5
truncate-0-6	0	06	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 6
truncate-0-7	0	07	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 7
truncate-1-1	0	0178	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 1
truncate-1-5	0	0578	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 5
truncate-1-6	0	0678	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 6
truncate-1-7	0	0778	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 7
truncate-2-1	0	01789c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 1
truncate-2-5	0	05789c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 5
truncate-2-6	0	06789c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 6
truncate-2-7	0	07789c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 7
truncate-3-1	0	01789c4b	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 1
truncate-3-5	0	05789c4b	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 5
truncate-3-6	0	06789c4b	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 6
truncate-3-7	0	07789c4b	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 7
truncate-4-1	0	01789c4b4c	OK	61
truncate-4-5	0	05789c4b4c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 1 is does not match declared size 5
truncate-4-6	0	06789c4b4c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 1 is does not match declared size 6
truncate-4-7	0	07789c4b4c	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 1 is does not match declared size 7
truncate-5-1	0	01789c4b4c4a	OK	61
truncate-5-5	0	05789c4b4c4a	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 2 is does not match declared size 5
truncate-5-6	0	06789c4b4c4a	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 2 is does not match declared size 6
truncate-5-7	0	07789c4b4c4a	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 2 is does not match declared size 7
truncate-6-1	0	01789c4b4c4a4e	OK	61
truncate-6-5	0	05789c4b4c4a4e	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 3 is does not match declared size 5
truncate-6-6	0	06789c4b4c4a4e	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 3 is does not match declared size 6
truncate-6-7	0	07789c4b4c4a4e	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 3 is does not match declared size 7
truncate-7-1	0	01789c4b4c4a4e49	OK	61
truncate-7-5	0	05789c4b4c4a4e49	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 4 is does not match declared size 5
truncate-7-6	0	06789c4b4c4a4e49	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 4 is does not match declared size 6
truncate-7-7	0	07789c4b4c4a4e49	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 4 is does not match declared size 7
truncate-8-1	0	01789c4b4c4a4e494d	OK	61
truncate-8-5	0	05789c4b4c4a4e494d	OK	6162636465
truncate-8-6	0	06789c4b4c4a4e494d	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 5 is does not match declared size 6
truncate-8-7	0	07789c4b4c4a4e494d	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 5 is does not match declared size 7
truncate-9-1	0	01789c4b4c4a4e494d03	OK	61
truncate-9-5	0	05789c4b4c4a4e494d03	OK	6162636465
truncate-9-6	0	06789c4b4c4a4e494d03	OK	616263646566
truncate-9-7	0	07789c4b4c4a4e494d03	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
truncate-10-1	0	01789c4b4c4a4e494d0300	OK	61
truncate-10-5	0	05789c4b4c4a4e494d0300	OK	6162636465
truncate-10-6	0	06789c4b4c4a4e494d0300	OK	616263646566
truncate-10-7	0	07789c4b4c4a4e494d0300	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
truncate-11-1	0	01789c4b4c4a4e494d030008	OK	61
truncate-11-5	0	05789c4b4c4a4e494d030008	OK	6162636465
truncate-11-6	0	06789c4b4c4a4e494d030008	OK	616263646566
truncate-11-7	0	07789c4b4c4a4e494d030008	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
truncate-12-1	0	01789c4b4c4a4e494d0300081e	OK	61
truncate-12-5	0	05789c4b4c4a4e494d0300081e	OK	6162636465
truncate-12-6	0	06789c4b4c4a4e494d0300081e	OK	616263646566
truncate-12-7	0	07789c4b4c4a4e494d0300081e	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
truncate-13-1	0	01789c4b4c4a4e494d0300081e02	OK	61
truncate-13-5	0	05789c4b4c4a4e494d0300081e02	OK	6162636465
truncate-13-6	0	06789c4b4c4a4e494d0300081e02	OK	616263646566
truncate-13-7	0	07789c4b4c4a4e494d0300081e02	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
truncate-14-1	0	01789c4b4c4a4e494d0300081e0256	OK	61
truncate-14-5	0	05789c4b4c4a4e494d0300081e0256	OK	6162636465
truncate-14-6	0	06789c4b4c4a4e494d0300081e0256	OK	616263646566
truncate-14-7	0	07789c4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
damage-0-1	0	01799c4b4c4a4e494d0300081e0256	ERROR	DataFormatException:incorrect header check
damage-0-6	0	06799c4b4c4a4e494d0300081e0256	ERROR	DataFormatException:incorrect header check
damage-1-1	0	01789d4b4c4a4e494d0300081e0256	ERROR	DataFormatException:incorrect header check
damage-1-6	0	06789d4b4c4a4e494d0300081e0256	ERROR	DataFormatException:incorrect header check
damage-2-1	0	01789c4a4c4a4e494d0300081e0256	OK	61
damage-2-6	0	06789c4a4c4a4e494d0300081e0256	ERROR	DataFormatException:invalid stored block lengths
damage-3-1	0	01789c4b4d4a4e494d0300081e0256	OK	65
damage-3-6	0	06789c4b4d4a4e494d0300081e0256	ERROR	DataFormatException:incorrect data check
damage-4-1	0	01789c4b4c4b4e494d0300081e0256	OK	61
damage-4-6	0	06789c4b4c4b4e494d0300081e0256	ERROR	DataFormatException:incorrect data check
damage-5-1	0	01789c4b4c4a4f494d0300081e0256	OK	61
damage-5-6	0	06789c4b4c4a4f494d0300081e0256	ERROR	DataFormatException:incorrect data check
damage-6-1	0	01789c4b4c4a4e484d0300081e0256	OK	61
damage-6-6	0	06789c4b4c4a4e484d0300081e0256	ERROR	DataFormatException:incorrect data check
damage-7-1	0	01789c4b4c4a4e494c0300081e0256	OK	61
damage-7-6	0	06789c4b4c4a4e494c0300081e0256	ERROR	DataFormatException:incorrect data check
damage-8-1	0	01789c4b4c4a4e494d0200081e0256	OK	61
damage-8-6	0	06789c4b4c4a4e494d0200081e0256	ERROR	DataFormatException:incorrect data check
damage-9-1	0	01789c4b4c4a4e494d0301081e0256	OK	61
damage-9-6	0	06789c4b4c4a4e494d0301081e0256	OK	616263646566
damage-10-1	0	01789c4b4c4a4e494d0300091e0256	OK	61
damage-10-6	0	06789c4b4c4a4e494d0300091e0256	ERROR	DataFormatException:incorrect data check
damage-11-1	0	01789c4b4c4a4e494d0300081f0256	OK	61
damage-11-6	0	06789c4b4c4a4e494d0300081f0256	ERROR	DataFormatException:incorrect data check
damage-12-1	0	01789c4b4c4a4e494d0300081e0356	OK	61
damage-12-6	0	06789c4b4c4a4e494d0300081e0356	ERROR	DataFormatException:incorrect data check
damage-13-1	0	01789c4b4c4a4e494d0300081e0257	OK	61
damage-13-6	0	06789c4b4c4a4e494d0300081e0257	ERROR	DataFormatException:incorrect data check
tail-00-1	0	01789c4b4c4a4e494d0300081e025600	OK	61
tail-00-6	0	06789c4b4c4a4e494d0300081e025600	OK	616263646566
tail-00-7	0	07789c4b4c4a4e494d0300081e025600	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
tail-00-12	0	0c789c4b4c4a4e494d0300081e025600	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 12
tail-ffff-1	0	01789c4b4c4a4e494d0300081e0256ffff	OK	61
tail-ffff-6	0	06789c4b4c4a4e494d0300081e0256ffff	OK	616263646566
tail-ffff-7	0	07789c4b4c4a4e494d0300081e0256ffff	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
tail-ffff-12	0	0c789c4b4c4a4e494d0300081e0256ffff	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 12
tail-789c2b4e4dcecf4b010008ca027d-1	0	01789c4b4c4a4e494d0300081e0256789c2b4e4dcecf4b010008ca027d	OK	61
tail-789c2b4e4dcecf4b010008ca027d-6	0	06789c4b4c4a4e494d0300081e0256789c2b4e4dcecf4b010008ca027d	OK	616263646566
tail-789c2b4e4dcecf4b010008ca027d-7	0	07789c4b4c4a4e494d0300081e0256789c2b4e4dcecf4b010008ca027d	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 7
tail-789c2b4e4dcecf4b010008ca027d-12	0	0c789c4b4c4a4e494d0300081e0256789c2b4e4dcecf4b010008ca027d	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 6 is does not match declared size 12
dictionary	0	0678bb3c9506db4b4c4a4e494d0300081e0256	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 6
raw-empty	100	00	OK	
raw-above-threshold	1	00616263646566	OK	616263646566
empty-zlib	0	01789c030000000001	ERROR	DecoderException:Badly compressed packet - actual length of uncompressed payload 0 is does not match declared size 1
"#;
