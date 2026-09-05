//! Independent current-chunk inputs use small synthetic registry metadata.

#[path = "common/world_registry_fixture.rs"]
mod fixture;

use arrow_mc::nbt::{self, Compound, Limits, NamedTag, Tag};
use arrow_mc::snbt;
use arrow_mc::world::storage::chunk::{
    ChunkDecodeError, DATA_VERSION, DimensionHeight, StoredChunkDraft, decode_current_chunk,
};
use arrow_mc::world::storage::registry::ChunkRegistrySnapshot;
use serde_json::json;
use std::sync::OnceLock;

const DECODED_BYTES: usize = 4 * 1024 * 1024;

fn registries() -> &'static ChunkRegistrySnapshot {
    static REGISTRIES: OnceLock<ChunkRegistrySnapshot> = OnceLock::new();
    REGISTRIES.get_or_init(|| {
        let mut flags = vec![0; 1025];
        flags[0] = 1;
        flags[2] = 2;
        let blocks = json!({
            "state_count": 1025, "state_flags": flags,
            "blocks": [
                {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                {"id":"minecraft:stone","default_state":1,"properties":[],"states":[1]},
                {"id":"minecraft:water","default_state":2,"properties":[],"states":[2]},
                {"id":"minecraft:oak_log","default_state":4,
                 "properties":[{"name":"axis","values":["x","y","z"],"default_index":1}],
                 "states":[3,4,5]},
                {"id":"test:varied","default_state":6,
                 "properties":[{"name":"value","values":(0..1019).map(|v|v.to_string()).collect::<Vec<_>>(),"default_index":0}],
                 "states":(6..1025).collect::<Vec<_>>()}
            ]
        });
        let biomes = json!([
            {"id":"minecraft:plains","protocol_id":0},
            {"id":"minecraft:desert","protocol_id":1},
            {"id":"test:highland","protocol_id":2}
        ]);
        fixture::Fixture::from_data(blocks, biomes).load()
    })
}

fn root(text: &str) -> Compound {
    snbt::parse_compound(text, snbt::Limits::default()).unwrap()
}

fn bytes(root: Compound) -> Vec<u8> {
    let mut bytes = Vec::new();
    nbt::write_named(
        &NamedTag {
            name: "synthetic chunk".into(),
            tag: Tag::Compound(root),
        },
        &mut bytes,
        Limits::default(),
    )
    .unwrap();
    bytes
}

fn decode(root: Compound) -> Result<StoredChunkDraft, ChunkDecodeError> {
    decode_current_chunk(
        &mut bytes(root),
        registries(),
        DimensionHeight::new(-64, 384).unwrap(),
        Limits::default(),
        DECODED_BYTES,
    )
}

fn current(text: &str) -> Compound {
    let mut root = root(text);
    root.insert("DataVersion".into(), Tag::Int(DATA_VERSION))
        .unwrap();
    root
}

fn failure(root: Compound) -> ChunkDecodeError {
    match decode(root) {
        Ok(_) => panic!("expected stored-chunk rejection"),
        Err(error) => error,
    }
}

fn packed(indices: &[usize], bits: usize) -> Vec<i64> {
    let per_word = 64 / bits;
    indices
        .chunks(per_word)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0u64, |word, (offset, &value)| {
                    word | ((value as u64) << (offset * bits))
                }) as i64
        })
        .collect()
}

fn section_root(palette: Vec<Tag>, data: Option<Vec<i64>>) -> Compound {
    collection_root("block_states", Tag::List(palette), data.map(Tag::LongArray))
}

fn collection_root(field: &str, palette: Tag, data: Option<Tag>) -> Compound {
    let mut container = Compound::new();
    container.insert("palette".into(), palette).unwrap();
    if let Some(data) = data {
        container.insert("data".into(), data).unwrap();
    }
    let mut section = Compound::new();
    section.insert("Y".into(), Tag::Byte(-4)).unwrap();
    section
        .insert(field.into(), Tag::Compound(container))
        .unwrap();
    let mut root = current("{Status:'minecraft:full'}");
    root.insert("sections".into(), Tag::List(vec![Tag::Compound(section)]))
        .unwrap();
    root
}

#[test]
fn disk_data_accepts_primitive_arrays_and_numeric_lists_for_both_palettes() {
    for (field, word_count, names, first_id) in [
        (
            "block_states",
            256,
            ["minecraft:stone", "minecraft:water"],
            1,
        ),
        ("biomes", 1, ["minecraft:plains", "minecraft:desert"], 0),
    ] {
        for kind in [
            "list_int",
            "list_double",
            "int_array",
            "byte_array",
            "mixed_numeric",
            "mixed_invalid",
        ] {
            let data = match kind {
                "int_array" => Tag::IntArray(vec![0; word_count]),
                "byte_array" => Tag::ByteArray(vec![0; word_count]),
                "list_double" => Tag::List(vec![Tag::Double(0.9); word_count]),
                _ => {
                    let mut values = vec![Tag::Int(0); word_count];
                    if kind == "mixed_numeric" {
                        values[0] = Tag::Float(-0.9);
                    } else if kind == "mixed_invalid" {
                        values[0] = Tag::String("0".into());
                    }
                    Tag::List(values)
                }
            };
            let palette = Tag::List(
                names
                    .into_iter()
                    .map(|name| Tag::String(name.into()))
                    .collect(),
            );
            let input = collection_root(field, palette, Some(data));
            if kind == "mixed_invalid" {
                assert!(
                    matches!(failure(input), ChunkDecodeError::MissingPackedData),
                    "{field}/{kind}"
                );
                continue;
            }
            let draft = decode(input.clone()).unwrap();
            assert_eq!(draft.root(), &input, "{field}/{kind}");
            assert_eq!(draft.warnings.fallback_palette_entries, 0);
            let section = draft.sections()[0].section.as_ref().unwrap();
            let container = if field == "block_states" {
                &section.blocks
            } else {
                &section.biomes
            };
            let cells = if field == "block_states" { 4096 } else { 64 };
            for index in 0..cells {
                assert_eq!(
                    container.get(index).unwrap(),
                    first_id,
                    "{field}/{kind}/{index}"
                );
            }
        }
    }
}

#[test]
fn numeric_palette_arrays_recover_each_entry_and_empty_arrays_remain_errors() {
    for field in ["block_states", "biomes"] {
        for palette in [
            Tag::ByteArray(vec![0]),
            Tag::IntArray(vec![0]),
            Tag::LongArray(vec![0]),
        ] {
            let input = collection_root(field, palette, None);
            let draft = decode(input.clone()).unwrap();
            assert_eq!(draft.root(), &input);
            assert_eq!(draft.warnings.fallback_palette_entries, 1);
            let section = draft.sections()[0].section.as_ref().unwrap();
            assert_eq!(section.blocks.get(0).unwrap(), 0);
            assert_eq!(section.biomes.get(0).unwrap(), 0);
        }
        for empty in [
            Tag::ByteArray(vec![]),
            Tag::IntArray(vec![]),
            Tag::LongArray(vec![]),
        ] {
            assert!(matches!(
                failure(collection_root(field, empty, None)),
                ChunkDecodeError::EmptyPalette
            ));
        }
        let words = if field == "block_states" { 256 } else { 1 };
        let draft = decode(collection_root(
            field,
            Tag::IntArray(vec![7, -1]),
            Some(Tag::LongArray(vec![0; words])),
        ))
        .unwrap();
        assert_eq!(draft.warnings.fallback_palette_entries, 2);
    }
}

#[test]
fn packed_numeric_stream_uses_number_long_value_truncation_and_saturation() {
    let numbers = [
        (Tag::Double(-0.9), 0),
        (Tag::Double(-1.9), -1),
        (Tag::Double(1.9), 1),
        (Tag::Double(-0.0), 0),
        (Tag::Double(f64::NAN), 0),
        (Tag::Double(f64::INFINITY), i64::MAX),
        (Tag::Double(f64::NEG_INFINITY), i64::MIN),
        (Tag::Double(f64::MAX), i64::MAX),
        (Tag::Double(f64::MIN), i64::MIN),
        (Tag::Double(9_223_372_036_854_775_808.0), i64::MAX),
        (
            Tag::Double(9_223_372_036_854_774_784.0),
            9_223_372_036_854_774_784,
        ),
        (Tag::Double(-9_223_372_036_854_775_808.0), i64::MIN),
        (Tag::Double(-9_223_372_036_854_777_856.0), i64::MIN),
        (Tag::Float(-0.9), 0),
        (Tag::Float(-1.9), -1),
        (Tag::Float(1.9), 1),
        (Tag::Float(f32::NAN), 0),
        (Tag::Float(f32::INFINITY), i64::MAX),
        (Tag::Float(f32::NEG_INFINITY), i64::MIN),
        (Tag::Float(f32::MAX), i64::MAX),
        (Tag::Float(f32::MIN), i64::MIN),
        (
            Tag::Float(9_223_371_487_098_961_920.0),
            9_223_371_487_098_961_920,
        ),
    ];
    for (case, (number, converted)) in numbers.into_iter().enumerate() {
        for field in ["block_states", "biomes"] {
            let (palette, word_count, bits, base_id, entries) = if field == "block_states" {
                (
                    Tag::List(
                        (0..16)
                            .map(|id| {
                                Tag::Compound(root(&format!(
                                    "{{id:'test:varied',properties:{{value:'{id}'}}}}"
                                )))
                            })
                            .collect(),
                    ),
                    256,
                    4,
                    6,
                    4096,
                )
            } else {
                (
                    Tag::List(vec![
                        Tag::String("minecraft:plains".into()),
                        Tag::String("minecraft:desert".into()),
                    ]),
                    1,
                    1,
                    0,
                    64,
                )
            };
            let mut words = vec![Tag::Long(0); word_count];
            words[0] = number.clone();
            let draft = decode(collection_root(field, palette, Some(Tag::List(words)))).unwrap();
            let section = draft.sections()[0].section.as_ref().unwrap();
            let container = if field == "block_states" {
                &section.blocks
            } else {
                &section.biomes
            };
            for index in 0..entries {
                let index_value = if index < 64 / bits {
                    ((converted as u64 >> (index * bits)) & ((1 << bits) - 1)) as u32
                } else {
                    0
                };
                assert_eq!(
                    container.get(index).unwrap(),
                    base_id + index_value,
                    "{field} case {case}, cell {index}"
                );
            }
        }
    }
}

#[test]
fn current_version_gate_does_not_silently_accept_legacy_or_future_data() {
    for (text, version) in [
        ("{Status:'minecraft:full'}", -1),
        ("{DataVersion:5017,Status:'minecraft:full'}", 5017),
        ("{DataVersion:'5018',Status:'minecraft:full'}", -1),
    ] {
        assert!(matches!(failure(root(text)), ChunkDecodeError::NeedsUpgrade(v) if v == version));
    }
    assert!(matches!(
        failure(root("{DataVersion:5019,Status:'minecraft:full'}")),
        ChunkDecodeError::UnsupportedDataVersion(5019)
    ));
    for text in ["{}", "{Status:4}"] {
        assert!(matches!(
            failure(current(text)),
            ChunkDecodeError::MissingLevelData
        ));
    }
    let result = decode(current("{Status:'minecraft:full'}")).unwrap();
    assert_eq!(result.data_version, 5018);
    assert_eq!(result.status.name(), "minecraft:full");
}

#[test]
fn numeric_defaults_status_fallback_and_negative_coordinates_match_java_semantics() {
    let result = decode(current(
        "{Status:'minecraft:terrain',xPos:1.75d,zPos:-1.75d,LastUpdate:2.9d,InhabitedTime:-2.9d,isLightOn:1b}",
    ))
    .unwrap();
    assert_eq!(result.position, (1, -2));
    assert_eq!((result.last_update, result.inhabited_time), (2, -3));
    assert!(result.light_correct);
    assert_eq!(result.status.name(), "minecraft:terrain");
    assert!(!result.warnings.unknown_status);
    for status in ["minecraft:no_such_status", "minecraft:noise"] {
        let result = decode(current(&format!("{{Status:'{status}'}}"))).unwrap();
        assert_eq!(result.status.name(), "minecraft:empty");
        assert!(result.warnings.unknown_status);
        assert_eq!(result.position, (0, 0));
        assert_eq!((result.last_update, result.inhabited_time), (0, 0));
        assert!(!result.light_correct);
    }
}

#[test]
fn all_current_statuses_accept_default_and_explicit_minecraft_namespaces() {
    for status in [
        "empty",
        "structure_starts",
        "structure_references",
        "biomes",
        "terrain",
        "features",
        "initialize_light",
        "light",
        "spawn",
        "full",
    ] {
        for prefix in ["", ":", "minecraft:"] {
            let input = current(&format!("{{Status:'{prefix}{status}'}}"));
            let draft = decode(input).unwrap();
            assert_eq!(draft.status.name(), format!("minecraft:{status}"));
            assert!(!draft.warnings.unknown_status, "{prefix}{status}");
        }
    }
    for status in ["other:full", "::full", "minecraft:Full", "full:"] {
        let draft = decode(current(&format!("{{Status:'{status}'}}"))).unwrap();
        assert_eq!(draft.status.name(), "minecraft:empty");
        assert!(draft.warnings.unknown_status, "{status}");
    }
}

#[test]
fn retains_all_raw_fields_while_extracting_only_current_sections() {
    let input = current(
        "{Status:'minecraft:full',entities:[1,{id:'minecraft:pig'}],block_entities:[{},2],Heightmaps:{WORLD_SURFACE:[L;1],UNKNOWN:[L;2]},structures:{Starts:{}},block_ticks:[{i:'test:unknown',x:-1,y:0,z:0,t:4}],unknown:{bytes:[B;-128,0,127],nested:[{future:'preserve me'}]},sections:[{Y:0b,extra:{value:99}}]}",
    );
    let result = decode(input.clone()).unwrap();
    assert_eq!(result.root(), &input);
    assert_eq!(result.sections().len(), 1);
    let section = result.sections()[0].section.as_ref().unwrap();
    assert_eq!(section.blocks.get(0).unwrap(), 0);
    assert_eq!(section.biomes.get(0).unwrap(), 0);
    assert_eq!(section.counts.non_empty_blocks, 0);
    assert!(result.retained_bytes() >= size_of::<Compound>());
}

#[test]
fn section_order_duplicates_wrapped_y_and_outside_data_are_retained() {
    let result = decode(current(
        "{Status:'minecraft:full',sections:[1,{Y:252},{Y:256},{Y:0b},{Y:-5b,block_states:{palette:[]}},{Y:20b},'ignored']}",
    ))
    .unwrap();
    let sections = result.sections();
    assert_eq!(
        sections.iter().map(|s| s.y).collect::<Vec<_>>(),
        [-4, 0, 0, -5, 20]
    );
    assert!(sections[..3].iter().all(|s| s.section.is_some()));
    assert!(sections[3..].iter().all(|s| s.section.is_none()));
}

#[test]
fn current_palette_names_properties_and_fallbacks_set_counts() {
    for (entry, id, fallback) in [
        ("'minecraft:stone'", 1, 0),
        ("'minecraft:water'", 2, 0),
        ("{id:'minecraft:oak_log',properties:{axis:'x'}}", 3, 0),
        ("{id:'minecraft:oak_log'}", 4, 0),
        ("{id:'minecraft:oak_log',properties:{axis:'wrong'}}", 4, 1),
        ("{Name:'minecraft:stone'}", 0, 1),
        ("'minecraft:missing'", 0, 1),
    ] {
        let result = decode(current(&format!(
            "{{Status:'full',sections:[{{block_states:{{palette:[{entry}]}},biomes:{{palette:['minecraft:desert']}}}}]}}"
        )))
        .unwrap();
        let section = result.sections()[0].section.as_ref().unwrap();
        assert_eq!(section.blocks.get(0).unwrap(), id, "{entry}");
        assert_eq!(section.blocks.get(4095).unwrap(), id, "{entry}");
        assert_eq!(section.biomes.get(63).unwrap(), 1);
        assert_eq!(
            section.counts.non_empty_blocks,
            if id == 0 { 0 } else { 4096 }
        );
        assert_eq!(section.counts.fluid_blocks, if id == 2 { 4096 } else { 0 });
        assert_eq!(result.warnings.fallback_palette_entries, fallback);
    }
}

#[test]
fn packed_disk_indices_count_air_and_fluid_cells() {
    let indices: Vec<_> = (0..4096).map(|index| index % 3).collect();
    let palette = ["minecraft:air", "minecraft:stone", "minecraft:water"]
        .into_iter()
        .map(|name| Tag::String(name.into()))
        .collect();
    let result = decode(section_root(palette, Some(packed(&indices, 4)))).unwrap();
    let section = result.sections()[0].section.as_ref().unwrap();
    for (index, &expected) in indices.iter().enumerate() {
        assert_eq!(section.blocks.get(index).unwrap(), expected as u32);
    }
    assert_eq!(section.counts.non_empty_blocks, 2730);
    assert_eq!(section.counts.fluid_blocks, 1365);
}

#[test]
fn biome_disk_indices_and_unknown_palette_values_use_the_biome_domain() {
    let indices: Vec<_> = (0..64).map(|index| (index * 7) % 3).collect();
    let mut palette = Compound::new();
    palette
        .insert(
            "palette".into(),
            Tag::List(
                ["test:highland", "minecraft:desert", "minecraft:missing"]
                    .into_iter()
                    .map(|name| Tag::String(name.into()))
                    .collect(),
            ),
        )
        .unwrap();
    palette
        .insert("data".into(), Tag::LongArray(packed(&indices, 2)))
        .unwrap();
    let mut section = Compound::new();
    section
        .insert("biomes".into(), Tag::Compound(palette))
        .unwrap();
    let mut input = current("{Status:'full'}");
    input
        .insert("sections".into(), Tag::List(vec![Tag::Compound(section)]))
        .unwrap();
    let result = decode(input).unwrap();
    let section = result.sections()[0].section.as_ref().unwrap();
    for (index, &palette_index) in indices.iter().enumerate() {
        assert_eq!(section.biomes.get(index).unwrap(), [2, 1, 0][palette_index]);
    }
    assert_eq!(section.blocks.get(0).unwrap(), 0);
    assert_eq!(result.warnings.fallback_palette_entries, 1);
}

#[test]
fn disk_local_palette_thresholds_reencode_high_ids_into_global_storage() {
    for count in [16usize, 17, 32, 33, 64, 65, 128, 129, 256, 257] {
        let palette = (0..count)
            .map(|index| {
                Tag::Compound(root(&format!(
                    "{{id:'test:varied',properties:{{value:'{}'}}}}",
                    1018 - index
                )))
            })
            .collect();
        let indices: Vec<_> = (0..4096).map(|index| index % count).collect();
        let bits = (usize::BITS - (count - 1).leading_zeros()).max(4) as usize;
        let mut data = packed(&indices, bits);
        // Disk values occupy whole-word slots; high and final unused bits are ignored.
        let per_word = 64 / bits;
        for (word_index, word) in data.iter_mut().enumerate() {
            let used = per_word.min(4096 - word_index * per_word) * bits;
            if used < 64 {
                *word = (*word as u64 | (u64::MAX << used)) as i64;
            }
        }
        let result = decode(section_root(palette, Some(data))).unwrap();
        let section = result.sections()[0].section.as_ref().unwrap();
        assert_eq!(
            section.blocks.bits(),
            if count == 257 { 11 } else { bits as u8 }
        );
        for (index, &palette_index) in indices.iter().enumerate() {
            assert_eq!(
                section.blocks.get(index).unwrap(),
                1024 - palette_index as u32
            );
        }
        assert_eq!(section.counts.non_empty_blocks, 4096);
        assert_eq!(section.counts.fluid_blocks, 0);
    }
}

#[test]
fn palette_errors_have_actionable_categories() {
    for (container, expected) in [
        ("{}", "missing_palette"),
        ("{palette:[]}", "empty_palette"),
        (
            "{palette:['minecraft:stone','minecraft:water']}",
            "missing_data",
        ),
        (
            "{palette:['minecraft:stone','minecraft:water'],data:[L;0]}",
            "length",
        ),
    ] {
        let error = failure(current(&format!(
            "{{Status:'full',sections:[{{block_states:{container}}}]}}"
        )));
        assert!(matches!(
            (expected, error),
            ("missing_palette", ChunkDecodeError::MissingPalette)
                | ("empty_palette", ChunkDecodeError::EmptyPalette)
                | ("missing_data", ChunkDecodeError::MissingPackedData)
                | (
                    "length",
                    ChunkDecodeError::PackedLength {
                        expected: 256,
                        actual: 1
                    }
                )
        ));
    }
    let palette = vec![
        Tag::String("minecraft:stone".into()),
        Tag::String("minecraft:water".into()),
    ];
    let mut data = vec![0; 256];
    data[0] = 2;
    assert!(matches!(
        failure(section_root(palette, Some(data))),
        ChunkDecodeError::PaletteIndex(2)
    ));
    let result = decode(section_root(
        vec![Tag::String("minecraft:stone".into())],
        Some(vec![999]),
    ))
    .unwrap();
    assert_eq!(
        result.sections()[0]
            .section
            .as_ref()
            .unwrap()
            .blocks
            .get(0)
            .unwrap(),
        1
    );
}

#[test]
fn light_arrays_are_owned_and_validated_even_outside_dimension() {
    for y in [-5, -4, 19, 20] {
        let mut section = root(&format!("{{Y:{y}b}}"));
        section
            .insert("BlockLight".into(), Tag::ByteArray(vec![-86; 2048]))
            .unwrap();
        section
            .insert("SkyLight".into(), Tag::ByteArray(vec![85; 2048]))
            .unwrap();
        let mut input = current("{Status:'full',isLightOn:1b}");
        input
            .insert("sections".into(), Tag::List(vec![Tag::Compound(section)]))
            .unwrap();
        let result = decode(input.clone()).unwrap();
        assert_eq!(result.root(), &input);
        assert_eq!(
            result.sections()[0].section.is_some(),
            (-4..=19).contains(&y)
        );
        assert_eq!(
            result.sections()[0].block_light.as_deref(),
            Some(&[0xaa; 2048][..])
        );
        assert_eq!(
            result.sections()[0].sky_light.as_deref(),
            Some(&[0x55; 2048][..])
        );
        assert!(matches!(
            failure(current(&format!(
                "{{Status:'full',sections:[{{Y:{y}b,BlockLight:[B;0]}}]}}"
            ))),
            ChunkDecodeError::LightLength(1)
        ));
    }
}

#[test]
fn independent_nbt_and_typed_allocation_limits_reject_before_returning_a_draft() {
    let input = current("{Status:'full',sections:[{Y:0b}]}");
    let height = DimensionHeight::new(-64, 384).unwrap();
    assert!(matches!(
        decode_current_chunk(
            &mut bytes(input.clone()),
            registries(),
            height,
            Limits::default(),
            0
        ),
        Err(ChunkDecodeError::AllocationLimit)
    ));
    let small_nbt = Limits {
        allocation_bytes: 1,
        ..Limits::default()
    };
    assert!(matches!(
        decode_current_chunk(
            &mut bytes(input.clone()),
            registries(),
            height,
            small_nbt,
            DECODED_BYTES
        ),
        Err(ChunkDecodeError::Nbt(nbt::Error::AllocationBudgetExceeded))
    ));
    let mut network = Vec::new();
    nbt::write_network(
        &Tag::Compound(input.clone()),
        &mut network,
        Limits::default(),
    )
    .unwrap();
    let (_, nbt_bytes) =
        nbt::read_network_accounted(&mut network.as_slice(), Limits::default()).unwrap();
    let result = decode(input).unwrap();
    assert_eq!(
        result.retained_bytes(),
        nbt_bytes + size_of::<arrow_mc::world::storage::chunk::StoredSection>()
    );

    let palette = vec![
        Tag::String("minecraft:stone".into()),
        Tag::String("minecraft:water".into()),
    ];
    let indices: Vec<_> = (0..4096).map(|index| index % 2).collect();
    let input = section_root(palette, Some(packed(&indices, 4)));
    // Permit the typed section and palette IDs, but not its packed storage.
    let insufficient = size_of::<arrow_mc::world::storage::chunk::StoredSection>() + 8;
    assert!(matches!(
        decode_current_chunk(
            &mut bytes(input),
            registries(),
            height,
            Limits::default(),
            insufficient
        ),
        Err(ChunkDecodeError::Section(
            arrow_mc::world::section::Error::AllocationBudgetExceeded
        ))
    ));
}

#[test]
fn named_root_is_skipped_without_utf_validation_and_tail_is_not_drained() {
    let input = current("{Status:'full',unknown:7}");
    let mut network = Vec::new();
    nbt::write_network(
        &Tag::Compound(input.clone()),
        &mut network,
        Limits::default(),
    )
    .unwrap();
    let mut named = vec![10, 0, 2, 0xff, 0xff];
    named.extend_from_slice(&network[1..]);
    named.extend_from_slice(&[0xff, 0xff]);
    let result = decode_current_chunk(
        &mut named,
        registries(),
        DimensionHeight::new(-64, 384).unwrap(),
        Limits::default(),
        DECODED_BYTES,
    )
    .unwrap();
    assert_eq!(result.root(), &input);
    for bad in [vec![], vec![10], vec![10, 0], vec![10, 0, 3, 0]] {
        assert!(matches!(
            decode_current_chunk(
                &mut bad.clone(),
                registries(),
                DimensionHeight::new(-64, 384).unwrap(),
                Limits::default(),
                DECODED_BYTES
            ),
            Err(ChunkDecodeError::Truncated)
        ));
    }
    assert!(matches!(
        decode_current_chunk(
            &mut vec![3, 0, 0, 0, 0, 0, 1],
            registries(),
            DimensionHeight::new(-64, 384).unwrap(),
            Limits::default(),
            DECODED_BYTES
        ),
        Err(ChunkDecodeError::RootType)
    ));
}

#[test]
fn dimension_height_requires_section_alignment_and_signed_section_range() {
    let height = DimensionHeight::new(-64, 384).unwrap();
    assert_eq!((height.min_section(), height.max_section()), (-4, 19));
    for (min, height) in [(-63, 384), (-64, 383), (-64, 0), (-2064, 16), (2032, 32)] {
        assert!(matches!(
            DimensionHeight::new(min, height),
            Err(ChunkDecodeError::InvalidHeight)
        ));
    }
    let full = DimensionHeight::new(-2048, 4096).unwrap();
    assert!(full.contains(-128));
    assert!(full.contains(127));
}
