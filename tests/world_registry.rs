//! Small independently authored registry snapshots; no official block or biome data.
#[path = "common/world_registry_fixture.rs"]
mod fixture;

use arrow_mc::{
    nbt::{Compound, NbtString, Tag},
    world::storage::registry::RegistryLoadLimits,
};
use fixture::{FILES, Fixture, hex, json_file, write_json};
use serde_json::{Value, json};
use std::fs;

type Mutation = (&'static str, fn(&mut Value));
type FileMutation = (&'static str, &'static str, fn(&mut Value));

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut result = Compound::new();
    for (name, value) in entries {
        result.insert(name.into(), value).unwrap();
    }
    Tag::Compound(result)
}
fn string(value: &str) -> Tag {
    Tag::String(value.into())
}
fn lamp(facing: &str, lit: &str) -> Tag {
    compound([
        ("id", string("test:lamp")),
        (
            "properties",
            compound([("lit", string(lit)), ("facing", string(facing))]),
        ),
    ])
}

#[test]
fn resolves_property_order_and_permuted_global_state_ids() {
    let fixture = Fixture::new();
    let snapshot = fixture.load();
    assert_eq!(snapshot.block_registry().state_count(), 5);
    assert_eq!(snapshot.biome_registry().state_count(), 2);
    assert_eq!(snapshot.air_id(), 0);
    assert_eq!(snapshot.plains_id(), 0);
    for (facing, lit, expected) in [
        ("north", "false", 4),
        ("north", "true", 1),
        ("south", "false", 3),
        ("south", "true", 2),
    ] {
        let resolved = snapshot.block_state(&lamp(facing, lit));
        assert_eq!(resolved.id, expected, "{facing}/{lit}");
        assert!(!resolved.used_fallback);
    }
    let air = snapshot.block_state(&compound([("id", string("minecraft:air"))]));
    assert_eq!(air.id, 0);
    assert!(!air.used_fallback);
    for (name, expected) in [("minecraft:plains", 0), ("minecraft:forest", 1)] {
        let resolved = snapshot.biome(&string(name));
        assert_eq!(resolved.id, expected);
        assert!(!resolved.used_fallback);
    }
    for (id, air, fluid) in [(0, true, false), (1, false, false), (2, false, true)] {
        let flags = snapshot.state_flags(id).unwrap();
        assert_eq!(flags.is_air, air);
        assert_eq!(flags.has_fluid, fluid);
    }
    assert!(snapshot.state_flags(5).is_none());
    assert!(snapshot.state_flags(u32::MAX).is_none());
}

#[test]
fn default_namespace_lookup_preserves_full_identifier_sort_order() {
    let fixture = Fixture::from_data(
        json!({"state_count":5,"state_flags":[1,0,0,0,0],"blocks":[
            {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
            {"id":"minecraft:stone","default_state":1,"properties":[],"states":[1]},
            {"id":"minecraft.foo:stone","default_state":2,"properties":[],"states":[2]},
            {"id":"minecraft0:stone","default_state":3,"properties":[],"states":[3]},
            {"id":"minecraft1:stone","default_state":4,"properties":[],"states":[4]}
        ]}),
        json!([
            {"id":"minecraft:plains","protocol_id":0},
            {"id":"minecraft:forest","protocol_id":1},
            {"id":"minecraft.foo:forest","protocol_id":2},
            {"id":"minecraft0:forest","protocol_id":3},
            {"id":"minecraft1:forest","protocol_id":4}
        ]),
    );
    let snapshot = fixture.load();
    for (name, expected) in [
        ("stone", 1),
        (":stone", 1),
        ("minecraft:stone", 1),
        ("minecraft.foo:stone", 2),
        ("minecraft0:stone", 3),
        ("minecraft1:stone", 4),
    ] {
        for input in [string(name), compound([("id", string(name))])] {
            let value = snapshot.block_state(&input);
            assert_eq!(value.id, expected, "{name}");
            assert!(!value.used_fallback, "{name}");
        }
    }
    for (name, expected) in [
        ("forest", 1),
        (":forest", 1),
        ("minecraft:forest", 1),
        ("minecraft.foo:forest", 2),
        ("minecraft0:forest", 3),
        ("minecraft1:forest", 4),
    ] {
        let value = snapshot.biome(&string(name));
        assert_eq!(value.id, expected, "{name}");
        assert!(!value.used_fallback, "{name}");
    }
}

#[test]
fn observed_codec_defaults_preserve_valid_properties_and_report_lossy_recovery() {
    // Default/type rules were observed through the pinned BlockState.CODEC; this
    // lamp uses synthetic names, defaults, and IDs. The isolated-surrogate cases
    // separately exercise the borrowed UTF-16 input boundary.
    let fixture = Fixture::new();
    let snapshot = fixture.load();
    let with_properties =
        |properties| compound([("id", string("test:lamp")), ("properties", properties)]);
    let surrogate = || Tag::String(NbtString::from_utf16(vec![0xd800]));
    let cases = [
        ("string shorthand", string("test:lamp"), 1, false),
        (
            "missing properties",
            compound([("id", string("test:lamp"))]),
            1,
            false,
        ),
        ("empty properties", with_properties(compound([])), 1, false),
        (
            "unknown property",
            with_properties(compound([("unknown", Tag::Int(2))])),
            1,
            false,
        ),
        (
            "unknown and valid property",
            with_properties(compound([
                ("unknown", Tag::Int(2)),
                ("facing", string("south")),
            ])),
            2,
            false,
        ),
        (
            "uppercase Properties ignored",
            compound([
                ("id", string("test:lamp")),
                ("Properties", compound([("facing", string("south"))])),
            ]),
            1,
            false,
        ),
        (
            "invalid first property",
            with_properties(compound([
                ("facing", string("INVALID")),
                ("lit", string("false")),
            ])),
            4,
            true,
        ),
        (
            "nonstring first property",
            with_properties(compound([
                ("facing", Tag::Int(2)),
                ("lit", string("false")),
            ])),
            4,
            true,
        ),
        (
            "invalid second property",
            with_properties(compound([
                ("facing", string("south")),
                ("lit", string("INVALID")),
            ])),
            2,
            true,
        ),
        (
            "boolean tag is not property text",
            with_properties(compound([
                ("facing", string("south")),
                ("lit", Tag::Byte(0)),
            ])),
            2,
            true,
        ),
        (
            "boolean property text",
            with_properties(compound([
                ("facing", string("south")),
                ("lit", string("false")),
            ])),
            3,
            false,
        ),
        ("integer properties", with_properties(Tag::Int(42)), 1, true),
        ("string properties", with_properties(string("x")), 1, true),
        (
            "list properties",
            with_properties(Tag::List(vec![])),
            1,
            true,
        ),
        (
            "propertyless block ignores properties type",
            compound([
                ("id", string("minecraft:air")),
                ("properties", Tag::Int(42)),
            ]),
            0,
            false,
        ),
        ("missing id", compound([]), 0, true),
        (
            "legacy Name",
            compound([("Name", string("test:lamp"))]),
            0,
            true,
        ),
        ("wrong id type", compound([("id", Tag::Int(42))]), 0, true),
        (
            "unknown id",
            compound([("id", string("test:missing"))]),
            0,
            true,
        ),
        (
            "invalid id",
            compound([("id", string("Test:LAMP"))]),
            0,
            true,
        ),
        ("nonstring noncompound block", Tag::Int(42), 0, true),
        (
            "isolated surrogate id",
            compound([("id", surrogate())]),
            0,
            true,
        ),
        ("isolated surrogate shorthand", surrogate(), 0, true),
    ];
    for (name, input, id, used_fallback) in cases {
        let actual = snapshot.block_state(&input);
        assert_eq!(
            (actual.id, actual.used_fallback),
            (id, used_fallback),
            "{name}"
        );
    }
    for input in [Tag::Int(42), string("test:missing"), surrogate()] {
        let actual = snapshot.biome(&input);
        assert_eq!(actual.id, snapshot.plains_id());
        assert!(actual.used_fallback);
    }
}

#[test]
fn independent_anchors_reject_reauthored_and_corrupted_files() {
    for file in FILES {
        let fixture = Fixture::new();
        let mut bytes = fs::read(fixture.root.join(file)).unwrap();
        bytes.push(b' ');
        fs::write(fixture.root.join(file), bytes).unwrap();
        assert!(fixture.rejects(RegistryLoadLimits::default()), "{file}");
    }
    let fixture = Fixture::new();
    let path = fixture.root.join("blocks.json");
    let mut blocks = json_file(&path);
    blocks["blocks"][1]["states"] = json!([3, 1, 4, 2]);
    write_json(&path, &blocks);
    fixture.refresh_descriptors();
    // The bundle's rewritten hashes cannot replace the caller's original anchor.
    assert!(fixture.rejects(RegistryLoadLimits::default()));

    for field in 0..4 {
        let mut fixture = Fixture::new();
        match field {
            0 => fixture.expected.manifest_sha256 = [0; 32],
            1 => fixture.expected.configuration_manifest_sha256 = [0; 32],
            2 => fixture.expected.source_jar_sha256 = [0; 32],
            _ => fixture.expected.source_jar_bytes += 1,
        }
        assert!(
            fixture.rejects(RegistryLoadLimits::default()),
            "anchor {field}"
        );
    }
}

#[test]
fn rejects_inconsistent_manifest_metadata_and_file_descriptors() {
    let mutations: [FileMutation; 20] = [
        ("format", "manifest.json", |v| {
            v["format_version"] = json!(1)
        }),
        ("version", "manifest.json", |v| {
            v["minecraft_version"] = json!("26.4")
        }),
        ("protocol", "manifest.json", |v| v["protocol"] = json!(0)),
        ("source hash", "manifest.json", |v| {
            v["source_jar"]["sha256"] = json!(hex(&[0; 32]))
        }),
        ("source size", "manifest.json", |v| {
            v["source_jar"]["bytes"] = json!(1)
        }),
        ("configuration hash", "manifest.json", |v| {
            v["configuration_manifest_sha256"] = json!(hex(&[0; 32]))
        }),
        ("pack missing", "manifest.json", |v| {
            v["selected_packs"] = json!([])
        }),
        ("pack extra", "manifest.json", |v| {
            let pack = v["selected_packs"][0].clone();
            v["selected_packs"].as_array_mut().unwrap().push(pack);
        }),
        ("pack kind", "manifest.json", |v| {
            v["selected_packs"][0]["hash_kind"] = json!("unknown")
        }),
        ("pack fingerprint", "manifest.json", |v| {
            v["selected_packs"][0]["sha256"] = json!(hex(&[0; 32]))
        }),
        ("file missing", "manifest.json", |v| {
            v["files"].as_array_mut().unwrap().pop();
        }),
        ("file duplicate", "manifest.json", |v| {
            let file = v["files"][0].clone();
            v["files"][1] = file;
        }),
        ("file traversal", "manifest.json", |v| {
            v["files"][0]["path"] = json!("../blocks.json")
        }),
        ("file size", "manifest.json", |v| {
            v["files"][0]["bytes"] = json!(1)
        }),
        ("file digest", "manifest.json", |v| {
            v["files"][0]["sha256"] = json!(hex(&[0; 32]))
        }),
        ("metadata version", "export-metadata.json", |v| {
            v["minecraft_version"] = json!("26.4")
        }),
        ("metadata protocol", "export-metadata.json", |v| {
            v["protocol"] = json!(0)
        }),
        ("metadata source", "export-metadata.json", |v| {
            v["source_jar"]["bytes"] = json!(1)
        }),
        ("metadata blocks", "export-metadata.json", |v| {
            v["block_count"] = json!(3)
        }),
        ("metadata states", "export-metadata.json", |v| {
            v["state_count"] = json!(6)
        }),
    ];
    for (name, file, mutate) in mutations {
        let mut fixture = Fixture::new();
        fixture.edit(file, mutate);
        assert!(fixture.rejects(RegistryLoadLimits::default()), "{name}");
    }
}

#[test]
fn rejects_ambiguous_or_incomplete_state_domains() {
    let mutations: [Mutation; 19] = [
        ("missing air", |v| {
            v["blocks"][0]["id"] = json!("test:other")
        }),
        ("duplicate blocks", |v| {
            v["blocks"][1]["id"] = json!("minecraft:air")
        }),
        ("invalid block name", |v| {
            v["blocks"][1]["id"] = json!("Test:lamp")
        }),
        ("missing global state", |v| {
            v["state_count"] = json!(6);
            v["state_flags"] = json!([1, 0, 2, 0, 2, 0]);
        }),
        ("duplicate global state", |v| {
            v["blocks"][1]["states"] = json!([4, 1, 3, 3])
        }),
        ("out of range state", |v| {
            v["blocks"][1]["states"] = json!([4, 1, 3, 5])
        }),
        ("wrong state count", |v| {
            v["blocks"][1]["states"] = json!([4, 1, 3])
        }),
        ("empty states", |v| v["blocks"][0]["states"] = json!([])),
        ("default in other block", |v| {
            v["blocks"][1]["default_state"] = json!(0)
        }),
        ("default differs from properties", |v| {
            v["blocks"][1]["default_state"] = json!(4)
        }),
        ("duplicate property", |v| {
            v["blocks"][1]["properties"][1]["name"] = json!("facing")
        }),
        ("empty property name", |v| {
            v["blocks"][1]["properties"][0]["name"] = json!("")
        }),
        ("duplicate property value", |v| {
            v["blocks"][1]["properties"][0]["values"] = json!(["north", "north"])
        }),
        ("empty property domain", |v| {
            v["blocks"][1]["properties"][0]["values"] = json!([])
        }),
        ("property default out of range", |v| {
            v["blocks"][1]["properties"][0]["default_index"] = json!(2)
        }),
        ("missing flags", |v| v["state_flags"] = json!([1, 0, 2, 0])),
        ("unknown flag bits", |v| v["state_flags"][1] = json!(4)),
        ("negative state", |v| {
            v["blocks"][1]["states"][0] = json!(-1)
        }),
        ("oversized global count", |v| {
            v["state_count"] = json!(u64::MAX)
        }),
    ];
    for (name, mutate) in mutations {
        let mut fixture = Fixture::new();
        fixture.edit("blocks.json", mutate);
        let state_count = json_file(&fixture.root.join("blocks.json"))["state_count"].clone();
        fixture.edit("export-metadata.json", |value| {
            value["state_count"] = state_count
        });
        assert!(fixture.rejects(RegistryLoadLimits::default()), "{name}");
    }
}

#[test]
fn rejects_missing_or_noncontiguous_biomes() {
    let mutations: [Mutation; 6] = [
        ("missing plains", |v| v[0]["id"] = json!("test:other")),
        ("duplicate biome", |v| {
            v[1]["id"] = json!("minecraft:plains")
        }),
        ("invalid biome name", |v| v[1]["id"] = json!("Test:forest")),
        ("duplicate protocol ID", |v| v[1]["protocol_id"] = json!(0)),
        ("protocol ID gap", |v| v[1]["protocol_id"] = json!(2)),
        ("empty biome domain", |v| *v = json!([])),
    ];
    for (name, mutate) in mutations {
        let mut fixture = Fixture::new();
        fixture.edit("biomes.json", mutate);
        assert!(fixture.rejects(RegistryLoadLimits::default()), "{name}");
    }
}

#[test]
fn enforces_independent_file_allocation_and_domain_budgets() {
    let fixture = Fixture::new();
    for limits in [
        RegistryLoadLimits {
            file_bytes: 1,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            total_file_bytes: 1,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            allocation_bytes: 1,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            blocks: 1,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            states: 4,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            biomes: 1,
            ..RegistryLoadLimits::default()
        },
        RegistryLoadLimits {
            block_entity_types: 1,
            ..RegistryLoadLimits::default()
        },
    ] {
        assert!(fixture.rejects(limits));
    }
    assert!(!fixture.rejects(RegistryLoadLimits {
        blocks: 2,
        states: 5,
        biomes: 2,
        ..RegistryLoadLimits::default()
    }));
}

#[test]
fn all_heightmap_predicates_use_independent_tags_air_and_fluid() {
    // Enumerate every allowed flag/tag combination. In particular, a custom
    // motion tag on air affects the predicate, not literal-air prime skipping.
    for tags in 0u8..4 {
        for physical in 0u8..4 {
            let mut fixture = Fixture::new();
            fixture.edit("blocks.json", |data| {
                data["blocks"][0]["heightmap_tags"] = json!(tags);
                data["blocks"][1]["heightmap_tags"] = json!(tags);
                for id in 1..5 {
                    data["state_flags"][id] = json!(physical);
                }
            });
            let snapshot = fixture.load();
            let surface = if physical & 1 == 0 { 3 } else { 0 };
            let floor = if tags & 1 != 0 { 12 } else { 0 };
            let motion = if tags & 1 != 0 || physical & 2 != 0 {
                16
            } else {
                0
            };
            let no_leaves = if tags & 2 != 0 || physical & 2 != 0 {
                32
            } else {
                0
            };
            assert_eq!(
                snapshot.heightmap_mask(1),
                Some(surface | floor | motion | no_leaves)
            );
            assert_eq!(snapshot.state_flags(1).unwrap().is_air, physical & 1 != 0);
            assert_eq!(
                snapshot.state_flags(1).unwrap().has_fluid,
                physical & 2 != 0
            );
            assert_eq!(
                snapshot.heightmap_mask(0),
                Some(
                    floor | if tags & 1 != 0 { 16 } else { 0 } | if tags & 2 != 0 { 32 } else { 0 }
                )
            );
            assert_eq!(snapshot.heightmap_mask(5), None);
        }
    }
}

#[test]
fn v2_requires_tag_membership_and_valid_block_entity_domain() {
    for tags in [json!(-1), json!(4), json!(true), Value::Null] {
        let mut fixture = Fixture::new();
        fixture.edit("blocks.json", |data| {
            data["blocks"][1]["heightmap_tags"] = tags
        });
        assert!(fixture.rejects(RegistryLoadLimits::default()));
    }
    let mut fixture = Fixture::new();
    fixture.edit("blocks.json", |data| {
        data["blocks"][1]
            .as_object_mut()
            .unwrap()
            .remove("heightmap_tags");
    });
    assert!(fixture.rejects(RegistryLoadLimits::default()));
    for domain in [
        json!([]),
        json!([{"id":"test:lamp","protocol_id":1}]),
        json!([{"id":"test:lamp","protocol_id":0},{"id":"test:lamp","protocol_id":1}]),
        json!([{"id":"Bad:ID","protocol_id":0}]),
    ] {
        let mut fixture = Fixture::new();
        fixture.edit("block-entity-types.json", |data| *data = domain);
        assert!(fixture.rejects(RegistryLoadLimits::default()));
    }
    let snapshot = Fixture::new().load();
    assert_eq!(snapshot.block_entity_type_count(), 2);
    assert_eq!(snapshot.block_entity_type_id(&"chest".into()), Some(1));
    assert_eq!(snapshot.block_entity_type_id(&":chest".into()), Some(1));
    assert_eq!(snapshot.block_entity_type_id(&"test:lamp".into()), Some(0));
    assert_eq!(
        snapshot.block_entity_type_id(&"minecraft:absent".into()),
        None
    );
    assert_eq!(
        snapshot.block_entity_type_id(&NbtString::from_utf16(vec![0xd800])),
        None
    );
}
