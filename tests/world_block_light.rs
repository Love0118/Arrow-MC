#[path = "common/world_registry_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection, LightingChunk, LightingSource, SourceLimits,
        block::{BlockLightEngine, BlockLightError, BlockLightLimits},
        queue::QueueError,
        storage::{LAYER_RESERVATION_BYTES, LightSectionStorage, StorageError, StorageLimits},
    },
    preparation::ChunkAddress,
    section::{ContainerKind, PalettedContainer, Section, SectionCounts},
    storage::{chunk::DimensionHeight, registry::ChunkRegistrySnapshot},
};
use serde_json::json;
use std::sync::Arc;

fn block(x: i32, y: i32, z: i32) -> LightBlock {
    LightBlock { x, y, z }
}
fn chunk(x: i32) -> ChunkAddress {
    ChunkAddress { x, z: 0 }
}

fn registry() -> Arc<ChunkRegistrySnapshot> {
    let mut fixture = fixture::Fixture::from_data(
        json!({
            "state_count":5,"state_flags":[1,0,0,0,0],"blocks":[
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
                {"id":"test:bright","default_state":2,"properties":[],"states":[2]},
                {"id":"test:dim","default_state":3,"properties":[],"states":[3]},
                {"id":"test:wall","default_state":4,"properties":[],"states":[4]}
            ]
        }),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    );
    let mut materials = [[0u8; 16]; 5];
    materials[1][1] = 15;
    materials[2][0] = 15;
    materials[2][1] = 15;
    materials[3][0] = 7;
    materials[4][1] = 15;
    fixture.edit_lighting(|bytes| *bytes = fixture::lighting_bytes(&materials, 2, &[14]));
    Arc::new(fixture.load())
}

fn source(
    registry: &Arc<ChunkRegistrySnapshot>,
    columns: &[i32],
    changes: &[(LightBlock, u32)],
) -> LightingSource {
    // Small test producer explicitly admits at most two 16KiB dense scratch
    // grids and 1MiB of owned palettes before construction.
    assert!(columns.len() <= 2);
    let input = columns
        .iter()
        .map(|&x| {
            let mut dense = [0u32; 4096];
            for &(pos, state) in changes {
                if pos.column() == chunk(x) && (0..16).contains(&pos.y) {
                    dense[pos.local_index()] = state;
                }
            }
            let section = Section {
                counts: SectionCounts {
                    non_empty_blocks: dense.iter().filter(|&&id| id != 0).count() as u16,
                    fluid_blocks: 0,
                },
                blocks: PalettedContainer::from_dense(
                    ContainerKind::Blocks,
                    registry.block_registry(),
                    &dense,
                    1 << 20,
                )
                .unwrap(),
                biomes: PalettedContainer::single(
                    ContainerKind::Biomes,
                    registry.biome_registry(),
                    registry.plains_id(),
                )
                .unwrap(),
            };
            LightingChunk {
                address: chunk(x),
                sections: vec![Some(section)],
            }
        })
        .collect();
    LightingSource::from_sections(
        Arc::clone(registry),
        DimensionHeight::new(0, 16).unwrap(),
        input,
        SourceLimits {
            max_chunks: 2,
            metadata_bytes: 1 << 20,
            owned_section_bytes: 1 << 20,
        },
    )
    .unwrap()
}

fn limits(increases: usize) -> BlockLightLimits {
    BlockLightLimits {
        checks: 32,
        decreases: 16384,
        increases,
        queue_bytes: 1 << 20,
    }
}
fn storage(columns: &[i32], layer_bytes: usize) -> LightSectionStorage {
    let mut storage = LightSectionStorage::new(
        LightKind::Block,
        StorageLimits {
            max_sections: 128,
            max_columns: 64,
            max_notifications: 512,
            metadata_bytes: 1 << 20,
            layer_bytes,
        },
    )
    .unwrap();
    for &x in columns {
        storage
            .update_section_status(LightSection { x, y: 0, z: 0 }, false)
            .unwrap();
    }
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
    storage
}
fn finish(
    engine: &mut BlockLightEngine,
    source: &LightingSource,
    storage: &mut LightSectionStorage,
    budget: usize,
) {
    for _ in 0..100_000 {
        if engine.run(source, storage, budget).unwrap().complete {
            return;
        }
    }
    panic!("finite fixture did not converge");
}

#[test]
fn disabled_source_checks_do_not_materialize_zero_layers_but_disabled_neighbors_receive_light() {
    let registry = registry();
    let source = source(&registry, &[0, 1], &[(block(15, 8, 8), 2)]);
    let mut storage = storage(&[0, 1], 1 << 20);
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    engine.check_block(block(15, 8, 8)).unwrap();
    finish(&mut engine, &source, &mut storage, usize::MAX);
    assert!(storage.snapshot().sections().all(|key| {
        storage
            .snapshot()
            .layer(key)
            .unwrap()
            .is_definitely_homogeneous()
    }));
    assert_eq!(storage.snapshot().get_level(block(15, 8, 8)), 0);
    storage.set_enabled(chunk(0), true).unwrap();
    engine.check_block(block(15, 8, 8)).unwrap();
    finish(&mut engine, &source, &mut storage, usize::MAX);
    assert_eq!(storage.snapshot().get_level(block(15, 8, 8)), 15);
    assert_eq!(storage.snapshot().get_level(block(16, 8, 8)), 14);
    assert!(!storage.light_enabled(chunk(1)));
    // Neighboring unavailable chunk is BEDROCK even when its support layer exists.
    assert_eq!(storage.snapshot().get_level(block(15, 8, -1)), 0);
}

#[test]
fn source_removal_reintroduces_weaker_source_and_retains_allocated_zero_representation() {
    let registry = registry();
    let mut storage = storage(&[0], 1 << 20);
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    let lit = source(
        &registry,
        &[0],
        &[(block(4, 8, 8), 2), (block(10, 8, 8), 3)],
    );
    engine
        .propagate_light_sources(&lit, &mut storage, chunk(0))
        .unwrap();
    finish(&mut engine, &lit, &mut storage, 31);
    assert_eq!(storage.snapshot().get_level(block(4, 8, 8)), 15);
    let dim = source(&registry, &[0], &[(block(10, 8, 8), 3)]);
    engine.check_block(block(4, 8, 8)).unwrap();
    finish(&mut engine, &dim, &mut storage, 7);
    assert_eq!(storage.snapshot().get_level(block(10, 8, 8)), 7);
    assert_eq!(storage.snapshot().get_level(block(4, 8, 8)), 1);
    let dark = source(&registry, &[0], &[]);
    engine.check_block(block(10, 8, 8)).unwrap();
    finish(&mut engine, &dark, &mut storage, 19);
    let result = storage.snapshot();
    let mut allocated_zero = 0;
    for section in result.sections() {
        let layer = result.layer(section).unwrap();
        if !layer.is_definitely_homogeneous() {
            allocated_zero += 1;
        }
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(layer.get(x, y, z).unwrap(), 0);
                }
            }
        }
    }
    assert!(allocated_zero > 0);
}

#[test]
fn blocker_addition_and_removal_recompute_paths_across_chunk_boundary() {
    let registry = registry();
    let mut storage = storage(&[0, 1], 1 << 20);
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    let lit = source(&registry, &[0, 1], &[(block(15, 8, 8), 2)]);
    engine
        .propagate_light_sources(&lit, &mut storage, chunk(0))
        .unwrap();
    finish(&mut engine, &lit, &mut storage, usize::MAX);
    let blocked = source(
        &registry,
        &[0, 1],
        &[(block(15, 8, 8), 2), (block(16, 8, 8), 4)],
    );
    engine.check_block(block(16, 8, 8)).unwrap();
    finish(&mut engine, &blocked, &mut storage, 1);
    assert_eq!(storage.snapshot().get_level(block(16, 8, 8)), 0);
    assert_eq!(storage.snapshot().get_level(block(17, 8, 8)), 11);
    engine.check_block(block(16, 8, 8)).unwrap();
    finish(&mut engine, &lit, &mut storage, 1);
    assert_eq!(storage.snapshot().get_level(block(16, 8, 8)), 14);
    assert_eq!(storage.snapshot().get_level(block(17, 8, 8)), 13);
}

#[test]
fn queue_pressure_retains_current_entry_then_growth_resumes_without_partial_emission() {
    let registry = registry();
    let lit = source(&registry, &[0], &[(block(8, 8, 8), 2)]);
    let mut storage = storage(&[0], 1 << 20);
    storage.set_enabled(chunk(0), true).unwrap();
    let mut engine = BlockLightEngine::new(limits(1)).unwrap();
    assert!(engine.check_block(block(8, 8, 8)).unwrap());
    assert!(!engine.check_block(block(8, 8, 8)).unwrap());
    assert!(matches!(
        engine.run(&lit, &mut storage, usize::MAX),
        Err(BlockLightError::Queue(QueueError::Full))
    ));
    assert_eq!(storage.stored_level(block(8, 8, 8)), Some(0));
    assert!(
        storage
            .layer(block(8, 8, 8).section(), true)
            .unwrap()
            .is_definitely_homogeneous()
    );
    assert!(engine.has_work());
    assert!(matches!(
        engine.check_block(block(9, 8, 8)),
        Err(BlockLightError::RunActive)
    ));
    let replacement = source(&registry, &[0], &[(block(8, 8, 8), 2)]);
    assert!(matches!(
        engine.run(&replacement, &mut storage, 1),
        Err(BlockLightError::SourceMismatch)
    ));
    engine.grow_queues(limits(16384)).unwrap();
    finish(&mut engine, &lit, &mut storage, 1);
    assert!(!engine.has_work());
    assert_eq!(storage.snapshot().get_level(block(8, 8, 8)), 15);
    assert_eq!(storage.snapshot().get_level(block(9, 8, 8)), 14);
}

#[test]
fn held_visible_snapshot_budget_pressure_resumes_after_old_layers_are_released() {
    let registry = registry();
    let mut storage = storage(&[0], 30 * LAYER_RESERVATION_BYTES);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        27 * LAYER_RESERVATION_BYTES
    );
    let initial = storage.snapshot();
    let lit = source(&registry, &[0], &[(block(8, 8, 8), 2)]);
    let dark = source(&registry, &[0], &[]);
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    engine
        .propagate_light_sources(&lit, &mut storage, chunk(0))
        .unwrap();
    finish(&mut engine, &lit, &mut storage, usize::MAX);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        30 * LAYER_RESERVATION_BYTES
    );
    let bright = storage.snapshot();
    engine.check_block(block(8, 8, 8)).unwrap();
    assert!(matches!(
        engine.run(&dark, &mut storage, usize::MAX),
        Err(BlockLightError::Storage(StorageError::Budget))
    ));
    assert_eq!(storage.stored_level(block(8, 8, 8)), Some(15));
    assert_eq!(initial.get_level(block(8, 8, 8)), 0);
    assert_eq!(bright.get_level(block(8, 8, 8)), 15);
    drop(initial);
    finish(&mut engine, &dark, &mut storage, 5);
    assert_eq!(storage.snapshot().get_level(block(8, 8, 8)), 0);
    assert_eq!(bright.get_level(block(8, 8, 8)), 15);
    drop(bright);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        27 * LAYER_RESERVATION_BYTES
    );
}

#[test]
fn source_batch_admission_is_atomic_before_enabling_and_unstored_checks_are_noops() {
    let registry = registry();
    let lit = source(
        &registry,
        &[0],
        &[(block(4, 8, 8), 2), (block(10, 8, 8), 3)],
    );
    let mut storage = storage(&[0], 1 << 20);
    let mut engine = BlockLightEngine::new(limits(1)).unwrap();
    assert!(matches!(
        engine.propagate_light_sources(&lit, &mut storage, chunk(0)),
        Err(BlockLightError::Queue(QueueError::Full))
    ));
    assert!(!storage.light_enabled(chunk(0)));
    assert!(!engine.has_work());
    engine.check_block(block(1000, 8, 1000)).unwrap();
    finish(&mut engine, &lit, &mut storage, 1);
    assert_eq!(storage.snapshot().get_level(block(4, 8, 8)), 0);
    engine.grow_queues(limits(16384)).unwrap();
    engine
        .propagate_light_sources(&lit, &mut storage, chunk(0))
        .unwrap();
    finish(&mut engine, &lit, &mut storage, usize::MAX);
    assert!(storage.light_enabled(chunk(0)));
    assert_eq!(storage.snapshot().get_level(block(4, 8, 8)), 15);
}

#[test]
fn partial_run_rejects_a_different_storage_with_the_same_source_then_resumes_original() {
    let registry = registry();
    let lit = source(&registry, &[0], &[(block(8, 8, 8), 2)]);
    let mut first = storage(&[0], 1 << 20);
    let mut other = storage(&[0], 1 << 20);
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    engine
        .propagate_light_sources(&lit, &mut first, chunk(0))
        .unwrap();
    assert!(!engine.run(&lit, &mut first, 7).unwrap().complete);
    let first_level = first.stored_level(block(8, 8, 8));
    assert_eq!(first_level, Some(15));
    assert_eq!(first.snapshot().get_level(block(8, 8, 8)), 0);
    let other_stats = other.stats();
    assert!(matches!(
        engine.run(&lit, &mut other, 7),
        Err(BlockLightError::StorageMismatch)
    ));
    assert_eq!(other.stats(), other_stats);
    assert_eq!(other.stored_level(block(8, 8, 8)), Some(0));
    assert_eq!(first.stored_level(block(8, 8, 8)), first_level);
    finish(&mut engine, &lit, &mut first, 7);
    assert_eq!(first.snapshot().get_level(block(8, 8, 8)), 15);
    assert_eq!(other.snapshot().get_level(block(8, 8, 8)), 0);
}

#[test]
fn unsupported_check_coordinates_fail_before_queue_admission() {
    let mut engine = BlockLightEngine::new(limits(16384)).unwrap();
    let low = -2_097_061 * 16;
    let high = 2_097_061 * 16 + 15;
    for pos in [
        block(low - 1, 0, 0),
        block(high + 1, 0, 0),
        block(0, 0, low - 1),
        block(0, 0, high + 1),
        block(0, -2033, 0),
        block(0, 2032, 0),
        block(i32::MIN, i32::MAX, i32::MIN),
    ] {
        assert!(matches!(
            engine.check_block(pos),
            Err(BlockLightError::InvalidCoordinate)
        ));
        assert!(!engine.has_work());
    }
    assert!(engine.check_block(block(low, -2032, high)).unwrap());
    assert!(engine.check_block(block(high, 2031, low)).unwrap());
}
