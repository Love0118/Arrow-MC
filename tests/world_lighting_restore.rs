//! Canonical saved rows through the bounded restoration coordinator.
#[path = "common/lighting_fixture.rs"]
mod fixture;
use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{CpuPool, CpuPoolConfig},
    world::{
        lighting::{
            LightBlock, LightError, LightKind, LightSection, LightingSource, SourceLimits,
            block::BlockLightLimits,
            sky::SkyLimits,
            storage::{
                LAYER_RESERVATION_BYTES, LightSectionStorage, StorageError,
                StorageLimits as LightLimits,
            },
            work::{CompletedLighting, LightingError, LightingLimits, LightingWork, SkyWorkLimits},
        },
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        storage::{
            ChunkStore, StorageLimits,
            chunk::{DATA_VERSION, DimensionHeight},
        },
    },
};
use fixture::registry_fixture;
use serde_json::json;
use std::{fs, sync::Arc, time::Duration};

const CHUNK: ChunkAddress = ChunkAddress { x: 0, z: 0 };
const CENTER: LightBlock = LightBlock { x: 8, y: 8, z: 8 };
fn tag(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut out = Compound::new();
    for (key, value) in entries {
        out.insert(key.into(), value).unwrap();
    }
    Tag::Compound(out)
}
fn row(y: i8, state: Option<&str>, block: Option<u8>, sky: Option<u8>) -> Tag {
    let mut out = Compound::new();
    out.insert("Y".into(), Tag::Byte(y)).unwrap();
    if let Some(state) = state {
        out.insert(
            "block_states".into(),
            tag([("palette", Tag::List(vec![Tag::String(state.into())]))]),
        )
        .unwrap();
    }
    for (key, value) in [("BlockLight", block), ("SkyLight", sky)] {
        if let Some(value) = value {
            out.insert(key.into(), Tag::ByteArray(vec![value as i8; 2048]))
                .unwrap();
        }
    }
    Tag::Compound(out)
}
struct Fixture {
    _bundle: registry_fixture::Fixture,
    owner: ChunkLoadingOwner,
}
impl Fixture {
    async fn new(status: &str, flag: bool, sky: bool, rows: Vec<Tag>) -> Self {
        Self::with_chunks(sky, vec![(status, flag, rows)]).await
    }
    async fn with_chunks(sky: bool, chunks: Vec<(&str, bool, Vec<Tag>)>) -> Self {
        let chunk_count = chunks.len();
        let mut bundle = registry_fixture::Fixture::from_data(
            json!({
            "state_count":3,"state_flags":[1,0,0],"blocks":[
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
                {"id":"test:lamp","default_state":2,"properties":[],"states":[2]}]}),
            json!([{"id":"minecraft:plains","protocol_id":0}]),
        );
        let mut materials = [[0u8; 16]; 3];
        materials[1][1] = 15;
        materials[2][0] = 15;
        bundle.edit_lighting(|data| *data = registry_fixture::lighting_bytes(&materials, 2, &[14]));
        let registry = Arc::new(bundle.load());
        let mut region = vec![0; 8192];
        for (x, (status, flag, rows)) in chunks.into_iter().enumerate() {
            let root = NamedTag {
                name: "".into(),
                tag: tag([
                    ("DataVersion", Tag::Int(DATA_VERSION)),
                    ("xPos", Tag::Int(x as i32)),
                    ("zPos", Tag::Int(0)),
                    (
                        "Status",
                        Tag::String(format!("minecraft:{status}").as_str().into()),
                    ),
                    ("isLightOn", Tag::Byte(i8::from(flag))),
                    ("sections", Tag::List(rows)),
                ]),
            };
            let mut bytes = Vec::new();
            nbt::write_named(&root, &mut bytes, nbt::Limits::default()).unwrap();
            let count = (bytes.len() + 5).div_ceil(4096);
            let sector = region.len() / 4096;
            region[x * 4..x * 4 + 4]
                .copy_from_slice(&(((sector as u32) << 8) | count as u32).to_be_bytes());
            region.extend_from_slice(&((bytes.len() + 1) as u32).to_be_bytes());
            region.push(3);
            region.extend(bytes);
            region.resize((sector + count) * 4096, 0);
        }
        let path = bundle.root.join("region");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("r.0.0.mca"), region).unwrap();
        let cpu = Arc::new(
            CpuPool::new(CpuPoolConfig {
                workers: 1,
                max_jobs: 2,
                buffer_bytes: 64 << 20,
            })
            .unwrap(),
        );
        let height = DimensionHeight::new(0, 16).unwrap();
        let store = ChunkStore::new(
            path,
            Arc::clone(&cpu),
            Arc::clone(&registry),
            height,
            StorageLimits::default(),
            1,
        )
        .unwrap();
        let mut owner = ChunkLoadingOwner::new(
            1,
            registry,
            height,
            sky,
            LoadingLimits {
                max_chunks: chunk_count,
                metadata_bytes: 65536,
            },
            4 << 20,
        )
        .unwrap();
        for x in 0..chunk_count {
            let LoadDemand::Read(request) =
                owner.request(ChunkAddress { x: x as i32, z: 0 }).unwrap()
            else {
                panic!("read")
            };
            let LoadingReadOutcome::Decoded(output) =
                tokio::time::timeout(Duration::from_secs(5), request.read(&store))
                    .await
                    .unwrap()
                    .unwrap()
            else {
                panic!("decoded")
            };
            owner.publish(output).unwrap();
        }
        drop(store);
        Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
        Self {
            _bundle: bundle,
            owner,
        }
    }
    fn source(&self) -> LightingSource {
        LightingSource::from_canonical(&self.owner, &[CHUNK], SourceLimits::default()).unwrap()
    }
}
fn limits(sky: bool) -> LightingLimits {
    let storage = LightLimits {
        max_sections: 128,
        max_columns: 32,
        max_notifications: 512,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    };
    LightingLimits {
        max_chunks: 1,
        metadata_bytes: 8,
        block: BlockLightLimits {
            checks: 16,
            decreases: 32768,
            increases: 32768,
            queue_bytes: 2 << 20,
        },
        block_storage: storage,
        sky: sky.then_some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 16,
                queue_entries: 32768,
                source_chunks: 1,
                planned_writes: 256,
            },
            storage,
            engine_bytes: 2 << 20,
        }),
    }
}
fn complete(mut work: LightingWork, units: usize) -> CompletedLighting {
    for _ in 0..100_000 {
        let progress = work.step(units).unwrap();
        assert!(progress.processed <= units);
        assert!(work.retained_bytes() <= work.reservation_bytes());
        if progress.complete {
            return work.into_completed().unwrap_or_else(|_| panic!("complete"));
        }
        work = work
            .into_completed()
            .err()
            .expect("partial snapshots remain private");
    }
    panic!("finite fixture did not converge")
}

#[tokio::test]
async fn saved_status_and_flag_select_relight_without_discarding_queued_arrays() {
    for status in ["initialize_light", "light", "full"] {
        for flag in [false, true] {
            let fixture = Fixture::new(
                status,
                flag,
                false,
                vec![
                    row(0, Some("test:lamp"), Some(0), None),
                    row(120, None, Some(0x66), None),
                ],
            )
            .await;
            let completed = complete(
                LightingWork::new_restore(fixture.source(), limits(false)).unwrap(),
                64,
            );
            let lighted = status != "initialize_light" && flag;
            assert_eq!(
                completed.block().get_level(CENTER),
                if lighted { 0 } else { 15 },
                "{status}/{flag}"
            );
            let far = LightSection { x: 0, y: 120, z: 0 };
            assert!(completed.block().layer(far).is_none());
            assert_eq!(
                completed
                    .packet_block()
                    .layer(far)
                    .unwrap()
                    .get(0, 0, 0)
                    .unwrap(),
                6
            );
        }
    }
    let fixture = Fixture::new(
        "full",
        true,
        false,
        vec![row(0, Some("test:lamp"), None, None)],
    )
    .await;
    let completed = complete(
        LightingWork::new_restore(fixture.source(), limits(false)).unwrap(),
        1,
    );
    assert_eq!(
        completed.block().get_level(CENTER),
        0,
        "missing arrays do not force relight"
    );
}

#[tokio::test]
async fn original_duplicate_rows_preserve_last_present_per_kind_and_allocated_zero() {
    let fixture = Fixture::new(
        "full",
        true,
        true,
        vec![
            row(0, Some("minecraft:bedrock"), Some(0), None),
            row(1, None, None, Some(0)),
            row(120, None, Some(0x33), Some(0x22)),
            row(120, None, None, None),
            row(120, None, Some(0x77), None),
        ],
    )
    .await;
    let source = fixture.source();
    assert_eq!(source.saved_light(CHUNK).unwrap().rows.len(), 5);
    let result = complete(LightingWork::new_restore(source, limits(true)).unwrap(), 1);
    let far = LightSection { x: 0, y: 120, z: 0 };
    assert!(result.block().layer(far).is_none());
    assert!(result.sky().unwrap().layer(far).is_none());
    assert_eq!(
        result
            .packet_block()
            .layer(far)
            .unwrap()
            .get(0, 0, 0)
            .unwrap(),
        7
    );
    assert_eq!(
        result
            .packet_sky()
            .unwrap()
            .layer(far)
            .unwrap()
            .get(0, 0, 0)
            .unwrap(),
        2
    );
    let padding = LightSection { x: 0, y: 1, z: 0 };
    let saved = result.packet_sky().unwrap().layer(padding).unwrap();
    assert!(!saved.is_empty());
    assert!(!saved.is_definitely_homogeneous());
    assert_eq!(saved.get(0, 0, 0).unwrap(), 0);
    assert_eq!(
        result
            .sky()
            .unwrap()
            .get_level(LightBlock { x: 0, y: 16, z: 0 }),
        0
    );
}

#[tokio::test]
async fn no_sky_dimension_skips_sky_only_staging_and_owned_input_requires_saved_metadata() {
    let fixture = Fixture::new("full", true, false, vec![row(120, None, None, Some(0x77))]).await;
    let result = complete(
        LightingWork::new_restore(fixture.source(), limits(false)).unwrap(),
        1,
    );
    assert_eq!(result.packet_block().sections().count(), 0);
    assert!(result.packet_sky().is_none());
    let source = fixture::from_placements(
        fixture::synthetic_registry(),
        DimensionHeight::new(0, 16).unwrap(),
        &[CHUNK],
        &[],
    );
    assert!(matches!(
        LightingWork::new_restore(source, limits(false)),
        Err(LightingError::Source(LightError::MissingStoredLight))
    ));
}

#[tokio::test]
async fn a_failed_second_saved_layer_keeps_the_first_private_and_the_exact_row_cursor() {
    let fixture = Fixture::new(
        "full",
        true,
        false,
        vec![
            row(120, None, Some(0x22), None),
            row(121, None, Some(0x44), None),
        ],
    )
    .await;
    let mut small = limits(false);
    small.block_storage.layer_bytes = LAYER_RESERVATION_BYTES;
    let mut work = LightingWork::new_restore(fixture.source(), small).unwrap();
    assert!(matches!(
        work.step(64),
        Err(LightingError::Storage(StorageError::Budget))
    ));
    let held = work.retained_bytes();
    assert!(held >= LAYER_RESERVATION_BYTES);
    work = work
        .into_completed()
        .err()
        .expect("failed staging is not complete");
    assert!(matches!(
        work.step(64),
        Err(LightingError::Storage(StorageError::Budget))
    ));
    assert_eq!(work.retained_bytes(), held);
    drop(work);
    let complete = complete(
        LightingWork::new_restore(fixture.source(), limits(false)).unwrap(),
        1,
    );
    for (y, value) in [(120, 2), (121, 4)] {
        assert_eq!(
            complete
                .packet_block()
                .layer(LightSection { x: 0, y, z: 0 })
                .unwrap()
                .get(0, 0, 0)
                .unwrap(),
            value
        );
    }
}

#[tokio::test]
async fn packet_snapshot_metadata_failure_never_exposes_partial_completion() {
    let fixture = Fixture::new("full", true, true, vec![]).await;
    let mut small = limits(true);
    let minimal = LightLimits {
        max_sections: 0,
        max_columns: 1,
        max_notifications: 0,
        metadata_bytes: 1 << 20,
        layer_bytes: 0,
    };
    let probe = LightSectionStorage::new(LightKind::Sky, minimal).unwrap();
    let initial = probe.stats().metadata_bytes;
    let data = probe.data_snapshot().unwrap();
    assert!(probe.stats().metadata_bytes > initial);
    drop(data);
    drop(probe);
    small.sky.as_mut().unwrap().storage = LightLimits {
        metadata_bytes: initial,
        ..minimal
    };
    let mut work = LightingWork::new_restore(fixture.source(), small).unwrap();
    assert!(matches!(
        work.step(64),
        Err(LightingError::Storage(StorageError::MetadataLimit))
    ));
    let held = work.retained_bytes();
    work = work
        .into_completed()
        .err()
        .expect("block packet snapshot is private until sky capture succeeds");
    assert!(matches!(
        work.step(1),
        Err(LightingError::Storage(StorageError::MetadataLimit))
    ));
    assert_eq!(work.retained_bytes(), held);
    drop(work);
    let result = complete(
        LightingWork::new_restore(fixture.source(), limits(true)).unwrap(),
        1,
    );
    assert_eq!(result.packet_block().sections().count(), 0);
    assert_eq!(result.packet_sky().unwrap().sections().count(), 0);
}

#[tokio::test]
async fn capture_failure_keeps_cpu_lease_and_source_after_canonical_unload_until_drop() {
    let mut fixture = Fixture::new("full", true, true, vec![]).await;
    let mut small = limits(true);
    let minimal = LightLimits {
        max_sections: 0,
        max_columns: 1,
        max_notifications: 0,
        metadata_bytes: 1 << 20,
        layer_bytes: 0,
    };
    let probe = LightSectionStorage::new(LightKind::Sky, minimal).unwrap();
    small.sky.as_mut().unwrap().storage = LightLimits {
        metadata_bytes: probe.stats().metadata_bytes,
        ..minimal
    };
    drop(probe);
    let source_limits = SourceLimits {
        max_chunks: 1,
        metadata_bytes: 65536,
        owned_section_bytes: 0,
    };
    let reserved = small.reservation_bytes().unwrap() + source_limits.metadata_bytes;
    let cpu = CpuPool::new(CpuPoolConfig {
        workers: 1,
        max_jobs: 1,
        buffer_bytes: reserved,
    })
    .unwrap();
    let completion = cpu
        .try_reserve_canonical_lighting_restore(&fixture.owner, &[CHUNK], source_limits, small)
        .unwrap()
        .submit(64)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(completion.progress().is_err());
    assert_eq!(completion.light_level(LightKind::Block, CENTER), None);
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    let pending = completion
        .into_pending()
        .unwrap_or_else(|_| panic!("capture failure retains work"));
    assert!(fixture.owner.remove_demand(CHUNK));
    assert_eq!(
        fixture.owner.stats().residents,
        1,
        "private source still holds canonical allocation"
    );
    let completion = pending.submit(1).unwrap().wait().await.unwrap();
    assert!(completion.progress().is_err());
    assert_eq!(cpu.stats().reserved_buffer_bytes, reserved);
    drop(completion);
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    assert_eq!(fixture.owner.stats().residents, 0);
    cpu.shutdown().unwrap();
}

#[tokio::test]
async fn mixed_two_chunk_reuse_and_relight_decisions_survive_single_unit_slices() {
    let addresses = [CHUNK, ChunkAddress { x: 1, z: 0 }];
    for flags in [[true, false], [false, true]] {
        let chunks = flags
            .into_iter()
            .map(|flag| {
                (
                    "full",
                    flag,
                    vec![
                        row(
                            0,
                            Some(if flag {
                                "minecraft:bedrock"
                            } else {
                                "test:lamp"
                            }),
                            Some(0x55),
                            Some(0),
                        ),
                        row(120, None, Some(if flag { 0x22 } else { 0x66 }), None),
                    ],
                )
            })
            .collect();
        let fixture = Fixture::with_chunks(true, chunks).await;
        let mut configured = limits(true);
        configured.max_chunks = 2;
        configured.metadata_bytes = 16;
        configured.sky.as_mut().unwrap().engine.source_chunks = 2;
        let make = || {
            LightingSource::from_canonical(&fixture.owner, &addresses, SourceLimits::default())
                .unwrap()
        };
        let sliced = complete(LightingWork::new_restore(make(), configured).unwrap(), 1);
        let batch = complete(
            LightingWork::new_restore(make(), configured).unwrap(),
            usize::MAX,
        );
        for (x, flag) in flags.into_iter().enumerate() {
            let pos = LightBlock {
                x: x as i32 * 16 + 8,
                y: 8,
                z: 8,
            };
            assert_eq!(sliced.block().get_level(pos), if flag { 5 } else { 15 });
            let far = LightSection {
                x: x as i32,
                y: 120,
                z: 0,
            };
            assert!(sliced.block().layer(far).is_none());
            assert_eq!(
                sliced
                    .packet_block()
                    .layer(far)
                    .unwrap()
                    .get(0, 0, 0)
                    .unwrap(),
                if flag { 2 } else { 6 }
            );
        }
        for (a, b) in [
            (sliced.block(), batch.block()),
            (sliced.sky().unwrap(), batch.sky().unwrap()),
        ] {
            assert_eq!(
                a.sections().collect::<Vec<_>>(),
                b.sections().collect::<Vec<_>>()
            );
            for key in a.sections() {
                let a = a.layer(key).unwrap();
                let b = b.layer(key).unwrap();
                assert_eq!(a.is_empty(), b.is_empty());
                assert_eq!(a.is_definitely_homogeneous(), b.is_definitely_homogeneous());
                assert_eq!(a.bytes(), b.bytes());
                assert_eq!(a.get(0, 0, 0).unwrap(), b.get(0, 0, 0).unwrap());
            }
        }
        for (a, b) in [
            (sliced.packet_block(), batch.packet_block()),
            (sliced.packet_sky().unwrap(), batch.packet_sky().unwrap()),
        ] {
            assert_eq!(
                a.sections().collect::<Vec<_>>(),
                b.sections().collect::<Vec<_>>()
            );
            for key in a.sections() {
                let a = a.layer(key).unwrap();
                let b = b.layer(key).unwrap();
                assert_eq!(a.is_empty(), b.is_empty());
                assert_eq!(a.is_definitely_homogeneous(), b.is_definitely_homogeneous());
                assert_eq!(a.bytes(), b.bytes());
                assert_eq!(a.get(0, 0, 0).unwrap(), b.get(0, 0, 0).unwrap());
            }
        }
    }
}
