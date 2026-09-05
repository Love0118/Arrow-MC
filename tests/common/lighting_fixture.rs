#![allow(dead_code)]
//! Independent admitted block snapshots shared by lighting integration tests.

#[path = "world_registry_fixture.rs"]
pub(crate) mod registry_fixture;

use arrow_mc::{
    server::configuration_data::parse_sha256,
    world::{
        lighting::{LightBlock, LightingChunk, LightingSource, SourceLimits},
        preparation::ChunkAddress,
        section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        storage::{
            chunk::DimensionHeight,
            registry::{ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits},
        },
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const AIR: u32 = 0;
pub const BEDROCK: u32 = 1;
pub const BOTTOM_SLAB: u32 = 2;
pub const TOP_SLAB: u32 = 3;
pub const LEFT_FACE: u32 = 4;
pub const RIGHT_FACE: u32 = 5;
pub const DISABLED_SHAPE: u32 = 6;
pub const WATER: u32 = 7;

pub fn load_registry(reference: &Path) -> Arc<ChunkRegistrySnapshot> {
    let snapshot = env::var_os("ARROW_BLOCK_STATE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| reference.join("bootstrap/26.3-pre-2-block-states-v3"));
    let manifest = env::var("ARROW_BLOCK_STATE_MANIFEST_SHA256").unwrap_or_else(|_| {
        "19c81b4f667315d5981385cbab154e31b4e0ece899d171afb6fad51caa4a4a39".into()
    });
    let configuration = env::var("ARROW_CONFIGURATION_MANIFEST_SHA256").unwrap_or_else(|_| {
        "105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198".into()
    });
    let expected = ExpectedRegistryReference {
        manifest_sha256: parse_sha256(&manifest).unwrap(),
        configuration_manifest_sha256: parse_sha256(&configuration).unwrap(),
        source_jar_sha256: parse_sha256(
            "18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a",
        )
        .unwrap(),
        source_jar_bytes: 26_649_663,
    };
    let jar = fs::read(reference.join("artifacts/26.3-pre-2/server-26.3-pre-2.jar")).unwrap();
    assert_eq!(jar.len() as u64, expected.source_jar_bytes);
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(&jar)),
        expected.source_jar_sha256
    );
    Arc::new(
        ChunkRegistrySnapshot::load(&snapshot, &expected, RegistryLoadLimits::default()).unwrap(),
    )
}

pub fn chunk_from_dense(
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

pub fn from_dense(
    registry: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    address: ChunkAddress,
    dense: &[[u32; 4096]],
) -> LightingSource {
    let chunk = chunk_from_dense(&registry, address, dense);
    LightingSource::from_sections(registry, height, vec![chunk], SourceLimits::default()).unwrap()
}

/// Placements use world block coordinates; chunks absent from `chunks` stay unavailable.
pub fn from_placements(
    registry: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    chunks: &[ChunkAddress],
    placements: &[(LightBlock, u32)],
) -> LightingSource {
    let count = (i32::from(height.max_section()) - i32::from(height.min_section()) + 1) as usize;
    let mut input = Vec::new();
    for &address in chunks {
        let mut dense = vec![[registry.air_id(); 4096]; count];
        for &(pos, id) in placements {
            if pos.x.div_euclid(16) != address.x || pos.z.div_euclid(16) != address.z {
                continue;
            }
            let section = pos.y.div_euclid(16) - i32::from(height.min_section());
            assert!((0..count as i32).contains(&section));
            dense[section as usize][pos.local_index()] = id;
        }
        input.push(chunk_from_dense(&registry, address, &dense));
    }
    LightingSource::from_sections(registry, height, input, SourceLimits::default()).unwrap()
}

pub fn synthetic_registry() -> Arc<ChunkRegistrySnapshot> {
    let mut fixture = registry_fixture::Fixture::from_data(
        json!({"state_count":8,"state_flags":[1,0,0,0,0,0,0,2],"blocks":[
            {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
            {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
            {"id":"test:bottom","default_state":2,"properties":[],"states":[2]},
            {"id":"test:disabled","default_state":6,"properties":[],"states":[6]},
            {"id":"test:left","default_state":4,"properties":[],"states":[4]},
            {"id":"test:right","default_state":5,"properties":[],"states":[5]},
            {"id":"test:top","default_state":3,"properties":[],"states":[3]},
            {"id":"test:water","default_state":7,"properties":[],"states":[7]}
        ]}),
        json!([{"id":"minecraft:plains","protocol_id":0}]),
    );
    let mut materials = [[0u8; 16]; 8];
    materials[1][1] = 15;
    materials[7][1] = 1;
    for &state in &[2usize, 3, 4, 5] {
        materials[state][2] = 3;
    }
    materials[6][2] = 1;
    let set_face = |material: &mut [u8; 16], direction: usize, id: u16| {
        material[4 + direction * 2..6 + direction * 2].copy_from_slice(&id.to_le_bytes())
    };
    set_face(&mut materials[2], 0, 1);
    set_face(&mut materials[3], 1, 1);
    set_face(&mut materials[4], 0, 2);
    set_face(&mut materials[5], 1, 3);
    for direction in 0..6 {
        set_face(&mut materials[6], direction, 1);
    }
    let mut pairs = [0u8; 2];
    for a in 0..4 {
        for b in 0..4 {
            if a == 1 || b == 1 || (a == 2 && b == 3) || (a == 3 && b == 2) {
                let bit = a * 4 + b;
                pairs[bit / 8] |= 1 << (bit % 8);
            }
        }
    }
    fixture.edit_lighting(|bytes| *bytes = registry_fixture::lighting_bytes(&materials, 4, &pairs));
    Arc::new(fixture.load())
}
