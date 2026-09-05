import com.google.gson.*;
import java.io.*;
import java.nio.*;
import java.nio.file.*;
import java.security.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.Direction;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.util.Util;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/** Independent public-API observations; generated data stays in the local reference. */
public final class ExportLightingData {
    static { SharedConstants.tryDetectVersion(); }
    private static final Gson JSON = new GsonBuilder().disableHtmlEscaping().create();
    private static final Direction[] DIRECTIONS = {
        Direction.DOWN, Direction.UP, Direction.NORTH, Direction.SOUTH, Direction.WEST, Direction.EAST
    };
    private record Face(long[][] coordinates, byte[] cells, byte[] encoded) {}
    private record Variant(int id, VoxelShape shape) {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("Expected one output directory");
        Path output = Path.of(args[0]).toAbsolutePath();
        Files.createDirectories(output);
        Bootstrap.bootStrap();
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        var config = new WorldLoader.InitConfig(
            new WorldLoader.PackConfig(packs, WorldDataConfiguration.DEFAULT, false, false),
            Commands.CommandSelection.DEDICATED, PermissionSet.ALL_PERMISSIONS);
        try (ExecutorService worker = Executors.newFixedThreadPool(Math.max(1, Math.min(4, Runtime.getRuntime().availableProcessors())))) {
            WorldLoader.<WorldDataConfiguration, Boolean>load(config,
                context -> new WorldLoader.DataLoadOutput<>(context.dataConfiguration(), context.datapackDimensions()),
                (resources, managers, registries, worldConfig) -> {
                    try (resources) {
                        export(output, packs);
                        return true;
                    } catch (Exception error) {
                        throw new CompletionException(error);
                    }
                }, worker, Runnable::run).join();
        } finally {
            Util.shutdownExecutors();
        }
    }

    private static int coordinateIndex(long[] coordinates, double endpoint) {
        long bits = Double.doubleToRawLongBits(endpoint);
        for (int index = 0; index < coordinates.length; index++) {
            if (coordinates[index] == bits) return index;
        }
        throw new IllegalStateException("Box endpoint absent from raw coordinate domain");
    }

    private static Face capture(VoxelShape shape) {
        long[][] coordinates = new long[3][];
        Direction.Axis[] axes = {Direction.Axis.X, Direction.Axis.Y, Direction.Axis.Z};
        int[] sizes = new int[3];
        int cells = 1;
        int coordinateBytes = 0;
        for (int axis = 0; axis < axes.length; axis++) {
            var values = shape.getCoords(axes[axis]);
            if (values.isEmpty()) throw new IllegalStateException("Missing shape coordinate domain");
            coordinates[axis] = new long[values.size()];
            sizes[axis] = values.size() - 1;
            cells = Math.multiplyExact(cells, sizes[axis]);
            coordinateBytes = Math.addExact(coordinateBytes, Math.multiplyExact(values.size(), Long.BYTES));
            for (int index = 0; index < values.size(); index++) {
                double value = values.getDouble(index);
                if (!Double.isFinite(value) || index > 0 && !(value > values.getDouble(index - 1)))
                    throw new IllegalStateException("Shape coordinates must be finite and strictly increasing");
                coordinates[axis][index] = Double.doubleToRawLongBits(value);
            }
        }
        byte[] occupied = new byte[Math.addExact(cells, 7) / 8];
        shape.forAllBoxes((x0, y0, z0, x1, y1, z1) -> {
            int[] start = {coordinateIndex(coordinates[0], x0), coordinateIndex(coordinates[1], y0),
                           coordinateIndex(coordinates[2], z0)};
            int[] end = {coordinateIndex(coordinates[0], x1), coordinateIndex(coordinates[1], y1),
                         coordinateIndex(coordinates[2], z1)};
            for (int axis = 0; axis < 3; axis++) {
                if (start[axis] >= end[axis]) throw new IllegalStateException("Empty or reversed shape box");
            }
            for (int x = start[0]; x < end[0]; x++) {
                for (int y = start[1]; y < end[1]; y++) {
                    for (int z = start[2]; z < end[2]; z++) {
                        int bit = (x * sizes[1] + y) * sizes[2] + z;
                        occupied[bit >>> 3] |= (byte) (1 << (bit & 7));
                    }
                }
            }
        });
        ByteBuffer encoded = ByteBuffer.allocate(12 + coordinateBytes + occupied.length).order(ByteOrder.LITTLE_ENDIAN);
        for (long[] axis : coordinates) {
            encoded.putInt(axis.length);
            for (long value : axis) encoded.putLong(value);
        }
        encoded.put(occupied);
        return new Face(coordinates, occupied, encoded.array());
    }

    private static void export(Path output, PackRepository packs) throws Exception {
        List<String> selected = packs.getSelectedPacks().stream().map(Pack::getId).toList();
        if (!selected.equals(List.of("vanilla"))) throw new IllegalStateException("Expected Vanilla-only packs");
        List<Face> faces = new ArrayList<>();
        List<VoxelShape> shapes = new ArrayList<>();
        Map<String, Integer> ids = new LinkedHashMap<>();
        for (VoxelShape canonical : List.of(Shapes.empty(), Shapes.block())) {
            Face face = capture(canonical);
            ids.put(Base64.getEncoder().encodeToString(face.encoded()), faces.size());
            faces.add(face);
            shapes.add(canonical);
        }
        int states = Block.BLOCK_STATE_REGISTRY.size();
        ByteBuffer materials = ByteBuffer.allocate(Math.multiplyExact(states, 16)).order(ByteOrder.LITTLE_ENDIAN);
        IdentityHashMap<VoxelShape, Integer> cached = new IdentityHashMap<>();
        Map<String, Variant> variants = new LinkedHashMap<>();
        int disabledNonemptyFaces = 0;
        for (int stateId = 0; stateId < states; stateId++) {
            BlockState state = Block.BLOCK_STATE_REGISTRY.byId(stateId);
            if (state == null || Block.getId(state) != stateId) throw new IllegalStateException("Missing global state");
            int emission = state.getLightEmission();
            int dampening = state.getLightDampening();
            if (emission < 0 || emission > 15 || dampening < 0 || dampening > 15)
                throw new IllegalStateException("Lighting value outside 0..15");
            int flags = (state.canOcclude() ? 1 : 0) | (state.useShapeForLightOcclusion() ? 2 : 0);
            materials.put((byte) emission).put((byte) dampening).put((byte) flags).put((byte) 0);
            for (Direction direction : DIRECTIONS) {
                VoxelShape shape = state.getFaceOcclusionShape(direction);
                Integer id = cached.get(shape);
                if (id == null) {
                    Face face = capture(shape);
                    String key = Base64.getEncoder().encodeToString(face.encoded());
                    id = ids.get(key);
                    if (id == null) {
                        id = faces.size();
                        if (id > 65535) throw new IllegalStateException("Face dictionary exceeds u16");
                        ids.put(key, id);
                        faces.add(face);
                        shapes.add(shape);
                    }
                    cached.put(shape, id);
                    String runtime = id + ":" + shape.getClass().getName()
                        + ":" + (shape == Shapes.empty()) + ":" + (shape == Shapes.block());
                    for (Direction.Axis axis : Direction.Axis.values())
                        runtime += ":" + shape.getCoords(axis).getClass().getName();
                    variants.putIfAbsent(runtime, new Variant(id, shape));
                }
                if (flags != 3 && id != 0) disabledNonemptyFaces++;
                materials.putShort((short) (int) id);
            }
        }
        long descriptorBytes = faces.stream().mapToLong(face -> face.encoded().length).sum();
        int pairCount = Math.multiplyExact(faces.size(), faces.size());
        byte[] pairs = new byte[Math.addExact(pairCount, 7) / 8];
        int occludingPairs = 0;
        for (int first = 0; first < faces.size(); first++) {
            for (int second = 0; second < faces.size(); second++) {
                if (Shapes.faceShapeOccludes(shapes.get(first), shapes.get(second))) {
                    int bit = first * faces.size() + second;
                    pairs[bit >>> 3] |= (byte) (1 << (bit & 7));
                    occludingPairs++;
                }
            }
        }
        // Different public coordinate-list representations must agree before sharing a table row.
        for (Variant first : variants.values()) {
            for (Variant second : variants.values()) {
                int bit = first.id() * faces.size() + second.id();
                boolean expected = (pairs[bit >>> 3] & (1 << (bit & 7))) != 0;
                if (Shapes.faceShapeOccludes(first.shape(), second.shape()) != expected)
                    throw new IllegalStateException("Lossless geometry deduplication changed an ordered face result");
            }
        }
        ByteBuffer binary = ByteBuffer.allocate(Math.addExact(16 + materials.capacity(), pairs.length))
            .order(ByteOrder.LITTLE_ENDIAN);
        binary.put(new byte[] {'A', 'R', 'L', 'I', 'T', 'E', '3', 0});
        binary.putInt(states).putInt(faces.size()).put(materials.array()).put(pairs);
        Files.write(output.resolve("lighting.bin"), binary.array());
        JsonArray descriptors = new JsonArray();
        MessageDigest descriptorDigest = MessageDigest.getInstance("SHA-256");
        for (Face face : faces) {
            descriptorDigest.update(face.encoded());
            JsonObject descriptor = new JsonObject();
            JsonArray axes = new JsonArray();
            for (long[] axis : face.coordinates()) {
                JsonArray bits = new JsonArray();
                for (long value : axis) bits.add(Long.toUnsignedString(value));
                axes.add(bits);
            }
            descriptor.add("coordinate_raw_bits", axes);
            descriptor.addProperty("occupied_cells_hex", HexFormat.of().formatHex(face.cells()));
            descriptors.add(descriptor);
        }
        Files.writeString(output.resolve("lighting-face-descriptors.json"), JSON.toJson(descriptors) + "\n");
        JsonObject metadata = new JsonObject();
        metadata.addProperty("minecraft_version", SharedConstants.getCurrentVersion().id());
        metadata.addProperty("protocol", SharedConstants.getCurrentVersion().protocolVersion());
        metadata.addProperty("state_count", states);
        metadata.addProperty("face_count", faces.size());
        metadata.addProperty("runtime_variant_count", variants.size());
        metadata.addProperty("runtime_variant_ordered_pairs_verified", Math.multiplyExact(variants.size(), variants.size()));
        metadata.addProperty("descriptor_binary_bytes", descriptorBytes);
        metadata.addProperty("descriptor_binary_sha256", HexFormat.of().formatHex(descriptorDigest.digest()));
        metadata.addProperty("material_bytes", materials.array().length);
        metadata.addProperty("pair_bytes", pairs.length);
        metadata.addProperty("occluding_pairs", occludingPairs);
        metadata.addProperty("disabled_nonempty_cached_faces", disabledNonemptyFaces);
        metadata.add("selected_pack_ids", JSON.toJsonTree(selected));
        Path jar = Path.of(Block.class.getProtectionDomain().getCodeSource().getLocation().toURI());
        JsonObject source = new JsonObject();
        source.addProperty("sha256", HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(jar))));
        source.addProperty("bytes", Files.size(jar));
        metadata.add("source_jar", source);
        Files.writeString(output.resolve("lighting-export-metadata.json"), JSON.toJson(metadata) + "\n");
        System.out.println("Lighting export: " + JSON.toJson(metadata));
    }
}
