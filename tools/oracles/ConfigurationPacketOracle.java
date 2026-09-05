import com.google.gson.*;
import io.netty.buffer.*;
import it.unimi.dsi.fastutil.ints.IntArrayList;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;
import java.util.function.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.RegistrySynchronization;
import net.minecraft.nbt.*;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.network.chat.Component;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.*;
import net.minecraft.network.protocol.common.custom.*;
import net.minecraft.network.protocol.configuration.*;
import net.minecraft.network.protocol.cookie.ServerboundCookieResponsePacket;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.repository.KnownPack;
import net.minecraft.tags.TagNetworkSerialization;

/** Synthetic inputs and public codec calls; no copied Minecraft implementation bodies. */
public final class ConfigurationPacketOracle {
    static { SharedConstants.tryDetectVersion(); }
    private static final Gson JSON = new GsonBuilder().disableHtmlEscaping().setPrettyPrinting().create();
    private static final JsonArray CASES = new JsonArray();

    private static byte[] hex(String value) { return HexFormat.of().parseHex(value); }
    private static String hex(byte[] value) { return HexFormat.of().formatHex(value); }
    private static byte[] wire(Consumer<FriendlyByteBuf> writer) {
        FriendlyByteBuf buffer = new FriendlyByteBuf(Unpooled.buffer());
        try {
            writer.accept(buffer);
            byte[] result = new byte[buffer.readableBytes()];
            buffer.getBytes(0, result);
            return result;
        } finally { buffer.release(); }
    }
    private static byte[] concat(byte[]... values) {
        int size = Arrays.stream(values).mapToInt(value -> value.length).sum();
        byte[] result = new byte[size];
        int offset = 0;
        for (byte[] value : values) {
            System.arraycopy(value, 0, result, offset, value.length);
            offset += value.length;
        }
        return result;
    }
    private static byte[] utf(String value) { return wire(buffer -> buffer.writeUtf(value)); }
    private static byte[] varint(int value) { return wire(buffer -> buffer.writeVarInt(value)); }
    private static Identifier id(String value) { return Identifier.parse(value); }
    private static JsonObject object(String key, String value) {
        JsonObject result = new JsonObject(); result.addProperty(key, value); return result;
    }
    private static JsonObject empty(Object ignored) { return new JsonObject(); }

    // Long boundary inputs contain synthetic repetitions, stored compactly in fixtures.
    private record Input(byte[] prefix, byte[] repeat, int count, byte[] suffix) {
        Input(byte[] value) { this(value, new byte[0], 0, new byte[0]); }
        byte[] bytes() {
            byte[] middle = new byte[repeat.length * count];
            for (int index = 0; index < count; index++)
                System.arraycopy(repeat, 0, middle, index * repeat.length, repeat.length);
            return concat(prefix, middle, suffix);
        }
        void describe(JsonObject result) {
            if (count == 0) result.addProperty("payload_hex", hex(prefix));
            else {
                result.addProperty("payload_prefix_hex", hex(prefix));
                result.addProperty("payload_repeat_hex", hex(repeat));
                result.addProperty("payload_repeat_count", count);
                result.addProperty("payload_suffix_hex", hex(suffix));
            }
        }
    }

    private static <T> void decode(String name, String direction, int packetId,
            StreamCodec<? super FriendlyByteBuf, T> codec, Input input, Function<T, JsonObject> describe) {
        JsonObject row = new JsonObject();
        row.addProperty("name", name);
        row.addProperty("direction", direction);
        row.addProperty("packet_id", packetId);
        input.describe(row);
        byte[] original = input.bytes();
        FriendlyByteBuf buffer = new FriendlyByteBuf(Unpooled.wrappedBuffer(original));
        try {
            T value = null;
            try {
                value = codec.decode(buffer);
                row.addProperty("ok", true);
            } catch (Exception error) {
                row.addProperty("ok", false);
                row.addProperty("error_class", error.getClass().getName());
            }
            if (value != null) {
                row.add("result", describe.apply(value));
                T decoded = value;
                byte[] canonical = wire(output -> codec.encode(output, decoded));
                if (canonical.length > 2048 && Arrays.equals(original, canonical))
                    row.addProperty("canonical_same_as_payload", true);
                else row.addProperty("canonical_hex", hex(canonical));
            }
        } finally {
            row.addProperty("consumed_bytes", buffer.readerIndex());
            row.addProperty("payload_bytes", original.length);
            buffer.release();
        }
        CASES.add(row);
    }
    private static <T> void sb(String name, int packetId, StreamCodec<? super FriendlyByteBuf, T> codec,
            byte[] bytes, Function<T, JsonObject> describe) {
        decode(name, "serverbound", packetId, codec, new Input(bytes), describe);
    }
    private static <T> void cb(String name, int packetId, StreamCodec<? super FriendlyByteBuf, T> codec,
            T value, Function<T, JsonObject> describe) {
        byte[] bytes = wire(buffer -> codec.encode(buffer, value));
        decode(name, "clientbound", packetId, codec, new Input(bytes), describe);
    }

    private static JsonObject information(ServerboundClientInformationPacket packet) {
        var value = packet.information();
        JsonObject result = object("language", value.language());
        result.addProperty("view_distance", value.viewDistance());
        result.addProperty("chat_visibility", value.chatVisibility().ordinal());
        result.addProperty("chat_colors", value.chatColors());
        result.addProperty("model_customisation", value.modelCustomisation());
        result.addProperty("main_hand", value.mainHand().ordinal());
        result.addProperty("text_filtering", value.textFilteringEnabled());
        result.addProperty("allows_listing", value.allowsListing());
        result.addProperty("particle_status", value.particleStatus().ordinal());
        return result;
    }
    private static byte[] info(String language, int distance, int chat, int arm, int particles) {
        return wire(buffer -> {
            buffer.writeUtf(language); buffer.writeByte(distance); buffer.writeVarInt(chat);
            buffer.writeByte(1); buffer.writeByte(255); buffer.writeVarInt(arm);
            buffer.writeByte(0); buffer.writeByte(1); buffer.writeVarInt(particles);
        });
    }
    private static JsonObject packs(List<KnownPack> values) {
        JsonObject result = new JsonObject();
        JsonArray packs = new JsonArray();
        for (KnownPack value : values) {
            JsonObject pack = object("namespace", value.namespace());
            pack.addProperty("id", value.id()); pack.addProperty("version", value.version());
            packs.add(pack);
        }
        result.add("known_packs", packs); return result;
    }
    private static JsonObject payload(CustomPacketPayload value) {
        JsonObject result = object("channel", value.type().id().toString());
        if (value instanceof BrandPayload brand) {
            result.addProperty("kind", "brand");
            if (brand.brand().length() <= 64) result.addProperty("brand", brand.brand());
            result.addProperty("brand_utf16_length", brand.brand().length());
        } else result.addProperty("kind", "discarded");
        return result;
    }
    private static JsonObject cookie(ServerboundCookieResponsePacket value) {
        JsonObject result = object("key", value.key().toString());
        result.addProperty("present", value.payload() != null);
        if (value.payload() != null) {
            result.addProperty("payload_bytes", value.payload().length);
            if (value.payload().length <= 64) result.addProperty("data_hex", hex(value.payload()));
        }
        return result;
    }
    private static JsonObject click(ServerboundCustomClickActionPacket value) {
        JsonObject result = object("id", value.id().toString());
        result.addProperty("present", value.payload().isPresent());
        value.payload().ifPresent(tag -> {
            result.addProperty("tag_id", tag.getId());
            if (tag instanceof ByteArrayTag array) result.addProperty("byte_array_length", array.getAsByteArray().length);
            else result.addProperty("snbt", tag.toString());
        });
        return result;
    }

    private static void serverbound() {
        var information = ServerboundClientInformationPacket.STREAM_CODEC;
        sb("information_normal", 0, information, info("en_us", 2, 0, 1, 0), ConfigurationPacketOracle::information);
        for (int value : new int[] {-1, 3, Integer.MIN_VALUE, Integer.MAX_VALUE})
            sb("information_wrapped_" + value, 0, information, info("", 32, value, 1, value), ConfigurationPacketOracle::information);
        for (int arm : new int[] {-1, 2})
            sb("information_arm_" + arm, 0, information, info("", 2, 0, arm, 0), ConfigurationPacketOracle::information);
        sb("information_signed_distance", 0, information, info("", 128, 2, 0, 2), ConfigurationPacketOracle::information);
        sb("information_boolean_nonzero", 0, information, hex("000200ffff01ff0200"), ConfigurationPacketOracle::information);
        sb("information_language_16", 0, information, info("abcdefghijklmnop", 2, 0, 1, 0), ConfigurationPacketOracle::information);
        sb("information_language_17", 0, information, info("abcdefghijklmnopq", 2, 0, 1, 0), ConfigurationPacketOracle::information);
        sb("information_language_8_supplementary", 0, information, info("😀".repeat(8), 2, 0, 1, 0), ConfigurationPacketOracle::information);
        sb("information_language_9_supplementary", 0, information, info("😀".repeat(9), 2, 0, 1, 0), ConfigurationPacketOracle::information);
        sb("information_malformed_utf8", 0, information, concat(hex("01ff"), hex("020001ff01000100")), ConfigurationPacketOracle::information);
        sb("information_truncated", 0, information, hex("00020001ff010001"), ConfigurationPacketOracle::information);
        sb("information_oversized_varint", 0, information, hex("00028080808080000000000000"), ConfigurationPacketOracle::information);
        sb("information_outer_trailing", 0, information, concat(info("", 2, 0, 1, 0), hex("55")), ConfigurationPacketOracle::information);

        var known = ServerboundSelectKnownPacks.STREAM_CODEC;
        Function<ServerboundSelectKnownPacks, JsonObject> knownResult = value -> packs(value.knownPacks());
        sb("known_packs_empty", 7, known, hex("00"), knownResult);
        sb("known_packs_one", 7, known, concat(hex("01"), utf("test"), utf("pack"), utf("v1")), knownResult);
        decode("known_packs_64", "serverbound", 7, known, new Input(hex("40"), hex("000000"), 64, hex("")), knownResult);
        sb("known_packs_65", 7, known, hex("41"), knownResult);
        sb("known_packs_negative", 7, known, varint(-1), knownResult);
        sb("known_packs_truncated", 7, known, hex("010000"), knownResult);

        var custom = ServerboundCustomPayloadPacket.STREAM_CODEC;
        Function<ServerboundCustomPayloadPacket, JsonObject> customResult = value -> payload(value.payload());
        sb("brand_normal", 2, custom, concat(utf("minecraft:brand"), utf("arrow-oracle")), customResult);
        sb("brand_default_namespace", 2, custom, concat(utf("brand"), utf("")), customResult);
        sb("brand_outer_trailing", 2, custom, concat(utf("minecraft:brand"), utf("a"), hex("ff")), customResult);
        sb("brand_string_over_limit", 2, custom, concat(utf("minecraft:brand"), varint(98302)), customResult);
        decode("brand_above_unknown_byte_limit", "serverbound", 2, custom,
            new Input(concat(utf("minecraft:brand"), varint(33000)), hex("e282ac"), 11000, hex("")), customResult);
        sb("custom_invalid_identifier", 2, custom, utf("Test:x"), customResult);
        sb("custom_unknown_empty", 2, custom, utf("test:unknown"), customResult);
        sb("custom_unknown_discards", 2, custom, concat(utf("test:unknown"), hex("aabbcc")), customResult);
        for (int size : new int[] {32767, 32768})
            decode("custom_unknown_" + size, "serverbound", 2, custom,
                new Input(utf("test:unknown"), hex("00"), size, hex("")), customResult);

        var cookie = ServerboundCookieResponsePacket.STREAM_CODEC;
        sb("cookie_absent", 1, cookie, concat(utf("test:cookie"), hex("00")), ConfigurationPacketOracle::cookie);
        sb("cookie_present_empty", 1, cookie, concat(utf("test:cookie"), hex("0100")), ConfigurationPacketOracle::cookie);
        sb("cookie_nonzero_presence", 1, cookie, concat(utf("test:cookie"), hex("ff02aabb")), ConfigurationPacketOracle::cookie);
        decode("cookie_maximum", "serverbound", 1, cookie,
            new Input(concat(utf("test:cookie"), hex("01"), varint(5120)), hex("00"), 5120, hex("")), ConfigurationPacketOracle::cookie);
        sb("cookie_over_limit", 1, cookie, concat(utf("test:cookie"), hex("01"), varint(5121)), ConfigurationPacketOracle::cookie);
        sb("cookie_negative_length", 1, cookie, concat(utf("test:cookie"), hex("01"), varint(-1)), ConfigurationPacketOracle::cookie);
        sb("cookie_truncated", 1, cookie, concat(utf("test:cookie"), hex("0102aa")), ConfigurationPacketOracle::cookie);

        for (String value : new String[] {"0000000000000000", "8000000000000000", "0102030405060708", "00000000000000"})
            sb("keep_alive_" + value, 4, ServerboundKeepAlivePacket.STREAM_CODEC, hex(value),
                packet -> object("id", Long.toString(packet.getId())));
        for (String value : new String[] {"80000000", "01020304", "000000"})
            sb("pong_" + value, 5, ServerboundPongPacket.STREAM_CODEC, hex(value),
                packet -> object("id", Integer.toString(packet.getId())));
        for (int action : new int[] {0, 3, 4, 7, 8, -1})
            sb("resource_pack_action_" + action, 6, ServerboundResourcePackPacket.STREAM_CODEC,
                concat(hex("00112233445566778899aabbccddeeff"), varint(action)), packet -> {
                    JsonObject result = object("uuid", packet.id().toString());
                    result.addProperty("action", packet.action().ordinal());
                    result.addProperty("terminal", packet.action().isTerminal()); return result;
                });

        var click = ServerboundCustomClickActionPacket.STREAM_CODEC;
        for (String[] value : new String[][] {
                {"absent", "0100"}, {"byte", "02017f"}, {"empty_compound", "020a00"},
                {"inner_trailing", "020055"}, {"outer_trailing", "010055"}, {"empty_slice", "00"},
                {"truncated_slice", "0200"}, {"invalid_tag", "017f"}, {"length_over_limit", "818004"}})
            sb("custom_click_" + value[0], 8, click, concat(utf("test:click"), hex(value[1])), ConfigurationPacketOracle::click);
        for (int depth : new int[] {15, 16}) {
            byte[] tag = wire(buffer -> {
                buffer.writeByte(10);
                for (int index = 0; index < depth; index++) buffer.writeBytes(hex("0a0000"));
                buffer.writeZero(depth + 1);
            });
            sb("custom_click_depth_" + depth, 8, click, concat(utf("test:click"), varint(tag.length), tag), ConfigurationPacketOracle::click);
        }
        for (int length : new int[] {32744, 32745}) {
            byte[] header = wire(buffer -> { buffer.writeByte(7); buffer.writeInt(length); });
            decode("custom_click_byte_array_" + length, "serverbound", 8, click,
                new Input(concat(utf("test:click"), varint(length + 5), header), hex("00"), length, hex("")), ConfigurationPacketOracle::click);
        }
        sb("finish_empty", 3, ServerboundFinishConfigurationPacket.STREAM_CODEC, hex(""), ConfigurationPacketOracle::empty);
        sb("finish_outer_trailing", 3, ServerboundFinishConfigurationPacket.STREAM_CODEC, hex("01"), ConfigurationPacketOracle::empty);
        sb("conduct_empty", 9, ServerboundAcceptCodeOfConductPacket.STREAM_CODEC, hex(""), ConfigurationPacketOracle::empty);
        sb("conduct_outer_trailing", 9, ServerboundAcceptCodeOfConductPacket.STREAM_CODEC, hex("01"), ConfigurationPacketOracle::empty);
    }

    private static void clientbound() {
        cb("clientbound_brand", 1, ClientboundCustomPayloadPacket.CONFIG_STREAM_CODEC,
            new ClientboundCustomPayloadPacket(new BrandPayload("arrow-oracle")), value -> payload(value.payload()));
        cb("clientbound_features", 13, ClientboundUpdateEnabledFeaturesPacket.STREAM_CODEC,
            new ClientboundUpdateEnabledFeaturesPacket(Set.of(id("test:feature"))), value -> object("feature", value.features().iterator().next().toString()));
        cb("clientbound_known_packs", 15, ClientboundSelectKnownPacks.STREAM_CODEC,
            new ClientboundSelectKnownPacks(List.of(new KnownPack("test", "pack", "v1"))), value -> packs(value.knownPacks()));
        var registry = ResourceKey.createRegistryKey(id("test:registry"));
        CompoundTag data = new CompoundTag(); data.putInt("answer", 42);
        cb("clientbound_registry", 7, ClientboundRegistryDataPacket.STREAM_CODEC,
            new ClientboundRegistryDataPacket(registry, List.of(
                new RegistrySynchronization.PackedRegistryEntry(id("test:present"), Optional.of(data)),
                new RegistrySynchronization.PackedRegistryEntry(id("test:omitted"), Optional.empty()))), value -> {
                    JsonObject result = object("registry", value.registry().identifier().toString());
                    JsonArray entries = new JsonArray();
                    for (var entry : value.entries()) {
                        JsonObject row = object("id", entry.id().toString());
                        row.addProperty("present", entry.data().isPresent());
                        entry.data().ifPresent(tag -> row.addProperty("snbt", tag.toString())); entries.add(row);
                    }
                    result.add("entries", entries); return result;
                });
        cb("clientbound_tags", 14, ClientboundUpdateTagsPacket.STREAM_CODEC,
            new ClientboundUpdateTagsPacket(Map.of(registry, new TagNetworkSerialization.NetworkPayload(
                Map.of(id("test:tag"), new IntArrayList(new int[] {0, 2, 128}))))), value -> {
                    JsonObject result = object("registry", value.tags().keySet().iterator().next().identifier().toString());
                    result.addProperty("tag", value.tags().values().iterator().next().tags().keySet().iterator().next().toString());
                    result.add("ids", JSON.toJsonTree(value.tags().values().iterator().next().tags().values().iterator().next().toIntArray()));
                    return result;
                });
        cb("clientbound_disconnect", 2, ClientboundDisconnectPacket.STREAM_CODEC,
            new ClientboundDisconnectPacket(Component.literal("oracle complete")), value -> object("text", value.reason().getString()));
        cb("clientbound_finish", 3, ClientboundFinishConfigurationPacket.STREAM_CODEC,
            ClientboundFinishConfigurationPacket.INSTANCE, ConfigurationPacketOracle::empty);
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("Expected fixture output path");
        Bootstrap.bootStrap();
        serverbound(); clientbound();
        if (CASES.size() > 80) throw new IllegalStateException("Oracle case budget exceeded");
        JsonObject result = new JsonObject();
        result.addProperty("format_version", 1);
        result.addProperty("minecraft_version", SharedConstants.getCurrentVersion().id());
        result.addProperty("protocol", SharedConstants.getCurrentVersion().protocolVersion());
        result.addProperty("java_version", System.getProperty("java.version"));
        result.addProperty("scope", "Synthetic packet field codec calls. Payload excludes packet ID and framing. Outer trailing bytes are recorded, not rejected by this harness.");
        result.add("cases", CASES);
        Files.writeString(Path.of(args[0]), JSON.toJson(result) + "\n", StandardCharsets.UTF_8);
    }
}
