#[path = "common/lighting_fixture.rs"]
mod fixture;

use arrow_mc::world::{
    lighting::{
        LightBlock,
        sources::{SkySources, SourcesError},
    },
    preparation::ChunkAddress,
    storage::chunk::DimensionHeight,
};
use fixture::{AIR, BEDROCK, BOTTOM_SLAB, DISABLED_SHAPE, LEFT_FACE, RIGHT_FACE, TOP_SLAB, WATER};
use std::sync::Arc;

const CHUNK: ChunkAddress = ChunkAddress { x: -2, z: 3 };
fn pos(y: i32) -> LightBlock {
    LightBlock { x: -29, y, z: 53 }
}

#[test]
fn empty_columns_extend_below_world_and_invalid_context_is_rejected() {
    let registry = fixture::synthetic_registry();
    let height = DimensionHeight::new(-64, 80).unwrap();
    let source = fixture::from_placements(Arc::clone(&registry), height, &[CHUNK], &[]);
    let mut cache = SkySources::initialize(&source, CHUNK).unwrap();
    for z in 0..16 {
        for x in 0..16 {
            assert_eq!(cache.lowest_source_y(x, z).unwrap(), i32::MIN);
        }
    }
    assert_eq!(cache.highest_lowest_source_y(), i32::MIN);
    assert_eq!(
        cache.lowest_source_y(16, 0),
        Err(SourcesError::InvalidCoordinate)
    );
    assert!(matches!(
        SkySources::initialize(&source, ChunkAddress { x: 0, z: 0 }),
        Err(SourcesError::MissingChunk)
    ));
    assert_eq!(
        cache.update(&source, pos(-65)),
        Err(SourcesError::InvalidCoordinate)
    );
    assert_eq!(
        cache.update(&source, LightBlock { x: 0, y: 0, z: 0 }),
        Err(SourcesError::InvalidCoordinate)
    );
    let different_height = fixture::from_placements(
        registry,
        DimensionHeight::new(-48, 64).unwrap(),
        &[CHUNK],
        &[],
    );
    assert_eq!(
        cache.update(&different_height, pos(0)),
        Err(SourcesError::ContextMismatch)
    );
    assert_eq!(cache.highest_lowest_source_y(), i32::MIN);
}

#[test]
fn air_section_skip_is_distinct_from_explicit_slab_edge_update() {
    let registry = fixture::synthetic_registry();
    let height = DimensionHeight::new(-64, 80).unwrap();
    let source = fixture::from_placements(
        Arc::clone(&registry),
        height,
        &[CHUNK],
        &[(pos(-48), BOTTOM_SLAB)],
    );
    let mut cache = SkySources::initialize(&source, CHUNK).unwrap();
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), i32::MIN);
    assert!(cache.update(&source, pos(-48)).unwrap());
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), -48);
    assert!(!cache.update(&source, pos(-48)).unwrap());
    let removed = fixture::from_placements(registry, height, &[CHUNK], &[]);
    assert!(cache.update(&removed, pos(-48)).unwrap());
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), i32::MIN);
}

#[test]
fn complementary_faces_dampening_and_shape_gate_choose_different_edges() {
    let registry = fixture::synthetic_registry();
    let height = DimensionHeight::new(-16, 48).unwrap();
    let placements = [(pos(8), LEFT_FACE), (pos(7), RIGHT_FACE), (pos(-5), WATER)];
    let source = fixture::from_placements(Arc::clone(&registry), height, &[CHUNK], &placements);
    let mut cache = SkySources::initialize(&source, CHUNK).unwrap();
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), 8);
    let open = fixture::from_placements(
        Arc::clone(&registry),
        height,
        &[CHUNK],
        &[(pos(8), LEFT_FACE), (pos(-5), WATER)],
    );
    assert!(cache.update(&open, pos(7)).unwrap());
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), -4);
    let disabled = fixture::from_placements(
        Arc::clone(&registry),
        height,
        &[CHUNK],
        &[(pos(8), DISABLED_SHAPE), (pos(-5), WATER)],
    );
    assert!(!cache.update(&disabled, pos(8)).unwrap());
    assert_eq!(cache.lowest_source_y(3, 5).unwrap(), -4);
    let roof = fixture::from_placements(
        Arc::clone(&registry),
        height,
        &[CHUNK],
        &[(pos(31), BEDROCK), (pos(-5), WATER)],
    );
    assert!(cache.update(&roof, pos(31)).unwrap());
    assert_eq!(cache.highest_lowest_source_y(), 32);
    assert!(!cache.update(&roof, pos(-5)).unwrap());
    let clear = fixture::from_placements(registry, height, &[CHUNK], &[]);
    assert!(cache.update(&clear, pos(31)).unwrap());
    assert_eq!(cache.highest_lowest_source_y(), i32::MIN);
}

#[test]
fn bottom_build_edge_and_top_faces_remain_real_source_heights() {
    let registry = fixture::synthetic_registry();
    let height = DimensionHeight::new(-64, 80).unwrap();
    for (state, expected) in [
        (BOTTOM_SLAB, -64),
        (TOP_SLAB, -63),
        (WATER, -63),
        (BEDROCK, -63),
        (AIR, i32::MIN),
    ] {
        let source = fixture::from_placements(
            Arc::clone(&registry),
            height,
            &[CHUNK],
            &[(pos(-64), state)],
        );
        let mut cache = SkySources::initialize(
            &fixture::from_placements(Arc::clone(&registry), height, &[CHUNK], &[]),
            CHUNK,
        )
        .unwrap();
        assert_eq!(cache.update(&source, pos(-64)).unwrap(), state != AIR);
        assert_eq!(cache.lowest_source_y(3, 5).unwrap(), expected);
    }
}
