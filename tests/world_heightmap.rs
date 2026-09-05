#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{CpuPool, CpuPoolConfig},
    world::{
        heightmap::{
            Heightmap, HeightmapError, HeightmapKind as Kind, HeightmapOrigin, HeightmapSet,
            HeightmapSource, RestoreOutcome, required_mask,
        },
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        storage::{
            ChunkStore, StorageLimits,
            chunk::{ChunkStatus, DATA_VERSION, DimensionHeight},
            registry::ChunkRegistrySnapshot,
        },
    },
};
use serde_json::json;
use std::{fs, sync::Arc};

fn registry(custom_air_tags: bool) -> registry_fixture::Fixture {
    registry_fixture::Fixture::from_data(
        json!({"state_count":7,"state_flags":[1,0,0,2,0,1,1],"blocks":[
            {"id":"minecraft:air","default_state":0,"properties":[],"states":[0],"heightmap_tags":if custom_air_tags{3}else{0}},
            {"id":"test:stone","default_state":1,"properties":[],"states":[1],"heightmap_tags":3},
            {"id":"test:leaves","default_state":2,"properties":[],"states":[2],"heightmap_tags":1},
            {"id":"test:water","default_state":3,"properties":[],"states":[3],"heightmap_tags":0},
            {"id":"test:flower","default_state":4,"properties":[],"states":[4],"heightmap_tags":0},
            {"id":"minecraft:cave_air","default_state":5,"properties":[],"states":[5],"heightmap_tags":if custom_air_tags{3}else{0}},
            {"id":"minecraft:void_air","default_state":6,"properties":[],"states":[6],"heightmap_tags":0}
        ]}),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    )
}

struct Terrain {
    height: DimensionHeight,
    sections: Vec<Section>,
}
impl Terrain {
    fn new(registry: &ChunkRegistrySnapshot, min: i32, height: u32) -> Self {
        Self {
            height: DimensionHeight::new(min, height).unwrap(),
            sections: (0..height / 16)
                .map(|_| Section {
                    counts: SectionCounts {
                        non_empty_blocks: 0,
                        fluid_blocks: 0,
                    },
                    blocks: PalettedContainer::single(
                        ContainerKind::Blocks,
                        registry.block_registry(),
                        0,
                    )
                    .unwrap(),
                    biomes: PalettedContainer::single(
                        ContainerKind::Biomes,
                        registry.biome_registry(),
                        0,
                    )
                    .unwrap(),
                })
                .collect(),
        }
    }
    fn set(&mut self, registry: &ChunkRegistrySnapshot, x: u8, y: i32, z: u8, id: u32) {
        let index = (y.div_euclid(16) - i32::from(self.height.min_section())) as usize;
        let section = &mut self.sections[index];
        let old = section
            .blocks
            .set(
                x as usize + 16 * z as usize + 256 * y.rem_euclid(16) as usize,
                id,
                usize::MAX,
            )
            .unwrap();
        for (state, added) in [(old, false), (id, true)] {
            let flags = registry.state_flags(state).unwrap();
            if !flags.is_air {
                if added {
                    section.counts.non_empty_blocks += 1;
                } else {
                    section.counts.non_empty_blocks -= 1;
                }
                if flags.has_fluid {
                    if added {
                        section.counts.fluid_blocks += 1;
                    } else {
                        section.counts.fluid_blocks -= 1;
                    }
                }
            }
        }
    }
    fn source<'a>(&'a self, registry: &'a ChunkRegistrySnapshot) -> HeightmapSource<'a> {
        HeightmapSource::from_sections(
            registry,
            self.height,
            &self.sections.iter().map(Some).collect::<Vec<_>>(),
        )
        .unwrap()
    }
}

#[test]
fn kind_ids_usage_and_status_selection_match_the_current_six_types() {
    for (id, kind) in Kind::ALL.into_iter().enumerate() {
        assert_eq!(kind.id(), id as u8);
        assert_eq!(Kind::from_id(id as u8), Some(kind));
        assert_eq!(kind.send_to_client(), [1, 4, 5].contains(&id));
        assert_eq!(kind.keep_after_worldgen(), [1, 3, 4, 5].contains(&id));
    }
    assert_eq!(Kind::from_id(6), None);
    for status in [
        ChunkStatus::Empty,
        ChunkStatus::StructureStarts,
        ChunkStatus::StructureReferences,
        ChunkStatus::Biomes,
    ] {
        assert_eq!(required_mask(status), 0b000101);
    }
    for status in [
        ChunkStatus::Terrain,
        ChunkStatus::Features,
        ChunkStatus::InitializeLight,
        ChunkStatus::Light,
        ChunkStatus::Spawn,
        ChunkStatus::Full,
    ] {
        assert_eq!(required_mask(status), 0b111010);
    }
}

#[test]
fn packed_width_relative_queries_and_allocation_boundary_cover_full_height_domain() {
    let fixture = registry(false);
    let registry = fixture.load();
    for (min, height, bits) in [
        (0, 16, 5),
        (-64, 384, 9),
        (0, 256, 9),
        (-2048, 4096, 13),
        (2032, 16, 5),
    ] {
        let terrain = Terrain::new(&registry, min, height);
        let source = terrain.source(&registry);
        let required = Heightmap::required_bytes(terrain.height);
        assert!(matches!(
            Heightmap::new(Kind::WorldSurface, &source, required - 1),
            Err(HeightmapError::AllocationLimit)
        ));
        let map = Heightmap::new(Kind::WorldSurface, &source, required).unwrap();
        assert_eq!(map.bits(), bits);
        assert_eq!(map.heap_bytes(), required);
        assert_eq!(map.raw().len(), 256usize.div_ceil(64 / bits as usize));
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(map.first_available(x, z), Ok(min));
                assert_eq!(map.highest_taken(x, z), Ok(min - 1));
            }
        }
    }
}

#[test]
fn prime_distinguishes_surface_motion_fluid_and_no_leaves_and_word_boundaries() {
    let fixture = registry(false);
    let registry = fixture.load();
    let mut terrain = Terrain::new(&registry, -64, 384);
    for (y, state) in [(0, 1), (10, 2), (20, 3), (30, 4)] {
        terrain.set(&registry, 0, y, 0, state);
    }
    for (y, state) in [(0, 1), (10, 2)] {
        terrain.set(&registry, 1, y, 0, state);
    }
    for (x, z, y) in [(6, 0, -64), (7, 0, 319), (15, 15, 17)] {
        terrain.set(&registry, x, y, z, 1);
    }
    let source = terrain.source(&registry);
    for kind in Kind::ALL {
        let mut map = Heightmap::new(kind, &source, 4096).unwrap();
        map.prime(&source).unwrap();
        let first = match kind {
            Kind::WorldSurfaceWg | Kind::WorldSurface => 31,
            Kind::OceanFloorWg | Kind::OceanFloor => 11,
            _ => 21,
        };
        assert_eq!(map.first_available(0, 0), Ok(first));
        assert_eq!(
            map.first_available(1, 0),
            Ok(if kind == Kind::MotionBlockingNoLeaves {
                1
            } else {
                11
            })
        );
        assert_eq!(map.first_available(6, 0), Ok(-63));
        assert_eq!(map.first_available(7, 0), Ok(320));
        assert_eq!(map.first_available(15, 15), Ok(18));
        assert_eq!(map.first_available(2, 0), Ok(-64));
        assert!(map.raw().iter().all(|word| word >> 63 == 0));
    }
}

#[test]
fn restore_preserves_arbitrary_bits_and_mismatch_prime_keeps_unmatched_columns() {
    let fixture = registry(false);
    let registry = fixture.load();
    let mut terrain = Terrain::new(&registry, -64, 384);
    terrain.set(&registry, 7, 10, 0, 1);
    let source = terrain.source(&registry);
    let mut map = Heightmap::new(Kind::WorldSurface, &source, 4096).unwrap();
    let mut raw = vec![u64::MAX; map.raw().len()];
    assert_eq!(map.restore(&raw, &source), Ok(RestoreOutcome::Restored));
    assert_eq!(map.raw(), raw);
    assert_eq!(map.first_available(0, 0), Ok(447));
    assert_eq!(map.restore(&[], &source), Ok(RestoreOutcome::Reprimed));
    assert_eq!(map.first_available(0, 0), Ok(447));
    assert_eq!(map.first_available(7, 0), Ok(11));
    raw[0] &= !511;
    map.restore(&raw, &source).unwrap();
    assert_eq!(map.update(0, -64, 0, 1, &source), Ok(true));
    raw[0] |= 1;
    assert_eq!(
        map.raw(),
        raw,
        "only the addressed value changes; unused bits survive"
    );
}

#[test]
fn update_matches_top_insert_noop_removal_and_lower_column_scan() {
    let fixture = registry(false);
    let registry = fixture.load();
    let mut terrain = Terrain::new(&registry, -64, 384);
    terrain.set(&registry, 3, -64, 4, 1);
    terrain.set(&registry, 3, 0, 4, 1);
    terrain.set(&registry, 3, 12, 4, 1);
    let mut map = Heightmap::new(Kind::WorldSurface, &terrain.source(&registry), 4096).unwrap();
    map.prime(&terrain.source(&registry)).unwrap();
    assert_eq!(
        map.update(3, 10, 4, 1, &terrain.source(&registry)),
        Ok(false)
    );
    assert_eq!(
        map.update(3, 12, 4, 1, &terrain.source(&registry)),
        Ok(false)
    );
    assert_eq!(
        map.update(3, 13, 4, 0, &terrain.source(&registry)),
        Ok(false)
    );
    for (y, expected) in [(12, 1), (0, -63), (-64, -64)] {
        terrain.set(&registry, 3, y, 4, 0);
        assert_eq!(map.update(3, y, 4, 0, &terrain.source(&registry)), Ok(true));
        assert_eq!(map.first_available(3, 4), Ok(expected));
    }
    terrain.set(&registry, 3, 319, 4, 1);
    assert_eq!(
        map.update(3, 319, 4, 1, &terrain.source(&registry)),
        Ok(true)
    );
    assert_eq!(map.first_available(3, 4), Ok(320));
}

#[test]
fn custom_air_tags_keep_literal_air_prime_skip_and_empty_section_lookup() {
    let fixture = registry(true);
    let registry = fixture.load();
    let mut terrain = Terrain::new(&registry, 0, 32);
    terrain.set(&registry, 1, 5, 1, 5);
    let mut map = Heightmap::new(Kind::MotionBlocking, &terrain.source(&registry), 4096).unwrap();
    map.prime(&terrain.source(&registry)).unwrap();
    assert_eq!(
        map.first_available(1, 1),
        Ok(0),
        "cave air in all-air section is hidden"
    );
    terrain.set(&registry, 2, 0, 2, 1);
    map.prime(&terrain.source(&registry)).unwrap();
    assert_eq!(map.first_available(1, 1), Ok(6));
    assert_eq!(map.first_available(0, 0), Ok(0));
    assert_eq!(
        map.update(0, 10, 0, 0, &terrain.source(&registry)),
        Ok(true),
        "update tests supplied AIR predicate directly"
    );
    assert_eq!(map.first_available(0, 0), Ok(11));
}

#[test]
fn bad_context_inputs_and_failed_replacement_preserve_existing_map() {
    let fixture = registry(false);
    let registry = fixture.load();
    let other_fixture = registry_fixture::Fixture::new();
    let other = other_fixture.load();
    let terrain = Terrain::new(&registry, 0, 16);
    let source = terrain.source(&registry);
    let mut map = Heightmap::new(Kind::WorldSurface, &source, 4096).unwrap();
    map.update(1, 5, 1, 1, &source).unwrap();
    let original = map.raw().to_vec();
    assert_eq!(
        map.update(16, 0, 0, 1, &source),
        Err(HeightmapError::InvalidColumn)
    );
    assert_eq!(
        map.update(0, 16, 0, 1, &source),
        Err(HeightmapError::InvalidY)
    );
    assert_eq!(
        map.update(0, i32::MIN, 0, 1, &source),
        Err(HeightmapError::InvalidY)
    );
    assert_eq!(
        map.update(0, 0, 0, 7, &source),
        Err(HeightmapError::InvalidState(7))
    );
    let other_terrain = Terrain::new(&other, 0, 16);
    let other_source = other_terrain.source(&other);
    assert_eq!(
        map.prime(&other_source),
        Err(HeightmapError::ContextMismatch)
    );
    assert_eq!(
        map.restore(&original, &other_source),
        Err(HeightmapError::ContextMismatch)
    );
    assert!(matches!(
        Heightmap::new(Kind::WorldSurface, &source, 0),
        Err(HeightmapError::AllocationLimit)
    ));
    assert_eq!(map.raw(), original);
    assert!(matches!(
        HeightmapSource::from_sections(&registry, terrain.height, &[]),
        Err(HeightmapError::SectionCount)
    ));
}

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut result = Compound::new();
    for (key, value) in entries {
        result.insert(key.into(), value).unwrap();
    }
    Tag::Compound(result)
}

#[test]
fn stored_set_selects_required_maps_preserves_extra_arrays_and_ignores_wrong_tag_types() {
    let fixture = registry(false);
    let registry = fixture.load();
    let terrain = Terrain::new(&registry, 0, 16);
    let source = terrain.source(&registry);
    let count = Heightmap::required_bytes(terrain.height) / 8;
    let Tag::Compound(root) = compound([(
        "Heightmaps",
        compound([
            ("WORLD_SURFACE", Tag::LongArray(vec![-1; count])),
            ("OCEAN_FLOOR_WG", Tag::LongArray(vec![1])),
            ("WORLD_SURFACE_WG", Tag::List(vec![Tag::Long(1); count])),
            ("MOTION_BLOCKING", Tag::IntArray(vec![1; count])),
        ]),
    )]) else {
        unreachable!()
    };
    let set = HeightmapSet::from_stored(&source, &root, ChunkStatus::Biomes, 4096).unwrap();
    assert_eq!(set.heap_bytes(), count * 8 * 3);
    assert_eq!(
        set.origin(Kind::WorldSurface),
        Some(HeightmapOrigin::Restored)
    );
    assert_eq!(
        set.origin(Kind::OceanFloorWg),
        Some(HeightmapOrigin::Reprimed)
    );
    assert_eq!(
        set.origin(Kind::WorldSurfaceWg),
        Some(HeightmapOrigin::MissingPrimed)
    );
    assert!(set.get(Kind::MotionBlocking).is_none());
    assert_eq!(
        set.get(Kind::WorldSurface).unwrap().raw(),
        vec![u64::MAX; count]
    );
    let full = HeightmapSet::from_stored(&source, &root, ChunkStatus::Full, 4096).unwrap();
    assert_eq!(full.heap_bytes(), count * 8 * 5);
    assert_eq!(
        full.origin(Kind::MotionBlocking),
        Some(HeightmapOrigin::MissingPrimed)
    );
    assert!(matches!(
        HeightmapSet::from_stored(&source, &root, ChunkStatus::Full, count * 8 * 5 - 1),
        Err(HeightmapError::AllocationLimit)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn real_region_to_canonical_owner_to_restored_and_primed_heightmaps() {
    let fixture = registry(false);
    let registry = Arc::new(fixture.load());
    let height = DimensionHeight::new(-64, 384).unwrap();
    let raw = vec![-1; Heightmap::required_bytes(height) / 8];
    let root = NamedTag {
        name: "heightmap integration".into(),
        tag: compound([
            ("DataVersion", Tag::Int(DATA_VERSION)),
            ("xPos", Tag::Int(100)),
            ("zPos", Tag::Int(100)),
            ("Status", Tag::String("minecraft:full".into())),
            (
                "sections",
                Tag::List(vec![compound([
                    ("Y", Tag::Byte(0)),
                    (
                        "block_states",
                        compound([("palette", Tag::List(vec![Tag::String("test:stone".into())]))]),
                    ),
                ])]),
            ),
            (
                "Heightmaps",
                compound([
                    ("WORLD_SURFACE", Tag::LongArray(raw.clone())),
                    ("OCEAN_FLOOR", Tag::LongArray(vec![0])),
                ]),
            ),
        ]),
    };
    let mut record = Vec::new();
    nbt::write_named(&root, &mut record, nbt::Limits::default()).unwrap();
    let dir = fixture.root.join("region");
    fs::create_dir(&dir).unwrap();
    let sectors = (record.len() + 5).div_ceil(4096);
    let mut region = vec![0; 8192];
    region[..4].copy_from_slice(&((2u32 << 8) | sectors as u32).to_be_bytes());
    region.extend_from_slice(&((record.len() + 1) as i32).to_be_bytes());
    region.push(3);
    region.extend_from_slice(&record);
    region.resize((2 + sectors) * 4096, 0);
    fs::write(dir.join("r.0.0.mca"), region).unwrap();
    let pool = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 2,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    );
    let store = ChunkStore::new(
        dir,
        Arc::clone(&pool),
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
        true,
        LoadingLimits {
            max_chunks: 1,
            metadata_bytes: 16384,
        },
        16 * 1024 * 1024,
    )
    .unwrap();
    let address = ChunkAddress { x: 0, z: 0 };
    assert!(matches!(
        HeightmapSource::from_canonical(&owner, address),
        Err(HeightmapError::MissingResident)
    ));
    let LoadDemand::Read(request) = owner.request(address).unwrap() else {
        panic!("new request")
    };
    let LoadingReadOutcome::Decoded(output) = request.read(&store).await.unwrap() else {
        panic!("stored chunk")
    };
    assert!(owner.publish(output).unwrap().relocated.is_some());
    let set = HeightmapSet::from_canonical(&owner, address, 4096).unwrap();
    assert_eq!(set.heap_bytes(), 37 * 8 * 4);
    assert_eq!(
        set.origin(Kind::WorldSurface),
        Some(HeightmapOrigin::Restored)
    );
    assert_eq!(
        set.get(Kind::WorldSurface).unwrap().first_available(0, 0),
        Ok(447)
    );
    assert_eq!(
        set.origin(Kind::OceanFloor),
        Some(HeightmapOrigin::Reprimed)
    );
    for kind in [
        Kind::OceanFloor,
        Kind::MotionBlocking,
        Kind::MotionBlockingNoLeaves,
    ] {
        assert_eq!(set.get(kind).unwrap().first_available(15, 15), Ok(16));
    }
    assert_eq!(owner.stored_position(address), Some((100, 100)));
    assert_eq!(pool.stats().reserved_buffer_bytes, 0);
}
