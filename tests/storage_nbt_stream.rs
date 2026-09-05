use arrow_mc::nbt::{self, Compound, Limits, NamedTag, NbtString, Tag};
use arrow_mc::world::storage::compression::{CompressionKind, StorageDecoder};
use arrow_mc::world::storage::nbt_stream::{StreamError, read_disk_compound};
use std::io::Read;

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn load(input: &[u8], limits: Limits) -> Result<(Compound, usize), StreamError> {
    let mut decoder = StorageDecoder::new();
    let mut scratch = [];
    let mut reader = decoder
        .reader(CompressionKind::Raw, input, &mut scratch, input.len())
        .unwrap();
    let mut output = Vec::with_capacity(input.len());
    read_disk_compound(&mut reader, &mut output, input.len(), limits)
}

#[test]
fn disk_root_name_is_skipped_without_modified_utf_validation() {
    let input = bytes("0a0004f09f9880030001780000000700");
    let (compound, _) = load(&input, Limits::default()).unwrap();
    assert_eq!(compound.get(&"x".into()), Some(&Tag::Int(7)));
    assert_eq!(
        nbt::read_named(&mut input.as_slice(), Limits::default()),
        Err(nbt::Error::InvalidModifiedUtf8)
    );
}

#[test]
fn opaque_root_name_can_cross_buffer_refills_without_entering_capture() {
    let mut input = vec![10, 0xff, 0xff];
    input.resize(3 + 65535, 0xff);
    input.extend_from_slice(&[0, 42]);
    let mut decoder = StorageDecoder::new();
    let mut scratch = [];
    let mut reader = decoder
        .reader(CompressionKind::Raw, &input, &mut scratch, input.len())
        .unwrap();
    let mut captured = Vec::with_capacity(2);
    let (compound, allocated) =
        read_disk_compound(&mut reader, &mut captured, 2, Limits::default()).unwrap();
    assert!(compound.entries().is_empty());
    assert_eq!(allocated, 0);
    assert_eq!(captured, [10, 0]);
    let mut tail = [0];
    reader.read_exact(&mut tail).unwrap();
    assert_eq!(tail, [42]);
}

#[test]
fn one_root_stops_before_extra_tags_and_keeps_existing_capture_prefix() {
    let input = bytes("0a00000300017800000007000300000009");
    let mut decoder = StorageDecoder::new();
    let mut scratch = [];
    let mut reader = decoder
        .reader(CompressionKind::Raw, &input, &mut scratch, input.len())
        .unwrap();
    let mut output = Vec::with_capacity(input.len() + 1);
    output.push(42);
    let (compound, _) =
        read_disk_compound(&mut reader, &mut output, input.len(), Limits::default()).unwrap();
    assert_eq!(compound.get(&"x".into()), Some(&Tag::Int(7)));
    assert_eq!(output, bytes("2a0a030001780000000700"));
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, bytes("0300000009"));
}

#[test]
fn scanner_covers_every_tag_shape_and_modern_heterogeneous_lists() {
    let mut compound = Compound::new();
    for (name, value) in [
        ("byte", Tag::Byte(-1)),
        ("short", Tag::Short(-2)),
        ("int", Tag::Int(-3)),
        ("long", Tag::Long(-4)),
        ("float", Tag::Float(1.5)),
        ("double", Tag::Double(2.5)),
        ("bytes", Tag::ByteArray(vec![1, 2, -1])),
        ("ints", Tag::IntArray(vec![i32::MIN, i32::MAX])),
        ("longs", Tag::LongArray(vec![i64::MIN, i64::MAX])),
        (
            "text",
            Tag::String(NbtString::from_utf16(vec![0, 0xd800, 0x7ff])),
        ),
        (
            "mixed",
            Tag::List(vec![
                Tag::Int(7),
                Tag::String("x".into()),
                Tag::Compound(Compound::new()),
            ]),
        ),
    ] {
        compound.insert(name.into(), value).unwrap();
    }
    let tag = NamedTag {
        name: "disk".into(),
        tag: Tag::Compound(compound),
    };
    let mut encoded = Vec::new();
    nbt::write_named(&tag, &mut encoded, Limits::default()).unwrap();
    let (decoded, allocated) = load(&encoded, Limits::default()).unwrap();
    assert_eq!(Tag::Compound(decoded), tag.tag);
    assert!(allocated > 0);
}

#[test]
fn malformed_counts_strings_ids_and_missing_end_are_rejected() {
    for input in [
        "0a000007000178ffffffff00",
        "0a000009000178000000000100",
        "0a00000d00017800",
        "0a00000800017800018000",
        "0a00000300017800000007",
    ] {
        assert!(load(&bytes(input), Limits::default()).is_err(), "{input}");
    }
    let (compound, _) = load(&bytes("0a000009000178ff0000000000"), Limits::default()).unwrap();
    assert_eq!(compound.get(&"x".into()), Some(&Tag::List(vec![])));
    assert_eq!(
        load(&bytes("03000000000007"), Limits::default()),
        Err(StreamError::RootType)
    );
}

#[test]
fn named_root_scanning_and_decode_depth_use_the_same_512_boundary() {
    for depth in [512, 513] {
        let mut input = bytes("0a0000");
        for _ in 1..depth {
            input.extend_from_slice(&[10, 0, 1, b'x']);
        }
        input.resize(input.len() + depth, 0);
        let result = load(&input, Limits::default());
        if depth == 512 {
            Tag::Compound(result.unwrap().0).drop_iterative();
        } else {
            assert_eq!(result, Err(StreamError::Nbt(nbt::Error::DepthLimit)));
        }
    }
}

#[test]
fn input_truncation_and_capture_limits_do_not_leave_partial_captured_bytes() {
    let input = bytes("0a0000070001780000000301020300");
    for cutoff in 0..input.len() {
        let mut decoder = StorageDecoder::new();
        let mut scratch = [];
        let mut reader = decoder
            .reader(
                CompressionKind::Raw,
                &input[..cutoff],
                &mut scratch,
                input.len(),
            )
            .unwrap();
        let mut output = Vec::with_capacity(input.len() + 1);
        output.push(42);
        assert!(
            read_disk_compound(&mut reader, &mut output, input.len(), Limits::default()).is_err()
        );
        assert_eq!(output, [42]);
    }
    let mut decoder = StorageDecoder::new();
    let mut scratch = [];
    let mut reader = decoder
        .reader(CompressionKind::Raw, &input, &mut scratch, input.len())
        .unwrap();
    let mut output = Vec::with_capacity(3);
    assert_eq!(
        read_disk_compound(&mut reader, &mut output, input.len(), Limits::default()),
        Err(StreamError::BufferNotReserved)
    );
    assert_eq!(
        read_disk_compound(&mut reader, &mut output, 3, Limits::default()),
        Err(StreamError::InflatedLimit)
    );
    assert!(output.is_empty());
}

#[test]
fn consumed_nbt_preserves_gzip_and_zlib_checksum_boundary_differences() {
    use flate2::{
        Compression,
        write::{GzEncoder, ZlibEncoder},
    };
    use std::io::Write;
    for length in [0, 8177, 65521] {
        let mut compound = Compound::new();
        compound
            .insert("blob".into(), Tag::ByteArray(vec![0; length]))
            .unwrap();
        let mut raw = Vec::new();
        nbt::write_named(
            &NamedTag {
                name: NbtString::default(),
                tag: Tag::Compound(compound),
            },
            &mut raw,
            Limits::default(),
        )
        .unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&raw).unwrap();
        let mut gzip = gzip.finish().unwrap();
        let crc = gzip.len() - 8;
        gzip[crc] ^= 1;
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        zlib.write_all(&raw).unwrap();
        let mut zlib = zlib.finish().unwrap();
        let last = zlib.len() - 1;
        zlib[last] ^= 1;
        for (kind, input, succeeds) in [
            (CompressionKind::Gzip, gzip, true),
            (CompressionKind::Zlib, zlib, false),
        ] {
            let mut decoder = StorageDecoder::new();
            let mut scratch = [];
            let mut reader = decoder
                .reader(kind, &input, &mut scratch, 1024 * 1024)
                .unwrap();
            let mut captured = Vec::with_capacity(1024 * 1024);
            let result =
                read_disk_compound(&mut reader, &mut captured, 1024 * 1024, Limits::default());
            assert_eq!(
                result.is_ok(),
                succeeds,
                "kind {kind:?}, byte array {length}"
            );
        }
    }
}

#[test]
#[ignore = "requires local independently generated official storage-tail-observations.json"]
fn all_recorded_region_nbt_consumer_results_match() {
    use arrow_mc::world::storage::compression::lz4_scratch_required;
    use sha2::{Digest, Sha256};
    let reference =
        std::env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set local Decompile root");
    let file = std::path::Path::new(&reference)
        .parent()
        .unwrap()
        .join("Roadmap/research/storage-tail-observations.json");
    let observations: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(observations["minecraft"], "26.3-pre-2");
    assert_eq!(observations["case_count"], 988);
    let mut count = 0;
    let mut failures = Vec::new();
    for case in observations["observations"].as_array().unwrap() {
        let encoded = case["compressed_base64"]
            .as_str()
            .expect("all original 988 inputs required");
        let mut input = openssl::base64::decode_block(encoded).unwrap();
        input.truncate(case["compressed_bytes"].as_u64().unwrap() as usize);
        assert_eq!(
            format!("{:x}", Sha256::digest(&input)),
            case["compressed_sha256"].as_str().unwrap()
        );
        let kind = CompressionKind::try_from(case["version"].as_u64().unwrap() as u8).unwrap();
        let mut decoder = StorageDecoder::new();
        let mut scratch = vec![0; lz4_scratch_required(&input, 1024 * 1024)];
        let mut captured = Vec::with_capacity(1024 * 1024);
        let result = match decoder.reader(kind, &input, &mut scratch, 1024 * 1024) {
            Ok(mut reader) => read_disk_compound(
                &mut reader,
                &mut captured,
                1024 * 1024,
                Limits {
                    vanilla_quota_bytes: usize::MAX,
                    ..Limits::default()
                },
            ),
            Err(error) => Err(StreamError::Compression(error)),
        };
        let oracle = &case["RegionFile_NbtIo_read"];
        let name = format!(
            "v{} blob={} {} {}",
            case["version"], case["blob_length"], case["content"], case["mutation"]
        );
        if result.is_ok() != (oracle["outcome"] == "tag") {
            failures.push(format!(
                "{name}: Rust {:?}, Java {}",
                result.as_ref().err(),
                oracle["outcome"]
            ));
        }
        if let Ok((compound, _)) = result {
            let blob = match compound.get(&"blob".into()) {
                Some(Tag::ByteArray(value)) => value.as_slice(),
                None => &[],
                _ => panic!("wrong blob type"),
            };
            assert_eq!(
                blob.len(),
                oracle["blob_length"].as_u64().unwrap() as usize,
                "{name}"
            );
            let bytes: Vec<_> = blob.iter().map(|&byte| byte as u8).collect();
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                oracle["blob_sha256"].as_str().unwrap(),
                "{name}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 988);
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    println!("All {count} actual RegionFile NBT consumer observations matched");
}
