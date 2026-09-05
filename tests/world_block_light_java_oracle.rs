//! Opt-in block-light comparison against the actual locked server JAR.
//! The independent observer uses real ProtoChunks and public light-engine APIs;
//! it contains no Vanilla implementation bodies or generated game resources.

use arrow_mc::{
    server::configuration_data::parse_sha256,
    world::{
        lighting::{
            LightBlock, LightKind, LightSection, LightingChunk, LightingSource, SourceLimits,
            block::{BlockLightEngine, BlockLightLimits},
            storage::{LightSectionStorage, SectionType, StorageLimits},
        },
        preparation::ChunkAddress,
        section::{ContainerKind, PalettedContainer, Section, SectionCounts},
        storage::chunk::DimensionHeight,
        storage::registry::{
            ChunkRegistrySnapshot, EMPTY_FACE, ExpectedRegistryReference, RegistryLoadLimits,
        },
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::SystemTime,
};

const ORACLE: &str = r#"import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.util.Util;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.lighting.*;
import net.minecraft.world.phys.shapes.Shapes;

class BlockLightOracle {
    static { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().create();
    static final JsonArray SCENARIOS = new JsonArray();
    static final LinkedHashMap<String, BlockState> STATES = new LinkedHashMap<>();
    static final Direction[] DIRECTIONS = Direction.values();
    static PalettedContainerFactory factory;
    static World world;
    static JsonArray operations;

    static final class World implements LightChunkGetter {
        final LinkedHashMap<Long, ProtoChunk> chunks = new LinkedHashMap<>();
        final LevelHeightAccessor height = LevelHeightAccessor.create(0, 32);
        final LinkedHashSet<SectionPos> active = new LinkedHashSet<>();
        final TreeSet<SectionPos> stored = new TreeSet<>(Comparator.<SectionPos>comparingInt(SectionPos::x)
            .thenComparingInt(SectionPos::y).thenComparingInt(SectionPos::z));
        LevelLightEngine engine;
        public LightChunk getChunkForLighting(int x, int z) { return chunks.get(ChunkPos.pack(x, z)); }
        public BlockGetter getLevel() { return chunks.values().iterator().next(); }
    }

    static JsonObject operation(String kind, String label) {
        JsonObject operation = new JsonObject();
        operation.addProperty("op", kind);
        operation.addProperty("label", label);
        operations.add(operation);
        return operation;
    }
    static void position(JsonObject output, int x, int y, int z) {
        output.addProperty("x", x); output.addProperty("y", y); output.addProperty("z", z);
    }
    static void begin(String name, int firstX, int lastX) {
        world = new World();
        operations = new JsonArray();
        JsonObject scenario = new JsonObject();
        scenario.addProperty("name", name);
        scenario.addProperty("min_y", 0);
        scenario.addProperty("height", 32);
        scenario.add("operations", operations);
        SCENARIOS.add(scenario);
        for (int x = firstX; x <= lastX; x++) {
            world.chunks.put(ChunkPos.pack(x, 0), new ProtoChunk(new ChunkPos(x, 0), UpgradeData.EMPTY,
                world.height, factory, null));
            JsonObject added = operation("chunk", "available");
            added.addProperty("x", x); added.addProperty("z", 0);
        }
        world.engine = new LevelLightEngine(world, true, false);
        for (int x = firstX; x <= lastX; x++) for (int y = 0; y < 2; y++) {
            SectionPos section = SectionPos.of(x, y, 0);
            world.active.add(section);
            world.engine.updateSectionStatus(section, false);
            JsonObject added = operation("section", "stored");
            position(added, x, y, 0);
            for (int dx = -1; dx <= 1; dx++) for (int dy = -1; dy <= 1; dy++) for (int dz = -1; dz <= 1; dz++) {
                world.stored.add(SectionPos.of(x + dx, y + dy, dz));
            }
        }
        snapshot("initialized");
    }
    static void put(String label, int x, int y, int z, String stateName) {
        BlockState state = STATES.get(stateName);
        ProtoChunk chunk = world.chunks.get(ChunkPos.pack(x >> 4, z >> 4));
        if (chunk == null) throw new AssertionError("mutation outside available chunk");
        BlockState old = chunk.getSection(chunk.getSectionIndex(y)).setBlockState(x & 15, y & 15, z & 15, state);
        JsonObject operation = operation("put", label);
        position(operation, x, y, z);
        operation.addProperty("state", stateName);
        operation.addProperty("old_id", Block.getId(old));
        if (chunk.getBlockState(new BlockPos(x, y, z)) != state) throw new AssertionError("ProtoChunk state visibility");
    }
    static void check(int x, int y, int z) {
        world.engine.checkBlock(new BlockPos(x, y, z));
        position(operation("check", "check_block"), x, y, z);
    }
    static void enabled(int x, int z, boolean enabled) {
        world.engine.setLightEnabled(new ChunkPos(x, z), enabled);
        JsonObject operation = operation("enabled", "source_column");
        operation.addProperty("x", x); operation.addProperty("z", z); operation.addProperty("enabled", enabled);
    }
    static void sources(int x, int z) {
        world.engine.propagateLightSources(new ChunkPos(x, z));
        JsonObject operation = operation("sources", "discover_sources");
        operation.addProperty("x", x); operation.addProperty("z", z);
        JsonArray found = new JsonArray();
        LightChunk chunk = world.getChunkForLighting(x, z);
        if (chunk != null) chunk.findBlockLightSources((pos, state) -> {
            JsonObject source = new JsonObject();
            position(source, pos.getX(), pos.getY(), pos.getZ());
            source.addProperty("state_id", Block.getId(state));
            found.add(source);
        });
        operation.add("found", found);
    }
    static void snapshot(String label) {
        JsonObject operation = operation("run", label);
        operation.addProperty("had_work", world.engine.hasLightWork());
        JsonArray work = new JsonArray();
        do {
            work.add(world.engine.runLightUpdates());
            if (work.size() > 4) throw new AssertionError("unexpected non-quiescent light engine");
        } while (world.engine.hasLightWork());
        operation.add("java_queue_work", work);
        JsonArray sections = new JsonArray();
        var listener = world.engine.getLayerListener(LightLayer.BLOCK);
        for (SectionPos pos : world.stored) {
            DataLayer data = listener.getDataLayerData(pos);
            if (data == null) throw new AssertionError("missing declared stored section " + pos);
            JsonObject section = new JsonObject();
            position(section, pos.x(), pos.y(), pos.z());
            // Observe representation before access; materialize a detached copy
            // so taking a snapshot cannot alter subsequent engine operations.
            section.addProperty("empty", data.isEmpty());
            section.addProperty("homogeneous", data.isDefinitelyHomogenous());
            section.addProperty("data", HexFormat.of().formatHex(data.copy().getData()));
            section.addProperty("type", world.engine.getDebugSectionType(LightLayer.BLOCK, pos).name());
            for (int index = 0; index < 4096; index++) {
                int x = index & 15, z = (index >> 4) & 15, y = index >> 8;
                if (data.get(x, y, z) != listener.getLightValue(new BlockPos(pos.minBlockX()+x, pos.minBlockY()+y, pos.minBlockZ()+z))) {
                    throw new AssertionError("data/visible light mismatch");
                }
            }
            sections.add(section);
        }
        operation.add("sections", sections);
    }
    static void cases() {
        begin("cross_chunk_and_vertical_sources", 0, 1);
        put("edge_source", 15, 15, 8, "glowstone");
        check(15, 15, 8);
        snapshot("disabled_source_stays_dark");
        enabled(0, 0, true);
        snapshot("enable_alone_does_not_discover");
        check(15, 15, 8);
        snapshot("source_reaches_disabled_neighbor_column");
        put("second_source", 20, 16, 8, "torch");
        sources(1, 0);
        snapshot("two_sources");
        put("first_removed", 15, 15, 8, "air"); check(15, 15, 8);
        snapshot("second_source_reintroduced_after_decrease");
        put("first_restored", 15, 15, 8, "redstone_torch"); check(15, 15, 8);
        snapshot("weaker_source_with_stronger_neighbor");
        enabled(1, 0, false);
        snapshot("disable_alone_preserves_stored_light");
        check(20, 16, 8);
        snapshot("disabled_source_removed_by_check");
        sources(1, 0);
        snapshot("source_discovery_reenables");
        put("both_removed_a", 15, 15, 8, "air"); put("both_removed_b", 20, 16, 8, "air");
        check(20, 16, 8); check(15, 15, 8); check(20, 16, 8);
        snapshot("both_removed_duplicate_checks");
        check(1000, 15, 1000); snapshot("unstored_check_no_effect");

        begin("dampening_and_shape_changes", 0, 1);
        put("source", 14, 8, 8, "glowstone"); sources(0, 0);
        snapshot("air_path");
        for (String state : List.of("water", "leaves", "glass", "tinted_glass", "stone", "air")) {
            put("replace_target", 15, 8, 8, state); check(15, 8, 8); snapshot("target_" + state);
        }
        put("bottom_slab", 15, 8, 8, "bottom_slab"); check(15, 8, 8); snapshot("bottom_slab");
        put("top_slab", 15, 8, 8, "top_slab"); check(15, 8, 8); snapshot("top_slab");
        put("complement_left", 15, 8, 8, "bottom_slab");
        put("complement_right", 16, 8, 8, "top_slab"); check(15, 8, 8); check(16, 8, 8);
        snapshot("complementary_faces_block");
        put("matching_right", 16, 8, 8, "bottom_slab"); check(16, 8, 8); snapshot("matching_faces_open");
        put("stair_left", 15, 8, 8, "bottom_stairs");
        put("stair_right", 16, 8, 8, "top_stairs"); check(16, 8, 8); check(15, 8, 8);
        snapshot("stair_face_change");
        put("restore_left", 15, 8, 8, "air"); put("restore_right", 16, 8, 8, "air");
        check(15, 8, 8); check(16, 8, 8); snapshot("reopen_after_shapes");
        put("source_removed", 14, 8, 8, "air"); check(14, 8, 8); snapshot("all_removed");

        begin("unavailable_neighbors_and_source_scan", -1, -1);
        put("east_edge", -1, 15, 8, "glowstone");
        put("west_edge", -16, 16, 8, "torch");
        put("north_edge", -8, 7, 0, "redstone_torch");
        sources(-1, 0); snapshot("available_column_only");
        sources(0, 0); snapshot("unavailable_source_column");
        check(0, 15, 8); check(-17, 16, 8); snapshot("unavailable_neighbor_is_bedrock");
        put("remove_east", -1, 15, 8, "air"); check(-1, 15, 8); snapshot("remove_one_edge");
        put("replace_edge", -1, 15, 8, "torch"); check(-1, 15, 8); snapshot("restore_weaker_edge");
        enabled(-1, 0, false);
        check(-1, 15, 8); check(-16, 16, 8); check(-8, 7, 0); snapshot("disable_all_sources");
    }
    static JsonArray profiles() {
        JsonArray result = new JsonArray();
        for (var entry : STATES.entrySet()) {
            BlockState state = entry.getValue();
            JsonObject item = new JsonObject();
            item.addProperty("name", entry.getKey()); item.addProperty("id", Block.getId(state));
            item.addProperty("emission", state.getLightEmission());
            item.addProperty("dampening", state.getLightDampening());
            item.addProperty("can_occlude", state.canOcclude());
            item.addProperty("use_shape", state.useShapeForLightOcclusion());
            item.addProperty("empty_shape", !state.canOcclude() || !state.useShapeForLightOcclusion());
            JsonObject against = new JsonObject();
            for (var other : STATES.entrySet()) {
                JsonArray directions = new JsonArray();
                for (Direction direction : DIRECTIONS) {
                    directions.add(Shapes.faceShapeOccludes(LightEngine.getOcclusionShape(state, direction),
                        LightEngine.getOcclusionShape(other.getValue(), direction.getOpposite())));
                }
                against.add(other.getKey(), directions);
            }
            item.add("occludes", against); result.add(item);
        }
        return result;
    }
    public static void main(String[] args) throws Exception {
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong locked version");
        STATES.put("air", Blocks.AIR.defaultBlockState());
        STATES.put("bedrock", Blocks.BEDROCK.defaultBlockState());
        STATES.put("glowstone", Blocks.GLOWSTONE.defaultBlockState());
        STATES.put("torch", Blocks.TORCH.defaultBlockState());
        STATES.put("redstone_torch", Blocks.REDSTONE_TORCH.defaultBlockState());
        STATES.put("stone", Blocks.STONE.defaultBlockState());
        STATES.put("water", Blocks.WATER.defaultBlockState());
        STATES.put("leaves", Blocks.OAK_LEAVES.defaultBlockState());
        STATES.put("glass", Blocks.GLASS.defaultBlockState());
        STATES.put("tinted_glass", Blocks.TINTED_GLASS.defaultBlockState());
        STATES.put("bottom_slab", Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE, SlabType.BOTTOM));
        STATES.put("top_slab", Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE, SlabType.TOP));
        STATES.put("bottom_stairs", Blocks.OAK_STAIRS.defaultBlockState().setValue(BlockStateProperties.HALF, Half.BOTTOM));
        STATES.put("top_stairs", Blocks.OAK_STAIRS.defaultBlockState().setValue(BlockStateProperties.HALF, Half.TOP));
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var setup = new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false),
            Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        try (ExecutorService worker = Executors.newFixedThreadPool(2)) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(setup,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, config) -> {
                    try (resources) {
                        factory = PalettedContainerFactory.create(registries.compositeAccess());
                        cases();
                        return true;
                    }
                }, worker, Runnable::run).join();
            JsonObject output = new JsonObject();
            output.addProperty("version", SharedConstants.getCurrentVersion().id());
            output.add("profiles", profiles());
            JsonArray directions = new JsonArray();
            for (Direction direction : DIRECTIONS) directions.add(direction.getName());
            output.add("directions", directions);
            output.add("scenarios", SCENARIOS);
            Files.writeString(Path.of(args[0]), JSON.toJson(output));
        } finally { Util.shutdownExecutors(); }
    }
}
"#;

fn verify_materials(registry: &ChunkRegistrySnapshot, observations: &Value) {
    assert_eq!(observations["version"], "26.3-pre-2");
    assert_eq!(
        observations["directions"],
        serde_json::json!(["down", "up", "north", "south", "west", "east"])
    );
    let profiles = observations["profiles"].as_array().unwrap();
    for profile in profiles {
        let name = profile["name"].as_str().unwrap();
        let id = profile["id"].as_u64().unwrap() as u32;
        let material = registry.light_material(id).unwrap();
        assert_eq!(u64::from(material.emission), profile["emission"], "{name}");
        assert_eq!(
            u64::from(material.dampening),
            profile["dampening"],
            "{name}"
        );
        assert_eq!(material.can_occlude, profile["can_occlude"], "{name}");
        assert_eq!(
            material.use_shape_for_light_occlusion, profile["use_shape"],
            "{name}"
        );
        assert_eq!(material.empty_shape(), profile["empty_shape"], "{name}");
        if name == "bedrock" {
            assert_eq!(registry.bedrock_id(), Some(id));
        }
        if name == "air" {
            assert_eq!(registry.air_id(), id);
        }
        for other in profiles {
            let other_name = other["name"].as_str().unwrap();
            let other_material = registry
                .light_material(other["id"].as_u64().unwrap() as u32)
                .unwrap();
            for direction in 0..6 {
                let from_face = if material.empty_shape() {
                    EMPTY_FACE
                } else {
                    material.faces[direction]
                };
                let to_face = if other_material.empty_shape() {
                    EMPTY_FACE
                } else {
                    other_material.faces[direction ^ 1]
                };
                assert_eq!(
                    registry.face_occludes(from_face, to_face).unwrap(),
                    profile["occludes"][other_name][direction]
                        .as_bool()
                        .unwrap(),
                    "{name} -> {other_name}, direction {direction}"
                );
            }
        }
    }
}

fn number(value: &Value, field: &str) -> i32 {
    value[field].as_i64().unwrap().try_into().unwrap()
}

fn block(value: &Value) -> LightBlock {
    LightBlock {
        x: number(value, "x"),
        y: number(value, "y"),
        z: number(value, "z"),
    }
}

fn section(value: &Value) -> LightSection {
    LightSection {
        x: number(value, "x"),
        y: number(value, "y"),
        z: number(value, "z"),
    }
}

fn verify_sections(storage: &LightSectionStorage, operation: &Value, label: &str) -> usize {
    let expected = operation["sections"].as_array().unwrap();
    let snapshot = storage.snapshot();
    let mut actual_keys: Vec<_> = snapshot.sections().collect();
    actual_keys.sort_by_key(|key| (key.x, key.y, key.z));
    let expected_keys: Vec<_> = expected.iter().map(section).collect();
    assert_eq!(actual_keys, expected_keys, "{label} section presence");
    for row in expected {
        let key = section(row);
        let layer = storage.data_layer_data(key).unwrap();
        assert_eq!(
            layer.is_empty(),
            row["empty"].as_bool().unwrap(),
            "{label} {key:?} empty representation"
        );
        assert_eq!(
            layer.is_definitely_homogeneous(),
            row["homogeneous"].as_bool().unwrap(),
            "{label} {key:?} implicit representation"
        );
        let kind = match storage.section_type(key) {
            SectionType::Empty => "EMPTY",
            SectionType::LightOnly => "LIGHT_ONLY",
            SectionType::LightAndData => "LIGHT_AND_DATA",
        };
        assert_eq!(
            kind,
            row["type"].as_str().unwrap(),
            "{label} {key:?} section type"
        );
        let encoded = row["data"].as_str().unwrap();
        assert_eq!(encoded.len(), 4096);
        for byte_index in 0..2048 {
            let expected_byte =
                u8::from_str_radix(&encoded[byte_index * 2..byte_index * 2 + 2], 16).unwrap();
            for half in 0..2 {
                let index = byte_index * 2 + half;
                let x = (index & 15) as u8;
                let z = ((index >> 4) & 15) as u8;
                let y = (index >> 8) as u8;
                let expected_nibble = (expected_byte >> (half * 4)) & 15;
                assert_eq!(
                    layer.get(x, y, z).unwrap(),
                    i32::from(expected_nibble),
                    "{label} {key:?} cell {index}"
                );
                assert_eq!(
                    snapshot.get_level(LightBlock {
                        x: key.x * 16 + i32::from(x),
                        y: key.y * 16 + i32::from(y),
                        z: key.z * 16 + i32::from(z),
                    }),
                    expected_nibble,
                    "{label} {key:?} visible cell {index}"
                );
            }
        }
    }
    expected.len()
}

// Small fixture inputs are rebuilt only after a recorded state mutation. A
// cached immutable source is retained across every partial run and retry.
struct Scene {
    registry: Arc<ChunkRegistrySnapshot>,
    height: DimensionHeight,
    chunks: BTreeMap<ChunkAddress, BTreeMap<LightBlock, u32>>,
    cached: Option<LightingSource>,
}

impl Scene {
    fn source(&mut self) -> &LightingSource {
        if self.cached.is_none() {
            assert!(self.chunks.len() <= 4, "bounded fixture producer");
            let mut input = Vec::with_capacity(self.chunks.len());
            let air = self.registry.air_id();
            for (&address, states) in &self.chunks {
                let mut sections: Vec<Option<Section>> = vec![None, None];
                for (&pos, &id) in states {
                    let section = sections[(pos.y / 16) as usize].get_or_insert_with(|| Section {
                        counts: SectionCounts {
                            non_empty_blocks: 0,
                            fluid_blocks: 0,
                        },
                        blocks: PalettedContainer::single(
                            ContainerKind::Blocks,
                            self.registry.block_registry(),
                            air,
                        )
                        .unwrap(),
                        biomes: PalettedContainer::single(
                            ContainerKind::Biomes,
                            self.registry.biome_registry(),
                            self.registry.plains_id(),
                        )
                        .unwrap(),
                    });
                    assert_eq!(
                        section.blocks.set(pos.local_index(), id, 1 << 20).unwrap(),
                        air
                    );
                    let flags = self.registry.state_flags(id).unwrap();
                    section.counts.non_empty_blocks += u16::from(!flags.is_air);
                    section.counts.fluid_blocks += u16::from(!flags.is_air && flags.has_fluid);
                }
                input.push(LightingChunk { address, sections });
            }
            self.cached = Some(
                LightingSource::from_sections(
                    Arc::clone(&self.registry),
                    self.height,
                    input,
                    SourceLimits {
                        max_chunks: 4,
                        metadata_bytes: 1 << 20,
                        owned_section_bytes: 1 << 20,
                    },
                )
                .unwrap(),
            );
        }
        self.cached.as_ref().unwrap()
    }
}

#[derive(Default)]
struct Checked {
    snapshots: usize,
    sections: usize,
    java_work: usize,
    rust_work: usize,
    yields: usize,
}

fn replay(registry: &Arc<ChunkRegistrySnapshot>, observations: &Value, budget: usize) -> Checked {
    let states: BTreeMap<_, _> = observations["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| {
            (
                profile["name"].as_str().unwrap(),
                profile["id"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    let mut checked = Checked::default();
    for scenario in observations["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let mut scene = Scene {
            registry: Arc::clone(registry),
            height: DimensionHeight::new(
                number(scenario, "min_y"),
                scenario["height"].as_u64().unwrap() as u32,
            )
            .unwrap(),
            chunks: BTreeMap::new(),
            cached: None,
        };
        let mut storage = LightSectionStorage::new(
            LightKind::Block,
            StorageLimits {
                max_sections: 256,
                max_columns: 64,
                max_notifications: 1024,
                metadata_bytes: 8 << 20,
                layer_bytes: 4 << 20,
            },
        )
        .unwrap();
        let mut engine = BlockLightEngine::new(BlockLightLimits {
            checks: 256,
            decreases: 8192,
            increases: 8192,
            queue_bytes: 2 << 20,
        })
        .unwrap();
        for (index, operation) in scenario["operations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let label = format!(
                "{name}/{index}/{} budget {budget}",
                operation["label"].as_str().unwrap()
            );
            match operation["op"].as_str().unwrap() {
                "chunk" => {
                    let address = ChunkAddress {
                        x: number(operation, "x"),
                        z: number(operation, "z"),
                    };
                    assert!(scene.chunks.insert(address, BTreeMap::new()).is_none());
                    scene.cached = None;
                }
                "section" => storage
                    .update_section_status(section(operation), false)
                    .unwrap(),
                "put" => {
                    let pos = block(operation);
                    let values = scene.chunks.get_mut(&pos.column()).unwrap();
                    let previous = values.get(&pos).copied().unwrap_or(registry.air_id());
                    assert_eq!(
                        u64::from(previous),
                        operation["old_id"].as_u64().unwrap(),
                        "{label} previous state"
                    );
                    let id = states[operation["state"].as_str().unwrap()];
                    if id == registry.air_id() {
                        values.remove(&pos);
                    } else {
                        values.insert(pos, id);
                    }
                    scene.cached = None;
                }
                "check" => {
                    engine.check_block(block(operation)).unwrap();
                }
                "enabled" => storage
                    .set_enabled(
                        ChunkAddress {
                            x: number(operation, "x"),
                            z: number(operation, "z"),
                        },
                        operation["enabled"].as_bool().unwrap(),
                    )
                    .unwrap(),
                "sources" => {
                    let address = ChunkAddress {
                        x: number(operation, "x"),
                        z: number(operation, "z"),
                    };
                    let source = scene.source();
                    let found: Vec<_> = source.emission_sources(address).collect();
                    let expected: Vec<_> = operation["found"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|value| (block(value), value["state_id"].as_u64().unwrap() as u32))
                        .collect();
                    assert_eq!(found, expected, "{label} source enumeration");
                    engine
                        .propagate_light_sources(source, &mut storage, address)
                        .unwrap();
                }
                "run" => {
                    assert_eq!(
                        engine.has_work() || storage.has_inconsistencies(),
                        operation["had_work"].as_bool().unwrap(),
                        "{label} pending work"
                    );
                    let source = scene.source();
                    let mut calls = 0;
                    loop {
                        let progress = engine.run(source, &mut storage, budget).unwrap();
                        assert!(progress.processed <= budget, "{label} per-call work budget");
                        checked.rust_work += progress.processed;
                        calls += 1;
                        assert!(calls <= 100_000, "{label} convergence");
                        if progress.complete {
                            break;
                        }
                        checked.yields += 1;
                        assert!(
                            progress.processed > 0,
                            "{label} pending work makes progress"
                        );
                    }
                    assert!(!engine.has_work(), "{label} engine converged");
                    assert!(!storage.has_inconsistencies(), "{label} storage converged");
                    checked.java_work += operation["java_queue_work"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|value| value.as_u64().unwrap() as usize)
                        .sum::<usize>();
                    checked.sections += verify_sections(&storage, operation, &label);
                    checked.snapshots += 1;
                }
                value => panic!("unknown operation {value}"),
            }
        }
    }
    checked
}

fn registry(reference: &Path) -> ChunkRegistrySnapshot {
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
    ChunkRegistrySnapshot::load(&snapshot, &expected, RegistryLoadLimits::default()).unwrap()
}

fn java_observations(reference: &Path) -> (PathBuf, Value) {
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-block-light-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let oracle = directory.join("BlockLightOracle.java");
    fs::write(&oracle, ORACLE).unwrap();
    let output = directory.join("observations.json");
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let java = env::var_os("JAVA_HOME")
        .map(|home| {
            PathBuf::from(home).join(if cfg!(windows) {
                "bin/java.exe"
            } else {
                "bin/java"
            })
        })
        .unwrap_or_else(|| PathBuf::from("java"));
    let execution = Command::new(java)
        .arg("-Xmx1G")
        .arg("--class-path")
        .arg(
            env::join_paths([
                artifacts.join("server-26.3-pre-2.jar"),
                artifacts.join("libraries/*"),
            ])
            .unwrap(),
        )
        .arg(oracle)
        .arg(&output)
        .current_dir(&directory)
        .output()
        .expect("Java25 must be available");
    assert!(
        execution.status.success(),
        "Java oracle failed: {}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    let observations = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    (directory, observations)
}

#[test]
#[ignore = "requires Java25 and locked lighting-v3 data through ARROW_MC_JAVA_REFERENCE_ROOT"]
fn block_light_sections_match_actual_vanilla() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
            .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to the prepared Decompile directory"),
    );
    let registry = Arc::new(registry(&reference));
    let (directory, observations) = java_observations(&reference);
    verify_materials(&registry, &observations);
    for budget in [usize::MAX, 7] {
        let checked = replay(&registry, &observations, budget);
        assert_eq!(checked.snapshots, 34);
        assert_eq!(checked.sections, 1548);
        if budget == 7 {
            assert!(checked.yields > 0);
        }
        eprintln!(
            "Compared {} block-light snapshots / {} complete sections / {} nibbles against actual Vanilla 26.3-pre-2 (work budget {budget}, {} yields; Java dequeues {}, Rust work units {})",
            checked.snapshots,
            checked.sections,
            checked.sections * 4096,
            checked.yields,
            checked.java_work,
            checked.rust_work
        );
    }
    // Preserve failed observations for diagnosis; clean only the verified root.
    assert!(
        directory
            .canonicalize()
            .unwrap()
            .starts_with(env::temp_dir().canonicalize().unwrap())
    );
    fs::remove_dir_all(directory).unwrap();
}
