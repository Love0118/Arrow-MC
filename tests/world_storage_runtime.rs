//! Real region reads exercise CPU admission and result ownership, without world publication.
#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{
        AdmissionError, ChunkDecodeOutput, ChunkDecodeTask, ChunkReadKey, CpuPool, CpuPoolConfig,
        ResidentChunkBudget,
    },
    world::storage::{
        ChunkLoadError, ChunkReadOutcome, ChunkStore, StorageLimits,
        chunk::{ChunkDecodeError, ChunkStatus, DATA_VERSION, DimensionHeight},
        nbt_stream::StreamError,
        region::StreamVersion,
        registry::ChunkRegistrySnapshot,
    },
};
use flate2::{Compression, write::ZlibEncoder};
use std::{
    fs,
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{task::JoinSet, time::timeout};

const CPU_BYTES: usize = 128 * 1024 * 1024;
const RESIDENT_BYTES: usize = 32 * 1024 * 1024;

struct Record {
    x: i32,
    version: StreamVersion,
    bytes: Vec<u8>,
}

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut value = Compound::new();
    for (name, entry) in entries {
        value.insert(name.into(), entry).unwrap();
    }
    Tag::Compound(value)
}

fn disk_nbt(x: i32, marker: i64, version: i32, light_len: usize) -> Vec<u8> {
    let mut sections = Vec::new();
    for y in -4..20 {
        let block = if x % 2 == 0 {
            compound([("id", Tag::String("minecraft:air".into()))])
        } else {
            compound([
                ("id", Tag::String("test:lamp".into())),
                (
                    "properties",
                    compound([
                        ("facing", Tag::String("south".into())),
                        ("lit", Tag::String("true".into())),
                    ]),
                ),
            ])
        };
        let light = (x + i32::from(y) + 4) as i8;
        sections.push(compound([
            ("Y", Tag::Byte(y)),
            (
                "block_states",
                compound([("palette", Tag::List(vec![block]))]),
            ),
            (
                "biomes",
                compound([(
                    "palette",
                    Tag::List(vec![Tag::String("minecraft:forest".into())]),
                )]),
            ),
            ("BlockLight", Tag::ByteArray(vec![light; light_len])),
            ("SkyLight", Tag::ByteArray(vec![-1; 2048])),
        ]));
    }
    let root = NamedTag {
        name: "synthetic storage runtime record".into(),
        tag: compound([
            ("DataVersion", Tag::Int(version)),
            ("xPos", Tag::Int(x)),
            ("zPos", Tag::Int(0)),
            ("Status", Tag::String("minecraft:full".into())),
            ("LastUpdate", Tag::Long(marker)),
            ("isLightOn", Tag::Byte(1)),
            ("sections", Tag::List(sections)),
            ("test:load-marker", Tag::Long(marker)),
        ]),
    };
    let mut bytes = Vec::new();
    nbt::write_named(&root, &mut bytes, nbt::Limits::default()).unwrap();
    bytes
}

fn compressed_record(x: i32) -> Record {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&disk_nbt(x, i64::from(x) + 1000, DATA_VERSION, 2048))
        .unwrap();
    Record {
        x,
        version: StreamVersion::Zlib,
        bytes: encoder.finish().unwrap(),
    }
}

fn write_region(directory: &Path, records: &[Record]) {
    fs::create_dir_all(directory).unwrap();
    let mut file = vec![0u8; 8192];
    for record in records {
        assert!((0..32).contains(&record.x));
        let sectors = (record.bytes.len() + 5).div_ceil(4096);
        assert!(sectors < 256);
        let offset = file.len() / 4096;
        let slot = record.x as usize * 4;
        assert_eq!(&file[slot..slot + 4], &[0; 4]);
        let location = ((offset as u32) << 8) | sectors as u32;
        file[slot..slot + 4].copy_from_slice(&location.to_be_bytes());
        file.extend_from_slice(&((record.bytes.len() + 1) as i32).to_be_bytes());
        file.push(match record.version {
            StreamVersion::Gzip => 1,
            StreamVersion::Zlib => 2,
            StreamVersion::Raw => 3,
            StreamVersion::Lz4 => 4,
        });
        file.extend_from_slice(&record.bytes);
        file.resize((offset + sectors) * 4096, 0);
    }
    fs::write(directory.join("r.0.0.mca"), file).unwrap();
}

fn pool(workers: usize, jobs: usize, bytes: usize) -> Arc<CpuPool> {
    Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers,
            max_jobs: jobs,
            buffer_bytes: bytes,
        })
        .unwrap(),
    )
}

fn height() -> DimensionHeight {
    DimensionHeight::new(-64, 384).unwrap()
}

fn key(x: i32, epoch: u64, generation: u64) -> ChunkReadKey {
    ChunkReadKey {
        world_epoch: epoch,
        chunk_x: x,
        chunk_z: 0,
        generation,
    }
}

fn store(
    directory: &Path,
    cpu: &Arc<CpuPool>,
    registries: &Arc<ChunkRegistrySnapshot>,
    workers: usize,
) -> Arc<ChunkStore> {
    Arc::new(
        ChunkStore::new(
            directory.to_owned(),
            Arc::clone(cpu),
            Arc::clone(registries),
            height(),
            StorageLimits::default(),
            workers,
        )
        .unwrap(),
    )
}

fn decoded(outcome: ChunkReadOutcome) -> ChunkDecodeOutput {
    let ChunkReadOutcome::Decoded(output) = outcome else {
        panic!("expected an existing decoded chunk");
    };
    output
}

async fn read(store: &ChunkStore, key: ChunkReadKey) -> Result<ChunkReadOutcome, ChunkLoadError> {
    timeout(Duration::from_secs(5), store.read(key))
        .await
        .expect("bounded storage read stalled")
}

async fn wait(task: ChunkDecodeTask) -> Result<ChunkDecodeOutput, ChunkLoadError> {
    timeout(Duration::from_secs(5), task.wait())
        .await
        .expect("chunk CPU worker stalled")
}

fn assert_cpu(cpu: &CpuPool, jobs: usize, bytes: usize) {
    assert_eq!(cpu.stats().in_flight, jobs);
    assert_eq!(cpu.stats().reserved_buffer_bytes, bytes);
}

struct Measurement {
    elapsed: Duration,
    retained_bytes: usize,
    peak_job_bytes: usize,
    max_job_charge: usize,
    completed: u64,
}

async fn run_batches(
    directory: &Path,
    registries: &Arc<ChunkRegistrySnapshot>,
    records: &[Record],
    workers: usize,
) -> Measurement {
    let cpu = pool(workers, 4, CPU_BYTES);
    let store = store(directory, &cpu, registries, workers);
    let budget = ResidentChunkBudget::new(RESIDENT_BYTES);
    let limits = StorageLimits::default();
    let mut residents = Vec::new();
    let mut retained = 0;
    let mut max_job_charge = 0;
    let started = Instant::now();
    for batch in records.chunks(workers) {
        let mut jobs = JoinSet::new();
        let mut batch_charge = 0;
        for record in batch {
            let charge = limits
                .job_bytes_for(record.version, record.bytes.len())
                .unwrap();
            max_job_charge = max_job_charge.max(charge);
            batch_charge += charge;
            let store = Arc::clone(&store);
            let request = key(record.x, 23, record.x as u64 + 1);
            jobs.spawn(async move { read(&store, request).await });
        }
        let mut outputs = Vec::new();
        while let Some(result) = jobs.join_next().await {
            outputs.push(decoded(result.unwrap().unwrap()));
        }
        assert_cpu(&cpu, batch.len(), batch_charge);
        outputs.sort_unstable_by_key(|output| output.key().chunk_x);
        for (output, record) in outputs.into_iter().zip(batch) {
            assert_eq!(output.key(), key(record.x, 23, record.x as u64 + 1));
            let draft = output.draft();
            assert_eq!(draft.position, (record.x, 0));
            assert_eq!(draft.data_version, DATA_VERSION);
            assert_eq!(draft.status, ChunkStatus::Full);
            assert!(draft.light_correct);
            assert_eq!(draft.last_update, i64::from(record.x) + 1000);
            assert_eq!(draft.sections().len(), 24);
            for stored in draft.sections() {
                let section = stored.section.as_ref().unwrap();
                let block_id = if record.x % 2 == 0 { 0 } else { 2 };
                assert_eq!(section.blocks.get(0).unwrap(), block_id);
                assert_eq!(section.blocks.get(4095).unwrap(), block_id);
                assert_eq!(section.biomes.get(63).unwrap(), 1);
                let block_count = if block_id == 0 { 0 } else { 4096 };
                assert_eq!(section.counts.non_empty_blocks, block_count);
                assert_eq!(section.counts.fluid_blocks, block_count);
                assert_eq!(
                    stored.block_light.as_deref().unwrap(),
                    &[(record.x + i32::from(stored.y) + 4) as u8; 2048]
                );
                assert_eq!(stored.sky_light.as_deref().unwrap(), &[0xff; 2048]);
            }
            retained += output.retained_bytes();
            residents.push(output.try_adopt(&budget).unwrap());
        }
        assert_cpu(&cpu, 0, 0);
        assert_eq!(budget.stats().chunks, residents.len());
        assert_eq!(budget.stats().used_bytes, retained);
    }
    let elapsed = started.elapsed();
    let stats = cpu.stats();
    assert_eq!(stats.completed_jobs, records.len() as u64);
    assert!(stats.peak_running <= workers);
    assert!(stats.peak_reserved_buffer_bytes <= CPU_BYTES);
    assert_eq!(budget.stats().peak_bytes, retained);
    drop(residents);
    assert_eq!(budget.stats().chunks, 0);
    assert_eq!(budget.stats().used_bytes, 0);
    drop(store);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
    Measurement {
        elapsed,
        retained_bytes: retained,
        peak_job_bytes: stats.peak_reserved_buffer_bytes,
        max_job_charge,
        completed: stats.completed_jobs,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn one_two_and_four_workers_load_multiple_chunks_under_shared_admission() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    let records: Vec<_> = (0..8).map(compressed_record).collect();
    write_region(&directory, &records);
    let mut baseline_retained = None;
    for workers in [1, 2, 4] {
        let measured = run_batches(&directory, &registries, &records, workers).await;
        if let Some(expected) = baseline_retained {
            assert_eq!(measured.retained_bytes, expected);
        } else {
            baseline_retained = Some(measured.retained_bytes);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn old_epoch_and_generation_results_keep_their_identity_until_owner_adoption() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let record = compressed_record(0);
    let charge = StorageLimits::default()
        .job_bytes_for(record.version, record.bytes.len())
        .unwrap();
    let directory = fixture.root.join("region");
    write_region(&directory, &[record]);
    let cpu = pool(2, 4, CPU_BYTES);
    let store = store(&directory, &cpu, &registries, 2);
    let old_epoch = decoded(read(&store, key(0, 3, 7)).await.unwrap());
    let old_generation = decoded(read(&store, key(0, 4, 7)).await.unwrap());
    let current = decoded(read(&store, key(0, 4, 8)).await.unwrap());
    assert_eq!(old_epoch.key(), key(0, 3, 7));
    assert_eq!(old_generation.key(), key(0, 4, 7));
    assert_eq!(current.key(), key(0, 4, 8));
    assert_cpu(&cpu, 3, 3 * charge);
    let budget = ResidentChunkBudget::new(RESIDENT_BYTES);
    assert_eq!(budget.stats().chunks, 0);
    let retained = current.retained_bytes();
    let resident = current.try_adopt(&budget).unwrap();
    assert_eq!(resident.key(), key(0, 4, 8));
    assert_cpu(&cpu, 2, 2 * charge);
    assert_eq!(budget.stats().chunks, 1);
    assert_eq!(budget.stats().used_bytes, retained);
    // The owner discards stale identities; storage never publishes them by itself.
    drop(old_generation);
    drop(old_epoch);
    assert_cpu(&cpu, 0, 0);
    assert_eq!(resident.key(), key(0, 4, 8));
    assert_eq!(budget.stats().used_bytes, retained);
    drop(resident);
    assert_eq!(budget.stats().used_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_nbt_versions_and_section_data_release_job_bytes_before_retry() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    let records = vec![
        Record {
            x: 0,
            version: StreamVersion::Raw,
            bytes: vec![1, 0, 0, 0],
        },
        Record {
            x: 1,
            version: StreamVersion::Raw,
            bytes: vec![10, 0, 0, 8, 0, 1, b'x', 0, 4, b'a'],
        },
        Record {
            x: 2,
            version: StreamVersion::Raw,
            bytes: disk_nbt(2, 12, DATA_VERSION - 1, 2048),
        },
        Record {
            x: 3,
            version: StreamVersion::Raw,
            bytes: disk_nbt(3, 13, DATA_VERSION + 1, 2048),
        },
        Record {
            x: 4,
            version: StreamVersion::Raw,
            bytes: disk_nbt(4, 14, DATA_VERSION, 2047),
        },
        compressed_record(5),
    ];
    write_region(&directory, &records);
    let cpu = pool(1, 1, CPU_BYTES);
    let store = store(&directory, &cpu, &registries, 1);
    for x in 0..5 {
        let result = read(&store, key(x, 1, 1)).await;
        let expected = match (x, result) {
            (0, Err(ChunkLoadError::NbtStream(StreamError::RootType))) => true,
            (1, Err(ChunkLoadError::NbtStream(_))) => true,
            (2, Err(ChunkLoadError::Decode(ChunkDecodeError::NeedsUpgrade(version)))) => {
                version == DATA_VERSION - 1
            }
            (3, Err(ChunkLoadError::Decode(ChunkDecodeError::UnsupportedDataVersion(version)))) => {
                version == DATA_VERSION + 1
            }
            (4, Err(ChunkLoadError::Decode(ChunkDecodeError::LightLength(2047)))) => true,
            (_, Err(error)) => panic!("unexpected failure for record {x}: {error}"),
            _ => false,
        };
        assert!(expected, "malformed record {x} was accepted");
        assert_cpu(&cpu, 0, 0);
    }
    let output = decoded(read(&store, key(5, 1, 1)).await.unwrap());
    assert_eq!(output.draft().last_update, 1005);
    drop(output);
    assert_cpu(&cpu, 0, 0);
    assert_eq!(cpu.stats().completed_jobs, 6);
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_cpu_pool_rejects_a_present_disk_record_without_retaining_bytes() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(&directory, &[compressed_record(0)]);
    let cpu = pool(1, 1, CPU_BYTES);
    let store = store(&directory, &cpu, &registries, 1);
    cpu.close();
    assert!(matches!(
        read(&store, key(0, 9, 1)).await,
        Err(ChunkLoadError::Admission(AdmissionError::Closed))
    ));
    assert_cpu(&cpu, 0, 0);
    assert_eq!(cpu.stats().peak_reserved_buffer_bytes, 0);
    assert_eq!(cpu.stats().completed_jobs, 0);
}

#[test]
fn format_specific_reservations_charge_before_fill_and_reject_exhausted_bytes() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let limits = StorageLimits::default();
    let cpu = pool(1, 4, CPU_BYTES);
    let base =
        17 + limits.inflated_bytes + limits.nbt_limits.allocation_bytes + limits.decoded_bytes;
    for version in [
        StreamVersion::Raw,
        StreamVersion::Gzip,
        StreamVersion::Zlib,
        StreamVersion::Lz4,
    ] {
        let expected = base
            + if version == StreamVersion::Lz4 {
                limits.inflated_bytes
            } else {
                0
            };
        assert_eq!(limits.job_bytes_for(version, 17).unwrap(), expected);
        let mut pending = cpu
            .try_reserve_chunk_decode(
                key(0, 1, 1),
                version,
                17,
                Arc::clone(&registries),
                height(),
                limits,
            )
            .unwrap();
        assert_cpu(&cpu, 1, expected);
        assert_eq!(pending.compressed_mut(), &[0; 17]);
        drop(pending);
        assert_cpu(&cpu, 0, 0);
    }
    let cpu = pool(1, 4, base);
    let pending = cpu
        .try_reserve_chunk_decode(
            key(0, 1, 1),
            StreamVersion::Raw,
            17,
            Arc::clone(&registries),
            height(),
            limits,
        )
        .unwrap();
    assert_cpu(&cpu, 1, base);
    assert!(matches!(
        cpu.try_reserve_chunk_decode(
            key(1, 1, 1),
            StreamVersion::Raw,
            1,
            Arc::clone(&registries),
            height(),
            limits,
        ),
        Err(AdmissionError::ByteLimit)
    ));
    assert_cpu(&cpu, 1, base);
    cpu.close();
    assert!(matches!(pending.submit(), Err(AdmissionError::Closed)));
    assert_cpu(&cpu, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_and_cancelled_cpu_receivers_release_before_a_later_completion() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let record = compressed_record(0);
    let limits = StorageLimits::default();
    let charge = limits
        .job_bytes_for(record.version, record.bytes.len())
        .unwrap();
    let cpu = pool(1, 4, CPU_BYTES);
    let pending = |generation| {
        let mut pending = cpu
            .try_reserve_chunk_decode(
                key(0, 1, generation),
                record.version,
                record.bytes.len(),
                Arc::clone(&registries),
                height(),
                limits,
            )
            .unwrap();
        pending.compressed_mut().copy_from_slice(&record.bytes);
        pending
    };
    let unsubmitted = pending(1);
    assert_cpu(&cpu, 1, charge);
    drop(unsubmitted);
    assert_cpu(&cpu, 0, 0);
    drop(pending(2).submit().unwrap());
    let mut cancelled = pending(3).submit().unwrap();
    cancelled.cancel();
    assert!(matches!(
        wait(cancelled).await,
        Err(ChunkLoadError::Cancelled)
    ));
    // The one-worker FIFO sentinel follows every earlier cleanup path.
    let output = wait(pending(4).submit().unwrap()).await.unwrap();
    assert_eq!(output.key(), key(0, 1, 4));
    assert_cpu(&cpu, 1, charge);
    assert_eq!(cpu.stats().completed_jobs, 3);
    drop(output);
    assert_cpu(&cpu, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "small local timing sample; run explicitly with --ignored --nocapture"]
async fn measure_bounded_synthetic_chunk_read_batches() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    let records: Vec<_> = (0..12).map(compressed_record).collect();
    // File creation is outside timing and all worker counts use the same records.
    write_region(&directory, &records);
    println!(
        "synthetic 24-section zlib read/decode/adopt/check; CPU=128MiB, max_jobs=4, resident=32MiB; default storage caps; warm OS cache possible; debug/release depends on invocation"
    );
    for workers in [1, 2, 4] {
        let measured = run_batches(&directory, &registries, &records, workers).await;
        println!(
            "workers={workers} io_slots={workers} loaded={} elapsed_ms={:.3} loaded_per_s={:.1} retained_bytes={} peak_job_bytes={} max_job_charge={}",
            measured.completed,
            measured.elapsed.as_secs_f64() * 1000.0,
            measured.completed as f64 / measured.elapsed.as_secs_f64(),
            measured.retained_bytes,
            measured.peak_job_bytes,
            measured.max_job_charge,
        );
    }
}
