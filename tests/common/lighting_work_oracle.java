import com.google.gson.*;
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

/** Original small inputs observed through the actual combined light engine. */
class LightingWorkOracle {
    static { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().create();
    static final LinkedHashMap<String, BlockState> STATES = new LinkedHashMap<>();
    static PalettedContainerFactory factory;

    static final class World implements LightChunkGetter {
        final LinkedHashMap<ChunkPos, ProtoChunk> chunks = new LinkedHashMap<>();
        final LevelHeightAccessor height = LevelHeightAccessor.create(0, 48);

        World() {
            for (int x = 0; x < 2; x++) {
                ChunkPos position = new ChunkPos(x, 0);
                chunks.put(position, new ProtoChunk(position, UpgradeData.EMPTY, height, factory, null));
            }
        }
        public LightChunk getChunkForLighting(int x, int z) { return chunks.get(new ChunkPos(x, z)); }
        public BlockGetter getLevel() { return chunks.values().iterator().next(); }
    }

    static JsonObject position(int x, int y, int z) {
        JsonObject item = new JsonObject();
        item.addProperty("x", x); item.addProperty("y", y); item.addProperty("z", z);
        return item;
    }

    static JsonObject column(int x, int z) {
        JsonObject item = new JsonObject();
        item.addProperty("x", x); item.addProperty("z", z);
        return item;
    }

    static void put(World world, JsonArray placements, int x, int y, int z, String name) {
        BlockState state = STATES.get(name);
        ProtoChunk chunk = world.chunks.get(new ChunkPos(x >> 4, z >> 4));
        if (chunk == null) throw new AssertionError("unavailable placement");
        BlockState old = chunk.setBlockState(new BlockPos(x, y, z), state, 0);
        if (old != Blocks.AIR.defaultBlockState()) throw new AssertionError("fixture placement overlap");
        JsonObject item = position(x, y, z);
        item.addProperty("state", Block.getId(state)); item.addProperty("name", name);
        placements.add(item);
    }

    static JsonArray run(LevelLightEngine engine) {
        JsonArray work = new JsonArray();
        do {
            work.add(engine.runLightUpdates());
            if (work.size() > 4) throw new AssertionError("unexpected remaining combined work");
        } while (engine.hasLightWork());
        return work;
    }

    static JsonObject layer(LevelLightEngine engine, LightLayer kind) {
        JsonObject output = new JsonObject();
        JsonArray layers = new JsonArray();
        var listener = engine.getLayerListener(kind);
        // Non-air support is contained by x=0..1, y=0..2, z=0. Enumerate
        // its entire one-section shell, including unavailable chunk columns.
        for (int x = -1; x <= 2; x++) for (int y = -1; y <= 3; y++) for (int z = -1; z <= 1; z++) {
            SectionPos section = SectionPos.of(x, y, z);
            DataLayer data = listener.getDataLayerData(section);
            if (data == null) continue;
            JsonObject item = position(x, y, z);
            item.addProperty("type", engine.getDebugSectionType(kind, section).name());
            item.addProperty("empty", data.isEmpty());
            item.addProperty("uniform", data.isDefinitelyHomogenous());
            // Observation must not materialize the live engine layer.
            item.addProperty("bytes", HexFormat.of().formatHex(data.copy().getData()));
            layers.add(item);
            for (int index = 0; index < 4096; index++) {
                int localX = index & 15, localZ = (index >> 4) & 15, localY = index >> 8;
                BlockPos block = new BlockPos(x * 16 + localX, y * 16 + localY, z * 16 + localZ);
                if (listener.getLightValue(block) != data.get(localX, localY, localZ)) {
                    throw new AssertionError("stored/visible mismatch " + kind + " " + block);
                }
            }
        }
        output.add("layers", layers);
        JsonArray probes = new JsonArray();
        for (int[] position : new int[][] {
            {15, 8, 8}, {16, 8, 8}, {18, 9, 8}, {15, 30, 8}, {16, 32, 8},
            {31, 32, 8}, {32, 32, 8}, {0, -17, 0}, {0, 64, 0}, {-1, 8, 8}
        }) {
            JsonArray probe = new JsonArray();
            for (int coordinate : position) probe.add(coordinate);
            probe.add(listener.getLightValue(new BlockPos(position[0], position[1], position[2])));
            probes.add(probe);
        }
        output.add("probes", probes);
        return output;
    }

    static JsonObject scenario(String name, boolean highRoof) {
        World world = new World();
        JsonObject result = new JsonObject();
        result.addProperty("name", name);
        result.addProperty("min_y", 0); result.addProperty("height", 48);
        result.addProperty("has_sky", true);
        JsonArray chunks = new JsonArray();
        for (ChunkPos chunk : world.chunks.keySet()) chunks.add(column(chunk.x(), chunk.z()));
        result.add("chunks", chunks);
        JsonArray placements = new JsonArray();
        for (int x = 0; x < 32; x++) for (int z = 0; z < 16; z++) {
            if (!highRoof || (x + z) % 3 != 0) put(world, placements, x, 0, z, "stone");
        }
        if (highRoof) {
            for (int x = 0; x < 16; x++) for (int z = 0; z < 16; z++) {
                put(world, placements, x, 39, z, x == 15 ? "water" : "stone");
            }
            put(world, placements, 15, 32, 8, "glowstone");
            put(world, placements, 18, 32, 8, "torch");
            put(world, placements, 31, 32, 8, "redstone_torch");
            put(world, placements, 16, 33, 8, "bottom_slab");
            put(world, placements, 17, 33, 8, "top_slab");
        } else {
            for (int x = 8; x < 24; x++) for (int z = 4; z < 12; z++) {
                put(world, placements, x, 31, z, x == 15 ? "water" : x == 16 ? "glass" : "stone");
            }
            put(world, placements, 15, 8, 8, "glowstone");
            put(world, placements, 18, 9, 8, "torch");
            put(world, placements, 15, 9, 8, "bottom_slab");
            put(world, placements, 16, 9, 8, "top_slab");
        }
        result.add("placements", placements);
        LevelLightEngine engine = new LevelLightEngine(world, true, true);
        JsonArray active = new JsonArray();
        JsonArray sourceCaches = new JsonArray();
        for (var entry : world.chunks.entrySet()) {
            ChunkPos address = entry.getKey();
            ProtoChunk chunk = entry.getValue();
            chunk.initializeLightSources();
            JsonObject cache = column(address.x(), address.z());
            JsonArray lowest = new JsonArray();
            for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                lowest.add(chunk.getSkyLightSources().getLowestSourceY(x, z));
            }
            cache.add("lowest", lowest);
            cache.addProperty("highest", chunk.getSkyLightSources().getHighestLowestSourceY());
            sourceCaches.add(cache);
        }
        // Source caches for every available chunk precede support and seeding.
        for (var entry : world.chunks.entrySet()) {
            ChunkPos address = entry.getKey();
            ProtoChunk chunk = entry.getValue();
            for (int y = 0; y < 3; y++) if (!chunk.getSection(y).hasOnlyAir()) {
                engine.updateSectionStatus(SectionPos.of(address.x(), y, address.z()), false);
                active.add(position(address.x(), y, address.z()));
            }
        }
        result.add("active_sections", active); result.add("sky_sources", sourceCaches);
        JsonArray order = new JsonArray();
        for (ChunkPos chunk : world.chunks.keySet()) {
            engine.setLightEnabled(chunk, true);
            engine.propagateLightSources(chunk);
            order.add(column(chunk.x(), chunk.z()));
        }
        result.add("source_order", order);
        result.add("java_work", run(engine));
        result.add("block", layer(engine, LightLayer.BLOCK));
        result.add("sky", layer(engine, LightLayer.SKY));
        return result;
    }

    static JsonArray profiles() {
        JsonArray result = new JsonArray();
        for (var entry : STATES.entrySet()) {
            BlockState state = entry.getValue();
            JsonObject item = new JsonObject();
            item.addProperty("name", entry.getKey()); item.addProperty("id", Block.getId(state));
            item.addProperty("emission", state.getLightEmission()); item.addProperty("dampening", state.getLightDampening());
            item.addProperty("can_occlude", state.canOcclude()); item.addProperty("use_shape", state.useShapeForLightOcclusion());
            item.addProperty("empty_shape", !state.canOcclude() || !state.useShapeForLightOcclusion());
            result.add(item);
        }
        return result;
    }

    public static void main(String[] args) throws Exception {
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        STATES.put("air", Blocks.AIR.defaultBlockState());
        STATES.put("bedrock", Blocks.BEDROCK.defaultBlockState());
        STATES.put("stone", Blocks.STONE.defaultBlockState());
        STATES.put("water", Blocks.WATER.defaultBlockState());
        STATES.put("glass", Blocks.GLASS.defaultBlockState());
        STATES.put("glowstone", Blocks.GLOWSTONE.defaultBlockState());
        STATES.put("torch", Blocks.TORCH.defaultBlockState());
        STATES.put("redstone_torch", Blocks.REDSTONE_TORCH.defaultBlockState());
        STATES.put("bottom_slab", Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE, SlabType.BOTTOM));
        STATES.put("top_slab", Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE, SlabType.TOP));
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var setup = new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false),
            Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        JsonObject report = new JsonObject();
        try (ExecutorService worker = Executors.newFixedThreadPool(2)) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(setup,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, config) -> {
                    try (resources) {
                        managers.updateComponentsAndStaticRegistryTags();
                        factory = PalettedContainerFactory.create(registries.compositeAccess());
                        JsonArray scenarios = new JsonArray();
                        scenarios.add(scenario("partial_roof_cross_chunk_sources", false));
                        scenarios.add(scenario("high_roof_support_and_missing_neighbor", true));
                        report.add("scenarios", scenarios);
                        return true;
                    }
                }, worker, Runnable::run).join();
            report.addProperty("version", SharedConstants.getCurrentVersion().id());
            report.add("profiles", profiles());
            Files.writeString(Path.of(args[0]), JSON.toJson(report) + "\n");
            System.out.println("Recorded two combined block/sky initial-light scenarios.");
        } finally { Util.shutdownExecutors(); }
    }
}
