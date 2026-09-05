use arrow_mc::world::storage::compression::{
    CompressionError, CompressionKind, StorageDecoder, lz4_scratch_required,
};
use flate2::{
    Compression,
    write::{GzEncoder, ZlibEncoder},
};
use std::io::{Read, Write};

#[test]
fn pull_reader_does_not_force_unrequested_deflate_trailers() {
    let root = [10, 0, 0, 0];
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&root).unwrap();
    let mut gzip = gzip.finish().unwrap();
    gzip.truncate(gzip.len() - 8);
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    zlib.write_all(&root).unwrap();
    let mut zlib = zlib.finish().unwrap();
    zlib.truncate(zlib.len() - 4);
    for (kind, encoded) in [(CompressionKind::Gzip, gzip), (CompressionKind::Zlib, zlib)] {
        let mut decoder = StorageDecoder::new();
        let mut scratch = [];
        let mut reader = decoder.reader(kind, &encoded, &mut scratch, 8192).unwrap();
        let mut parsed = [0; 4];
        reader.read_exact(&mut parsed[..1]).unwrap();
        reader.read_exact(&mut parsed[1..3]).unwrap();
        reader.read_exact(&mut parsed[3..]).unwrap();
        assert_eq!(parsed, root);
        assert!(reader.read(&mut [0; 1]).is_err());
    }
}

#[test]
fn lz4_prefix_checks_needed_block_without_fetching_terminal() {
    let root = [10, 0, 0, 0];
    let mut encoded = b"LZ4Block".to_vec();
    encoded.push(0x10);
    encoded.extend(4u32.to_le_bytes());
    encoded.extend(4u32.to_le_bytes());
    encoded.extend((xxhash_rust::xxh32::xxh32(&root, 0x9747_b28c) & 0x0fff_ffff).to_le_bytes());
    encoded.extend(root);
    assert_eq!(lz4_scratch_required(&encoded, 8192), 4);
    let mut decoder = StorageDecoder::new();
    let mut scratch = [0; 4];
    let mut reader = decoder
        .reader(CompressionKind::Lz4, &encoded, &mut scratch, 8192)
        .unwrap();
    let mut output = [0; 4];
    reader.read_exact(&mut output).unwrap();
    assert_eq!(output, root);
    assert!(reader.read(&mut [0; 1]).is_err());
    encoded[17] ^= 1;
    let mut reader = decoder
        .reader(CompressionKind::Lz4, &encoded, &mut scratch, 8192)
        .unwrap();
    let error = reader.read(&mut [0; 1]).unwrap_err();
    assert_eq!(
        error.get_ref().unwrap().downcast_ref::<CompressionError>(),
        Some(&CompressionError::Checksum)
    );
}

#[test]
fn raw_skip_and_large_read_retain_java_buffer_boundaries() {
    let input: Vec<_> = (0..20000).map(|i| i as u8).collect();
    let mut decoder = StorageDecoder::new();
    let mut scratch = [];
    let mut reader = decoder
        .reader(CompressionKind::Raw, &input, &mut scratch, input.len())
        .unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    assert_eq!(reader.skip(9000).unwrap(), 8191);
    let mut output = [0; 9000];
    assert_eq!(reader.read(&mut output).unwrap(), 9000);
    assert_eq!(output.as_slice(), &input[8192..17192]);
}

#[test]
#[ignore = "requires local Roadmap storage-tail-observations.json, synthetic official API recordings"]
fn replay_recorded_java_nbt_read_requests() {
    let root = std::env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set local Decompile root");
    let file = std::path::Path::new(&root)
        .parent()
        .unwrap()
        .join("Roadmap/research/storage-tail-observations.json");
    let observations: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    let mut count = 0;
    for case in observations["observations"].as_array().unwrap() {
        let Some(encoded) = case["compressed_base64"].as_str() else {
            continue;
        };
        let oracle = &case["wrap_NbtIo_read"];
        let requests = oracle["decoded_requests_first_80"].as_array().unwrap();
        if requests.len() >= 80 {
            continue;
        }
        let mut input = openssl::base64::decode_block(encoded).unwrap();
        // OpenSSL's block helper includes padding bytes; the recording has exact size.
        input.truncate(case["compressed_bytes"].as_u64().unwrap() as usize);
        let kind = CompressionKind::try_from(case["version"].as_u64().unwrap() as u8).unwrap();
        let mut decoder = StorageDecoder::new();
        let mut scratch = vec![0; lz4_scratch_required(&input, 1024 * 1024)];
        let reader = decoder.reader(kind, &input, &mut scratch, 1024 * 1024);
        let mut failed = false;
        let mut total = 0;
        if let Ok(mut reader) = reader {
            for request in requests {
                let size = request.as_u64().unwrap() as usize;
                let mut buffer = vec![0; size];
                match reader.read(&mut buffer) {
                    Ok(n) => {
                        total += n;
                        if n == 0 {
                            failed = true;
                            break;
                        }
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
        } else {
            failed = true;
        }
        let name = format!(
            "v{} blob={} {} {}",
            case["version"], case["blob_length"], case["content"], case["mutation"]
        );
        assert_eq!(
            failed,
            oracle["outcome"].as_str().unwrap() != "tag",
            "{name}"
        );
        if !failed {
            assert_eq!(
                total,
                oracle["decoded_bytes_delivered"].as_u64().unwrap() as usize,
                "{name}"
            );
        }
        count += 1;
    }
    assert!(count >= 200);
    println!("Replayed {count} recorded NBT read sequences");
}
