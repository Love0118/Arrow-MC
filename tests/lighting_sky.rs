#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection, LightingSource,
        sky::{SkyError, SkyLightEngine, SkyLimits},
        storage::{LightSectionStorage, LightSnapshot, StorageLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};

const CHUNK: ChunkAddress = ChunkAddress { x: 0, z: 0 };
fn world(extra: Option<(LightBlock, u32)>) -> LightingSource {
    let mut placements: Vec<_> = (0..16)
        .flat_map(|z| (0..16).map(move |x| (LightBlock { x, y: 0, z }, fixture::BEDROCK)))
        .collect();
    placements.extend(extra);
    fixture::from_placements(
        fixture::synthetic_registry(),
        DimensionHeight::new(0, 32).unwrap(),
        &[CHUNK],
        &placements,
    )
}
fn engine(source: &LightingSource, queue_entries: usize, planned_writes: usize) -> SkyLightEngine {
    let storage = LightSectionStorage::new(
        LightKind::Sky,
        StorageLimits {
            max_sections: 64,
            max_columns: 16,
            max_notifications: 128,
            metadata_bytes: 1 << 20,
            layer_bytes: 1 << 20,
        },
    )
    .unwrap();
    let mut remaining = 8 << 20;
    let mut engine = SkyLightEngine::new(
        storage,
        SkyLimits {
            checks: 16,
            queue_entries,
            source_chunks: 2,
            planned_writes,
        },
        &mut remaining,
    )
    .unwrap();
    engine.initialize_sources(source, CHUNK).unwrap();
    engine
        .storage_mut()
        .unwrap()
        .update_section_status(LightSection { x: 0, y: 0, z: 0 }, false)
        .unwrap();
    engine.run_updates(source).unwrap();
    engine
}
fn assert_snapshot_equal(left: &LightSnapshot, right: &LightSnapshot) {
    let left_keys: Vec<_> = left.sections().collect();
    assert_eq!(left_keys, right.sections().collect::<Vec<_>>());
    for key in left_keys {
        let a = left.layer(key).unwrap();
        let b = right.layer(key).unwrap();
        assert_eq!(a.is_empty(), b.is_empty(), "{key:?}");
        assert_eq!(
            a.is_definitely_homogeneous(),
            b.is_definitely_homogeneous(),
            "{key:?}"
        );
        for index in 0..4096 {
            let x = (index & 15) as u8;
            let y = (index >> 8) as u8;
            let z = ((index >> 4) & 15) as u8;
            assert_eq!(
                a.get(x, y, z).unwrap(),
                b.get(x, y, z).unwrap(),
                "{key:?}/{index}"
            );
        }
    }
}

#[test]
fn population_pressure_retains_cursor_and_never_publishes_partial_sources() {
    let source = world(None);
    let mut limited = engine(&source, 1, 64);
    let visible = limited.storage().snapshot();
    let mut baseline = engine(&source, 32768, 64);
    baseline.propagate_light_sources(CHUNK).unwrap();
    baseline.run_updates(&source).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            limited.propagate_light_sources(CHUNK),
            Err(SkyError::QueueCapacity {
                increase: 2,
                decrease: 0
            })
        ));
        assert!(limited.has_work());
        assert_snapshot_equal(&visible, &limited.storage().snapshot());
        assert!(matches!(limited.run_updates(&source), Err(SkyError::Busy)));
        assert!(matches!(limited.storage_mut(), Err(SkyError::Busy)));
    }
    let mut remaining = 4 << 20;
    limited.grow_queues(32768, &mut remaining).unwrap();
    limited.propagate_light_sources(CHUNK).unwrap();
    limited.run_updates(&source).unwrap();
    assert!(!limited.has_work());
    assert_snapshot_equal(
        &baseline.storage().snapshot(),
        &limited.storage().snapshot(),
    );
}

#[test]
fn budgeted_steps_keep_source_identity_and_visible_snapshot_until_complete() {
    let source = world(None);
    let replacement = world(None);
    let mut limited = engine(&source, 32768, 64);
    let visible = limited.storage().snapshot();
    let mut columns = 0;
    while !limited.populate_budgeted(CHUNK, 1).unwrap() {
        columns += 1;
        assert!(limited.has_work());
        assert!(matches!(limited.storage_mut(), Err(SkyError::Busy)));
    }
    assert_eq!(columns, 511);
    let step = limited.run_budgeted(&source, 1).unwrap();
    assert_eq!(step.processed, 1);
    assert!(!step.complete);
    assert!(matches!(
        limited.run_updates(&replacement),
        Err(SkyError::StaleSource)
    ));
    assert!(matches!(
        limited.check_block(LightBlock { x: 8, y: 1, z: 8 }),
        Err(SkyError::Busy)
    ));
    assert_snapshot_equal(&visible, &limited.storage().snapshot());
    let mut count = step.processed;
    loop {
        let progress = limited.run_budgeted(&source, 7).unwrap();
        assert!(progress.processed <= 7);
        count += progress.processed;
        if progress.complete {
            break;
        }
    }
    assert!(count >= 256);
    assert!(!limited.has_work());
    assert_eq!(
        limited
            .storage()
            .snapshot()
            .get_level(LightBlock { x: 8, y: 1, z: 8 }),
        15
    );
}

#[test]
fn an_oversized_column_plan_resumes_after_explicit_scratch_growth() {
    let source = world(None);
    let changed_pos = LightBlock { x: 8, y: 31, z: 8 };
    let changed = world(Some((changed_pos, fixture::BEDROCK)));
    let mut limited = engine(&source, 32768, 16);
    limited.propagate_light_sources(CHUNK).unwrap();
    limited.run_updates(&source).unwrap();
    let before = limited.storage().snapshot();
    limited.update_sources(&changed, changed_pos).unwrap();
    limited.check_block(changed_pos).unwrap();
    assert!(matches!(
        limited.run_updates(&changed),
        Err(SkyError::PlanFull)
    ));
    assert_snapshot_equal(&before, &limited.storage().snapshot());
    let mut remaining = 1 << 20;
    limited.grow_plan(128, &mut remaining).unwrap();
    limited.run_updates(&changed).unwrap();
    assert_eq!(limited.storage().snapshot().get_level(changed_pos), 0);
    assert!(!limited.has_work());
}

#[test]
fn rejected_constructor_does_not_consume_callers_budget() {
    let storage = LightSectionStorage::new(
        LightKind::Block,
        StorageLimits {
            max_sections: 16,
            max_columns: 4,
            max_notifications: 16,
            metadata_bytes: 1 << 20,
            layer_bytes: 1 << 20,
        },
    )
    .unwrap();
    let mut budget = 100_000;
    assert!(matches!(
        SkyLightEngine::new(
            storage,
            SkyLimits {
                checks: 8,
                queue_entries: 8,
                source_chunks: 1,
                planned_writes: 16,
            },
            &mut budget
        ),
        Err(SkyError::InvalidStorage)
    ));
    assert_eq!(budget, 100_000);
}

#[test]
fn initialized_source_context_rejects_a_different_dimension_before_work() {
    let source = world(None);
    let other = fixture::from_placements(
        fixture::synthetic_registry(),
        DimensionHeight::new(-16, 48).unwrap(),
        &[CHUNK],
        &[],
    );
    let mut engine = engine(&source, 32768, 64);
    let visible = engine.storage().snapshot();
    assert!(matches!(
        engine.initialize_sources(&other, CHUNK),
        Err(SkyError::Sources(
            arrow_mc::world::lighting::sources::SourcesError::ContextMismatch
        ))
    ));
    assert!(matches!(
        engine.run_updates(&other),
        Err(SkyError::Sources(
            arrow_mc::world::lighting::sources::SourcesError::ContextMismatch
        ))
    ));
    assert_snapshot_equal(&visible, &engine.storage().snapshot());
    assert!(!engine.has_work());
}

fn enable_engine(source: &LightingSource, layer_count: usize) -> SkyLightEngine {
    use arrow_mc::world::lighting::storage::LAYER_RESERVATION_BYTES;
    let storage = LightSectionStorage::new(
        LightKind::Sky,
        StorageLimits {
            max_sections: 64,
            max_columns: 16,
            max_notifications: 512,
            metadata_bytes: 1 << 20,
            layer_bytes: layer_count * LAYER_RESERVATION_BYTES,
        },
    )
    .unwrap();
    let mut budget = 8 << 20;
    let mut engine = SkyLightEngine::new(
        storage,
        SkyLimits {
            checks: 16,
            queue_entries: 32768,
            source_chunks: 2,
            planned_writes: 64,
        },
        &mut budget,
    )
    .unwrap();
    engine.initialize_sources(source, CHUNK).unwrap();
    for y in [0, 1] {
        engine
            .storage_mut()
            .unwrap()
            .update_section_status(LightSection { x: 0, y, z: 0 }, false)
            .unwrap();
    }
    engine.run_updates(source).unwrap();
    assert_eq!(
        engine.storage().stats().reserved_layer_bytes,
        36 * LAYER_RESERVATION_BYTES
    );
    engine
}

#[test]
fn interrupted_enable_blocks_publication_and_unrelated_mutation() {
    use arrow_mc::world::lighting::storage::StorageError;
    let source = world(None);
    let mut engine = enable_engine(&source, 37);
    let before = engine.storage().snapshot();
    for _ in 0..2 {
        assert!(matches!(
            engine.set_light_enabled(CHUNK, true),
            Err(SkyError::Storage(StorageError::Budget))
        ));
        assert!(engine.has_work());
        assert!(matches!(engine.run_updates(&source), Err(SkyError::Busy)));
        assert_snapshot_equal(&before, &engine.storage().snapshot());
    }
    assert!(matches!(engine.storage_mut(), Err(SkyError::Busy)));
    assert!(matches!(
        engine.set_light_enabled(CHUNK, false),
        Err(SkyError::Busy)
    ));
    assert!(matches!(
        engine.set_light_enabled(ChunkAddress { x: 1, z: 0 }, true),
        Err(SkyError::Busy)
    ));
    assert!(matches!(
        engine.check_block(LightBlock { x: 0, y: 1, z: 0 }),
        Err(SkyError::Busy)
    ));
    assert!(matches!(
        engine.update_sources(&source, LightBlock { x: 0, y: 1, z: 0 }),
        Err(SkyError::Busy)
    ));
    assert!(matches!(
        engine.initialize_sources(&source, CHUNK),
        Err(SkyError::Busy)
    ));
    assert!(matches!(engine.remove_sources(CHUNK), Err(SkyError::Busy)));
    assert!(matches!(
        engine.populate_budgeted(CHUNK, 1),
        Err(SkyError::Busy)
    ));
}

#[test]
fn interrupted_enable_resumes_after_an_older_snapshot_releases_memory() {
    use arrow_mc::world::lighting::storage::StorageError;
    let source = world(None);
    let mut engine = enable_engine(&source, 38);
    let older = engine.storage().snapshot();
    // Retain exactly one old layer through an earlier visible generation.
    engine
        .storage_mut()
        .unwrap()
        .set_stored_level(LightBlock { x: 0, y: -16, z: 0 }, 1)
        .unwrap();
    engine.run_updates(&source).unwrap();
    let before = engine.storage().snapshot();
    assert!(matches!(
        engine.set_light_enabled(CHUNK, true),
        Err(SkyError::Storage(StorageError::Budget))
    ));
    assert!(matches!(engine.run_updates(&source), Err(SkyError::Busy)));
    assert_snapshot_equal(&before, &engine.storage().snapshot());
    drop(older);
    engine.set_light_enabled(CHUNK, true).unwrap();
    assert_snapshot_equal(&before, &engine.storage().snapshot());
    engine.run_updates(&source).unwrap();
    assert!(!engine.has_work());
    for y in [1, 2] {
        let visible = engine.storage().snapshot();
        let layer = visible.layer(LightSection { x: 0, y, z: 0 }).unwrap();
        assert!(layer.is_definitely_homogeneous());
        assert_eq!(layer.get(0, 0, 0).unwrap(), 15);
    }
}

#[test]
fn unsupported_public_coordinates_are_rejected_before_queue_or_column_changes() {
    let source = world(None);
    let mut engine = engine(&source, 16, 64);
    let min = -2_097_061 * 16;
    let max = 2_097_061 * 16 + 15;
    for pos in [
        LightBlock {
            x: min - 1,
            y: 0,
            z: 0,
        },
        LightBlock {
            x: max + 1,
            y: 0,
            z: 0,
        },
        LightBlock {
            x: 0,
            y: -2033,
            z: 0,
        },
        LightBlock {
            x: 0,
            y: 2032,
            z: 0,
        },
        LightBlock {
            x: 0,
            y: 0,
            z: min - 1,
        },
        LightBlock {
            x: 0,
            y: 0,
            z: max + 1,
        },
        LightBlock {
            x: i32::MAX,
            y: i32::MIN,
            z: i32::MAX,
        },
    ] {
        assert!(matches!(
            engine.check_block(pos),
            Err(SkyError::InvalidCoordinate)
        ));
        assert!(matches!(
            engine.update_sources(&source, pos),
            Err(SkyError::InvalidCoordinate)
        ));
    }
    for chunk in [
        ChunkAddress {
            x: -2_097_062,
            z: 0,
        },
        ChunkAddress { x: 2_097_062, z: 0 },
        ChunkAddress {
            x: 0,
            z: -2_097_062,
        },
        ChunkAddress { x: 0, z: 2_097_062 },
        ChunkAddress {
            x: i32::MAX,
            z: i32::MIN,
        },
    ] {
        assert!(matches!(
            engine.initialize_sources(&source, chunk),
            Err(SkyError::InvalidCoordinate)
        ));
        assert!(matches!(
            engine.set_light_enabled(chunk, true),
            Err(SkyError::InvalidCoordinate)
        ));
        assert!(matches!(
            engine.propagate_light_sources(chunk),
            Err(SkyError::InvalidCoordinate)
        ));
        assert!(!engine.storage().light_enabled(chunk));
    }
    assert!(!engine.has_work());
    for pos in [
        LightBlock {
            x: min,
            y: -2032,
            z: min,
        },
        LightBlock {
            x: max,
            y: 2031,
            z: max,
        },
    ] {
        engine.check_block(pos).unwrap();
    }
    engine.run_updates(&source).unwrap();
    assert!(!engine.has_work());
}
