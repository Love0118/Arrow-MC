//! Opt-in heightmaps against actual Vanilla ProtoChunk and bound tag predicates.
//!
//! Set ARROW_MC_JAVA_REFERENCE_ROOT to the sibling Decompile directory and run
//! cargo test --test world_heightmap_java_oracle -- --ignored --nocapture.
//! The embedded API driver writes only synthetic operation observations; official
//! JARs, registry data, and generated results remain local and are not bundled.

#[path = "common/world_registry_fixture.rs"]
mod fixture;

use arrow_mc::{
    server::configuration_data::parse_sha256,
    world::{
        heightmap::{Heightmap, HeightmapKind, HeightmapSource, RestoreOutcome, required_mask},
        section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        storage::{
            chunk::{ChunkStatus, DimensionHeight},
            registry::{ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits},
        },
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf, process::Command, time::SystemTime};

const ORACLE: &str = r#"import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.tags.*;
import net.minecraft.util.Util;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;

/** Independently authored public-API driver; no server, fake level, or world files. */
public class HeightmapOracle {
    static { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().setPrettyPrinting().create();
    static final EnumSet<Heightmap.Types> ALL = EnumSet.allOf(Heightmap.Types.class);
    static final JsonArray SCENARIOS = new JsonArray();
    static final JsonObject PROFILES = new JsonObject();
    static final JsonArray STATE_MASKS = new JsonArray();
    static PalettedContainerFactory factory;
    static ProtoChunk chunk;
    static JsonArray operations;
    static String profile = "vanilla";
    static final BlockState AIR = Blocks.AIR.defaultBlockState();
    static final BlockState STONE = Blocks.STONE.defaultBlockState();
    static final BlockState WATER = Blocks.WATER.defaultBlockState();
    static final BlockState LEAVES = Blocks.OAK_LEAVES.defaultBlockState();
    static final BlockState CAVE = Blocks.CAVE_AIR.defaultBlockState();

    static JsonArray types(Set<Heightmap.Types> values) {
        JsonArray result = new JsonArray();
        for (Heightmap.Types value : values) result.add(value.getSerializationKey());
        return result;
    }

    static JsonArray raw(long[] values) {
        JsonArray result = new JsonArray();
        for (long value : values) result.add(String.format("%016x", value));
        return result;
    }

    static JsonArray snapshot() {
        JsonArray result = new JsonArray();
        for (Heightmap.Types type : ALL) {
            Heightmap map = chunk.getOrCreateHeightmapUnprimed(type);
            JsonObject item = new JsonObject();
            item.addProperty("type", type.getSerializationKey());
            item.add("raw", raw(map.getRawData()));
            JsonArray first = new JsonArray();
            JsonArray highest = new JsonArray();
            for (int index = 0; index < 256; index++) {
                first.add(map.getFirstAvailable(index & 15, index >> 4));
                highest.add(map.getHighestTaken(index & 15, index >> 4));
            }
            item.add("first_available", first);
            item.add("highest_taken", highest);
            result.add(item);
        }
        return result;
    }

    static void begin(String name, int minY, int height) {
        chunk = new ProtoChunk(new ChunkPos(0, 0), UpgradeData.EMPTY,
            LevelHeightAccessor.create(minY, height), factory, null);
        operations = new JsonArray();
        JsonObject scenario = new JsonObject();
        scenario.addProperty("name", name);
        scenario.addProperty("tag_profile", profile);
        scenario.addProperty("min_y", minY);
        scenario.addProperty("height", height);
        scenario.add("operations", operations);
        scenario.add("initial", snapshot());
        SCENARIOS.add(scenario);
    }

    // Mutate real sections without ProtoChunk's automatic heightmap hook, so the
    // public Heightmap.update return value is observed exactly once per mutation.
    static void put(int x, int y, int z, BlockState state) {
        BlockState old = chunk.getSection(chunk.getSectionIndex(y)).setBlockState(x, y & 15, z, state);
        JsonObject operation = new JsonObject();
        operation.addProperty("op", "put");
        operation.addProperty("x", x);
        operation.addProperty("y", y);
        operation.addProperty("z", z);
        operation.addProperty("state", Block.getId(state));
        operation.addProperty("old_state", Block.getId(old));
        operation.addProperty("chunk_visible_state", Block.getId(chunk.getBlockState(new BlockPos(x, y, z))));
        operations.add(operation);
    }

    static void prime(String label) {
        Heightmap.primeHeightmaps(chunk, ALL);
        JsonObject operation = new JsonObject();
        operation.addProperty("op", "prime");
        operation.addProperty("label", label);
        operation.add("types", types(ALL));
        operation.add("maps", snapshot());
        operations.add(operation);
    }

    static void update(String label, int x, int y, int z, BlockState state) {
        put(x, y, z, state);
        JsonObject operation = new JsonObject();
        operation.addProperty("op", "update");
        operation.addProperty("label", label);
        operation.addProperty("x", x);
        operation.addProperty("y", y);
        operation.addProperty("z", z);
        operation.addProperty("state", Block.getId(state));
        JsonObject changed = new JsonObject();
        for (Heightmap.Types type : ALL) {
            changed.addProperty(type.getSerializationKey(), chunk.getOrCreateHeightmapUnprimed(type).update(x, y, z, state));
        }
        operation.add("changed", changed);
        operation.add("maps", snapshot());
        operations.add(operation);
    }

    static void loadRaw(String label, Heightmap.Types type, long[] input) {
        JsonObject operation = new JsonObject();
        operation.addProperty("op", "raw");
        operation.addProperty("label", label);
        operation.addProperty("type", type.getSerializationKey());
        operation.add("input", raw(input));
        Heightmap map = chunk.getOrCreateHeightmapUnprimed(type);
        map.setRawData(chunk, type, input);
        long[] beforeMutation = map.getRawData().clone();
        if (input.length != 0) input[0] ^= -1L;
        operation.addProperty("input_copied", Arrays.equals(beforeMutation, map.getRawData()));
        operation.add("maps", snapshot());
        operations.add(operation);
    }

    static void stateProfile(String name) {
        JsonArray result = new JsonArray();
        List<BlockState> states = List.of(AIR, CAVE, Blocks.VOID_AIR.defaultBlockState(), STONE, WATER,
            Blocks.LAVA.defaultBlockState(), LEAVES, LEAVES.setValue(BlockStateProperties.WATERLOGGED, true), Blocks.SHORT_GRASS.defaultBlockState(),
            Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.WATERLOGGED, true));
        for (BlockState state : states) {
            JsonObject item = new JsonObject();
            item.addProperty("id", Block.getId(state));
            item.addProperty("description", state.toString());
            item.addProperty("is_air", state.isAir());
            item.addProperty("is_air_block", state.is(Blocks.AIR));
            item.addProperty("has_fluid", !state.getFluidState().isEmpty());
            JsonObject matches = new JsonObject();
            for (Heightmap.Types type : ALL) matches.addProperty(type.getSerializationKey(), type.isOpaque().test(state));
            item.add("matches", matches);
            result.add(item);
        }
        PROFILES.add(name, result);
    }

    static void ordinaryCases() {
        begin("layered_updates", -64, 384);
        put(0, -64, 0, STONE);
        put(0, -5, 0, STONE);
        put(0, 0, 0, WATER);
        put(0, 5, 0, LEAVES);
        put(0, 9, 0, Blocks.SHORT_GRASS.defaultBlockState());
        put(0, 12, 0, CAVE);
        put(0, 13, 0, Blocks.VOID_AIR.defaultBlockState());
        put(1, 319, 0, STONE);
        put(2, 319, 0, CAVE);
        prime("layered");
        update("below_top", 0, -64, 0, WATER);
        update("remove_grass", 0, 9, 0, AIR);
        update("same_opaque", 0, -5, 0, STONE);
        update("remove_leaves", 0, 5, 0, AIR);
        update("remove_water", 0, 0, 0, AIR);
        update("remove_stone", 0, -5, 0, AIR);
        update("remove_bottom", 0, -64, 0, AIR);
        update("insert_bottom_water", 0, -64, 0, WATER);
        update("insert_top_leaves", 0, 319, 0, LEAVES);
        update("same_top_leaves", 0, 319, 0, LEAVES);
        update("replace_top_stone", 0, 319, 0, STONE);
        update("remove_top", 0, 319, 0, AIR);
        prime("reprime_after_updates");

        begin("all_columns_index_order", -64, 384);
        for (int index = 0; index < 256; index++) {
            int y = -64 + (index * 37) % 384;
            put(index & 15, y, index >> 4, index % 3 == 0 ? WATER : index % 3 == 1 ? LEAVES : STONE);
        }
        prime("all_columns");

        begin("reprime_retains_unmatched_columns", -64, 384);
        put(0, 7, 0, STONE);
        prime("nonempty");
        put(0, 7, 0, AIR);
        prime("now_empty_no_reset");
        for (Heightmap.Types type : ALL) loadRaw("wrong_length_empty", type, new long[0]);
        put(1, 15, 0, STONE);
        for (Heightmap.Types type : ALL) loadRaw("wrong_length_mixed", type, new long[1]);

        for (int[] dimension : List.of(new int[]{0, 16}, new int[]{-128, 256}, new int[]{0, 512}, new int[]{-2048, 4096})) {
            begin("dimension_"+dimension[0]+"_"+dimension[1], dimension[0], dimension[1]);
            put(0, dimension[0], 0, STONE);
            put(15, dimension[0]+dimension[1]-1, 15, STONE);
            prime("min_max");
            update("remove_bottom", 0, dimension[0], 0, AIR);
            update("remove_top", 15, dimension[0]+dimension[1]-1, 15, AIR);
        }

        begin("raw_copy_padding_and_out_of_range", -64, 384);
        for (Heightmap.Types type : ALL) {
            int size = chunk.getOrCreateHeightmapUnprimed(type).getRawData().length;
            long[] data = new long[size];
            Arrays.fill(data, -1L);
            loadRaw("all_bits_set", type, data);
        }
        prime("empty_reprime_preserves_raw");
        put(3, -10, 7, STONE);
        prime("one_column_reprime");
        for (Heightmap.Types type : ALL) loadRaw("wrong_length_reprime", type, new long[2]);
    }

    static void customTagCases() {
        Registry<Block> registry = BuiltInRegistries.BLOCK;
        Map<TagKey<Block>, List<Holder<Block>>> saved = new HashMap<>();
        registry.listTags().forEach(tag -> saved.put(tag.key(), tag.stream().toList()));
        Map<TagKey<Block>, List<Holder<Block>>> patched = new HashMap<>(saved);
        for (TagKey<Block> key : List.of(BlockTags.BLOCKS_MOTION_IN_HEIGHTMAP, BlockTags.BLOCKS_MOTION_IN_HEIGHTMAP_NO_LEAVES)) {
            List<Holder<Block>> entries = new ArrayList<>(saved.get(key));
            entries.add(registry.wrapAsHolder(Blocks.AIR));
            entries.add(registry.wrapAsHolder(Blocks.CAVE_AIR));
            patched.put(key, entries);
        }
        registry.prepareTagReload(new TagLoader.LoadResult<>(registry.key(), patched)).apply();
        try {
            profile = "air_and_cave_block_motion";
            stateProfile(profile);
            begin("custom_tags_prime_air_shortcut", -64, 384);
            // A real non-air keeps this section visible, exposing CAVE_AIR rather
            // than ProtoChunk's all-air section fast path.
            put(1, 0, 0, STONE);
            put(0, 5, 0, CAVE);
            prime("prime_skips_air_but_matches_cave");
            update("update_air_uses_tag_predicate", 0, 10, 0, AIR);
            update("remove_tagged_air_with_grass", 0, 10, 0, Blocks.SHORT_GRASS.defaultBlockState());

            begin("custom_tags_all_air_section", -64, 384);
            put(0, 5, 0, CAVE);
            prime("cave_hidden_by_empty_section");
            update("cave_argument_still_matches", 0, 5, 0, CAVE);
        } finally {
            registry.prepareTagReload(new TagLoader.LoadResult<>(registry.key(), saved)).apply();
            profile = "vanilla";
        }
    }

    public static void main(String[] args) throws Exception {
        Bootstrap.bootStrap();
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var setup = new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false),
            Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        try (ExecutorService worker = Executors.newFixedThreadPool(2)) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(setup,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, config) -> {
                    try (resources) {
                        factory = PalettedContainerFactory.create(registries.compositeAccess());
                        stateProfile("vanilla");
                        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
                            int mask = 0;
                            for (Heightmap.Types type : ALL) if (type.isOpaque().test(state)) mask |= 1 << type.ordinal();
                            STATE_MASKS.add(mask);
                        }
                        if (!STONE.is(BlockTags.BLOCKS_MOTION_IN_HEIGHTMAP)) throw new AssertionError("static tags not bound");
                        ordinaryCases();
                        customTagCases();
                        return true;
                    }
                }, worker, Runnable::run).join();
            JsonObject output = new JsonObject();
            output.addProperty("version", SharedConstants.getCurrentVersion().id());
            output.addProperty("data_version", SharedConstants.getCurrentVersion().dataVersion().version());
            output.add("state_profiles", PROFILES);
            output.add("state_masks", STATE_MASKS);
            JsonArray statuses = new JsonArray();
            for (ChunkStatus status : List.of(ChunkStatus.EMPTY, ChunkStatus.STRUCTURE_STARTS, ChunkStatus.STRUCTURE_REFERENCES,
                    ChunkStatus.BIOMES, ChunkStatus.TERRAIN, ChunkStatus.FEATURES, ChunkStatus.INITIALIZE_LIGHT,
                    ChunkStatus.LIGHT, ChunkStatus.SPAWN, ChunkStatus.FULL)) {
                JsonObject item = new JsonObject();
                item.addProperty("status", status.getName());
                item.add("heightmaps", types(status.heightmapsAfter()));
                statuses.add(item);
            }
            output.add("statuses", statuses);
            JsonArray metadata = new JsonArray();
            for (Heightmap.Types type : ALL) {
                JsonObject item = new JsonObject();
                item.addProperty("type", type.getSerializationKey());
                item.addProperty("send_to_client", type.sendToClient());
                item.addProperty("keep_after_worldgen", type.keepAfterWorldgen());
                metadata.add(item);
            }
            output.add("types", metadata);
            output.add("scenarios", SCENARIOS);
            Files.writeString(Path.of(args[0]), JSON.toJson(output)+"\n");
        } finally { Util.shutdownExecutors(); }
    }
}
"#;

fn source<'a>(
    registry: &'a ChunkRegistrySnapshot,
    height: DimensionHeight,
    sections: &'a [Option<Section>],
) -> HeightmapSource<'a> {
    let borrowed: Vec<_> = sections.iter().map(Option::as_ref).collect();
    HeightmapSource::from_sections(registry, height, &borrowed).unwrap()
}

fn verify_maps(maps: &[Heightmap], expected: &Value, label: &str) {
    assert_eq!(maps.len(), expected.as_array().unwrap().len(), "{label}");
    for (map, row) in maps.iter().zip(expected.as_array().unwrap()) {
        assert_eq!(map.kind().serialization_key(), row["type"], "{label}");
        let words: Vec<_> = row["raw"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| u64::from_str_radix(word.as_str().unwrap(), 16).unwrap())
            .collect();
        assert_eq!(
            map.raw(),
            words,
            "{label}/{} raw",
            map.kind().serialization_key()
        );
        for index in 0..256 {
            let x = (index & 15) as u8;
            let z = (index >> 4) as u8;
            assert_eq!(
                i64::from(map.first_available(x, z).unwrap()),
                row["first_available"][index].as_i64().unwrap(),
                "{label}/{}/column{index} first",
                map.kind().serialization_key()
            );
            assert_eq!(
                i64::from(map.highest_taken(x, z).unwrap()),
                row["highest_taken"][index].as_i64().unwrap(),
                "{label}/{}/column{index} highest",
                map.kind().serialization_key()
            );
        }
    }
}

fn verify_profile(registry: &ChunkRegistrySnapshot, expected: &Value) {
    for state in expected.as_array().unwrap() {
        let id = state["id"].as_u64().unwrap() as u32;
        let flags = registry.state_flags(id).unwrap();
        assert_eq!(flags.is_air, state["is_air"].as_bool().unwrap());
        assert_eq!(flags.has_fluid, state["has_fluid"].as_bool().unwrap());
        assert_eq!(
            id == registry.air_id(),
            state["is_air_block"].as_bool().unwrap()
        );
        let mask = registry.heightmap_mask(id).unwrap();
        for kind in HeightmapKind::ALL {
            assert_eq!(
                mask & (1 << kind.id()) != 0,
                state["matches"][kind.serialization_key()]
                    .as_bool()
                    .unwrap(),
                "state {id}/{}",
                kind.serialization_key()
            );
        }
    }
}

fn replay(registry: &ChunkRegistrySnapshot, scenario: &Value) -> usize {
    let min_y = scenario["min_y"].as_i64().unwrap() as i32;
    let height_blocks = scenario["height"].as_u64().unwrap() as u32;
    let height = DimensionHeight::new(min_y, height_blocks).unwrap();
    let mut sections: Vec<Option<Section>> = (0..height_blocks / 16).map(|_| None).collect();
    let initial_source = source(registry, height, &sections);
    let mut maps: Vec<_> = HeightmapKind::ALL
        .into_iter()
        .map(|kind| Heightmap::new(kind, &initial_source, 4096).unwrap())
        .collect();
    let scenario_name = scenario["name"].as_str().unwrap();
    verify_maps(&maps, &scenario["initial"], scenario_name);
    let mut checked_snapshots = 1;
    for (operation_index, operation) in scenario["operations"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let label = format!("{scenario_name}/{operation_index}/{}", operation["label"]);
        match operation["op"].as_str().unwrap() {
            "put" => {
                let x = operation["x"].as_u64().unwrap() as usize;
                let y = operation["y"].as_i64().unwrap() as i32;
                let z = operation["z"].as_u64().unwrap() as usize;
                let id = operation["state"].as_u64().unwrap() as u32;
                let index = ((y - min_y) / 16) as usize;
                let section = sections[index].get_or_insert_with(|| Section {
                    counts: SectionCounts {
                        non_empty_blocks: 0,
                        fluid_blocks: 0,
                    },
                    blocks: PalettedContainer::single(
                        ContainerKind::Blocks,
                        registry.block_registry(),
                        registry.air_id(),
                    )
                    .unwrap(),
                    biomes: PalettedContainer::single(
                        ContainerKind::Biomes,
                        registry.biome_registry(),
                        registry.plains_id(),
                    )
                    .unwrap(),
                });
                let cell = x + 16 * z + 256 * y.rem_euclid(16) as usize;
                let old = section.blocks.set(cell, id, 1 << 20).unwrap();
                assert_eq!(
                    u64::from(old),
                    operation["old_state"].as_u64().unwrap(),
                    "{label}"
                );
                let old_flags = registry.state_flags(old).unwrap();
                let flags = registry.state_flags(id).unwrap();
                section.counts.non_empty_blocks -= u16::from(!old_flags.is_air);
                section.counts.non_empty_blocks += u16::from(!flags.is_air);
                section.counts.fluid_blocks -= u16::from(!old_flags.is_air && old_flags.has_fluid);
                section.counts.fluid_blocks += u16::from(!flags.is_air && flags.has_fluid);
                let visible = if section.counts.non_empty_blocks == 0 {
                    registry.air_id()
                } else {
                    id
                };
                assert_eq!(
                    u64::from(visible),
                    operation["chunk_visible_state"].as_u64().unwrap(),
                    "{label}"
                );
            }
            "prime" => {
                let source = source(registry, height, &sections);
                for map in &mut maps {
                    map.prime(&source).unwrap();
                }
            }
            "update" => {
                let source = source(registry, height, &sections);
                for map in &mut maps {
                    let changed = map
                        .update(
                            operation["x"].as_u64().unwrap() as u8,
                            operation["y"].as_i64().unwrap() as i32,
                            operation["z"].as_u64().unwrap() as u8,
                            operation["state"].as_u64().unwrap() as u32,
                            &source,
                        )
                        .unwrap();
                    assert_eq!(
                        changed,
                        operation["changed"][map.kind().serialization_key()]
                            .as_bool()
                            .unwrap(),
                        "{label}/{}",
                        map.kind().serialization_key()
                    );
                }
            }
            "raw" => {
                let source = source(registry, height, &sections);
                let map = maps
                    .iter_mut()
                    .find(|map| {
                        map.kind().serialization_key() == operation["type"].as_str().unwrap()
                    })
                    .unwrap();
                let mut input: Vec<_> = operation["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|word| u64::from_str_radix(word.as_str().unwrap(), 16).unwrap())
                    .collect();
                let expected = if input.len() == map.raw().len() {
                    RestoreOutcome::Restored
                } else {
                    RestoreOutcome::Reprimed
                };
                assert_eq!(map.restore(&input, &source).unwrap(), expected, "{label}");
                let before = map.raw().to_vec();
                if let Some(word) = input.first_mut() {
                    *word ^= u64::MAX;
                }
                assert_eq!(map.raw(), before, "{label}: caller input must be copied");
                assert_eq!(operation["input_copied"], true);
            }
            unknown => panic!("unknown oracle operation {unknown}"),
        }
        if !operation["maps"].is_null() {
            verify_maps(&maps, &operation["maps"], &label);
            checked_snapshots += 1;
        }
    }
    checked_snapshots
}

#[test]
#[ignore = "requires Java 25, pinned server JAR and version-2 block-state metadata"]
fn matches_actual_java_heightmap_operations_and_bound_predicates() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT"),
    );
    let snapshot = env::var_os("ARROW_BLOCK_STATE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| reference.join("bootstrap/26.3-pre-2-block-states-v2"));
    let manifest = env::var("ARROW_BLOCK_STATE_MANIFEST_SHA256").unwrap_or_else(|_| {
        "ac40352daeef56d8a273116f9573d1684c0e13c96e5d93e485900b4a021c5557".into()
    });
    let config = env::var("ARROW_CONFIGURATION_MANIFEST_SHA256").unwrap_or_else(|_| {
        "105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198".into()
    });
    let expected = ExpectedRegistryReference {
        manifest_sha256: parse_sha256(&manifest).unwrap(),
        configuration_manifest_sha256: parse_sha256(&config).unwrap(),
        source_jar_sha256: parse_sha256(
            "18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a",
        )
        .unwrap(),
        source_jar_bytes: 26_649_663,
    };
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let jar = artifacts.join("server-26.3-pre-2.jar");
    let jar_bytes = fs::read(&jar).unwrap();
    assert_eq!(jar_bytes.len() as u64, expected.source_jar_bytes);
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(&jar_bytes)),
        expected.source_jar_sha256
    );
    drop(jar_bytes);
    let vanilla =
        ChunkRegistrySnapshot::load(&snapshot, &expected, RegistryLoadLimits::default()).unwrap();

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-heightmap-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let oracle = directory.join("HeightmapOracle.java");
    fs::write(&oracle, ORACLE).unwrap();
    let output_path = directory.join("observations.json");
    let execution = Command::new("java")
        .arg("-Xmx1G")
        .arg("--class-path")
        .arg(env::join_paths([jar, artifacts.join("libraries/*")]).unwrap())
        .arg(&oracle)
        .arg(&output_path)
        .current_dir(&directory)
        .output()
        .unwrap();
    assert!(
        execution.status.success(),
        "Java oracle failed:\n{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    let observations: Value = serde_json::from_slice(&fs::read(&output_path).unwrap()).unwrap();
    assert_eq!(observations["version"], "26.3-pre-2");
    assert_eq!(observations["data_version"], 5018);
    let masks = observations["state_masks"].as_array().unwrap();
    assert_eq!(masks.len(), vanilla.state_count() as usize);
    for (id, mask) in masks.iter().enumerate() {
        assert_eq!(
            u64::from(vanilla.heightmap_mask(id as u32).unwrap()),
            mask.as_u64().unwrap(),
            "bound state predicate {id}"
        );
    }
    verify_profile(&vanilla, &observations["state_profiles"]["vanilla"]);

    // Explicitly re-authenticate a local test-only tag variation. Production
    // callers still require their independently supplied manifest anchors.
    let mut blocks = fixture::json_file(&snapshot.join("blocks.json"));
    for block in blocks["blocks"].as_array_mut().unwrap() {
        if ["minecraft:air", "minecraft:cave_air"].contains(&block["id"].as_str().unwrap()) {
            block["heightmap_tags"] = serde_json::json!(3);
        }
    }
    let custom =
        fixture::Fixture::from_data(blocks, fixture::json_file(&snapshot.join("biomes.json")))
            .load();
    verify_profile(
        &custom,
        &observations["state_profiles"]["air_and_cave_block_motion"],
    );
    for (kind, row) in HeightmapKind::ALL
        .into_iter()
        .zip(observations["types"].as_array().unwrap())
    {
        assert_eq!(kind.serialization_key(), row["type"]);
        assert_eq!(
            kind.send_to_client(),
            row["send_to_client"].as_bool().unwrap()
        );
        assert_eq!(
            kind.keep_after_worldgen(),
            row["keep_after_worldgen"].as_bool().unwrap()
        );
    }
    for (status, row) in [
        ChunkStatus::Empty,
        ChunkStatus::StructureStarts,
        ChunkStatus::StructureReferences,
        ChunkStatus::Biomes,
        ChunkStatus::Terrain,
        ChunkStatus::Features,
        ChunkStatus::InitializeLight,
        ChunkStatus::Light,
        ChunkStatus::Spawn,
        ChunkStatus::Full,
    ]
    .into_iter()
    .zip(observations["statuses"].as_array().unwrap())
    {
        assert_eq!(status.name(), row["status"]);
        let expected = row["heightmaps"]
            .as_array()
            .unwrap()
            .iter()
            .fold(0u8, |mask, name| {
                mask | (1
                    << HeightmapKind::ALL
                        .iter()
                        .find(|kind| kind.serialization_key() == name.as_str().unwrap())
                        .unwrap()
                        .id())
            });
        assert_eq!(required_mask(status), expected, "{}", status.name());
    }
    let mut checked_snapshots = 0;
    let scenarios = observations["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 10);
    for scenario in scenarios {
        let registry = if scenario["tag_profile"] == "vanilla" {
            &vanilla
        } else {
            &custom
        };
        checked_snapshots += replay(registry, scenario);
    }
    assert_eq!(checked_snapshots, 70);
    // Keep failed runs for diagnosis; clean only this verified temporary root.
    let resolved = fs::canonicalize(&directory).unwrap();
    let temporary = fs::canonicalize(env::temp_dir()).unwrap();
    assert_eq!(resolved.parent(), Some(temporary.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("arrow-heightmap-oracle-")
    );
    fs::remove_dir_all(resolved).unwrap();
    eprintln!(
        "Compared {} bound state predicates and {checked_snapshots} full snapshots of all six heightmaps across {} actual ProtoChunk scenarios",
        masks.len(),
        scenarios.len()
    );
}
