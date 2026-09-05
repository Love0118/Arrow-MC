use arrow_mc::world::storage::compression::{CompressionError, CompressionKind, StorageDecoder};
use flate2::{
    Compression,
    write::{GzEncoder, ZlibEncoder},
};
use std::io::Write;

fn encoded(kind: CompressionKind, plain: &[u8]) -> Vec<u8> {
    match kind {
        CompressionKind::Raw => plain.to_vec(),
        CompressionKind::Gzip => {
            let mut w = GzEncoder::new(Vec::new(), Compression::default());
            w.write_all(plain).unwrap();
            w.finish().unwrap()
        }
        CompressionKind::Zlib => {
            let mut w = ZlibEncoder::new(Vec::new(), Compression::default());
            w.write_all(plain).unwrap();
            w.finish().unwrap()
        }
        CompressionKind::Lz4 => {
            let mut data = raw_lz4(plain);
            data.extend(end_lz4());
            data
        }
    }
}
fn raw_lz4(plain: &[u8]) -> Vec<u8> {
    let mut result = b"LZ4Block".to_vec();
    result.push(0x1f);
    result.extend((plain.len() as u32).to_le_bytes());
    result.extend((plain.len() as u32).to_le_bytes());
    result.extend((xxhash_rust::xxh32::xxh32(plain, 0x9747_b28c) & 0x0fff_ffff).to_le_bytes());
    result.extend(plain);
    result
}
fn end_lz4() -> Vec<u8> {
    let mut result = b"LZ4Block".to_vec();
    result.push(0x10);
    result.extend([0; 12]);
    result
}
fn decode(kind: CompressionKind, input: &[u8], limit: usize) -> Result<Vec<u8>, CompressionError> {
    let mut out = Vec::with_capacity(limit);
    StorageDecoder::new().decompress(kind, input, &mut out, limit)?;
    Ok(out)
}

#[test]
fn kinds_limits_empty_streams_and_caller_reservation() {
    for (id, kind) in [
        (1, CompressionKind::Gzip),
        (2, CompressionKind::Zlib),
        (3, CompressionKind::Raw),
        (4, CompressionKind::Lz4),
    ] {
        assert_eq!(CompressionKind::try_from(id), Ok(kind));
        let empty = if kind == CompressionKind::Lz4 {
            end_lz4()
        } else {
            encoded(kind, b"")
        };
        assert_eq!(decode(kind, &empty, 0).unwrap(), b"");
        for size in [1, 8192, 65537] {
            let input: Vec<_> = (0..size).map(|i| (i * 31) as u8).collect();
            let compressed = encoded(kind, &input);
            assert_eq!(decode(kind, &compressed, size).unwrap(), input);
            assert_eq!(
                decode(kind, &compressed, size - 1),
                Err(CompressionError::OutputLimit)
            );
        }
    }
    for id in [0, 5, 127, 129, 255] {
        assert_eq!(
            CompressionKind::try_from(id),
            Err(CompressionError::Unsupported(id))
        );
    }
    let mut empty = Vec::new();
    assert_eq!(
        StorageDecoder::new().decompress(CompressionKind::Raw, b"x", &mut empty, 1),
        Err(CompressionError::OutputNotReserved)
    );
}

#[test]
fn append_rollback_and_reused_backend_never_grow_capacity() {
    let mut decoder = StorageDecoder::new();
    let mut output = Vec::with_capacity(200_004);
    output.extend(b"keep");
    let cap = output.capacity();
    for kind in [
        CompressionKind::Gzip,
        CompressionKind::Zlib,
        CompressionKind::Lz4,
        CompressionKind::Raw,
    ] {
        let data = encoded(kind, &vec![11; 100_000]);
        decoder
            .decompress(kind, &data, &mut output, 100_000)
            .unwrap();
        assert_eq!(output.len(), 100_004);
        output.truncate(4);
        let result = decoder.decompress(kind, &data, &mut output, 99_999);
        assert_eq!(result, Err(CompressionError::OutputLimit));
        assert_eq!(output, b"keep");
        assert_eq!(cap, output.capacity());
    }
}

#[test]
fn gzip_header_crc_data_crc_length_and_concatenation() {
    let mut data = encoded(CompressionKind::Gzip, b"abc");
    let len = data.len();
    data[len - 8] ^= 1;
    assert_eq!(
        decode(CompressionKind::Gzip, &data, 32),
        Err(CompressionError::Checksum)
    );
    let mut data = encoded(CompressionKind::Gzip, b"abc");
    let len = data.len();
    data[len - 4] ^= 1;
    assert_eq!(
        decode(CompressionKind::Gzip, &data, 32),
        Err(CompressionError::Checksum)
    );
    let original = encoded(CompressionKind::Gzip, b"abc");
    for n in 0..8 {
        assert!(
            decode(
                CompressionKind::Gzip,
                &original[..original.len() - n - 1],
                32
            )
            .is_err()
        );
    }
    let mut joined = original.clone();
    joined.extend(encoded(CompressionKind::Gzip, b"def"));
    assert_eq!(
        decode(CompressionKind::Gzip, &joined, 6).unwrap(),
        b"abcdef"
    );
    joined.extend(b"not a gzip member");
    assert_eq!(
        decode(CompressionKind::Gzip, &joined, 6).unwrap(),
        b"abcdef"
    );
    let mut header = original[..10].to_vec();
    header[3] |= 2 | 4 | 8 | 16;
    header.extend([2, 0, 1, 2]);
    header.extend(b"name\0comment\0");
    let mut crc = flate2::Crc::new();
    crc.update(&header);
    header.extend((crc.sum() as u16).to_le_bytes());
    header.extend(&original[10..]);
    assert_eq!(decode(CompressionKind::Gzip, &header, 3).unwrap(), b"abc");
    header[10] ^= 1;
    assert!(decode(CompressionKind::Gzip, &header, 3).is_err());
}

#[test]
fn zlib_requires_trailer_but_ignores_following_bytes() {
    let data = encoded(CompressionKind::Zlib, b"abc");
    for n in 1..=4 {
        assert!(decode(CompressionKind::Zlib, &data[..data.len() - n], 3).is_err());
    }
    let mut corrupted = data.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(decode(CompressionKind::Zlib, &corrupted, 3).is_err());
    let mut suffix = data;
    suffix.extend(b"ignored");
    assert_eq!(decode(CompressionKind::Zlib, &suffix, 3).unwrap(), b"abc");
}

#[test]
fn java_lz4_fixture_and_masked_checksum() {
    // Independently generated small compound via the pinned official writer.
    let hex = "4c5a34426c6f636b262100000026000000dfba8406c10a00000300047a506f7300000b0013780b00d008000570726f626500026f6b004c5a34426c6f636b16000000000000000000000000";
    let input: Vec<_> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    let decoded = decode(CompressionKind::Lz4, &input, 38).unwrap();
    assert_eq!(decoded.len(), 38);
    assert_eq!(decoded[0], 10);
    let mut bad = input.clone();
    bad[17] ^= 1;
    assert_eq!(
        decode(CompressionKind::Lz4, &bad, 38),
        Err(CompressionError::Checksum)
    );
    let mut bad = input.clone();
    bad[20] |= 0x80;
    assert_eq!(
        decode(CompressionKind::Lz4, &bad, 38),
        Err(CompressionError::Checksum)
    );
    let mut missing = input[..input.len() - 21].to_vec();
    assert_eq!(
        decode(CompressionKind::Lz4, &missing, 38),
        Err(CompressionError::Truncated)
    );
    missing.extend(end_lz4());
    missing.extend(b"ignored");
    assert_eq!(decode(CompressionKind::Lz4, &missing, 38).unwrap(), decoded);
}

#[test]
fn lz4_header_lengths_unknown_tokens_and_all_truncations() {
    let original = encoded(CompressionKind::Lz4, b"abc");
    for i in 0..original.len() {
        assert!(
            decode(CompressionKind::Lz4, &original[..i], 3).is_err(),
            "prefix {i}"
        );
    }
    for (index, value) in [(0, 0), (8, 0x30), (9, 0), (13, 0), (16, 0xff), (20, 0xff)] {
        let mut bad = original.clone();
        bad[index] = value;
        assert!(decode(CompressionKind::Lz4, &bad, 3).is_err());
    }
    let mut streams = raw_lz4(b"abc");
    streams.extend(raw_lz4(b"def"));
    streams.extend(end_lz4());
    assert_eq!(
        decode(CompressionKind::Lz4, &streams, 6).unwrap(),
        b"abcdef"
    );
    assert_eq!(
        decode(CompressionKind::Lz4, &streams, 5),
        Err(CompressionError::OutputLimit)
    );
}
