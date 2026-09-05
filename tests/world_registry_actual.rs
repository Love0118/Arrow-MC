//! Opt-in verification against locally prepared official data, never bundled here.
use arrow_mc::{
    nbt::{Compound, Tag},
    server::configuration_data::parse_sha256,
    world::storage::registry::{
        ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits,
    },
};
use serde_json::Value;
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires the separately prepared official block-state snapshot"]
fn actual_official_registry_resolves_every_state_and_biome() {
    let root = std::env::var_os("ARROW_BLOCK_STATE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("Decompile/bootstrap/26.3-pre-2-block-states-v3")
        });
    // These defaults were recorded from trusted preparation stdout. Reprepared
    // bundles must supply their independently recorded digests, not read this file.
    let anchor = |name, default: &str| {
        parse_sha256(&std::env::var(name).unwrap_or_else(|_| default.into())).unwrap()
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
    let snapshot =
        ChunkRegistrySnapshot::load(&root, &expected, RegistryLoadLimits::default()).unwrap();
    assert_eq!(snapshot.block_count(), 1286);
    assert_eq!(snapshot.state_count(), 35723);
    assert_eq!(snapshot.block_registry().bits(), 16);
    assert_eq!(snapshot.biome_count(), 67);
    assert_eq!(snapshot.block_entity_type_count(), 49);
    assert_eq!(snapshot.face_count(), 377);
    assert_eq!(
        snapshot.bedrock_id(),
        Some(
            snapshot
                .block_state(&Tag::String("minecraft:bedrock".into()))
                .id
        )
    );
    assert_eq!(
        snapshot.configuration_manifest_sha256(),
        expected.configuration_manifest_sha256
    );
    let data: Value = serde_json::from_slice(&fs::read(root.join("blocks.json")).unwrap()).unwrap();
    let mut checked = 0;
    for block in data["blocks"].as_array().unwrap() {
        let name = block["id"].as_str().unwrap();
        assert_eq!(
            snapshot.block_state(&Tag::String(name.into())).id,
            block["default_state"].as_u64().unwrap() as u32
        );
        let properties = block["properties"].as_array().unwrap();
        for (ordinal, id) in block["states"].as_array().unwrap().iter().enumerate() {
            let mut remaining = ordinal;
            let mut map = Compound::new();
            for property in properties.iter().rev() {
                let values = property["values"].as_array().unwrap();
                let value = values[remaining % values.len()].as_str().unwrap();
                remaining /= values.len();
                map.insert(
                    property["name"].as_str().unwrap().into(),
                    Tag::String(value.into()),
                )
                .unwrap();
            }
            let mut tag = Compound::new();
            tag.insert("id".into(), Tag::String(name.into())).unwrap();
            tag.insert("properties".into(), Tag::Compound(map)).unwrap();
            let resolved = snapshot.block_state(&Tag::Compound(tag));
            let expected_id = id.as_u64().unwrap() as u32;
            assert_eq!(resolved.id, expected_id, "{name} ordinal {ordinal}");
            assert!(!resolved.used_fallback, "{name} ordinal {ordinal}");
            let flags = data["state_flags"][expected_id as usize].as_u64().unwrap();
            let actual = snapshot.state_flags(expected_id).unwrap();
            assert_eq!(actual.is_air, flags & 1 != 0);
            assert_eq!(actual.has_fluid, flags & 2 != 0);
            let tags = block["heightmap_tags"].as_u64().unwrap();
            let expected_mask = (u8::from(flags & 1 == 0) * 3)
                | (u8::from(tags & 1 != 0) * 12)
                | (u8::from(tags & 1 != 0 || flags & 2 != 0) * 16)
                | (u8::from(tags & 2 != 0 || flags & 2 != 0) * 32);
            assert_eq!(snapshot.heightmap_mask(expected_id), Some(expected_mask));
            checked += 1;
        }
    }
    assert_eq!(checked, 35723);
    let biomes: Value =
        serde_json::from_slice(&fs::read(root.join("biomes.json")).unwrap()).unwrap();
    for biome in biomes.as_array().unwrap() {
        let actual = snapshot.biome(&Tag::String(biome["id"].as_str().unwrap().into()));
        assert_eq!(actual.id, biome["protocol_id"].as_u64().unwrap() as u32);
        assert!(!actual.used_fallback);
    }
    let types: Value =
        serde_json::from_slice(&fs::read(root.join("block-entity-types.json")).unwrap()).unwrap();
    for entry in types.as_array().unwrap() {
        assert_eq!(
            snapshot.block_entity_type_id(&entry["id"].as_str().unwrap().into()),
            Some(entry["protocol_id"].as_u64().unwrap() as u32)
        );
    }
    // This binary contains the actual initialized official API observations,
    // including exhaustive ordered face pairs, anchored by the trusted manifest.
    let light = fs::read(root.join("lighting.bin")).unwrap();
    assert_eq!(light.len(), 589351);
    let mut disabled_cached_faces = 0;
    for (id, encoded) in light[16..16 + 35723 * 16].chunks_exact(16).enumerate() {
        let material = snapshot.light_material(id as u32).unwrap();
        assert_eq!(material.emission, encoded[0], "emission {id}");
        assert_eq!(material.dampening, encoded[1], "dampening {id}");
        assert_eq!(material.can_occlude, encoded[2] & 1 != 0);
        assert_eq!(material.use_shape_for_light_occlusion, encoded[2] & 2 != 0);
        assert_eq!(material.empty_shape(), encoded[2] != 3);
        for direction in 0..6 {
            let face = u16::from_le_bytes(
                encoded[4 + direction * 2..6 + direction * 2]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(material.faces[direction], face, "face {id}/{direction}");
            if material.empty_shape() && face != 0 {
                disabled_cached_faces += 1;
            }
        }
    }
    assert_eq!(disabled_cached_faces, 68636);
    let pairs = &light[16 + 35723 * 16..];
    let mut occluding_pairs = 0;
    for first in 0..377u16 {
        for second in 0..377u16 {
            let bit = first as usize * 377 + second as usize;
            let expected = pairs[bit / 8] & (1 << (bit % 8)) != 0;
            assert_eq!(
                snapshot.face_occludes(first, second),
                Some(expected),
                "pair {first}/{second}"
            );
            occluding_pairs += usize::from(expected);
        }
    }
    assert_eq!(occluding_pairs, 22921);
}
