use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use arrow_mc::world::storage::region::{
    RegionError, RegionLocation, RegionReadLimits, StreamVersion, UnavailableReason, locate,
};

const SECTOR: usize = 4096;
const HEADER: usize = 2 * SECTOR;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

// Independent synthetic fixtures exercise the 26.3-pre-2 Java read-oracle outcomes.
// Codec and NBT validation belong to the CPU consumer, not this file locator.
struct Fixture {
    temp_root: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = temp_root.join(format!(
            "arrow-mc-region-test-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self { temp_root, path }
    }

    fn region_path(&self, x: i32, z: i32) -> PathBuf {
        self.path
            .join(format!("r.{}.{}.mca", x.div_euclid(32), z.div_euclid(32)))
    }

    fn external_path(&self, x: i32, z: i32) -> PathBuf {
        self.path.join(format!("c.{x}.{z}.mcc"))
    }

    fn write_region(&self, x: i32, z: i32, bytes: &[u8]) {
        fs::write(self.region_path(x, z), bytes).unwrap();
    }

    fn locate(&self, x: i32, z: i32) -> Result<RegionLocation, RegionError> {
        locate(&self.path, x, z, limits(1024 * 1024))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(resolved) = self.path.canonicalize() else {
            return;
        };
        // Only remove this uniquely created direct child of the resolved OS temp root.
        assert_eq!(resolved.parent(), Some(self.temp_root.as_path()));
        assert_eq!(resolved, self.path);
        assert!(
            resolved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("arrow-mc-region-test-")
        );
        fs::remove_dir_all(resolved).unwrap();
    }
}

fn limits(compressed_bytes: usize) -> RegionReadLimits {
    RegionReadLimits { compressed_bytes }
}

fn set_entry(bytes: &mut [u8], x: i32, z: i32, sector: u32, count: u8) {
    let entry = (x.rem_euclid(32) + 32 * z.rem_euclid(32)) as usize * 4;
    bytes[entry..entry + 4].copy_from_slice(&((sector << 8) | u32::from(count)).to_be_bytes());
}

fn stream(x: i32, z: i32, length: i32, version: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= SECTOR - 5);
    let mut bytes = vec![0; 3 * SECTOR];
    set_entry(&mut bytes, x, z, 2, 1);
    bytes[HEADER..HEADER + 4].copy_from_slice(&length.to_be_bytes());
    bytes[HEADER + 4] = version;
    bytes[HEADER + 5..HEADER + 5 + payload.len()].copy_from_slice(payload);
    bytes
}

fn ordinary_stream(x: i32, z: i32, version: u8, payload: &[u8]) -> Vec<u8> {
    stream(x, z, payload.len() as i32 + 1, version, payload)
}

fn custom_identifier(encoded: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + encoded.len());
    payload.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
    payload.extend_from_slice(encoded);
    payload
}

fn unavailable(fixture: &Fixture, expected: UnavailableReason) {
    match fixture.locate(0, 0).unwrap() {
        RegionLocation::Unavailable(actual) => assert_eq!(actual, expected),
        RegionLocation::Missing => panic!("expected unavailable stream, got missing entry"),
        RegionLocation::Present(_) => panic!("expected unavailable stream, got readable bytes"),
    }
}

fn read_present(
    root: &Path,
    x: i32,
    z: i32,
    expected_version: StreamVersion,
    expected: &[u8],
    external_warning: bool,
) {
    let RegionLocation::Present(chunk) = locate(root, x, z, limits(expected.len())).unwrap() else {
        panic!("expected a located stream");
    };
    assert_eq!(chunk.version(), expected_version);
    assert_eq!(chunk.compressed_len(), expected.len());
    assert_eq!(
        chunk.has_external_internal_length_warning(),
        external_warning
    );
    assert!(!chunk.has_truncated_region_header_warning());
    let mut bytes = vec![0; chunk.compressed_len()];
    chunk.read_into(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
}

#[test]
fn absent_directory_region_and_entry_are_missing_without_creating_files() {
    let fixture = Fixture::new();
    let absent_directory = fixture.path.join("absent");
    assert!(matches!(
        locate(&absent_directory, 0, 0, limits(16)).unwrap(),
        RegionLocation::Missing
    ));
    assert!(!absent_directory.exists());
    assert!(matches!(
        fixture.locate(0, 0).unwrap(),
        RegionLocation::Missing
    ));
    assert_eq!(fs::read_dir(&fixture.path).unwrap().count(), 0);

    let bytes = vec![0; HEADER];
    fixture.write_region(0, 0, &bytes);
    assert!(matches!(
        fixture.locate(0, 0).unwrap(),
        RegionLocation::Missing
    ));
    assert_eq!(fs::read(fixture.region_path(0, 0)).unwrap(), bytes);
    assert_eq!(fs::read_dir(&fixture.path).unwrap().count(), 1);
}

#[test]
fn negative_coordinates_select_the_floor_region_and_wrapped_entry() {
    let fixture = Fixture::new();
    let bytes = ordinary_stream(-33, -1, 3, b"negative coordinate payload");
    fixture.write_region(-33, -1, &bytes);
    assert!(fixture.path.join("r.-2.-1.mca").is_file());
    read_present(
        &fixture.path,
        -33,
        -1,
        StreamVersion::Raw,
        b"negative coordinate payload",
        false,
    );
    assert!(matches!(
        fixture.locate(-64, -32).unwrap(),
        RegionLocation::Missing
    ));
    assert_eq!(fs::read(fixture.region_path(-33, -1)).unwrap(), bytes);
}

#[test]
fn empty_and_partial_zero_headers_are_missing_and_never_padded() {
    let fixture = Fixture::new();
    for length in [0, 1, SECTOR - 1, SECTOR, HEADER - 1] {
        let bytes = vec![0; length];
        fixture.write_region(0, 0, &bytes);
        assert!(matches!(
            fixture.locate(0, 0).unwrap(),
            RegionLocation::Missing
        ));
        assert_eq!(fs::read(fixture.region_path(0, 0)).unwrap(), bytes);
    }
}

#[test]
fn invalid_sector_locations_are_distinct_from_absent_entries() {
    let fixture = Fixture::new();
    for (sector, count) in [(0, 1), (1, 1), (2, 0), (4, 1), (0x00ff_ffff, 1)] {
        let mut bytes = vec![0; 3 * SECTOR];
        set_entry(&mut bytes, 0, 0, sector, count);
        fixture.write_region(0, 0, &bytes);
        unavailable(&fixture, UnavailableReason::InvalidSector);
    }
}

#[test]
fn sector_at_eof_and_partial_chunk_headers_are_truncated() {
    let fixture = Fixture::new();
    for available in 0..5 {
        let mut bytes = vec![0; HEADER + available];
        set_entry(&mut bytes, 0, 0, 2, 1);
        fixture.write_region(0, 0, &bytes);
        unavailable(&fixture, UnavailableReason::TruncatedChunkHeader);
    }
    let mut bytes = vec![0; 3 * SECTOR];
    set_entry(&mut bytes, 0, 0, 3, 1);
    fixture.write_region(0, 0, &bytes);
    unavailable(&fixture, UnavailableReason::TruncatedChunkHeader);
}

#[test]
fn a_complete_payload_is_readable_when_declared_sectors_extend_beyond_eof() {
    let fixture = Fixture::new();
    let mut bytes = ordinary_stream(0, 0, 3, b"complete");
    set_entry(&mut bytes, 0, 0, 2, 2);
    fixture.write_region(0, 0, &bytes);
    read_present(&fixture.path, 0, 0, StreamVersion::Raw, b"complete", false);
    bytes.truncate(HEADER + 5 + b"complete".len());
    fixture.write_region(0, 0, &bytes);
    read_present(&fixture.path, 0, 0, StreamVersion::Raw, b"complete", false);
    assert_eq!(fs::read(fixture.region_path(0, 0)).unwrap(), bytes);
}

#[test]
fn internal_lengths_preserve_zero_negative_overflow_and_extent_classifications() {
    let fixture = Fixture::new();
    for (length, expected) in [
        (0, UnavailableReason::MissingStream),
        (-1, UnavailableReason::NegativeStreamLength),
        (i32::MIN, UnavailableReason::TruncatedStream),
        (i32::MAX, UnavailableReason::TruncatedStream),
        (SECTOR as i32 - 3, UnavailableReason::TruncatedStream),
    ] {
        fixture.write_region(0, 0, &stream(0, 0, length, 3, &[]));
        unavailable(&fixture, expected);
    }
    let mut bytes = ordinary_stream(0, 0, 3, b"unfinished");
    bytes.truncate(HEADER + 5 + b"unfinished".len() - 1);
    fixture.write_region(0, 0, &bytes);
    unavailable(&fixture, UnavailableReason::TruncatedStream);
}

#[test]
fn supported_streams_are_returned_unmodified_without_codec_validation() {
    let fixture = Fixture::new();
    let payload = b"deliberately not compressed or NBT";
    for (id, version) in [
        (1, StreamVersion::Gzip),
        (2, StreamVersion::Zlib),
        (3, StreamVersion::Raw),
        (4, StreamVersion::Lz4),
    ] {
        fixture.write_region(0, 0, &ordinary_stream(0, 0, id, payload));
        read_present(&fixture.path, 0, 0, version, payload, false);
        fixture.write_region(0, 0, &stream(0, 0, 1, id, &[]));
        read_present(&fixture.path, 0, 0, version, &[], false);
    }
}

#[test]
fn unknown_compression_ids_are_unavailable() {
    let fixture = Fixture::new();
    for id in [0, 5, 126] {
        fixture.write_region(0, 0, &ordinary_stream(0, 0, id, b"bytes"));
        unavailable(&fixture, UnavailableReason::UnknownVersion(id));
    }
}

#[test]
fn custom_identifiers_are_checked_without_implementing_a_custom_decoder() {
    let fixture = Fixture::new();
    for identifier in [
        "",
        ":",
        "minecraft:",
        "example:codec",
        "codec",
        "a_b.c-d:e/f",
    ] {
        let payload = custom_identifier(identifier.as_bytes());
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, &payload));
        unavailable(&fixture, UnavailableReason::UnsupportedCustomCompression);
    }
    for identifier in ["Upper:case", "a:b:c", "bad namespace:codec", "a/b:codec"] {
        let payload = custom_identifier(identifier.as_bytes());
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, &payload));
        unavailable(
            &fixture,
            UnavailableReason::InvalidCustomCompressionIdentifier,
        );
    }
}

#[test]
fn custom_modified_utf_accepts_java_overlong_sequences_but_rejects_bad_syntax() {
    let fixture = Fixture::new();
    for encoded in [&[0xc1, 0xa1][..], &[0xe0, 0x81, 0xa1][..]] {
        let payload = custom_identifier(encoded);
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, &payload));
        unavailable(&fixture, UnavailableReason::UnsupportedCustomCompression);
    }
    for encoded in [&[0][..], &[0xc0, 0x80][..], &[0xed, 0xa0, 0x80][..]] {
        let payload = custom_identifier(encoded);
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, &payload));
        unavailable(
            &fixture,
            UnavailableReason::InvalidCustomCompressionIdentifier,
        );
    }
}

#[test]
fn malformed_modified_utf_and_incomplete_custom_payloads_remain_errors() {
    let fixture = Fixture::new();
    for encoded in [
        &[0x80][..],
        &[0xc0][..],
        &[0xc2, b'a'][..],
        &[0xe0, 0x80][..],
        &[0xf0, 0x90, 0x80, 0x80][..],
    ] {
        let payload = custom_identifier(encoded);
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, &payload));
        assert!(matches!(
            fixture.locate(0, 0),
            Err(RegionError::InvalidCustomUtf)
        ));
    }
    for payload in [&[][..], &[0][..], &[0, 2, b'a'][..], &[0, 2, 0xc0][..]] {
        fixture.write_region(0, 0, &ordinary_stream(0, 0, 127, payload));
        assert!(matches!(
            fixture.locate(0, 0),
            Err(RegionError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));
    }
}

#[test]
fn external_missing_file_and_directory_remain_distinct_without_creating_data() {
    let fixture = Fixture::new();
    let region = stream(0, 0, 1, 0x83, &[]);
    fixture.write_region(0, 0, &region);
    unavailable(&fixture, UnavailableReason::ExternalMissing);
    assert!(!fixture.external_path(0, 0).exists());
    fs::create_dir(fixture.external_path(0, 0)).unwrap();
    unavailable(&fixture, UnavailableReason::ExternalNotFile);
    assert!(fixture.external_path(0, 0).is_dir());
    assert_eq!(fs::read(fixture.region_path(0, 0)).unwrap(), region);
}

#[test]
fn external_streams_use_absolute_chunk_coordinates_and_unmodified_codec_bytes() {
    let fixture = Fixture::new();
    let payload = b"external bytes";
    fs::write(fixture.external_path(-33, -1), payload).unwrap();
    for (id, version) in [
        (1, StreamVersion::Gzip),
        (2, StreamVersion::Zlib),
        (3, StreamVersion::Raw),
        (4, StreamVersion::Lz4),
    ] {
        let bytes = stream(-33, -1, 1, 0x80 | id, &[]);
        fixture.write_region(-33, -1, &bytes);
        read_present(&fixture.path, -33, -1, version, payload, false);
        assert_eq!(fs::read(fixture.region_path(-33, -1)).unwrap(), bytes);
    }
    assert!(fixture.path.join("c.-33.-1.mcc").is_file());
    assert_eq!(fs::read(fixture.external_path(-33, -1)).unwrap(), payload);
    assert_eq!(fs::read_dir(&fixture.path).unwrap().count(), 2);
}

#[test]
fn external_flag_precedes_nonzero_internal_length_validation_and_emits_warning() {
    let fixture = Fixture::new();
    fs::write(fixture.external_path(0, 0), b"external wins").unwrap();
    for length in [-1, i32::MIN, i32::MAX, 2, 4093] {
        fixture.write_region(0, 0, &stream(0, 0, length, 0x83, b"ignored internal"));
        read_present(
            &fixture.path,
            0,
            0,
            StreamVersion::Raw,
            b"external wins",
            true,
        );
    }
    fixture.write_region(0, 0, &stream(0, 0, 0, 0x83, &[]));
    unavailable(&fixture, UnavailableReason::MissingStream);
}

#[test]
fn empty_external_stream_is_located_for_the_cpu_consumer() {
    let fixture = Fixture::new();
    fixture.write_region(0, 0, &stream(0, 0, 1, 0x83, &[]));
    fs::write(fixture.external_path(0, 0), []).unwrap();
    read_present(&fixture.path, 0, 0, StreamVersion::Raw, &[], false);
}

#[test]
fn external_unknown_and_custom_versions_keep_their_classification() {
    let fixture = Fixture::new();
    fs::write(fixture.external_path(0, 0), b"opaque").unwrap();
    fixture.write_region(0, 0, &stream(0, 0, 1, 0x85, &[]));
    unavailable(&fixture, UnavailableReason::UnknownVersion(5));
    let payload = custom_identifier(b"example:codec");
    fs::write(fixture.external_path(0, 0), &payload).unwrap();
    fixture.write_region(0, 0, &stream(0, 0, 1, 0xff, &[]));
    unavailable(&fixture, UnavailableReason::UnsupportedCustomCompression);
}

#[test]
fn internal_compressed_limit_is_checked_before_returning_a_readable_handle() {
    let fixture = Fixture::new();
    fixture.write_region(0, 0, &ordinary_stream(0, 0, 3, b"12345678"));
    assert!(matches!(
        locate(&fixture.path, 0, 0, limits(7)),
        Err(RegionError::CompressedLimit {
            length: 8,
            limit: 7
        })
    ));
    read_present(&fixture.path, 0, 0, StreamVersion::Raw, b"12345678", false);
}

#[test]
fn external_file_length_is_bounded_before_any_payload_allocation() {
    let fixture = Fixture::new();
    fixture.write_region(0, 0, &stream(0, 0, 1, 0x83, &[]));
    let external = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(fixture.external_path(0, 0))
        .unwrap();
    external.set_len(1024 * 1024 + 1).unwrap();
    drop(external);
    assert!(matches!(
        locate(&fixture.path, 0, 0, limits(1024)),
        Err(RegionError::CompressedLimit {
            length: 1_048_577,
            limit: 1024
        })
    ));
    assert_eq!(
        fs::metadata(fixture.external_path(0, 0)).unwrap().len(),
        1_048_577
    );
}

#[test]
fn read_into_requires_an_exact_caller_owned_buffer_without_partial_fill() {
    let fixture = Fixture::new();
    fixture.write_region(0, 0, &ordinary_stream(0, 0, 3, b"four"));
    for length in [0, 3, 5] {
        let RegionLocation::Present(chunk) = fixture.locate(0, 0).unwrap() else {
            panic!("expected readable chunk");
        };
        let mut output = vec![0xa5; length];
        assert!(matches!(
            chunk.read_into(&mut output),
            Err(RegionError::OutputLength { expected: 4, actual }) if actual == length
        ));
        assert_eq!(output, vec![0xa5; length]);
    }
    read_present(&fixture.path, 0, 0, StreamVersion::Raw, b"four", false);
}

#[test]
fn file_truncation_after_locate_is_a_read_error_for_internal_and_external_data() {
    let fixture = Fixture::new();
    for external in [false, true] {
        let path = if external {
            fixture.write_region(0, 0, &stream(0, 0, 1, 0x83, &[]));
            fs::write(fixture.external_path(0, 0), b"four").unwrap();
            fixture.external_path(0, 0)
        } else {
            fixture.write_region(0, 0, &ordinary_stream(0, 0, 3, b"four"));
            fixture.region_path(0, 0)
        };
        let RegionLocation::Present(chunk) = fixture.locate(0, 0).unwrap() else {
            panic!("expected readable chunk before truncation");
        };
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(if external { 3 } else { (HEADER + 8) as u64 })
            .unwrap();
        drop(file);
        assert!(matches!(
            chunk.read_into(&mut [0; 4]),
            Err(RegionError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));
    }
}
