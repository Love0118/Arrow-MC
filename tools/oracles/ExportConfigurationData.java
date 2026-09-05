import com.google.gson.*;
import java.io.*;
import java.nio.file.*;
import java.security.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
import net.minecraft.nbt.*;
import net.minecraft.resources.*;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.packs.resources.CloseableResourceManager;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.tags.TagNetworkSerialization;
import net.minecraft.util.Util;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.WorldDataConfiguration;

/** Independently authored public-API exporter. Output is local reference data, not distributable assets. */
public final class ExportConfigurationData {
    static { SharedConstants.tryDetectVersion(); }
    private static final Gson JSON = new GsonBuilder().disableHtmlEscaping().setPrettyPrinting().create();

    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("Expected one output directory");
        Path output = Path.of(args[0]).toAbsolutePath();
        Files.createDirectories(output.resolve("entries"));
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
                        export(output, packs, resources, registries, worldConfig);
                        return true;
                    } catch (Exception error) {
                        throw new CompletionException(error);
                    }
                }, worker, Runnable::run).join();
        } finally {
            Util.shutdownExecutors();
        }
    }

    private static JsonObject pack(KnownPack value) {
        JsonObject item = new JsonObject();
        item.addProperty("namespace", value.namespace());
        item.addProperty("id", value.id());
        item.addProperty("version", value.version());
        return item;
    }

    private static void write(Path output, String name, JsonElement value) throws IOException {
        Files.writeString(output.resolve(name), JSON.toJson(value) + "\n");
    }

    private static JsonObject digest(Path file) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (InputStream input = Files.newInputStream(file)) {
            byte[] block = new byte[65536];
            for (int read; (read = input.read(block)) != -1;) digest.update(block, 0, read);
        }
        JsonObject result = new JsonObject();
        result.addProperty("sha256", HexFormat.of().formatHex(digest.digest()));
        result.addProperty("bytes", Files.size(file));
        return result;
    }

    private static <T> Optional<KnownPack> entryPack(Registry<T> registry, Identifier id) {
        return registry.registrationInfo(ResourceKey.create(registry.key(), id)).flatMap(RegistrationInfo::knownPackInfo);
    }

    private static <T> int entryId(Registry<T> registry, Identifier id) {
        return registry.getId(registry.getValue(id));
    }

    private static <T> JsonObject staticDomain(Registry<T> registry) {
        JsonObject domain = new JsonObject();
        domain.addProperty("id", registry.key().identifier().toString());
        JsonArray entries = new JsonArray();
        registry.listElements().sorted(Comparator.comparingInt(holder -> registry.getId(holder.value()))).forEach(holder -> {
            JsonObject entry = new JsonObject();
            entry.addProperty("id", holder.key().identifier().toString());
            entry.addProperty("protocol_id", registry.getId(holder.value()));
            entries.add(entry);
        });
        domain.add("entries", entries);
        return domain;
    }

    private static void export(Path output, PackRepository packs, CloseableResourceManager resources,
        LayeredRegistryAccess<RegistryLayer> registries, WorldDataConfiguration worldConfig) throws Exception {
        JsonArray knownPacks = new JsonArray();
        resources.listPacks().flatMap(resource -> resource.knownPackInfo().stream()).forEach(value -> knownPacks.add(pack(value)));
        write(output, "known-packs.json", knownPacks);
        JsonArray features = new JsonArray();
        FeatureFlags.REGISTRY.toNames(worldConfig.enabledFeatures()).stream().map(Identifier::toString).sorted().forEach(features::add);
        write(output, "features.json", features);

        RegistryAccess worldAccess = registries.getAccessFrom(RegistryLayer.WORLD);
        var ops = registries.compositeAccess().createSerializationContext(NbtOps.INSTANCE);
        JsonArray networkRegistries = new JsonArray();
        int[] fileIndex = {0};
        RegistrySynchronization.packRegistries(ops, worldAccess, Set.of(), (registryKey, values) -> {
            try {
                Registry<?> registry = worldAccess.lookupOrThrow(registryKey);
                JsonObject registryJson = new JsonObject();
                registryJson.addProperty("id", registryKey.identifier().toString());
                JsonArray entries = new JsonArray();
                for (int index = 0; index < values.size(); index++) {
                    var value = values.get(index);
                    int actualId = entryId(registry, value.id());
                    if (actualId != index) throw new IllegalStateException("Non-contiguous registry " + registryKey + ": " + actualId + " != " + index);
                    String filename = String.format(Locale.ROOT, "entries/%05d.nbt", fileIndex[0]++);
                    Path target = output.resolve(filename);
                    try (DataOutputStream stream = new DataOutputStream(Files.newOutputStream(target))) {
                        NbtIo.writeAnyTag(value.data().orElseThrow(), stream);
                    }
                    JsonObject entry = digest(target);
                    entry.addProperty("id", value.id().toString());
                    entry.addProperty("protocol_id", index);
                    entry.addProperty("network_nbt_file", filename);
                    entry.add("known_pack", entryPack(registry, value.id()).<JsonElement>map(ExportConfigurationData::pack).orElse(JsonNull.INSTANCE));
                    entries.add(entry);
                }
                registryJson.add("entries", entries);
                networkRegistries.add(registryJson);
            } catch (Exception error) {
                throw new CompletionException(error);
            }
        });
        if (networkRegistries.size() != RegistryDataLoader.SYNCHRONIZED_REGISTRIES.size()) throw new IllegalStateException("Missing synchronized registry");
        write(output, "registries.json", networkRegistries);

        JsonArray tagRegistries = new JsonArray();
        TagNetworkSerialization.serializeTagsToNetwork(registries).entrySet().stream()
            .sorted(Comparator.comparing(entry -> entry.getKey().identifier().toString())).forEach(registry -> {
                JsonObject tagged = new JsonObject();
                tagged.addProperty("id", registry.getKey().identifier().toString());
                JsonArray tags = new JsonArray();
                registry.getValue().tags().entrySet().stream().sorted(Map.Entry.comparingByKey()).forEach(tag -> {
                    JsonObject item = new JsonObject();
                    item.addProperty("id", tag.getKey().toString());
                    JsonArray members = new JsonArray();
                    tag.getValue().forEach((int member) -> members.add(member));
                    item.add("members", members);
                    tags.add(item);
                });
                tagged.add("tags", tags);
                tagRegistries.add(tagged);
            });
        write(output, "tags.json", tagRegistries);

        JsonArray staticDomains = new JsonArray();
        registries.getLayer(RegistryLayer.STATIC).registries().sorted(Comparator.comparing(entry -> entry.key().identifier().toString()))
            .forEach(entry -> staticDomains.add(staticDomain(entry.value())));
        write(output, "static-domains.json", staticDomains);
        JsonObject metadata = new JsonObject();
        metadata.addProperty("minecraft_version", SharedConstants.getCurrentVersion().id());
        metadata.addProperty("protocol", SharedConstants.getCurrentVersion().protocolVersion());
        JsonArray selectedIds = new JsonArray();
        packs.getSelectedPacks().forEach(selected -> selectedIds.add(selected.getId()));
        metadata.add("selected_pack_ids", selectedIds);
        metadata.add("known_packs", knownPacks);
        metadata.add("source_jar", digest(Path.of(SharedConstants.class.getProtectionDomain().getCodeSource().getLocation().toURI())));
        write(output, "export-metadata.json", metadata);
        System.out.println("Configuration snapshot: " + networkRegistries.size() + " registries, " + fileIndex[0] + " entry payloads, " + tagRegistries.size() + " tagged registries");
    }
}
