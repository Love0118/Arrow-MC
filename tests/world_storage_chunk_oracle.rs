//! Opt-in actual-JAR disk-chunk decoding checks; no JAR or generated registry data is bundled.
//!
//! Set `ARROW_MC_JAVA_REFERENCE_ROOT` to the sibling `Decompile`, prepare the block-state
//! snapshot, then run `cargo test --test world_storage_chunk_oracle -- --ignored --nocapture`.
//! `ARROW_BLOCK_STATE_SNAPSHOT`, `ARROW_BLOCK_STATE_MANIFEST_SHA256`, and
//! `ARROW_CONFIGURATION_MANIFEST_SHA256` may select independently verified regenerated data.
//! The embedded public-API driver is independently authored from the team's local
//! ChunkDecodeProbe; it contains no translated Vanilla implementation. Java writes the
//! actual named NBT, avoiding dependence on Arrow's SNBT writer for oracle inputs.

use arrow_mc::{
    nbt::{Compound, Limits, Tag},
    server::configuration_data::parse_sha256,
    world::storage::{
        chunk::{ChunkDecodeError, DimensionHeight, StoredChunkDraft, decode_current_chunk},
        registry::{ChunkRegistrySnapshot, ExpectedRegistryReference, RegistryLoadLimits},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf, process::Command, time::SystemTime};

const ORACLE: &str = r#"
import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.nbt.*;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.util.SimpleBitStorage;
import net.minecraft.util.Util;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.storage.*;
import io.netty.buffer.Unpooled;
import net.minecraft.network.FriendlyByteBuf;

class StoredChunkOracle {
    static { SharedConstants.tryDetectVersion(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().serializeNulls().create();
    static final JsonArray CASES = new JsonArray();
    static Path outputDirectory;

    static void test(String name, String snbt, PalettedContainerFactory factory) throws Exception {
        // The parser may return a shared empty compound. Never attach a version to it.
        test(name, TagParser.parseCompoundFully(snbt).copy(), factory);
    }

    static void test(String name, CompoundTag input, PalettedContainerFactory factory) throws Exception {
        input.putInt("DataVersion", SharedConstants.getCurrentVersion().dataVersion().version());
        NbtIo.write(input, outputDirectory.resolve(name + ".nbt"));
        JsonObject result = new JsonObject();
        result.addProperty("case", name);
        try {
            SerializableChunkData data = SerializableChunkData.parse(LevelHeightAccessor.create(-64, 384), factory, input);
            if (data == null) {
                result.addProperty("result", "null");
            } else {
                result.addProperty("result", "ok");
                result.addProperty("status", data.chunkStatus().getName());
                result.addProperty("x", data.chunkPos().x());
                result.addProperty("z", data.chunkPos().z());
                result.addProperty("last_update", data.lastUpdateTime());
                result.addProperty("inhabited", data.inhabitedTime());
                result.addProperty("light_correct", data.lightCorrect());
                result.addProperty("entities", data.entities().size());
                result.addProperty("block_entities", data.blockEntities().size());
                result.addProperty("heightmaps", data.heightmaps().size());
                JsonArray sections = new JsonArray();
                for (var section : data.sectionData()) {
                    JsonObject item = new JsonObject();
                    item.addProperty("y", section.y());
                    item.add("block_light", section.blockLight() == null ? JsonNull.INSTANCE : JSON.toJsonTree(HexFormat.of().formatHex(section.blockLight().getData())));
                    item.add("sky_light", section.skyLight() == null ? JsonNull.INSTANCE : JSON.toJsonTree(HexFormat.of().formatHex(section.skyLight().getData())));
                    if (section.chunkSection() == null) item.addProperty("section", "outside");
                    else {
                        var value = section.chunkSection();
                        JsonArray blocks = new JsonArray();
                        for (int index = 0; index < 4096; index++)
                            blocks.add(Block.getId(value.getBlockState(index & 15, index >> 8, (index >> 4) & 15)));
                        JsonArray biomes = new JsonArray();
                        for (int index = 0; index < 64; index++)
                            biomes.add(value.getNoiseBiome(index & 3, index >> 4, (index >> 2) & 3).getRegisteredName());
                        item.add("blocks", blocks);
                        item.add("biomes", biomes);
                        item.addProperty("only_air", value.hasOnlyAir());
                        item.addProperty("has_fluid", value.hasFluid());
                        FriendlyByteBuf bytes = new FriendlyByteBuf(Unpooled.buffer());
                        try {
                            value.write(bytes);
                            item.addProperty("non_empty_count", bytes.readUnsignedShort());
                            item.addProperty("fluid_count", bytes.readUnsignedShort());
                            item.addProperty("block_network_bits", bytes.readUnsignedByte());
                        } finally { bytes.release(); }
                    }
                    sections.add(item);
                }
                result.add("sections", sections);
            }
        } catch (Throwable error) {
            result.addProperty("result", "error");
            result.addProperty("class", error.getClass().getName());
            result.addProperty("message", String.valueOf(error.getMessage()));
        }
        CASES.add(result);
    }

    static void generatedPalette(String name, int paletteSize, PalettedContainerFactory factory) throws Exception {
        CompoundTag input = TagParser.parseCompoundFully("{Status:'minecraft:full',xPos:-33,zPos:32}");
        CompoundTag section = new CompoundTag();
        section.putByte("Y", (byte) -4);
        ListTag palette = new ListTag();
        for (int index = 0; index < paletteSize; index++) {
            int id = Block.BLOCK_STATE_REGISTRY.size() - paletteSize + index;
            palette.add(BlockState.CODEC.encodeStart(NbtOps.INSTANCE, Block.stateById(id)).getOrThrow());
        }
        CompoundTag states = new CompoundTag();
        states.put("palette", palette);
        if (paletteSize > 1) {
            // 257 local entries need 9-bit disk indices, independently of the global ID width.
            SimpleBitStorage packed = new SimpleBitStorage(9, 4096);
            for (int index = 0; index < 4096; index++) packed.set(index, index % paletteSize);
            packed.set(4095, paletteSize - 1);
            states.putLongArray("data", packed.getRaw());
        }
        section.put("block_states", states);
        byte[] blockLight = new byte[2048];
        byte[] skyLight = new byte[2048];
        for (int index = 0; index < 2048; index++) {
            blockLight[index] = (byte) index;
            skyLight[index] = (byte) ~index;
        }
        section.putByteArray("BlockLight", blockLight);
        section.putByteArray("SkyLight", skyLight);
        ListTag sections = new ListTag();
        sections.add(section);
        input.put("sections", sections);
        test(name, input, factory);
    }

    static CompoundTag paletteInput(String field, int size) throws Exception {
        CompoundTag input = TagParser.parseCompoundFully("{Status:'minecraft:full',sections:[{Y:0b}]}").copy();
        ListTag palette = new ListTag();
        for (int index = 0; index < size; index++) {
            palette.add(field.equals("block_states")
                ? BlockState.CODEC.encodeStart(NbtOps.INSTANCE, Block.stateById(index)).getOrThrow()
                : StringTag.valueOf(index == 0 ? "minecraft:plains" : "minecraft:desert"));
        }
        CompoundTag data = new CompoundTag();
        data.put("palette", palette);
        input.getListOrEmpty("sections").getCompound(0).orElseThrow().put(field, data);
        return input;
    }

    static void collectionCases(PalettedContainerFactory factory) throws Exception {
        for (String field : List.of("block_states", "biomes")) {
            String names = field.equals("block_states") ? "['minecraft:stone','minecraft:dirt']" : "['minecraft:plains','minecraft:desert']";
            int count = field.equals("block_states") ? 256 : 1;
            for (String type : List.of("list_int", "list_float", "int_array", "byte_array", "mixed_numeric", "mixed_invalid")) {
                CompoundTag input = TagParser.parseCompoundFully("{Status:'minecraft:full',sections:[{Y:0b,"+field+":{palette:"+names+"}}]}").copy();
                CompoundTag data = input.getListOrEmpty("sections").getCompound(0).orElseThrow().getCompound(field).orElseThrow();
                Tag words;
                if (type.equals("int_array")) words = new IntArrayTag(new int[count]);
                else if (type.equals("byte_array")) words = new ByteArrayTag(new byte[count]);
                else {
                    ListTag list = new ListTag();
                    for (int index = 0; index < count; index++) list.add(type.equals("list_float") ? DoubleTag.valueOf(0.9) : IntTag.valueOf(0));
                    if (type.equals("mixed_numeric")) list.setTag(0, FloatTag.valueOf(-0.9f));
                    if (type.equals("mixed_invalid")) list.setTag(0, StringTag.valueOf("0"));
                    words = list;
                }
                data.put("data", words);
                test("collection_"+field+"_"+type, input, factory);
            }
            for (String literal : List.of("[B;0]", "[I;0]", "[L;0]", "[B;]")) {
                test("palette_"+field+"_"+literal.charAt(1)+(literal.length()==4?"_empty":""), "{Status:'minecraft:full',sections:[{Y:0b,"+field+":{palette:"+literal+"}}]}", factory);
            }
            Map<String, Tag> boundary = new LinkedHashMap<>();
            boundary.put("negative_double_fraction", DoubleTag.valueOf(-0.9));
            boundary.put("negative_float_fraction", FloatTag.valueOf(-0.9f));
            boundary.put("positive_double_fraction", DoubleTag.valueOf(1.9));
            boundary.put("double_nan", DoubleTag.valueOf(Double.NaN));
            boundary.put("float_nan", FloatTag.valueOf(Float.NaN));
            boundary.put("positive_infinity", DoubleTag.valueOf(Double.POSITIVE_INFINITY));
            boundary.put("negative_infinity", DoubleTag.valueOf(Double.NEGATIVE_INFINITY));
            boundary.put("positive_extreme", DoubleTag.valueOf(Double.MAX_VALUE));
            boundary.put("negative_extreme", DoubleTag.valueOf(-Double.MAX_VALUE));
            boundary.put("below_long_max", DoubleTag.valueOf(Math.nextDown(0x1.0p63)));
            boundary.put("exact_long_min", DoubleTag.valueOf(-0x1.0p63));
            for (var entry : boundary.entrySet()) {
                // All possible 4-bit block indices are valid, so even saturation
                // to MIN/MAX_VALUE can be compared as complete decoded cells.
                CompoundTag input = paletteInput(field, field.equals("block_states") ? 16 : 2);
                ListTag words = new ListTag();
                for (int index = 0; index < count; index++) words.add(entry.getValue());
                input.getListOrEmpty("sections").getCompound(0).orElseThrow().getCompound(field).orElseThrow().put("data", words);
                test("numeric_"+field+"_"+entry.getKey(), input, factory);
            }
        }
    }

    static void run(PalettedContainerFactory factory) throws Exception {
        test("missing_status", "{}", factory);
        test("status_wrong_type", "{Status:4}", factory);
        test("unknown_status", "{Status:'minecraft:no_such_status'}", factory);
        test("old_status_noise", "{Status:'minecraft:noise'}", factory);
        test("terrain_status", "{Status:'minecraft:terrain'}", factory);
        test("status_empty_namespace", "{Status:':full'}", factory);
        test("numeric_defaults", "{Status:'minecraft:full',xPos:1.75d,zPos:-1.75d,LastUpdate:2.9d,InhabitedTime:-2.9d,isLightOn:1b}", factory);
        test("missing_sections", "{Status:'minecraft:full',xPos:-33,zPos:32}", factory);
        test("default_section", "{Status:'minecraft:full',sections:[{Y:-4b}]}", factory);
        test("y_numeric_wrapping", "{Status:'minecraft:full',sections:[{Y:252},{Y:256},{Y:-5b},{Y:20b}]}", factory);
        test("duplicate_y", "{Status:'minecraft:full',sections:[{Y:0b},{Y:0b}]}", factory);
        test("section_noncompound", "{Status:'minecraft:full',sections:[1,{Y:0b},'ignored']}", factory);
        test("current_string_palette", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:stone']},biomes:{palette:['minecraft:desert']}}]}", factory);
        test("current_compound_palette", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:[{id:'minecraft:oak_log',properties:{axis:'x'}}]}}]}", factory);
        test("current_missing_properties", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:[{id:'minecraft:oak_log'}]}}]}", factory);
        test("current_bad_property", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:[{id:'minecraft:oak_log',properties:{axis:'wrong'}}]}}]}", factory);
        test("old_name_palette", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:[{Name:'minecraft:stone'}]}}]}", factory);
        test("unknown_block", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:no_such_block']}}]}", factory);
        test("unknown_biome", "{Status:'minecraft:full',sections:[{Y:0b,biomes:{palette:['minecraft:no_such_biome']}}]}", factory);
        test("empty_palette", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:[]}}]}", factory);
        test("missing_palette", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{}}]}", factory);
        test("two_values_missing_data", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:stone','minecraft:dirt']}}]}", factory);
        test("two_values_short_data", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:stone','minecraft:dirt'],data:[L;0]}}]}", factory);
        test("single_value_extra_data", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:stone'],data:[L;999]}}]}", factory);
        test("wrong_light_length", "{Status:'minecraft:full',sections:[{Y:0b,BlockLight:[B;0]}]}", factory);
        test("outside_wrong_light_length", "{Status:'minecraft:full',sections:[{Y:20b,BlockLight:[B;0]}]}", factory);
        test("water_section", "{Status:'minecraft:full',sections:[{Y:0b,block_states:{palette:['minecraft:water']}}]}", factory);
        test("auxiliary_filter", "{Status:'minecraft:full',entities:[1,{id:'minecraft:pig'}],block_entities:[{},2],Heightmaps:{WORLD_SURFACE:[L;1],UNKNOWN:[L;2]},structures:{Starts:{}}}", factory);
        generatedPalette("highest_state", 1, factory);
        generatedPalette("disk_palette_257", 257, factory);
        collectionCases(factory);
    }

    public static void main(String[] args) throws Exception {
        outputDirectory = Path.of(args[0]);
        Bootstrap.bootStrap();
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var setup = new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false), Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        try (ExecutorService worker = Executors.newFixedThreadPool(2)) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(setup,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, config) -> {
                    try (resources) {
                        run(PalettedContainerFactory.create(registries.compositeAccess()));
                        return true;
                    } catch (Exception error) { throw new RuntimeException(error); }
                }, worker, Runnable::run).join();
            JsonObject output = new JsonObject();
            output.addProperty("version", SharedConstants.getCurrentVersion().id());
            output.addProperty("data_version", SharedConstants.getCurrentVersion().dataVersion().version());
            output.addProperty("block_state_count", Block.BLOCK_STATE_REGISTRY.size());
            output.add("cases", CASES);
            Files.writeString(outputDirectory.resolve("expected.json"), JSON.toJson(output));
        } finally { Util.shutdownExecutors(); }
    }
}
"#;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

fn field<'a>(compound: &'a Compound, name: &str) -> Option<&'a Tag> {
    compound.get(&name.into())
}

fn compound_count(root: &Compound, name: &str) -> usize {
    match field(root, name) {
        Some(Tag::List(values)) => values
            .iter()
            .filter(|tag| matches!(tag, Tag::Compound(_)))
            .count(),
        _ => 0,
    }
}

fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Object(left), Value::Object(right)) if left.keys().eq(right.keys()) => left
            .iter()
            .find_map(|(name, value)| difference(value, &right[name], &format!("{path}/{name}"))),
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => left
            .iter()
            .zip(right)
            .enumerate()
            .find_map(|(index, (value, expected))| {
                difference(value, expected, &format!("{path}/{index}"))
            }),
        _ => (actual != expected).then(|| path.to_owned()),
    }
}

fn summary(draft: &StoredChunkDraft, expected: &Value, registry: &ChunkRegistrySnapshot) -> Value {
    let sections: Vec<_> = draft
        .sections()
        .iter()
        .map(|entry| {
            let mut result = json!({
                "y": entry.y,
                "block_light": entry.block_light.as_deref().map(hex),
                "sky_light": entry.sky_light.as_deref().map(hex),
            });
            if let Some(section) = &entry.section {
                let blocks: Vec<_> = (0..4096)
                    .map(|index| section.blocks.get(index).unwrap())
                    .collect();
                let biomes: Vec<_> = (0..64)
                    .map(|index| section.biomes.get(index).unwrap())
                    .collect();
                result["blocks"] = json!(blocks);
                result["biomes"] = json!(biomes);
                result["only_air"] = json!(section.counts.non_empty_blocks == 0);
                result["has_fluid"] = json!(section.counts.fluid_blocks != 0);
                result["non_empty_count"] = json!(section.counts.non_empty_blocks);
                result["fluid_count"] = json!(section.counts.fluid_blocks);
                result["block_network_bits"] = json!(section.blocks.bits());
            } else {
                result["section"] = json!("outside");
            }
            result
        })
        .collect();
    // Auxiliary activation is deferred. Check the retained raw data's matching
    // compound projection; this does not claim entity/heightmap activation exists.
    let heightmaps = match field(draft.root(), "Heightmaps") {
        Some(Tag::Compound(values)) => values
            .entries()
            .iter()
            .filter(|entry| {
                matches!(entry.value, Tag::LongArray(_))
                    && [
                        "WORLD_SURFACE_WG",
                        "WORLD_SURFACE",
                        "OCEAN_FLOOR_WG",
                        "OCEAN_FLOOR",
                        "MOTION_BLOCKING",
                        "MOTION_BLOCKING_NO_LEAVES",
                    ]
                    .iter()
                    .any(|name| entry.name == (*name).into())
            })
            .count(),
        _ => 0,
    };
    let mut expected = expected.clone();
    for section in expected["sections"].as_array_mut().unwrap() {
        if let Some(biomes) = section.get_mut("biomes") {
            for biome in biomes.as_array_mut().unwrap() {
                let resolved = registry.biome(&Tag::String(biome.as_str().unwrap().into()));
                assert!(!resolved.used_fallback, "Java produced an unknown biome");
                *biome = json!(resolved.id);
            }
        }
    }
    let mut actual = json!({
        "case": expected["case"], "result": "ok", "status": draft.status.name(),
        "x": draft.position.0, "z": draft.position.1,
        "last_update": draft.last_update, "inhabited": draft.inhabited_time,
        "light_correct": draft.light_correct,
        "entities": compound_count(draft.root(), "entities"),
        "block_entities": compound_count(draft.root(), "block_entities"),
        "heightmaps": heightmaps, "sections": sections,
    });
    let name = expected["case"].as_str().unwrap().to_owned();
    if name.starts_with("collection_") || name.starts_with("numeric_") {
        for (rust, java) in actual["sections"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(expected["sections"].as_array_mut().unwrap())
        {
            if rust["block_network_bits"] != java["block_network_bits"] {
                // Only permit the observed uniform-section compaction: the
                // generated disk palette has unused entries, Java keeps four
                // bits, and Arrow has one actual value. Other widths stay exact.
                assert_eq!(java["block_network_bits"], 4, "{name}: Java width");
                assert_eq!(rust["block_network_bits"], 0, "{name}: Rust width");
                let blocks = java["blocks"].as_array().unwrap();
                assert!(blocks.iter().all(|value| value == &blocks[0]));
                rust.as_object_mut().unwrap().remove("block_network_bits");
                java.as_object_mut().unwrap().remove("block_network_bits");
            }
        }
    }
    if let Some(path) = difference(&actual, &expected, "") {
        panic!(
            "Java/Rust chunk mismatch for {} at {path}",
            expected["case"]
        );
    }
    actual
}

#[test]
#[ignore = "requires Java 25, locked Vanilla jars, and the prepared block-state snapshot"]
fn matches_actual_java_current_disk_chunks() {
    let reference = PathBuf::from(
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
            .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to the sibling Decompile directory"),
    );
    let artifacts = reference.join("artifacts/26.3-pre-2");
    let snapshot = env::var_os("ARROW_BLOCK_STATE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| reference.join("bootstrap/26.3-pre-2-block-states-v2"));
    let anchor = |name, default: &str| {
        parse_sha256(&env::var(name).unwrap_or_else(|_| default.into())).unwrap()
    };
    let expected = ExpectedRegistryReference {
        manifest_sha256: anchor(
            "ARROW_BLOCK_STATE_MANIFEST_SHA256",
            "ac40352daeef56d8a273116f9573d1684c0e13c96e5d93e485900b4a021c5557",
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
    let jar = artifacts.join("server-26.3-pre-2.jar");
    let jar_bytes = fs::read(&jar).expect("prepare the locked server JAR first");
    assert_eq!(jar_bytes.len() as u64, expected.source_jar_bytes);
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(&jar_bytes)),
        expected.source_jar_sha256
    );
    drop(jar_bytes);
    let registry =
        ChunkRegistrySnapshot::load(&snapshot, &expected, RegistryLoadLimits::default()).unwrap();
    assert_eq!(registry.state_count(), 35723);

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-disk-chunk-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("StoredChunkOracle.java");
    fs::write(&source, ORACLE).unwrap();
    let classpath = env::join_paths([jar, artifacts.join("libraries/*")]).unwrap();
    let execution = Command::new("java")
        .arg("-Xmx1G")
        .arg("--class-path")
        .arg(classpath)
        .arg(&source)
        .arg(&directory)
        .current_dir(&directory)
        .output()
        .expect("Java 25 must be installed and on PATH");
    assert!(
        execution.status.success(),
        "Java oracle failed in {}:\n{}\n{}",
        directory.display(),
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    let oracle: Value =
        serde_json::from_slice(&fs::read(directory.join("expected.json")).unwrap()).unwrap();
    assert_eq!(oracle["version"], "26.3-pre-2");
    assert_eq!(oracle["data_version"], 5018);
    assert_eq!(oracle["block_state_count"], registry.state_count());
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 72);
    let mut valid = 0;
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let mut bytes = fs::read(directory.join(format!("{name}.nbt"))).unwrap();
        let decoded = decode_current_chunk(
            &mut bytes,
            &registry,
            DimensionHeight::new(-64, 384).unwrap(),
            Limits::default(),
            4 * 1024 * 1024,
        );
        match case["result"].as_str().unwrap() {
            "ok" => {
                let draft = decoded.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(draft.data_version, 5018);
                let actual = summary(&draft, case, &registry);
                if name == "highest_state" || name == "disk_palette_257" {
                    assert_eq!(actual["sections"][0]["blocks"][4095], 35722);
                    assert_eq!(
                        actual["sections"][0]["block_network_bits"],
                        if name == "highest_state" { 0 } else { 16 }
                    );
                }
                if name == "auxiliary_filter" {
                    let Some(Tag::List(entities)) = field(draft.root(), "entities") else {
                        panic!("raw entities missing")
                    };
                    assert_eq!(
                        entities.len(),
                        2,
                        "retain unsupported data for future activation"
                    );
                    assert!(field(draft.root(), "structures").is_some());
                }
                valid += 1;
            }
            "null" => assert!(
                matches!(decoded, Err(ChunkDecodeError::MissingLevelData)),
                "{name}: expected missing level data"
            ),
            "error" => {
                let error = decoded
                    .err()
                    .unwrap_or_else(|| panic!("{name}: Java rejected input but Rust accepted it"));
                let matches = match name {
                    "empty_palette" | "palette_block_states_B_empty" | "palette_biomes_B_empty" => {
                        matches!(error, ChunkDecodeError::EmptyPalette)
                    }
                    "missing_palette" => matches!(error, ChunkDecodeError::MissingPalette),
                    "two_values_missing_data"
                    | "collection_block_states_mixed_invalid"
                    | "collection_biomes_mixed_invalid" => {
                        matches!(error, ChunkDecodeError::MissingPackedData)
                    }
                    "two_values_short_data" => matches!(
                        error,
                        ChunkDecodeError::PackedLength {
                            expected: 256,
                            actual: 1
                        }
                    ),
                    "wrong_light_length" | "outside_wrong_light_length" => {
                        matches!(error, ChunkDecodeError::LightLength(1))
                    }
                    _ => panic!("unexpected Java error for {name}: {case}"),
                };
                assert!(
                    matches,
                    "{name}: wrong Rust failure {error}; Java failure {case}"
                );
            }
            other => panic!("unexpected Java result {other}"),
        }
    }
    assert_eq!(valid, 60);
    fs::remove_dir_all(&directory).unwrap();
    eprintln!(
        "Compared 72 actual-JAR disk-chunk fixtures: 60 accepted, 2 missing-status, 10 rejected; complete cells, lights, counts and metadata, including state 35722, 257-entry disk palette, primitive collections and floating long conversion boundaries."
    );
}
