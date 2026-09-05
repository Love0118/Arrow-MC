//! Snapshot ownership and input admission, independently of light propagation.
#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{CpuPool, CpuPoolConfig},
    world::{
        lighting::{
            LightBlock, LightError, LightSection, LightingChunk, LightingSource, SourceLimits,
        },
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        storage::{
            ChunkStore, StorageLimits,
            chunk::{DATA_VERSION, DimensionHeight},
            registry::ChunkRegistrySnapshot,
        },
    },
};
use serde_json::json;
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::time::timeout;

const AIR: u32 = 0;
const BEDROCK: u32 = 1;
const BRIGHT: u32 = 2;
const DIM: u32 = 3;
const VOID_AIR: u32 = 4;

fn address(x: i32) -> ChunkAddress {
    ChunkAddress { x, z: 0 }
}

fn block(x: i32, y: i32, z: i32) -> LightBlock {
    LightBlock { x, y, z }
}

fn height() -> DimensionHeight {
    DimensionHeight::new(-16, 48).unwrap()
}

fn fixture() -> registry_fixture::Fixture {
    let mut fixture = registry_fixture::Fixture::from_data(
        json!({"state_count":8,"state_flags":[1,0,0,0,1,0,0,2],"blocks":[
            {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
            {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
            {"id":"minecraft:void_air","default_state":4,"properties":[],"states":[4]},
            {"id":"test:bright","default_state":2,"properties":[],"states":[2]},
            {"id":"test:dim","default_state":3,"properties":[],"states":[3]},
            {"id":"test:filler_a","default_state":5,"properties":[],"states":[5]},
            {"id":"test:filler_b","default_state":6,"properties":[],"states":[6]},
            {"id":"test:water","default_state":7,"properties":[],"states":[7]}
        ]}),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    );
    let mut materials = [[0u8; 16]; 8];
    materials[7][1] = 1;
    materials[BEDROCK as usize][1] = 15;
    materials[BRIGHT as usize][0] = 15;
    materials[DIM as usize][0] = 4;
    fixture.edit_lighting(|bytes| {
        *bytes = registry_fixture::lighting_bytes(&materials, 2, &[14]);
    });
    fixture
}

fn chunk_from_dense(
    registry: &ChunkRegistrySnapshot,
    address: ChunkAddress,
    dense: &[[u32; 4096]],
) -> LightingChunk {
    let sections = dense
        .iter()
        .map(|states| {
            if states.iter().all(|&id| id == registry.air_id()) {
                return None;
            }
            let mut counts = SectionCounts {
                non_empty_blocks: 0,
                fluid_blocks: 0,
            };
            for &id in states {
                let flags = registry.state_flags(id).unwrap();
                counts.non_empty_blocks += u16::from(!flags.is_air);
                counts.fluid_blocks += u16::from(flags.has_fluid);
            }
            Some(Section {
                counts,
                blocks: PalettedContainer::from_dense(
                    ContainerKind::Blocks,
                    registry.block_registry(),
                    states,
                    65536,
                )
                .unwrap(),
                biomes: PalettedContainer::single(
                    ContainerKind::Biomes,
                    registry.biome_registry(),
                    registry.plains_id(),
                )
                .unwrap(),
            })
        })
        .collect();
    LightingChunk { address, sections }
}

fn from_placements(
    registry: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    addresses: &[ChunkAddress],
    placements: &[(LightBlock, u32)],
) -> LightingSource {
    let section_count =
        (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize;
    let input = addresses
        .iter()
        .map(|&address| {
            let mut dense = vec![[registry.air_id(); 4096]; section_count];
            for &(position, id) in placements {
                if position.column() == address {
                    let section = position.y.div_euclid(16) - i32::from(height.min_section());
                    assert!((0..section_count as i32).contains(&section));
                    dense[section as usize][position.local_index()] = id;
                }
            }
            chunk_from_dense(&registry, address, &dense)
        })
        .collect();
    LightingSource::from_sections(registry, height, input, SourceLimits::default()).unwrap()
}

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut result = Compound::new();
    for (name, value) in entries {
        result.insert(name.into(), value).unwrap();
    }
    Tag::Compound(result)
}

fn disk_section(y: i8, state: &'static str) -> Tag {
    compound([
        ("Y", Tag::Byte(y)),
        (
            "block_states",
            compound([("palette", Tag::List(vec![Tag::String(state.into())]))]),
        ),
    ])
}

fn disk_chunk(x: i32, sections: Vec<Tag>) -> Vec<u8> {
    let mut bytes = Vec::new();
    nbt::write_named(
        &NamedTag {
            name: "lighting source fixture".into(),
            tag: compound([
                ("DataVersion", Tag::Int(DATA_VERSION)),
                ("xPos", Tag::Int(x)),
                ("zPos", Tag::Int(0)),
                ("Status", Tag::String("minecraft:full".into())),
                ("sections", Tag::List(sections)),
            ]),
        },
        &mut bytes,
        nbt::Limits::default(),
    )
    .unwrap();
    bytes
}

fn region(directory: &Path, records: &[(i32, Vec<Tag>)]) {
    fs::create_dir_all(directory).unwrap();
    let mut bytes = vec![0u8; 8192];
    for (x, sections) in records {
        assert!((0..32).contains(x));
        let payload = disk_chunk(*x, sections.clone());
        let sector = bytes.len() / 4096;
        let count = (payload.len() + 5).div_ceil(4096);
        let slot = *x as usize * 4;
        bytes[slot..slot + 4]
            .copy_from_slice(&(((sector as u32) << 8) | count as u32).to_be_bytes());
        bytes.extend_from_slice(&((payload.len() + 1) as i32).to_be_bytes());
        bytes.push(3); // Uncompressed Anvil record.
        bytes.extend_from_slice(&payload);
        bytes.resize((sector + count) * 4096, 0);
    }
    fs::write(directory.join("r.0.0.mca"), bytes).unwrap();
}

fn owner(registry: &Arc<ChunkRegistrySnapshot>) -> ChunkLoadingOwner {
    ChunkLoadingOwner::new(
        17,
        Arc::clone(registry),
        height(),
        true,
        LoadingLimits {
            max_chunks: 4,
            metadata_bytes: 65536,
        },
        1024 * 1024,
    )
    .unwrap()
}

fn store(path: &Path, registry: &Arc<ChunkRegistrySnapshot>) -> ChunkStore {
    let cpu = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 2,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    );
    ChunkStore::new(
        path.to_owned(),
        cpu,
        Arc::clone(registry),
        height(),
        StorageLimits::default(),
        1,
    )
    .unwrap()
}

async fn publish(owner: &mut ChunkLoadingOwner, store: &ChunkStore, x: i32) {
    let LoadDemand::Read(request) = owner.request(address(x)).unwrap() else {
        panic!("expected new demand");
    };
    let outcome = timeout(Duration::from_secs(5), request.read(store))
        .await
        .unwrap()
        .unwrap();
    let LoadingReadOutcome::Decoded(completion) = outcome else {
        panic!("expected real decoded Anvil fixture");
    };
    owner.publish(completion).unwrap();
}

fn empty_chunk(address: ChunkAddress, height: DimensionHeight) -> LightingChunk {
    let count = (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize;
    LightingChunk {
        address,
        sections: (0..count).map(|_| None).collect(),
    }
}

fn reject(result: Result<LightingSource, LightError>, expected: LightError) {
    match result {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected rejection {expected:?}, but input was accepted"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn source_keeps_actual_resident_budget_until_the_last_snapshot_is_dropped() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    region(&directory, &[(0, vec![disk_section(0, "test:bright")])]);
    let store = store(&directory, &registry);
    let mut owner = owner(&registry);
    publish(&mut owner, &store, 0).await;
    let retained = owner.stats().resident_bytes;
    assert!(retained > 0);
    let first =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    let second =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    assert_eq!(owner.stats().resident_bytes, retained);
    assert_eq!(first.owned_section_bytes(), 0);
    assert!(owner.remove_demand(address(0)));
    assert!(owner.resident(address(0)).is_none());
    assert!(!first.is_current(&owner));
    assert!(!second.is_current(&owner));
    assert_eq!(owner.stats().resident_bytes, retained);
    assert_eq!(first.state_at(block(3, 4, 5)), BRIGHT);
    drop(first);
    assert_eq!(owner.stats().resident_bytes, retained);
    drop(second);
    assert_eq!(owner.stats().resident_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn saved_light_borrows_original_rows_without_canonical_reordering_or_layer_copies() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("saved-light-region");
    let mut first = disk_section(0, "test:bright");
    let Tag::Compound(first_row) = &mut first else {
        unreachable!()
    };
    first_row
        .insert("BlockLight".into(), Tag::ByteArray(vec![0x12; 2048]))
        .unwrap();
    let far = compound([
        ("Y", Tag::Byte(-128)),
        ("SkyLight", Tag::ByteArray(vec![0x34; 2048])),
    ]);
    let last = disk_section(0, "test:dim");
    region(&directory, &[(0, vec![first, far, last])]);
    let store = store(&directory, &registry);
    let mut owner = owner(&registry);
    publish(&mut owner, &store, 0).await;
    let source =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    let resident_bytes = owner.stats().resident_bytes;
    let original = owner.resident(address(0)).unwrap().draft().sections();
    let saved = source.saved_light(address(0)).unwrap();
    assert_eq!(
        saved.status,
        arrow_mc::world::storage::chunk::ChunkStatus::Full
    );
    assert!(!saved.light_correct);
    assert_eq!(saved.rows.as_ptr(), original.as_ptr());
    assert_eq!(
        saved.rows.iter().map(|row| row.y).collect::<Vec<_>>(),
        [0, -128, 0]
    );
    assert_eq!(
        saved.rows[0].block_light.as_deref(),
        Some(&[0x12; 2048][..])
    );
    assert_eq!(saved.rows[1].sky_light.as_deref(), Some(&[0x34; 2048][..]));
    assert!(saved.rows[2].block_light.is_none());
    assert_eq!(source.state_at(block(0, 0, 0)), DIM);
    assert!(source.saved_light(address(1)).is_none());
    assert_eq!(owner.stats().resident_bytes, resident_bytes);
    assert!(owner.remove_demand(address(0)));
    assert!(!source.is_current(&owner));
    assert_eq!(
        source.saved_light(address(0)).unwrap().rows[0]
            .block_light
            .as_deref(),
        Some(&[0x12; 2048][..])
    );
    assert_eq!(owner.stats().resident_bytes, resident_bytes);
    drop(source);
    assert_eq!(owner.stats().resident_bytes, 0);
}

#[test]
fn producer_owned_terrain_does_not_invent_saved_light_metadata() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let source = LightingSource::from_sections(
        registry,
        height(),
        vec![empty_chunk(address(0), height())],
        SourceLimits::default(),
    )
    .unwrap();
    assert!(source.has_chunk(address(0)));
    assert!(source.saved_light(address(0)).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn publication_of_a_previously_absent_or_excluded_neighbor_invalidates_the_source() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    region(
        &directory,
        &[
            (0, vec![disk_section(0, "test:bright")]),
            (1, vec![disk_section(0, "test:dim")]),
            (2, vec![disk_section(0, "minecraft:bedrock")]),
        ],
    );
    let store = store(&directory, &registry);
    let mut owner = owner(&registry);
    publish(&mut owner, &store, 0).await;
    let absent =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    assert!(absent.is_current(&owner));
    assert_eq!(absent.state_at(block(16, 0, 0)), BEDROCK);
    publish(&mut owner, &store, 1).await;
    assert!(!absent.is_current(&owner));

    // Resident 1 exists but is deliberately unavailable to this lighting domain.
    let excluded =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    assert!(excluded.is_current(&owner));
    assert!(!excluded.has_chunk(address(1)));
    assert_eq!(excluded.state_at(block(16, 0, 0)), BEDROCK);
    assert!(owner.remove_demand(address(1)));
    assert!(!excluded.is_current(&owner));
    let before_republish =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    publish(&mut owner, &store, 1).await;
    assert!(!before_republish.is_current(&owner));

    // The revision covers topology outside the explicit selected address list.
    let before_other_publish =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    publish(&mut owner, &store, 2).await;
    assert!(!before_other_publish.is_current(&owner));
}

#[tokio::test(flavor = "current_thread")]
async fn reload_and_foreign_owner_invalidate_without_releasing_live_snapshot_data() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    region(&directory, &[(0, vec![disk_section(0, "test:dim")])]);
    let store = store(&directory, &registry);
    let mut first_owner = owner(&registry);
    let mut foreign_owner = owner(&registry);
    publish(&mut first_owner, &store, 0).await;
    publish(&mut foreign_owner, &store, 0).await;
    let source =
        LightingSource::from_canonical(&first_owner, &[address(0)], SourceLimits::default())
            .unwrap();
    assert!(source.is_current(&first_owner));
    assert!(!source.is_current(&foreign_owner));
    let retained = first_owner.stats().resident_bytes;
    first_owner
        .reload(Arc::clone(&registry), height(), true)
        .unwrap();
    assert!(!source.is_current(&first_owner));
    assert_eq!(first_owner.stats().resident_bytes, retained);
    assert_eq!(source.state_at(block(0, 0, 0)), DIM);
    drop(source);
    assert_eq!(first_owner.stats().resident_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_duplicate_y_defaults_and_air_counts_use_retained_palette_ids() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    region(
        &directory,
        &[(
            0,
            vec![
                disk_section(0, "test:dim"),
                disk_section(0, "test:bright"),
                disk_section(1, "minecraft:void_air"),
            ],
        )],
    );
    let store = store(&directory, &registry);
    let mut owner = owner(&registry);
    publish(&mut owner, &store, 0).await;
    assert_eq!(
        owner.section(address(0), 1).unwrap().blocks.get(0).unwrap(),
        VOID_AIR
    );
    let retained = owner.stats().resident_bytes;
    let source = LightingSource::from_canonical(
        &owner,
        &[address(0)],
        SourceLimits {
            metadata_bytes: 1024,
            owned_section_bytes: 0,
            ..SourceLimits::default()
        },
    )
    .unwrap();
    assert_eq!(source.owned_section_bytes(), 0);
    assert!(source.metadata_bytes() <= 1024);
    assert_eq!(source.heap_bytes(), source.metadata_bytes());
    assert_eq!(owner.stats().resident_bytes, retained);
    assert!(std::ptr::eq(source.registry(), registry.as_ref()));
    for (y, expected) in [
        (-16, AIR),
        (-1, AIR),
        (0, BRIGHT),
        (15, BRIGHT),
        (16, AIR),
        (31, AIR),
    ] {
        for (x, z) in [(0, 0), (7, 8), (15, 15)] {
            assert_eq!(source.state_in_chunk(address(0), x, y, z), Some(expected));
        }
    }
    assert!(source.section_has_only_air(LightSection { x: 0, y: -1, z: 0 }));
    assert!(!source.section_has_only_air(LightSection { x: 0, y: 0, z: 0 }));
    assert!(source.section_has_only_air(LightSection { x: 0, y: 1, z: 0 }));
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_missing_duplicate_and_budget_errors_do_not_retain_extra_resident_leases() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let directory = fixture.root.join("region");
    region(&directory, &[(0, vec![disk_section(0, "test:bright")])]);
    let store = store(&directory, &registry);
    let mut owner = owner(&registry);
    publish(&mut owner, &store, 0).await;
    reject(
        LightingSource::from_canonical(&owner, &[address(0), address(0)], SourceLimits::default()),
        LightError::DuplicateChunk,
    );
    reject(
        LightingSource::from_canonical(&owner, &[address(0), address(1)], SourceLimits::default()),
        LightError::MissingChunk,
    );
    reject(
        LightingSource::from_canonical(
            &owner,
            &[address(0)],
            SourceLimits {
                max_chunks: 0,
                ..SourceLimits::default()
            },
        ),
        LightError::InvalidLimits,
    );
    let source =
        LightingSource::from_canonical(&owner, &[address(0)], SourceLimits::default()).unwrap();
    let metadata = source.metadata_bytes();
    drop(source);
    reject(
        LightingSource::from_canonical(
            &owner,
            &[address(0)],
            SourceLimits {
                metadata_bytes: metadata - 1,
                ..SourceLimits::default()
            },
        ),
        LightError::AllocationLimit,
    );
    let exact = LightingSource::from_canonical(
        &owner,
        &[address(0)],
        SourceLimits {
            metadata_bytes: metadata,
            ..SourceLimits::default()
        },
    )
    .unwrap();
    drop(exact);
    owner.remove_demand(address(0));
    assert_eq!(owner.stats().resident_bytes, 0);
}

#[test]
fn available_padding_is_lighting_air_but_unavailable_chunks_are_bedrock() {
    let registry = Arc::new(fixture().load());
    let source = from_placements(
        registry,
        height(),
        &[ChunkAddress { x: -1, z: -1 }],
        &[(block(-1, -1, -1), 7)],
    );
    assert_eq!(source.state_at(block(-1, -1, -1)), 7);
    for y in [-2048, -17, 32, 2047] {
        assert_eq!(source.state_at(block(-1, y, -1)), AIR);
        assert_eq!(source.state_at(block(0, y, -1)), BEDROCK);
    }
    assert_eq!(
        source.state_in_chunk(ChunkAddress { x: -1, z: -1 }, 16, 0, 0),
        None
    );
    assert_eq!(
        source.state_in_chunk(ChunkAddress { x: -1, z: -1 }, 0, 0, 16),
        None
    );
    assert_eq!(source.state_in_chunk(address(0), 0, 0, 0), None);
}

#[test]
fn emission_iteration_uses_section_y_then_local_y_z_x_order() {
    let fixture = fixture();
    let registry = Arc::new(fixture.load());
    let address = ChunkAddress { x: -2, z: 3 };
    let placements = [
        (block(-17, 31, 63), DIM),
        (block(-30, -16, 48), BRIGHT),
        (block(-32, 0, 48), BRIGHT),
        (block(-32, -16, 49), DIM),
        (block(-31, -16, 48), DIM),
        (block(-32, -15, 48), BRIGHT),
        (block(-32, -16, 48), BEDROCK),
    ];
    let source = from_placements(registry, height(), &[address], &placements);
    let expected = vec![
        placements[4],
        placements[1],
        placements[3],
        placements[5],
        placements[2],
        placements[0],
    ];
    assert_eq!(
        source.emission_sources(address).collect::<Vec<_>>(),
        expected
    );
    let mut missing = source.emission_sources(ChunkAddress { x: 8, z: 8 });
    assert_eq!(missing.next(), None);
    assert_eq!(missing.next(), None);
}

#[test]
fn owned_sections_are_explicitly_producer_admitted_and_preserve_capacity_accounting() {
    let registry = Arc::new(fixture().load());
    let height = DimensionHeight::new(0, 16).unwrap();
    let build = || {
        let mut dense = [[AIR; 4096]];
        dense[0][23] = BRIGHT;
        dense[0][4095] = 7;
        chunk_from_dense(&registry, address(0), &dense)
    };
    let input = build();
    let section = input.sections[0].as_ref().unwrap();
    let bytes = section.blocks.heap_bytes() + section.biomes.heap_bytes();
    assert!(bytes > 0);
    // The producer constructs/budgets these buffers before handing ownership over;
    // SourceLimits checks retention, and cannot retroactively reserve that memory.
    let source = LightingSource::from_sections(
        Arc::clone(&registry),
        height,
        vec![input],
        SourceLimits {
            owned_section_bytes: bytes,
            ..SourceLimits::default()
        },
    )
    .unwrap();
    assert_eq!(source.owned_section_bytes(), bytes);
    assert_eq!(source.heap_bytes(), source.metadata_bytes() + bytes);
    assert!(source.metadata_bytes() >= size_of::<Option<Section>>());
    assert_eq!(source.state_at(block(7, 0, 1)), BRIGHT);
    assert_eq!(source.state_at(block(15, 15, 15)), 7);
    assert!(!source.is_current(&owner(&registry)));
    reject(
        LightingSource::from_sections(
            Arc::clone(&registry),
            height,
            vec![build()],
            SourceLimits {
                owned_section_bytes: bytes - 1,
                ..SourceLimits::default()
            },
        ),
        LightError::AllocationLimit,
    );
    reject(
        LightingSource::from_sections(
            Arc::clone(&registry),
            height,
            vec![build()],
            SourceLimits {
                metadata_bytes: 0,
                ..SourceLimits::default()
            },
        ),
        LightError::AllocationLimit,
    );
}

#[test]
fn source_lists_are_sorted_and_duplicate_owned_chunk_coordinates_are_rejected() {
    let registry = Arc::new(fixture().load());
    let positions = [
        ChunkAddress { x: 2, z: -5 },
        ChunkAddress { x: -1, z: 8 },
        ChunkAddress { x: 2, z: -6 },
    ];
    let input = positions
        .map(|address| empty_chunk(address, height()))
        .into();
    let source = LightingSource::from_sections(
        Arc::clone(&registry),
        height(),
        input,
        SourceLimits::default(),
    )
    .unwrap();
    assert_eq!(
        source.chunk_addresses().collect::<Vec<_>>(),
        [positions[1], positions[2], positions[0]]
    );
    reject(
        LightingSource::from_sections(
            Arc::clone(&registry),
            height(),
            vec![
                empty_chunk(address(0), height()),
                empty_chunk(address(0), height()),
            ],
            SourceLimits::default(),
        ),
        LightError::DuplicateChunk,
    );
    reject(
        LightingSource::from_sections(
            registry,
            height(),
            vec![empty_chunk(address(0), height())],
            SourceLimits {
                max_chunks: 0,
                ..SourceLimits::default()
            },
        ),
        LightError::InvalidLimits,
    );
}

#[test]
fn owned_metadata_budget_includes_spare_producer_vec_capacity() {
    let registry = Arc::new(fixture().load());
    let height = DimensionHeight::new(0, 16).unwrap();
    let mut input = Vec::with_capacity(1024);
    input.push(empty_chunk(address(0), height));
    let required_input = input.capacity() * size_of::<LightingChunk>();
    reject(
        LightingSource::from_sections(
            Arc::clone(&registry),
            height,
            input,
            SourceLimits {
                metadata_bytes: required_input - 1,
                ..SourceLimits::default()
            },
        ),
        LightError::AllocationLimit,
    );
    let mut section_slots = Vec::with_capacity(1024);
    section_slots.push(None);
    let required_slots = section_slots.capacity() * size_of::<Option<Section>>();
    reject(
        LightingSource::from_sections(
            registry,
            height,
            vec![LightingChunk {
                address: address(0),
                sections: section_slots,
            }],
            SourceLimits {
                metadata_bytes: required_slots - 1,
                ..SourceLimits::default()
            },
        ),
        LightError::AllocationLimit,
    );
}

#[test]
fn malformed_counts_and_out_of_domain_state_ids_are_rejected() {
    let registry = Arc::new(fixture().load());
    let height = DimensionHeight::new(0, 16).unwrap();
    for counts in [
        SectionCounts {
            non_empty_blocks: 0,
            fluid_blocks: 0,
        },
        SectionCounts {
            non_empty_blocks: 4097,
            fluid_blocks: 0,
        },
        SectionCounts {
            non_empty_blocks: 4096,
            fluid_blocks: 4097,
        },
        SectionCounts {
            non_empty_blocks: 4096,
            fluid_blocks: 1,
        },
    ] {
        let section = Section {
            counts,
            blocks: PalettedContainer::single(
                ContainerKind::Blocks,
                registry.block_registry(),
                BEDROCK,
            )
            .unwrap(),
            biomes: PalettedContainer::single(
                ContainerKind::Biomes,
                registry.biome_registry(),
                registry.plains_id(),
            )
            .unwrap(),
        };
        reject(
            LightingSource::from_sections(
                Arc::clone(&registry),
                height,
                vec![LightingChunk {
                    address: address(0),
                    sections: vec![Some(section)],
                }],
                SourceLimits::default(),
            ),
            LightError::InvalidState,
        );
    }
    let section = Section {
        counts: SectionCounts {
            non_empty_blocks: 4096,
            fluid_blocks: 0,
        },
        blocks: PalettedContainer::single(
            ContainerKind::Blocks,
            arrow_mc::world::section::Registry::new(9).unwrap(),
            8,
        )
        .unwrap(),
        biomes: PalettedContainer::single(
            ContainerKind::Biomes,
            registry.biome_registry(),
            registry.plains_id(),
        )
        .unwrap(),
    };
    reject(
        LightingSource::from_sections(
            registry,
            height,
            vec![LightingChunk {
                address: address(0),
                sections: vec![Some(section)],
            }],
            SourceLimits::default(),
        ),
        LightError::InvalidState,
    );
}

#[test]
fn invalid_section_count_padding_height_and_overflowing_coordinates_are_rejected() {
    let registry = Arc::new(fixture().load());
    for count in [0, 2, 4] {
        let sections = (0..count).map(|_| None).collect();
        reject(
            LightingSource::from_sections(
                Arc::clone(&registry),
                height(),
                vec![LightingChunk {
                    address: address(0),
                    sections,
                }],
                SourceLimits::default(),
            ),
            LightError::InvalidLimits,
        );
    }
    for height in [
        DimensionHeight::new(-2048, 16).unwrap(),
        DimensionHeight::new(2016, 32).unwrap(),
    ] {
        reject(
            LightingSource::from_sections(
                Arc::clone(&registry),
                height,
                vec![empty_chunk(address(0), height)],
                SourceLimits::default(),
            ),
            LightError::InvalidLimits,
        );
    }
    for height in [
        DimensionHeight::new(-2032, 16).unwrap(),
        DimensionHeight::new(2016, 16).unwrap(),
    ] {
        assert!(
            LightingSource::from_sections(
                Arc::clone(&registry),
                height,
                vec![empty_chunk(address(0), height)],
                SourceLimits::default()
            )
            .is_ok()
        );
    }
    for position in [
        address(i32::MIN),
        address(i32::MAX),
        ChunkAddress { x: 0, z: i32::MIN },
        ChunkAddress { x: 0, z: i32::MAX },
    ] {
        reject(
            LightingSource::from_sections(
                Arc::clone(&registry),
                height(),
                vec![empty_chunk(position, height())],
                SourceLimits::default(),
            ),
            LightError::InvalidCoordinate,
        );
    }
}

#[test]
fn independent_owned_snapshots_have_distinct_stamps() {
    let registry = Arc::new(fixture().load());
    let first = LightingSource::from_sections(
        Arc::clone(&registry),
        height(),
        vec![empty_chunk(address(0), height())],
        SourceLimits::default(),
    )
    .unwrap();
    let second = LightingSource::from_sections(
        registry,
        height(),
        vec![empty_chunk(address(0), height())],
        SourceLimits::default(),
    )
    .unwrap();
    assert_eq!(first.stamp(), first.stamp());
    assert_ne!(first.stamp(), second.stamp());
}

#[test]
#[ignore = "requires the separately prepared pinned v3 official registry snapshot"]
fn official_air_variants_have_identical_lighting_materials_for_available_padding() {
    use arrow_mc::{
        server::configuration_data::parse_sha256,
        world::storage::registry::{ExpectedRegistryReference, RegistryLoadLimits},
    };
    use std::{env, path::PathBuf};

    let root = env::var_os("ARROW_BLOCK_STATE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("Decompile/bootstrap/26.3-pre-2-block-states-v3")
        });
    let anchor = |variable, recorded: &str| {
        parse_sha256(&env::var(variable).unwrap_or_else(|_| recorded.into())).unwrap()
    };
    let expected = ExpectedRegistryReference {
        manifest_sha256: anchor(
            "ARROW_BLOCK_STATE_MANIFEST_SHA256",
            "19c81b4f667315d5981385cbab154e31b4e0ece899d171afb6fad51caa4a4a39",
        ),
        configuration_manifest_sha256: anchor(
            "ARROW_CONFIGURATION_MANIFEST_SHA256",
            "105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198",
        ),
        source_jar_sha256: parse_sha256(
            "18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a",
        )
        .unwrap(),
        source_jar_bytes: 26_649_663,
    };
    let registry = Arc::new(
        ChunkRegistrySnapshot::load(&root, &expected, RegistryLoadLimits::default()).unwrap(),
    );
    let air = registry.light_material(registry.air_id()).unwrap();
    assert_eq!(air.emission, 0);
    assert_eq!(air.dampening, 0);
    assert!(air.empty_shape());
    for name in ["minecraft:void_air", "minecraft:cave_air"] {
        let resolved = registry.block_state(&Tag::String(name.into()));
        assert!(!resolved.used_fallback, "{name}");
        assert_ne!(resolved.id, registry.air_id());
        assert_eq!(registry.light_material(resolved.id), Some(air), "{name}");
    }
    let source = LightingSource::from_sections(
        Arc::clone(&registry),
        height(),
        vec![empty_chunk(address(0), height())],
        SourceLimits::default(),
    )
    .unwrap();
    assert_eq!(source.state_at(block(0, -17, 0)), registry.air_id());
    assert_eq!(source.state_at(block(0, 32, 0)), registry.air_id());
    assert_eq!(
        source.state_at(block(16, 32, 0)),
        registry.bedrock_id().unwrap()
    );
}
