import com.google.gson.*;
import java.io.*;
import java.nio.file.*;
import java.security.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.Util;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;

/** Independently authored public API caller. Generated registry data stays in local Decompile. */
public final class ExportBlockStateData {
    static { SharedConstants.tryDetectVersion(); }
    private static final Gson JSON = new GsonBuilder().disableHtmlEscaping().create();

    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("Expected one output directory");
        Path output = Path.of(args[0]).toAbsolutePath();
        Files.createDirectories(output);
        try {
            Bootstrap.bootStrap();
            JsonObject root = new JsonObject();
            int stateCount = Block.BLOCK_STATE_REGISTRY.size();
            root.addProperty("state_count", stateCount);
            int[] flags = new int[stateCount];
            boolean[] seen = new boolean[stateCount];
            JsonArray blocks = new JsonArray();
            for (Block block : BuiltInRegistries.BLOCK) {
                JsonObject row = new JsonObject();
                row.addProperty("id", BuiltInRegistries.BLOCK.getKey(block).toString());
                row.addProperty("default_state", Block.getId(block.defaultBlockState()));
                List<Property<?>> properties = new ArrayList<>(block.getStateDefinition().getProperties());
                properties.sort(Comparator.comparing(Property::getName));
                JsonArray propertyRows = new JsonArray();
                int combinations = 1;
                for (Property<?> property : properties) {
                    JsonObject item = new JsonObject();
                    item.addProperty("name", property.getName());
                    List<String> names = names(property);
                    item.add("values", JSON.toJsonTree(names));
                    item.addProperty("default_index", names.indexOf(valueName(property, block.defaultBlockState())));
                    propertyRows.add(item);
                    combinations = Math.multiplyExact(combinations, names.size());
                }
                row.add("properties", propertyRows);
                int[] states = new int[combinations];
                Arrays.fill(states, -1);
                for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                    int ordinal = 0;
                    for (Property<?> property : properties) {
                        List<String> names = names(property);
                        int index = names.indexOf(valueName(property, state));
                        if (index < 0) throw new IllegalStateException("Unknown property value");
                        ordinal = Math.addExact(Math.multiplyExact(ordinal, names.size()), index);
                    }
                    int id = Block.getId(state);
                    if (id < 0 || id >= stateCount || seen[id] || states[ordinal] != -1)
                        throw new IllegalStateException("Duplicate or invalid state ID/combination");
                    seen[id] = true;
                    states[ordinal] = id;
                    flags[id] = (state.isAir() ? 1 : 0) | (state.getFluidState().isEmpty() ? 0 : 2);
                }
                for (int id : states) if (id < 0) throw new IllegalStateException("Missing property combination");
                row.add("states", JSON.toJsonTree(states));
                blocks.add(row);
            }
            for (boolean present : seen) if (!present) throw new IllegalStateException("Missing global state ID");
            root.add("state_flags", JSON.toJsonTree(flags));
            root.add("blocks", blocks);
            write(output, "blocks.json", root);
            JsonObject metadata = new JsonObject();
            metadata.addProperty("minecraft_version", SharedConstants.getCurrentVersion().id());
            metadata.addProperty("protocol", SharedConstants.getCurrentVersion().protocolVersion());
            metadata.addProperty("block_count", blocks.size());
            metadata.addProperty("state_count", stateCount);
            Path jar = Path.of(Block.class.getProtectionDomain().getCodeSource().getLocation().toURI());
            JsonObject source = new JsonObject();
            source.addProperty("sha256", sha256(jar));
            source.addProperty("bytes", Files.size(jar));
            metadata.add("source_jar", source);
            write(output, "export-metadata.json", metadata);
        } finally {
            Util.shutdownExecutors();
        }
    }

    private static <T extends Comparable<T>> List<String> names(Property<T> property) {
        return property.getPossibleValues().stream().map(property::getName).toList();
    }
    private static <T extends Comparable<T>> String valueName(Property<T> property, BlockState state) {
        return property.getName(state.getValue(property));
    }
    private static void write(Path output, String name, JsonElement value) throws IOException {
        Files.writeString(output.resolve(name), JSON.toJson(value) + "\n");
    }
    private static String sha256(Path path) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (InputStream input = Files.newInputStream(path)) {
            byte[] buffer = new byte[65536];
            for (int count; (count = input.read(buffer)) >= 0;) digest.update(buffer, 0, count);
        }
        return HexFormat.of().formatHex(digest.digest());
    }
}
