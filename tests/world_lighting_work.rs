#[path = "common/lighting_fixture.rs"]
mod fixture;
use fixture::registry_fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection, LightingSource,
        block::{BlockLightEngine, BlockLightError, BlockLightLimits},
        queue::QueueError,
        sky::{SkyError, SkyLightEngine, SkyLimits},
        storage::{LightSectionStorage, LightSnapshot, StorageLimits},
        work::{CompletedLighting, LightingError, LightingLimits, LightingWork, SkyWorkLimits},
    },
    preparation::ChunkAddress,
    storage::{chunk::DimensionHeight, registry::ChunkRegistrySnapshot},
};
use serde_json::json;
use std::sync::Arc;

const CHUNKS: [ChunkAddress; 2] = [ChunkAddress { x: 0, z: 0 }, ChunkAddress { x: 1, z: 0 }];
const EMITTER: LightBlock = LightBlock { x: 15, y: 8, z: 8 };

fn registry() -> Arc<ChunkRegistrySnapshot> {
    let mut data = registry_fixture::Fixture::from_data(
        json!({
            "state_count":4,"state_flags":[1,0,0,2],"blocks":[
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
                {"id":"test:lamp","default_state":2,"properties":[],"states":[2]},
                {"id":"test:water","default_state":3,"properties":[],"states":[3]}
            ]
        }),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    );
    let mut materials = [[0; 16]; 4];
    materials[1][1] = 15;
    materials[2][0] = 15;
    materials[2][1] = 15;
    materials[3][1] = 1;
    data.edit_lighting(|bytes| *bytes = registry_fixture::lighting_bytes(&materials, 2, &[14]));
    Arc::new(data.load())
}

fn source(registry: &Arc<ChunkRegistrySnapshot>, empty: bool) -> LightingSource {
    let mut placements = Vec::new();
    if !empty {
        for x in 0..32 {
            for z in 0..16 {
                placements.push((LightBlock { x, y: 0, z }, 1));
                if (16..24).contains(&x) {
                    placements.push((LightBlock { x, y: 15, z }, 1));
                }
            }
        }
        placements.push((EMITTER, 2));
        placements.push((LightBlock { x: 4, y: 20, z: 4 }, 3));
    }
    fixture::from_placements(
        Arc::clone(registry),
        DimensionHeight::new(0, 32).unwrap(),
        &CHUNKS,
        &placements,
    )
}

fn storage_limits() -> StorageLimits {
    StorageLimits {
        max_sections: 128,
        max_columns: 64,
        max_notifications: 512,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    }
}
fn limits(sky: bool) -> LightingLimits {
    LightingLimits {
        max_chunks: 2,
        metadata_bytes: 2 * size_of::<ChunkAddress>(),
        block: BlockLightLimits {
            checks: 16,
            decreases: 32768,
            increases: 32768,
            queue_bytes: 2 << 20,
        },
        block_storage: storage_limits(),
        sky: sky.then_some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 16,
                queue_entries: 32768,
                source_chunks: 2,
                planned_writes: 256,
            },
            storage: storage_limits(),
            engine_bytes: 4 << 20,
        }),
    }
}

fn complete(mut work: LightingWork, budget: usize) -> (CompletedLighting, usize) {
    let mut calls = 0;
    for _ in 0..100_000 {
        let progress = work.step(budget).unwrap();
        calls += 1;
        assert!(progress.processed <= budget);
        assert!(work.retained_bytes() <= work.reservation_bytes());
        if progress.complete {
            return (
                work.into_completed()
                    .unwrap_or_else(|_| panic!("complete flag without complete owner")),
                calls,
            );
        }
        work = work
            .into_completed()
            .err()
            .expect("partial work exposed completion");
    }
    panic!("admitted finite world failed to converge");
}

fn assert_layers(a: &LightSnapshot, b: &LightSnapshot) {
    assert_eq!(a.kind(), b.kind());
    let keys: Vec<_> = a.sections().collect();
    assert_eq!(keys, b.sections().collect::<Vec<_>>());
    for key in keys {
        let left = a.layer(key).unwrap();
        let right = b.layer(key).unwrap();
        assert_eq!(left.is_empty(), right.is_empty(), "{key:?}");
        assert_eq!(
            left.is_definitely_homogeneous(),
            right.is_definitely_homogeneous(),
            "{key:?}"
        );
        for index in 0..4096 {
            let x = (index & 15) as u8;
            let y = (index >> 8) as u8;
            let z = ((index >> 4) & 15) as u8;
            assert_eq!(left.get(x, y, z), right.get(x, y, z), "{key:?}/{index}");
        }
    }
}

/// Independent composition of the existing oracle-verified public kernel APIs.
/// This test is integration evidence, not another Java differential corpus.
fn manual(
    source: &LightingSource,
    limits: LightingLimits,
) -> (LightSnapshot, Option<LightSnapshot>) {
    let mut block = BlockLightEngine::new(limits.block).unwrap();
    let mut storage = LightSectionStorage::new(LightKind::Block, limits.block_storage).unwrap();
    let mut sky = limits.sky.map(|limits| {
        let mut remaining = limits.engine_bytes;
        SkyLightEngine::new(
            LightSectionStorage::new(LightKind::Sky, limits.storage).unwrap(),
            limits.engine,
            &mut remaining,
        )
        .unwrap()
    });
    for chunk in source.chunk_addresses() {
        for y in i32::from(source.height().min_section())..=i32::from(source.height().max_section())
        {
            let section = LightSection {
                x: chunk.x,
                y,
                z: chunk.z,
            };
            if !source.section_has_only_air(section) {
                storage.update_section_status(section, false).unwrap();
                if let Some(sky) = &mut sky {
                    sky.storage_mut()
                        .unwrap()
                        .update_section_status(section, false)
                        .unwrap();
                }
            }
        }
    }
    if let Some(sky) = &mut sky {
        for chunk in source.chunk_addresses() {
            sky.initialize_sources(source, chunk).unwrap();
        }
        for chunk in source.chunk_addresses() {
            sky.set_light_enabled(chunk, true).unwrap();
            sky.propagate_light_sources(chunk).unwrap();
        }
    }
    for chunk in source.chunk_addresses() {
        block
            .propagate_light_sources(source, &mut storage, chunk)
            .unwrap();
    }
    assert!(
        block
            .run(source, &mut storage, usize::MAX)
            .unwrap()
            .complete
    );
    if let Some(sky) = &mut sky {
        sky.run_updates(source).unwrap();
    }
    (storage.snapshot(), sky.map(|sky| sky.storage().snapshot()))
}

#[test]
fn both_layers_match_explicit_multi_chunk_kernel_sequence_at_fine_and_coarse_steps() {
    let registry = registry();
    let input = source(&registry, false);
    let (expected_block, expected_sky) = manual(&input, limits(true));
    let (coarse, _) = complete(LightingWork::new(input, limits(true)).unwrap(), usize::MAX);
    let (fine, calls) = complete(
        LightingWork::new(source(&registry, false), limits(true)).unwrap(),
        7,
    );
    assert!(calls > 100);
    assert_layers(coarse.block(), &expected_block);
    assert_layers(coarse.sky().unwrap(), expected_sky.as_ref().unwrap());
    assert_layers(fine.block(), coarse.block());
    assert_layers(fine.sky().unwrap(), coarse.sky().unwrap());
    assert_eq!(fine.source().chunk_addresses().collect::<Vec<_>>(), CHUNKS);
    assert_eq!(fine.block().get_level(EMITTER), 15);
    assert_eq!(fine.block().get_level(LightBlock { x: 16, ..EMITTER }), 14);
    assert_eq!(
        fine.sky()
            .unwrap()
            .get_level(LightBlock { x: 8, y: 30, z: 8 }),
        15
    );
}

#[test]
fn dimension_without_sky_finishes_block_and_never_fabricates_a_sky_snapshot() {
    let registry = registry();
    let input = source(&registry, false);
    let (expected, _) = manual(&input, limits(false));
    let (done, _) = complete(LightingWork::new(input, limits(false)).unwrap(), 1);
    assert!(done.sky().is_none());
    assert_layers(done.block(), &expected);
    assert!(!limits(false).has_sky_light());
    assert!(limits(true).has_sky_light());
}

#[test]
fn empty_available_chunks_create_no_fake_non_air_support_sections() {
    let registry = registry();
    let (done, _) = complete(
        LightingWork::new(source(&registry, true), limits(true)).unwrap(),
        1,
    );
    assert_eq!(done.block().sections().count(), 0);
    assert_eq!(done.sky().unwrap().sections().count(), 0);
    assert_eq!(done.block().get_level(EMITTER), 0);
    assert_eq!(done.sky().unwrap().get_level(EMITTER), 15);
}

#[test]
fn zero_work_and_incomplete_conversion_preserve_the_original_owner() {
    let registry = registry();
    let mut work = LightingWork::new(source(&registry, false), limits(true)).unwrap();
    let retained = work.retained_bytes();
    let reserved = work.reservation_bytes();
    for _ in 0..3 {
        assert_eq!(work.step(0).unwrap().processed, 0);
        assert!(!work.step(0).unwrap().complete);
        assert_eq!(work.retained_bytes(), retained);
        assert_eq!(work.reservation_bytes(), reserved);
        work = work
            .into_completed()
            .err()
            .expect("construction cannot produce completion");
    }
    complete(work, 13);
}

#[test]
fn block_queue_pressure_after_sky_initialization_keeps_work_for_explicit_budgeted_growth() {
    let registry = registry();
    let mut small = limits(true);
    small.block.increases = 1;
    let mut work = LightingWork::new(source(&registry, false), small).unwrap();
    assert!(matches!(
        work.step(usize::MAX),
        Err(LightingError::Block(BlockLightError::Queue(
            QueueError::Full
        )))
    ));
    let retained = work.retained_bytes();
    let reserved = work.reservation_bytes();
    work = work
        .into_completed()
        .err()
        .expect("block incomplete cannot expose finished sky state");
    assert!(matches!(
        work.step(usize::MAX),
        Err(LightingError::Block(BlockLightError::Queue(
            QueueError::Full
        )))
    ));
    assert_eq!(work.retained_bytes(), retained);
    work.grow_block_queues(16, 32768, 32768).unwrap();
    assert_eq!(work.reservation_bytes(), reserved);
    let (resumed, _) = complete(work, 7);
    let (baseline, _) = complete(
        LightingWork::new(source(&registry, false), limits(true)).unwrap(),
        usize::MAX,
    );
    assert_layers(resumed.block(), baseline.block());
    assert_layers(resumed.sky().unwrap(), baseline.sky().unwrap());
}

#[test]
fn sky_population_pressure_keeps_exact_column_until_explicit_growth() {
    let registry = registry();
    let mut small = limits(true);
    small.sky.as_mut().unwrap().engine.queue_entries = 1;
    let mut work = LightingWork::new(source(&registry, false), small).unwrap();
    assert!(matches!(
        work.step(usize::MAX),
        Err(LightingError::Sky(SkyError::QueueCapacity { .. }))
    ));
    let retained = work.retained_bytes();
    let reserved = work.reservation_bytes();
    assert!(matches!(
        work.step(usize::MAX),
        Err(LightingError::Sky(SkyError::QueueCapacity { .. }))
    ));
    assert_eq!(work.retained_bytes(), retained);
    work = work
        .into_completed()
        .err()
        .expect("population is not convergence");
    work.grow_sky_queues(32768).unwrap();
    assert_eq!(work.reservation_bytes(), reserved);
    let (resumed, _) = complete(work, 7);
    let (baseline, _) = complete(
        LightingWork::new(source(&registry, false), limits(true)).unwrap(),
        usize::MAX,
    );
    assert_layers(resumed.block(), baseline.block());
    assert_layers(resumed.sky().unwrap(), baseline.sky().unwrap());
}

#[test]
fn reservation_sum_covers_original_limits_and_rejects_arithmetic_or_address_admission_failure() {
    let mut limits = limits(true);
    let sky = limits.sky.unwrap();
    let configured = limits.metadata_bytes
        + limits.block.queue_bytes
        + limits.block_storage.metadata_bytes
        + limits.block_storage.layer_bytes
        + sky.engine_bytes
        + sky.storage.metadata_bytes
        + sky.storage.layer_bytes;
    assert_eq!(
        limits.reservation_bytes().unwrap(),
        configured + size_of::<LightingWork>()
    );
    limits.metadata_bytes = usize::MAX;
    assert!(matches!(
        limits.reservation_bytes(),
        Err(LightingError::AllocationLimit)
    ));
    limits.metadata_bytes = 0;
    let registry = registry();
    assert!(matches!(
        LightingWork::new(source(&registry, false), limits),
        Err(LightingError::AllocationLimit)
    ));
    limits.metadata_bytes = 16;
    limits.max_chunks = 1;
    assert!(matches!(
        LightingWork::new(source(&registry, false), limits),
        Err(LightingError::InvalidLimits)
    ));
}

#[test]
fn partial_support_failure_retains_buffers_and_cannot_be_mistaken_for_completion() {
    let registry = registry();
    let mut small = limits(true);
    small.sky.as_mut().unwrap().storage.max_sections = 1;
    let mut work = LightingWork::new(source(&registry, false), small).unwrap();
    assert!(work.step(1).unwrap().processed == 1);
    assert!(matches!(work.step(1), Err(LightingError::Storage(_))));
    let retained = work.retained_bytes();
    work = work
        .into_completed()
        .err()
        .expect("only block support exists");
    assert!(work.step(1).is_err());
    assert_eq!(work.retained_bytes(), retained);
    // Fixed storage limits require cancellation/restart, never an automatic busy retry.
    drop(work);
}
