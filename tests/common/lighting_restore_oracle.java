import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
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
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.chunk.storage.SerializableChunkData;
import net.minecraft.world.level.lighting.*;

/** Original finite saved-light transactions observed through actual public APIs.
 * This does not instantiate the Threaded dispatcher or emulate its task batching. */
class LightingRestoreOracle {
    static { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().serializeNulls().create();
    static final LevelHeightAccessor HEIGHT = LevelHeightAccessor.create(0, 32);
    static final ChunkPos ADDRESS = new ChunkPos(0, 0);
    static PalettedContainerFactory factory;
    static Path output;

    static final class World implements LightChunkGetter {
        final ProtoChunk chunk = new ProtoChunk(ADDRESS, UpgradeData.EMPTY, HEIGHT, factory, null);
        ProtoChunk neighbor;
        public LightChunk getChunkForLighting(int x, int z) { return z != 0 ? null : x == 0 ? chunk : x == 1 ? neighbor : null; }
        public BlockGetter getLevel() { return chunk; }
    }

    static byte[] pattern(int seed) {
        byte[] bytes = new byte[2048];
        if (seed != 0) for (int i = 0; i < bytes.length; i++) {
            int low = (seed + i * 3) & 15, high = (seed * 5 + i * 7) & 15;
            bytes[i] = (byte)(low | (high << 4));
        }
        return bytes;
    }

    static CompoundTag terrain(boolean occupied) {
        CompoundTag data = new CompoundTag();
        ListTag palette = new ListTag();
        palette.add(BlockState.CODEC.encodeStart(NbtOps.INSTANCE, Blocks.AIR.defaultBlockState()).getOrThrow());
        if (occupied) {
            palette.add(BlockState.CODEC.encodeStart(NbtOps.INSTANCE, Blocks.GLOWSTONE.defaultBlockState()).getOrThrow());
            palette.add(BlockState.CODEC.encodeStart(NbtOps.INSTANCE, Blocks.STONE.defaultBlockState()).getOrThrow());
            SimpleBitStorage bits = new SimpleBitStorage(4, 4096);
            bits.set(8 | (8 << 4) | (8 << 8), 1);
            bits.set(0, 2);
            data.putLongArray("data", bits.getRaw());
        }
        data.put("palette", palette);
        return data;
    }

    static CompoundTag row(int y, boolean occupied, Integer block, Integer sky) {
        CompoundTag row = new CompoundTag();
        row.putByte("Y", (byte)y);
        if (y == 0 || y == 1) row.put("block_states", terrain(occupied));
        if (block != null) row.putByteArray("BlockLight", pattern(block));
        if (sky != null) row.putByteArray("SkyLight", pattern(sky));
        return row;
    }

    static CompoundTag input(String status, boolean flag, String layout) {
        CompoundTag input = new CompoundTag();
        input.putInt("DataVersion", 5018);
        input.putInt("xPos", 0); input.putInt("zPos", 0);
        input.putString("Status", "minecraft:" + status);
        input.putBoolean("isLightOn", flag);
        ListTag rows = new ListTag();
        boolean occupied = !layout.equals("queued_only");
        if (layout.equals("absent") || layout.equals("wrong_type")) {
            CompoundTag section = row(0, true, null, null);
            if (layout.equals("wrong_type")) {
                section.putString("BlockLight", "ignored");
                section.putIntArray("SkyLight", new int[]{0, 1, 2});
            }
            rows.add(section);
        } else if (layout.equals("zero")) {
            rows.add(row(0, true, 0, 0));
            rows.add(row(1, false, 0, 0));
            rows.add(row(2, false, 0, 0));
        } else if (layout.equals("sky_only")) {
            rows.add(row(0, false, null, 5));
            rows.add(row(120, false, null, 11));
        } else {
            // A later missing field must preserve a preceding saved layer, while
            // subsequent present values replace each kind independently.
            rows.add(row(0, occupied, 3, 0));
            rows.add(row(0, occupied, null, null));
            rows.add(row(0, occupied, 7, null));
            rows.add(row(1, false, null, 11));
            rows.add(row(-1, false, 0, null));
            rows.add(row(2, false, 5, 0));
            rows.add(row(120, false, 13, 9));
            rows.add(row(0, occupied, null, 2));
        }
        input.put("sections", rows);
        return input;
    }

    static JsonObject data(DataLayer layer) {
        if (layer == null) return null;
        JsonObject value = new JsonObject();
        value.addProperty("empty", layer.isEmpty());
        value.addProperty("uniform", layer.isDefinitelyHomogenous());
        value.addProperty("bytes", HexFormat.of().formatHex(layer.copy().getData()));
        return value;
    }

    static JsonObject location(int x, int y, int z) {
        JsonObject row = new JsonObject();
        row.addProperty("x", x); row.addProperty("y", y); row.addProperty("z", z);
        return row;
    }

    static JsonObject phase(World world, LevelLightEngine engine, String name) {
        JsonObject phase = new JsonObject();
        phase.addProperty("name", name);
        phase.addProperty("light_correct", world.chunk.isLightCorrect());
        phase.addProperty("has_work", engine.hasLightWork());
        phase.addProperty("enabled", engine.lightOnInColumn(SectionPos.of(0, 0, 0).asLong()));
        JsonArray rows = new JsonArray();
        for (LightLayer kind : LightLayer.values()) for (int y : new int[]{-1, 0, 1, 2, 120}) {
            var listener = engine.getLayerListener(kind);
            JsonObject row = location(0, y, 0);
            row.addProperty("kind", kind.name());
            row.addProperty("support", engine.getDebugSectionType(kind, SectionPos.of(0, y, 0)).name());
            DataLayer layer = listener.getDataLayerData(SectionPos.of(0, y, 0));
            row.add("data", data(layer));
            JsonArray samples = new JsonArray();
            for (int index : new int[]{0, 1, 137, 2184, 4095}) {
                samples.add(listener.getLightValue(new BlockPos(index & 15, y * 16 + (index >> 8), (index >> 4) & 15)));
            }
            row.add("visible_samples", samples);
            rows.add(row);
        }
        phase.add("rows", rows);
        return phase;
    }

    static JsonArray finished(LevelLightEngine engine, LightLayer kind, int maxChunkX) {
        JsonArray rows = new JsonArray();
        var listener = engine.getLayerListener(kind);
        for (int x = -1; x <= maxChunkX + 1; x++) for (int z = -1; z <= 1; z++) {
            for (int y : new int[]{-1, 0, 1, 2, 120}) {
                JsonObject row = location(x, y, z);
                row.addProperty("support", engine.getDebugSectionType(kind, SectionPos.of(x, y, z)).name());
                row.add("data", data(listener.getDataLayerData(SectionPos.of(x, y, z))));
                byte[] visible = new byte[2048];
                for (int index = 0; index < 4096; index++) {
                    int light = listener.getLightValue(new BlockPos(x * 16 + (index & 15), y * 16 + (index >> 8), z * 16 + ((index >> 4) & 15)));
                    visible[index >> 1] |= (byte)(light << ((index & 1) * 4));
                }
                row.addProperty("visible", HexFormat.of().formatHex(visible));
                rows.add(row);
            }
        }
        return rows;
    }

    static JsonObject scenario(String status, boolean flag, boolean sky, String layout) throws Exception {
        String name = status + "_" + flag + "_" + sky + "_" + layout;
        CompoundTag input = input(status, flag, layout);
        NbtIo.write(input, output.resolve(name + ".nbt"));
        SerializableChunkData parsed = Objects.requireNonNull(SerializableChunkData.parse(HEIGHT, factory, input));
        World world = new World();
        for (var row : parsed.sectionData()) if (row.chunkSection() != null) {
            world.chunk.getSections()[HEIGHT.getSectionIndexFromSectionY(row.y())] = row.chunkSection();
        }
        world.chunk.setPersistedStatus(parsed.chunkStatus());
        world.chunk.setLightCorrect(parsed.lightCorrect());
        LevelLightEngine engine = new LevelLightEngine(world, true, sky);
        world.chunk.setLightEngine(engine);
        JsonObject result = new JsonObject();
        result.addProperty("name", name); result.addProperty("status", status);
        result.addProperty("flag", flag); result.addProperty("has_sky", sky); result.addProperty("layout", layout);
        JsonArray files = new JsonArray();
        JsonObject file = location(0, 0, 0); file.addProperty("nbt", name + ".nbt"); files.add(file);
        result.add("chunks", files);
        JsonArray phases = new JsonArray();
        boolean retain = parsed.sectionData().stream().anyMatch(row -> row.blockLight() != null || (sky && row.skyLight() != null));
        if (retain) engine.retainData(ADDRESS, true);
        for (var row : parsed.sectionData()) {
            SectionPos position = SectionPos.of(ADDRESS, row.y());
            if (row.blockLight() != null) engine.queueSectionData(LightLayer.BLOCK, position, row.blockLight());
            if (sky && row.skyLight() != null) engine.queueSectionData(LightLayer.SKY, position, row.skyLight());
        }
        phases.add(phase(world, engine, "staged"));
        world.chunk.initializeLightSources();
        for (int section = 0; section < HEIGHT.getSectionsCount(); section++) {
            if (!world.chunk.getSection(section).hasOnlyAir()) engine.updateSectionStatus(SectionPos.of(ADDRESS, HEIGHT.getSectionYFromSectionIndex(section)), false);
        }
        engine.runLightUpdates();
        phases.add(phase(world, engine, "initialize_update"));
        boolean reuse = parsed.chunkStatus().isOrAfter(ChunkStatus.LIGHT) && parsed.lightCorrect();
        engine.setLightEnabled(ADDRESS, reuse);
        engine.retainData(ADDRESS, false);
        phases.add(phase(world, engine, "initialize_post"));
        world.chunk.setLightCorrect(false);
        if (!reuse) engine.propagateLightSources(ADDRESS);
        phases.add(phase(world, engine, "light_pre"));
        engine.runLightUpdates();
        phases.add(phase(world, engine, "light_update"));
        world.chunk.setLightCorrect(true);
        phases.add(phase(world, engine, "light_post"));
        result.addProperty("reuse", reuse); result.addProperty("retained_saved", retain);
        result.addProperty("finished_has_work", engine.hasLightWork());
        result.add("phases", phases);
        result.add("block", finished(engine, LightLayer.BLOCK, 0));
        if (sky) result.add("sky", finished(engine, LightLayer.SKY, 0));
        return result;
    }

    static CompoundTag boundaryTerrain(int chunkX) {
        CompoundTag states = terrain(true);
        SimpleBitStorage bits = new SimpleBitStorage(4, 4096);
        // A partial roof spans x=12..19 and a relit emitter at world x=16
        // can affect the saved/reused neighbor across x=15/16.
        for (int z = 5; z <= 10; z++) for (int localX = 0; localX < 16; localX++) {
            int worldX = chunkX * 16 + localX;
            if (worldX >= 12 && worldX <= 19) bits.set(localX | (z << 4) | (15 << 8), 2);
        }
        if (chunkX == 1) bits.set((8 << 4) | (8 << 8), 1);
        states.putLongArray("data", bits.getRaw());
        return states;
    }

    static JsonObject mixedNeighbors() throws Exception {
        String name = "mixed_neighbors_reuse_left_relight_right";
        World world = new World();
        world.neighbor = new ProtoChunk(new ChunkPos(1, 0), UpgradeData.EMPTY, HEIGHT, factory, null);
        ProtoChunk[] chunks = {world.chunk, world.neighbor};
        SerializableChunkData[] saved = new SerializableChunkData[2];
        JsonArray files = new JsonArray();
        for (int x = 0; x < 2; x++) {
            CompoundTag input = input(x == 0 ? "light" : "initialize_light", x == 0, "mixed");
            input.putInt("xPos", x);
            for (CompoundTag row : input.getListOrEmpty("sections").compoundStream().toList()) {
                if (row.getByteOr("Y", (byte)0) == 0) row.put("block_states", boundaryTerrain(x));
            }
            String fileName = name + "_" + x + ".nbt";
            NbtIo.write(input, output.resolve(fileName));
            JsonObject file = location(x, 0, 0); file.addProperty("nbt", fileName); files.add(file);
            saved[x] = Objects.requireNonNull(SerializableChunkData.parse(HEIGHT, factory, input));
            for (var row : saved[x].sectionData()) if (row.chunkSection() != null) {
                chunks[x].getSections()[HEIGHT.getSectionIndexFromSectionY(row.y())] = row.chunkSection();
            }
            chunks[x].setPersistedStatus(saved[x].chunkStatus());
            chunks[x].setLightCorrect(saved[x].lightCorrect());
        }
        LevelLightEngine engine = new LevelLightEngine(world, true, true);
        JsonArray phases = new JsonArray();
        for (int x = 0; x < 2; x++) {
            chunks[x].setLightEngine(engine);
            engine.retainData(chunks[x].getPos(), true);
            for (var row : saved[x].sectionData()) {
                SectionPos pos = SectionPos.of(chunks[x].getPos(), row.y());
                if (row.blockLight() != null) engine.queueSectionData(LightLayer.BLOCK, pos, row.blockLight());
                if (row.skyLight() != null) engine.queueSectionData(LightLayer.SKY, pos, row.skyLight());
            }
        }
        phases.add(phase(world, engine, "staged"));
        for (ProtoChunk chunk : chunks) chunk.initializeLightSources();
        for (ProtoChunk chunk : chunks) for (int section = 0; section < HEIGHT.getSectionsCount(); section++) {
            if (!chunk.getSection(section).hasOnlyAir()) engine.updateSectionStatus(SectionPos.of(chunk.getPos(), HEIGHT.getSectionYFromSectionIndex(section)), false);
        }
        engine.runLightUpdates();
        phases.add(phase(world, engine, "initialize_update"));
        boolean[] reuse = new boolean[2];
        for (int x = 0; x < 2; x++) {
            reuse[x] = chunks[x].getPersistedStatus().isOrAfter(ChunkStatus.LIGHT) && chunks[x].isLightCorrect();
            engine.setLightEnabled(chunks[x].getPos(), reuse[x]);
            engine.retainData(chunks[x].getPos(), false);
        }
        if (!reuse[0] || reuse[1]) throw new AssertionError("mixed fixture lost reuse distinction");
        phases.add(phase(world, engine, "initialize_post"));
        for (ProtoChunk chunk : chunks) chunk.setLightCorrect(false);
        for (int x = 0; x < 2; x++) if (!reuse[x]) engine.propagateLightSources(chunks[x].getPos());
        phases.add(phase(world, engine, "light_pre"));
        engine.runLightUpdates();
        phases.add(phase(world, engine, "light_update"));
        for (ProtoChunk chunk : chunks) chunk.setLightCorrect(true);
        phases.add(phase(world, engine, "light_post"));
        JsonObject result = new JsonObject();
        result.addProperty("name", name); result.addProperty("layout", "mixed_neighbors");
        result.addProperty("status", "light"); result.addProperty("flag", true);
        result.addProperty("has_sky", true); result.addProperty("reuse", true);
        result.addProperty("finished_has_work", engine.hasLightWork());
        result.add("chunks", files); result.add("phases", phases);
        result.add("block", finished(engine, LightLayer.BLOCK, 1));
        result.add("sky", finished(engine, LightLayer.SKY, 1));
        return result;
    }

    static JsonObject invalid(String kind, int length, boolean flag, boolean sky) throws Exception {
        String name = "invalid_" + kind + "_" + length + "_" + flag + "_" + sky;
        CompoundTag input = input("initialize_light", flag, "absent");
        input.getListOrEmpty("sections").getCompound(0).orElseThrow().putByteArray(kind, new byte[length]);
        NbtIo.write(input, output.resolve(name + ".nbt"));
        JsonObject result = new JsonObject();
        result.addProperty("name", name); result.addProperty("length", length);
        result.addProperty("kind", kind); result.addProperty("flag", flag); result.addProperty("has_sky", sky);
        try {
            SerializableChunkData.parse(HEIGHT, factory, input);
            throw new AssertionError("wrong length accepted");
        } catch (IllegalArgumentException expected) {
            result.addProperty("error", expected.getClass().getName());
        }
        return result;
    }

    public static void main(String[] args) throws Exception {
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong version");
        output = Path.of(args[0]);
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var setup = new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false),
            Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        JsonObject report = new JsonObject();
        try (ExecutorService workers = Executors.newFixedThreadPool(2)) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(setup,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, config) -> {
                    try (resources) {
                        managers.updateComponentsAndStaticRegistryTags();
                        factory = PalettedContainerFactory.create(registries.compositeAccess());
                        JsonArray cases = new JsonArray();
                        for (String status : List.of("initialize_light", "light", "full")) for (boolean flag : new boolean[]{false, true}) for (boolean sky : new boolean[]{false, true}) {
                            cases.add(scenario(status, flag, sky, "mixed"));
                        }
                        for (String layout : List.of("absent", "zero", "queued_only", "sky_only", "wrong_type")) for (boolean sky : new boolean[]{false, true}) {
                            cases.add(scenario("full", true, sky, layout));
                        }
                        cases.add(mixedNeighbors());
                        report.add("scenarios", cases);
                        JsonArray errors = new JsonArray();
                        for (String kind : List.of("BlockLight", "SkyLight")) for (int length : new int[]{2047, 2049}) for (boolean flag : new boolean[]{false, true}) for (boolean sky : new boolean[]{false, true}) {
                            errors.add(invalid(kind, length, flag, sky));
                        }
                        report.add("parse_errors", errors);
                        return true;
                    } catch (Exception error) { throw new RuntimeException(error); }
                }, workers, Runnable::run).join();
            report.addProperty("version", SharedConstants.getCurrentVersion().id());
            report.addProperty("transaction", "saved staging; initialize PRE/UPDATE/POST; light PRE/UPDATE/POST, one selected domain");
            Files.writeString(output.resolve("observations.json"), JSON.toJson(report) + "\n");
            System.out.println("Recorded 23 saved-light initialization transactions, including a mixed two-chunk domain, 138 phase boundaries and 16 parse rejections.");
        } finally { Util.shutdownExecutors(); }
    }
}
