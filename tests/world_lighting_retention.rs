#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        block::BlockLightLimits,
        sky::SkyLimits,
        storage::{LAYER_RESERVATION_BYTES, LightSectionStorage, StorageLimits},
        work::{CompletedLighting, LightingLimits, LightingWork, SkyWorkLimits},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};
use std::sync::Arc;

const SECTION: LightSection = LightSection { x: 0, y: 0, z: 0 };
const POSITION: LightBlock = LightBlock { x: 8, y: 8, z: 8 };
const CHUNK: ChunkAddress = ChunkAddress { x: 0, z: 0 };

fn storage_limits() -> StorageLimits {
    StorageLimits {
        max_sections: 128,
        max_columns: 32,
        max_notifications: 512,
        metadata_bytes: 1 << 20,
        layer_bytes: 1 << 20,
    }
}
fn storage(kind: LightKind) -> LightSectionStorage {
    LightSectionStorage::new(kind, storage_limits()).unwrap()
}
fn initialize(storage: &mut LightSectionStorage) {
    storage.update_section_status(SECTION, false).unwrap();
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
}
fn limits(sky: bool, allowance: usize) -> LightingLimits {
    LightingLimits {
        max_chunks: 1,
        metadata_bytes: 64,
        block: BlockLightLimits {
            checks: 16,
            decreases: 16384,
            increases: 16384,
            queue_bytes: allowance,
        },
        block_storage: storage_limits(),
        sky: sky.then_some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 16,
                queue_entries: 16384,
                source_chunks: 1,
                planned_writes: 256,
            },
            storage: storage_limits(),
            engine_bytes: allowance,
        }),
    }
}
fn complete(work: LightingWork) -> CompletedLighting {
    let mut work = work;
    for _ in 0..100_000 {
        if work.step(64).unwrap().complete {
            return work
                .into_completed()
                .unwrap_or_else(|_| panic!("finished work rejected completion"));
        }
    }
    panic!("small fixture did not converge");
}

#[test]
fn implicit_zero_and_allocated_zero_keep_equal_allowances_and_distinct_representations() {
    let mut storage = storage(LightKind::Block);
    initialize(&mut storage);
    let implicit = storage.snapshot();
    let expected = implicit.retained_bytes().unwrap();
    let before = storage.stats();
    for _ in 0..100 {
        assert_eq!(implicit.retained_bytes().unwrap(), expected);
        assert!(implicit.layer(SECTION).unwrap().is_definitely_homogeneous());
    }
    assert_eq!(storage.stats(), before);
    storage.set_stored_level(POSITION, 9).unwrap();
    storage.set_stored_level(POSITION, 0).unwrap();
    storage.publish_visible().unwrap();
    let allocated = storage.snapshot();
    assert!(
        !allocated
            .layer(SECTION)
            .unwrap()
            .is_definitely_homogeneous()
    );
    assert!(!allocated.layer(SECTION).unwrap().is_empty());
    assert_eq!(allocated.get_level(POSITION), 0);
    assert_eq!(allocated.retained_bytes().unwrap(), expected);
    assert_eq!(implicit.retained_bytes().unwrap(), expected);
    assert!(storage.stats().reserved_layer_bytes > before.reserved_layer_bytes);
    drop(implicit);
    assert_eq!(
        storage.stats().reserved_layer_bytes,
        before.reserved_layer_bytes
    );
    assert_eq!(allocated.retained_bytes().unwrap(), expected);
}

#[test]
fn snapshot_charge_is_stable_while_cow_and_unrelated_work_change_shared_ledgers() {
    let mut storage = storage(LightKind::Sky);
    initialize(&mut storage);
    let old = storage.snapshot();
    let charge = old.retained_bytes().unwrap();
    storage.set_stored_level(POSITION, 15).unwrap();
    storage.publish_visible().unwrap();
    let new = storage.snapshot();
    assert_eq!(old.get_level(POSITION), 0);
    assert_eq!(new.get_level(POSITION), 15);
    assert_eq!(old.retained_bytes().unwrap(), charge);
    assert_eq!(new.retained_bytes().unwrap(), charge);
    let ledger = storage.stats().reserved_layer_bytes;
    storage
        .update_section_status(LightSection { x: 5, y: 1, z: 0 }, false)
        .unwrap();
    storage.process_inconsistencies().unwrap();
    storage.publish_visible().unwrap();
    assert!(storage.stats().reserved_layer_bytes > ledger);
    assert_eq!(old.retained_bytes().unwrap(), charge);
    assert_eq!(new.retained_bytes().unwrap(), charge);
    drop(storage);
    assert_eq!(old.retained_bytes().unwrap(), charge);
    assert_eq!(new.retained_bytes().unwrap(), charge);
    assert_eq!(new.get_level(POSITION), 15);
}

#[test]
fn empty_snapshot_body_and_sky_top_metadata_are_included_without_working_storage_maxima() {
    let block = storage(LightKind::Block);
    let sky = storage(LightKind::Sky);
    let empty_block = block.snapshot();
    let empty_sky = sky.snapshot();
    assert_eq!(empty_block.sections().count(), 0);
    assert!(empty_block.retained_bytes().unwrap() > size_of_val(&empty_block));
    assert_eq!(
        empty_block.retained_bytes().unwrap(),
        empty_sky.retained_bytes().unwrap()
    );
    let mut block = block;
    let mut sky = sky;
    initialize(&mut block);
    initialize(&mut sky);
    assert_eq!(block.snapshot().sections().count(), 27);
    let block_bytes = block.snapshot().retained_bytes().unwrap();
    let sky_bytes = sky.snapshot().retained_bytes().unwrap();
    assert!(block_bytes >= 27 * LAYER_RESERVATION_BYTES);
    assert!(
        sky_bytes > block_bytes,
        "sky top-column metadata must be retained"
    );
    assert!(
        sky_bytes < storage_limits().metadata_bytes,
        "working arrays are not reachable from a snapshot"
    );
}

#[test]
fn completed_charge_owns_source_and_layers_but_not_discarded_engine_reservations() {
    let registry = fixture::synthetic_registry();
    let source = fixture::from_placements(
        Arc::clone(&registry),
        DimensionHeight::new(0, 32).unwrap(),
        &[CHUNK],
        &[(POSITION, fixture::BEDROCK)],
    );
    let source_bytes = source.heap_bytes();
    let source_id = source.stamp();
    let requested = limits(true, 16 << 20).reservation_bytes().unwrap();
    let completed = complete(LightingWork::new(source, limits(true, 16 << 20)).unwrap());
    let retained = completed.retained_bytes().unwrap();
    let expected = size_of::<CompletedLighting>()
        + source_bytes
        + 4 * size_of::<usize>()
        + completed.block().retained_bytes().unwrap()
        + completed.sky().unwrap().retained_bytes().unwrap()
        + completed.packet_block().retained_bytes().unwrap()
        + completed.packet_sky().unwrap().retained_bytes().unwrap();
    assert_eq!(retained, expected);
    assert_eq!(completed.source().stamp(), source_id);
    assert!(retained < requested / 10);
    let second = complete(
        LightingWork::new(
            fixture::from_placements(
                Arc::clone(&registry),
                DimensionHeight::new(0, 32).unwrap(),
                &[CHUNK],
                &[(POSITION, fixture::BEDROCK)],
            ),
            limits(true, 2 << 20),
        )
        .unwrap(),
    );
    assert_eq!(
        second.retained_bytes().unwrap(),
        retained,
        "freed queue maxima must not affect charge"
    );
    assert_eq!(
        completed.retained_bytes().unwrap(),
        retained,
        "other result does not change immutable retention"
    );
    eprintln!(
        "Completed lighting retains {retained} bytes versus {requested} configured CPU work bytes (source backing {source_bytes}); no payload copied"
    );
}

#[test]
fn retained_old_completed_source_survives_producer_drop_and_new_completion() {
    let registry = fixture::synthetic_registry();
    let weak = Arc::downgrade(&registry);
    let old = complete(
        LightingWork::new(
            fixture::from_placements(
                Arc::clone(&registry),
                DimensionHeight::new(0, 16).unwrap(),
                &[CHUNK],
                &[(POSITION, fixture::BEDROCK)],
            ),
            limits(false, 2 << 20),
        )
        .unwrap(),
    );
    let charge = old.retained_bytes().unwrap();
    let new = complete(
        LightingWork::new(
            fixture::from_placements(
                Arc::clone(&registry),
                DimensionHeight::new(0, 16).unwrap(),
                &[CHUNK],
                &[],
            ),
            limits(false, 2 << 20),
        )
        .unwrap(),
    );
    assert!(old.sky().is_none());
    assert_eq!(old.source().state_at(POSITION), fixture::BEDROCK);
    assert_eq!(new.source().state_at(POSITION), fixture::AIR);
    assert!(old.retained_bytes().unwrap() > new.retained_bytes().unwrap());
    drop(registry);
    drop(new);
    assert!(weak.upgrade().is_some());
    assert_eq!(old.retained_bytes().unwrap(), charge);
    assert_eq!(old.source().state_at(POSITION), fixture::BEDROCK);
    drop(old);
    assert!(weak.upgrade().is_none());
}
