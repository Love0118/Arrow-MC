//! Owner publication is exercised with real Anvil reads and the shared CPU pool.
#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{ChunkReadKey, CpuPool, CpuPoolConfig},
    world::{
        loading::{
            ChunkLoadingOwner, LoadDemand, LoadingError, LoadingLimits, LoadingReadCompletion,
            LoadingReadOutcome, LoadingReadRequest, MAX_REQUEST_CHUNK_COORDINATE,
        },
        preparation::ChunkAddress,
        section::Section,
        storage::{
            ChunkStore, StorageLimits,
            chunk::{DATA_VERSION, DimensionHeight},
            registry::ChunkRegistrySnapshot,
        },
    },
};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::time::timeout;

const METADATA_BYTES: usize = 1024 * 1024;
const RESIDENT_BYTES: usize = 8 * 1024 * 1024;

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut result = Compound::new();
    for (name, value) in entries {
        result.insert(name.into(), value).unwrap();
    }
    Tag::Compound(result)
}

fn section(y: i8, lamp: bool, block_light: Option<i8>, sky_light: Option<i8>) -> Tag {
    let block = if lamp {
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
    } else {
        compound([("id", Tag::String("minecraft:air".into()))])
    };
    let mut fields = vec![
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
    ];
    if let Some(value) = block_light {
        fields.push(("BlockLight", Tag::ByteArray(vec![value; 2048])));
    }
    if let Some(value) = sky_light {
        fields.push(("SkyLight", Tag::ByteArray(vec![value; 2048])));
    }
    compound(fields)
}

fn disk_chunk(stored: (i32, i32), sections: Vec<Tag>, marker: i64) -> Vec<u8> {
    let root = NamedTag {
        name: "loading owner synthetic fixture".into(),
        tag: compound([
            ("DataVersion", Tag::Int(DATA_VERSION)),
            ("xPos", Tag::Int(stored.0)),
            ("zPos", Tag::Int(stored.1)),
            ("Status", Tag::String("minecraft:full".into())),
            ("isLightOn", Tag::Byte(1)),
            ("sections", Tag::List(sections)),
            ("test:owner-marker", Tag::Long(marker)),
        ]),
    };
    let mut output = Vec::new();
    nbt::write_named(&root, &mut output, nbt::Limits::default()).unwrap();
    output
}

fn write_region(directory: &Path, records: &[(i32, Vec<u8>)]) {
    fs::create_dir_all(directory).unwrap();
    let mut bytes = vec![0u8; 8192];
    for (x, record) in records {
        assert!((0..32).contains(x));
        let sector = bytes.len() / 4096;
        let count = (record.len() + 5).div_ceil(4096);
        assert!(count < 256);
        let slot = *x as usize * 4;
        let location = ((sector as u32) << 8) | count as u32;
        bytes[slot..slot + 4].copy_from_slice(&location.to_be_bytes());
        bytes.extend_from_slice(&((record.len() + 1) as i32).to_be_bytes());
        bytes.push(3);
        bytes.extend_from_slice(record);
        bytes.resize((sector + count) * 4096, 0);
    }
    fs::write(directory.join("r.0.0.mca"), bytes).unwrap();
}

fn address(x: i32) -> ChunkAddress {
    ChunkAddress { x, z: 0 }
}

fn height() -> DimensionHeight {
    DimensionHeight::new(-64, 384).unwrap()
}

fn cpu() -> Arc<CpuPool> {
    Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 4,
            buffer_bytes: 128 * 1024 * 1024,
        })
        .unwrap(),
    )
}

fn owner(registries: &Arc<ChunkRegistrySnapshot>) -> ChunkLoadingOwner {
    ChunkLoadingOwner::new(
        17,
        Arc::clone(registries),
        height(),
        true,
        LoadingLimits {
            max_chunks: 4,
            metadata_bytes: METADATA_BYTES,
        },
        RESIDENT_BYTES,
    )
    .unwrap()
}

fn store(
    directory: &Path,
    cpu: &Arc<CpuPool>,
    registries: &Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
) -> ChunkStore {
    ChunkStore::new(
        directory.to_owned(),
        Arc::clone(cpu),
        Arc::clone(registries),
        height,
        StorageLimits::default(),
        2,
    )
    .unwrap()
}

fn request(
    owner: &mut ChunkLoadingOwner,
    address: ChunkAddress,
) -> (LoadingReadRequest, ChunkReadKey) {
    let LoadDemand::Read(request) = owner.request(address).unwrap() else {
        panic!("expected new disk demand")
    };
    let key = request.key();
    (request, key)
}

async fn read(store: &ChunkStore, request: &LoadingReadRequest) -> LoadingReadCompletion {
    let result = timeout(Duration::from_secs(5), request.read(store))
        .await
        .unwrap()
        .unwrap();
    let LoadingReadOutcome::Decoded(output) = result else {
        panic!("fixture was not decoded")
    };
    output
}

#[tokio::test(flavor = "current_thread")]
async fn demand_deduplicates_and_removal_or_empty_finish_requires_a_new_generation() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(0, disk_chunk((0, 0), vec![section(0, true, None, None)], 1))],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut owner = owner(&registries);
    assert!(owner.section(address(0), 0).is_none());
    let (old_request, old) = request(&mut owner, address(0));
    assert!(matches!(owner.request(address(0)).unwrap(), LoadDemand::Pending(key) if key == old));
    assert!(owner.section(address(0), 0).is_none());
    let delayed = read(&store, &old_request).await;
    assert!(owner.remove_demand(address(0)));
    assert!(!owner.remove_demand(address(0)));
    let (_, current) = request(&mut owner, address(0));
    assert_ne!(current.generation, old.generation);
    let stale = owner.publish(delayed).unwrap_err();
    assert_eq!(stale.kind(), LoadingError::StaleRequest);
    let stale = stale.into_output();
    assert_eq!(stale.key(), old);
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(
        owner.finish_without_chunk(old),
        Err(LoadingError::StaleRequest)
    );
    assert!(
        matches!(owner.request(address(0)).unwrap(), LoadDemand::Pending(key) if key == current)
    );
    drop(stale);
    assert_eq!(cpu.stats().in_flight, 0);
    owner.finish_without_chunk(current).unwrap();
    let (retry_request, retry) = request(&mut owner, address(0));
    assert_ne!(retry.generation, current.generation);
    let output = read(&store, &retry_request).await;
    owner.publish(output).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert!(
        matches!(owner.request(address(0)).unwrap(), LoadDemand::Resident(key) if key == retry)
    );
    assert_eq!(
        owner.finish_without_chunk(retry),
        Err(LoadingError::DuplicateCompletion)
    );
    assert_eq!(
        owner.section(address(0), 0).unwrap().blocks.get(0).unwrap(),
        2
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_publication_and_old_reload_results_cannot_replace_resident_sections() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(0, disk_chunk((0, 0), vec![section(0, true, None, None)], 1))],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut owner = owner(&registries);
    let (key_request, key) = request(&mut owner, address(0));
    let first = read(&store, &key_request).await;
    let duplicate = read(&store, &key_request).await;
    let delayed = read(&store, &key_request).await;
    owner.publish(first).unwrap();
    let duplicate = owner.publish(duplicate).unwrap_err();
    assert_eq!(duplicate.kind(), LoadingError::DuplicateCompletion);
    let duplicate = duplicate.into_output();
    drop(duplicate);
    assert_eq!(
        owner
            .section(address(0), 0)
            .unwrap()
            .blocks
            .get(4095)
            .unwrap(),
        2
    );
    let epoch = owner
        .reload(Arc::clone(&registries), height(), true)
        .unwrap();
    assert!(epoch > key.world_epoch);
    assert!(owner.section(address(0), 0).is_none());
    let (current_request, current) = request(&mut owner, address(0));
    assert_eq!(current.world_epoch, epoch);
    let delayed = owner.publish(delayed).unwrap_err();
    assert_eq!(delayed.kind(), LoadingError::StaleRequest);
    drop(delayed.into_output());
    assert!(owner.section(address(0), 0).is_none());
    assert!(
        matches!(owner.request(address(0)).unwrap(), LoadDemand::Pending(key) if key == current)
    );
    owner.publish(read(&store, &current_request).await).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn context_rejects_changed_registry_configuration_and_decode_height_before_adoption() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(0, disk_chunk((0, 0), vec![section(0, true, None, None)], 1))],
    );
    let cpu = cpu();
    let mut owner = owner(&registries);
    let (key_request, key) = request(&mut owner, address(0));

    let mut altered_fixture = registry_fixture::Fixture::new();
    altered_fixture.edit("blocks.json", |value| {
        value["state_flags"][2] = serde_json::json!(0)
    });
    let altered = Arc::new(altered_fixture.load());
    let altered_store = store(&directory, &cpu, &altered, height());
    let output = read(&altered_store, &key_request).await;
    let error = owner.publish(output).unwrap_err();
    assert_eq!(error.kind(), LoadingError::ContextMismatch);
    drop(error.into_output());
    assert_eq!(cpu.stats().in_flight, 0);
    assert!(owner.section(address(0), 0).is_none());

    let mut configuration_fixture = registry_fixture::Fixture::new();
    let new_configuration = [0x66; 32];
    configuration_fixture.edit("manifest.json", |value| {
        value["configuration_manifest_sha256"] =
            serde_json::json!(registry_fixture::hex(&new_configuration));
    });
    configuration_fixture.expected.configuration_manifest_sha256 = new_configuration;
    let configuration = Arc::new(configuration_fixture.load());
    let configuration_store = store(&directory, &cpu, &configuration, height());
    let error = owner
        .publish(read(&configuration_store, &key_request).await)
        .unwrap_err();
    assert_eq!(error.kind(), LoadingError::ContextMismatch);
    drop(error.into_output());
    assert_eq!(cpu.stats().in_flight, 0);

    let wrong_height = store(
        &directory,
        &cpu,
        &registries,
        DimensionHeight::new(0, 256).unwrap(),
    );
    let error = owner
        .publish(read(&wrong_height, &key_request).await)
        .unwrap_err();
    assert_eq!(error.kind(), LoadingError::ContextMismatch);
    drop(error.into_output());
    assert_eq!(cpu.stats().in_flight, 0);
    assert!(
        matches!(owner.request(address(0)).unwrap(), LoadDemand::Pending(value) if value == key)
    );

    // Equal authenticated content in another Arc is the same context.
    let independently_loaded = Arc::new(fixture.load());
    assert!(!Arc::ptr_eq(&registries, &independently_loaded));
    let valid = store(&directory, &cpu, &independently_loaded, height());
    owner.publish(read(&valid, &key_request).await).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(
        owner.section(address(0), 0).unwrap().counts.fluid_blocks,
        4096
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stored_coordinate_mismatch_reports_relocation_and_uses_requested_address() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(
            0,
            disk_chunk((-77, 88), vec![section(0, true, None, None)], 19),
        )],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut owner = owner(&registries);
    let (key_request, key) = request(&mut owner, address(0));
    let report = owner.publish(read(&store, &key_request).await).unwrap();
    assert_eq!(report.key, key);
    let relocated = report.relocated.unwrap();
    assert_eq!(relocated.stored, (-77, 88));
    assert_eq!(relocated.requested, address(0));
    assert_eq!(owner.stored_position(address(0)), Some((-77, 88)));
    assert_eq!(
        owner.section(address(0), 0).unwrap().blocks.get(0).unwrap(),
        2
    );
    assert!(owner.section(ChunkAddress { x: -77, z: 88 }, 0).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_sections_select_last_terrain_and_last_present_light_independently() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    let sections = vec![
        section(0, true, Some(0x11), Some(0x22)),
        section(0, true, Some(0x33), None),
        section(0, false, None, Some(0x44)),
        section(-5, true, Some(0x55), Some(0x66)),
        section(20, true, Some(0x77), Some(0x12)),
        section(-6, true, Some(0x13), Some(0x14)),
        section(21, true, Some(0x15), Some(0x16)),
        section(i8::MIN, true, Some(0x17), Some(0x18)),
        section(i8::MAX, true, Some(0x19), Some(0x1a)),
    ];
    write_region(&directory, &[(0, disk_chunk((0, 0), sections, 20))]);
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut owner = owner(&registries);
    let (key_request, _) = request(&mut owner, address(0));
    owner.publish(read(&store, &key_request).await).unwrap();
    let section = owner.section(address(0), 0).unwrap();
    assert_eq!(section.blocks.get(0).unwrap(), 0);
    assert_eq!(section.biomes.get(0).unwrap(), 1);
    assert_eq!(section.counts.non_empty_blocks, 0);
    assert_eq!(owner.block_light(address(0), 0).unwrap(), &[0x33; 2048]);
    assert_eq!(owner.sky_light(address(0), 0).unwrap(), &[0x44; 2048]);
    for (y, block, sky) in [
        (-5, 0x55, 0x66),
        (20, 0x77, 0x12),
        (-6, 0x13, 0x14),
        (21, 0x15, 0x16),
        (i32::from(i8::MIN), 0x17, 0x18),
        (i32::from(i8::MAX), 0x19, 0x1a),
    ] {
        assert!(owner.section(address(0), y).is_none());
        assert_eq!(owner.block_light(address(0), y).unwrap(), &[block; 2048]);
        assert_eq!(owner.sky_light(address(0), y).unwrap(), &[sky; 2048]);
    }
    for y in [i32::MIN, i32::MAX] {
        assert!(owner.section(address(0), y).is_none());
        assert!(owner.block_light(address(0), y).is_none());
        assert!(owner.sky_light(address(0), y).is_none());
    }
    owner
        .reload(Arc::clone(&registries), height(), false)
        .unwrap();
    let (key_request, _) = request(&mut owner, address(0));
    owner.publish(read(&store, &key_request).await).unwrap();
    for y in [-6, -5, 0, 20, 21] {
        assert!(owner.block_light(address(0), y).is_some());
        assert!(owner.sky_light(address(0), y).is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn omitted_sections_are_real_air_plains_after_publication_and_prepare_on_shared_cpu() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(0, disk_chunk((0, 0), vec![section(0, true, None, None)], 2))],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut owner = owner(&registries);
    assert!(owner.section(address(0), -4).is_none());
    let (key_request, key) = request(&mut owner, address(0));
    assert!(owner.prepare_section(address(0), 0, &cpu).is_err());
    owner.publish(read(&store, &key_request).await).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    for y in [-4, -1, 1, 19] {
        let empty = owner.section(address(0), y).unwrap();
        assert_eq!(empty.blocks.get(4095).unwrap(), 0);
        assert_eq!(empty.biomes.get(63).unwrap(), 0);
        assert_eq!(empty.counts.non_empty_blocks, 0);
        assert_eq!(empty.counts.fluid_blocks, 0);
    }
    for y in [-4, 0, 19] {
        let task = owner.prepare_section(address(0), y, &cpu).unwrap();
        let completion = task.wait().unwrap();
        assert_eq!(completion.key().world_epoch, key.world_epoch);
        assert_eq!(completion.key().section_y, y);
        let prepared = owner.accept_prepared(completion).unwrap();
        let mut bytes = prepared.bytes();
        let decoded = Section::read_network(
            &mut bytes,
            registries.block_registry(),
            registries.biome_registry(),
            65536,
        )
        .unwrap();
        assert!(bytes.is_empty());
        let expected = owner.section(address(0), y).unwrap();
        assert_eq!(decoded.counts, expected.counts);
        assert_eq!(
            decoded.blocks.get(4095).unwrap(),
            expected.blocks.get(4095).unwrap()
        );
        assert_eq!(
            decoded.biomes.get(63).unwrap(),
            expected.biomes.get(63).unwrap()
        );
        drop(prepared);
        assert_eq!(cpu.stats().in_flight, 0);
    }
    let stale = owner
        .prepare_section(address(0), 0, &cpu)
        .unwrap()
        .wait()
        .unwrap();
    assert!(owner.remove_demand(address(0)));
    let (_, next) = request(&mut owner, address(0));
    assert_ne!(next.generation, key.generation);
    assert!(owner.accept_prepared(stale).is_err());
    assert_eq!(cpu.stats().in_flight, 0);
    assert!(owner.section(address(0), 0).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn resident_rejection_retains_cpu_output_and_retry_succeeds_after_owner_eviction() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[
            (
                0,
                disk_chunk((0, 0), vec![section(0, true, Some(0x12), None)], 0),
            ),
            (
                1,
                disk_chunk((1, 0), vec![section(0, true, Some(0x12), None)], 1),
            ),
        ],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut probe = owner(&registries);
    let (first_key_request, first_key) = request(&mut probe, address(0));
    let first = read(&store, &first_key_request).await;
    let capacity = first.retained_bytes();
    drop(probe);
    let mut owner = ChunkLoadingOwner::new(
        17,
        Arc::clone(&registries),
        height(),
        true,
        LoadingLimits {
            max_chunks: 2,
            metadata_bytes: METADATA_BYTES,
        },
        capacity,
    )
    .unwrap();
    drop(first);
    let (fresh_request, fresh_key) = request(&mut owner, address(0));
    assert_eq!(fresh_key, first_key);
    owner.publish(read(&store, &fresh_request).await).unwrap();
    let (second_key_request, second_key) = request(&mut owner, address(1));
    let second = read(&store, &second_key_request).await;
    assert_eq!(second.retained_bytes(), capacity);
    let charge = cpu.stats().reserved_buffer_bytes;
    let second = owner.publish(second).unwrap_err();
    assert_eq!(second.kind(), LoadingError::ResidentLimit);
    let second = second.into_output();
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(cpu.stats().reserved_buffer_bytes, charge);
    assert!(owner.section(address(0), 0).is_some());
    assert!(owner.section(address(1), 0).is_none());
    assert!(
        matches!(owner.request(address(1)).unwrap(), LoadDemand::Pending(key) if key == second_key)
    );
    assert!(owner.remove_demand(address(0)));
    owner.publish(second).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert!(owner.section(address(0), 0).is_none());
    assert!(owner.section(address(1), 0).is_some());
}

#[test]
fn chunk_and_metadata_admission_reject_before_changing_existing_demand() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    assert!(
        ChunkLoadingOwner::new(
            17,
            Arc::clone(&registries),
            height(),
            true,
            LoadingLimits {
                max_chunks: 4,
                metadata_bytes: 0
            },
            RESIDENT_BYTES
        )
        .is_err()
    );
    let mut owner = ChunkLoadingOwner::new(
        17,
        Arc::clone(&registries),
        height(),
        true,
        LoadingLimits {
            max_chunks: 1,
            metadata_bytes: METADATA_BYTES,
        },
        RESIDENT_BYTES,
    )
    .unwrap();
    let (_, first) = request(&mut owner, address(0));
    assert!(owner.request(address(1)).is_err());
    assert!(matches!(owner.request(address(0)).unwrap(), LoadDemand::Pending(key) if key == first));
    assert!(owner.remove_demand(address(0)));
    let (_, next) = request(&mut owner, address(1));
    assert!(next.generation > first.generation);
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_publication_failure_keeps_existing_resident_and_pending_output_intact() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[
            (
                0,
                disk_chunk((0, 0), vec![section(0, true, Some(0x12), None)], 0),
            ),
            (
                1,
                disk_chunk((1, 0), vec![section(0, true, Some(0x12), None)], 1),
            ),
        ],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut probe = ChunkLoadingOwner::new(
        17,
        Arc::clone(&registries),
        height(),
        true,
        LoadingLimits {
            max_chunks: 2,
            metadata_bytes: METADATA_BYTES,
        },
        RESIDENT_BYTES,
    )
    .unwrap();
    let (probe_key_request, _) = request(&mut probe, address(0));
    probe
        .publish(read(&store, &probe_key_request).await)
        .unwrap();
    let one_chunk_metadata = probe.stats().metadata_bytes;
    drop(probe);
    let mut owner = ChunkLoadingOwner::new(
        17,
        Arc::clone(&registries),
        height(),
        true,
        LoadingLimits {
            max_chunks: 2,
            metadata_bytes: one_chunk_metadata,
        },
        RESIDENT_BYTES,
    )
    .unwrap();
    let (first_request, _) = request(&mut owner, address(0));
    owner.publish(read(&store, &first_request).await).unwrap();
    let (second_request, _) = request(&mut owner, address(1));
    let before = owner.stats();
    let output = read(&store, &second_request).await;
    let cpu_bytes = cpu.stats().reserved_buffer_bytes;
    let error = owner.publish(output).unwrap_err();
    assert_eq!(error.kind(), LoadingError::MetadataLimit);
    let output = error.into_output();
    assert_eq!(owner.stats(), before);
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(cpu.stats().reserved_buffer_bytes, cpu_bytes);
    assert!(owner.section(address(0), 0).is_some());
    assert!(owner.section(address(1), 0).is_none());
    assert!(owner.remove_demand(address(0)));
    owner.publish(output).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(owner.stats().metadata_bytes, one_chunk_metadata);
    assert_eq!(owner.stats().residents, 1);
    assert_eq!(owner.stats().pending, 0);
}

#[test]
fn coordinate_boundaries_reject_without_allocating_or_consuming_generations() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let mut owner = owner(&registries);
    let before = owner.stats();
    for coordinate in [
        i32::MIN,
        i32::MAX,
        MAX_REQUEST_CHUNK_COORDINATE + 1,
        -MAX_REQUEST_CHUNK_COORDINATE - 1,
    ] {
        for address in [
            ChunkAddress {
                x: coordinate,
                z: 0,
            },
            ChunkAddress {
                x: 0,
                z: coordinate,
            },
        ] {
            assert!(matches!(
                owner.request(address),
                Err(LoadingError::InvalidCoordinate)
            ));
            assert_eq!(owner.stats(), before);
        }
    }
    let (_, first) = request(
        &mut owner,
        ChunkAddress {
            x: -MAX_REQUEST_CHUNK_COORDINATE,
            z: MAX_REQUEST_CHUNK_COORDINATE,
        },
    );
    assert_eq!(first.generation, 1);
    let (_, second) = request(
        &mut owner,
        ChunkAddress {
            x: MAX_REQUEST_CHUNK_COORDINATE,
            z: -MAX_REQUEST_CHUNK_COORDINATE,
        },
    );
    assert_eq!(second.generation, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_bytes_from_another_owner_are_rejected_even_when_public_keys_match() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    write_region(
        &directory,
        &[(0, disk_chunk((0, 0), vec![section(0, true, None, None)], 1))],
    );
    let cpu = cpu();
    let store = store(&directory, &cpu, &registries, height());
    let mut first_owner = owner(&registries);
    let mut second_owner = owner(&registries);
    let (first_key_request, first_key) = request(&mut first_owner, address(0));
    let (second_key_request, second_key) = request(&mut second_owner, address(0));
    assert_eq!(first_key, second_key);
    first_owner
        .publish(read(&store, &first_key_request).await)
        .unwrap();
    second_owner
        .publish(read(&store, &second_key_request).await)
        .unwrap();
    let completion = first_owner
        .prepare_section(address(0), 0, &cpu)
        .unwrap()
        .wait()
        .unwrap();
    assert!(matches!(
        second_owner.accept_prepared(completion),
        Err(LoadingError::ForeignPreparation)
    ));
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(
        second_owner
            .section(address(0), 0)
            .unwrap()
            .blocks
            .get(0)
            .unwrap(),
        2
    );
    let completion = second_owner
        .prepare_section(address(0), 0, &cpu)
        .unwrap()
        .wait()
        .unwrap();
    let prepared = second_owner.accept_prepared(completion).unwrap();
    assert!(!prepared.bytes().is_empty());
    drop(prepared);
    assert_eq!(cpu.stats().in_flight, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn another_owners_disk_completion_cannot_cross_publish_even_with_identical_public_keys() {
    let fixture = registry_fixture::Fixture::new();
    let registries = Arc::new(fixture.load());
    let first_directory = fixture.root.join("first-region");
    let second_directory = fixture.root.join("second-region");
    write_region(
        &first_directory,
        &[(
            0,
            disk_chunk((0, 0), vec![section(0, true, None, None)], 101),
        )],
    );
    write_region(
        &second_directory,
        &[(
            0,
            disk_chunk((0, 0), vec![section(0, false, None, None)], 202),
        )],
    );
    let cpu = cpu();
    let first_store = store(&first_directory, &cpu, &registries, height());
    let second_store = store(&second_directory, &cpu, &registries, height());
    let mut first_owner = owner(&registries);
    let mut second_owner = owner(&registries);
    let (first_request, first_key) = request(&mut first_owner, address(0));
    let (second_request, second_key) = request(&mut second_owner, address(0));
    assert_eq!(first_key, second_key);
    let first_output = read(&first_store, &first_request).await;
    let before = second_owner.stats();
    let cpu_bytes = cpu.stats().reserved_buffer_bytes;
    let rejected = second_owner.publish(first_output).unwrap_err();
    assert_eq!(rejected.kind(), LoadingError::ForeignRead);
    assert_eq!(second_owner.stats(), before);
    assert!(second_owner.section(address(0), 0).is_none());
    assert_eq!(cpu.stats().in_flight, 1);
    assert_eq!(cpu.stats().reserved_buffer_bytes, cpu_bytes);
    let first_output = rejected.into_output();
    assert_eq!(first_output.key(), first_key);
    first_owner.publish(first_output).unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    assert_eq!(
        first_owner
            .section(address(0), 0)
            .unwrap()
            .blocks
            .get(0)
            .unwrap(),
        2
    );
    second_owner
        .publish(read(&second_store, &second_request).await)
        .unwrap();
    assert_eq!(
        second_owner
            .section(address(0), 0)
            .unwrap()
            .blocks
            .get(0)
            .unwrap(),
        0
    );
    assert_eq!(
        first_owner
            .section(address(0), 0)
            .unwrap()
            .blocks
            .get(0)
            .unwrap(),
        2
    );
    assert_eq!(cpu.stats().in_flight, 0);
}
