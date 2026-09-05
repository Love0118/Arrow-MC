//! Opt-in actual block/sky storage oracle against the locked signed server JAR.
//! The driver never starts a server; unexpected world access throws immediately.
//! Rust storage owns queued inputs and accepts uniform values in 0..=15. Java
//! external-alias mutation and arbitrary defaults are separately observed in the
//! local Roadmap probe and are outside this storage ownership/domain comparison.

use arrow_mc::world::{
    lighting::{
        LightBlock, LightKind, LightSection,
        layer::{DataLayer, LAYER_BYTES},
        storage::{LightSectionStorage, LightSnapshot, SectionType, StorageLimits},
    },
    preparation::ChunkAddress,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write,
    fs,
    path::PathBuf,
    process::Command,
    time::SystemTime,
};

const ORACLE: &str = r##"
import java.nio.file.*;
import java.lang.reflect.*;
import java.security.MessageDigest;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.SectionPos;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.lighting.*;

/** Original operation driver using the actual storage classes and no world. */
public class LightStorageProbe {
    static final List<String> OUT = new ArrayList<>();
    static final Map<String, DataLayer> ALIASES = new TreeMap<>();
    static final class Recorder implements LightChunkGetter {
        final List<String> updates = new ArrayList<>();
        public LightChunk getChunkForLighting(int x, int z) { throw new AssertionError("unexpected world lookup"); }
        public BlockGetter getLevel() { throw new AssertionError("unexpected level lookup"); }
        public void onLightUpdate(LightLayer layer, SectionPos pos) { updates.add(layer + ":" + key(pos.asLong())); }
    }
    public static final class Block extends BlockLightSectionStorage {
        Block(Recorder recorder) { super(recorder); }
    }
    public static final class Sky extends SkyLightSectionStorage {
        Sky(Recorder recorder) { super(recorder); }
    }
    static LayerLightSectionStorage<?> storage;
    static Recorder recorder;
    static String kind;
    @SuppressWarnings("unchecked")
    static <T> T field(Object target, String name) throws Exception {
        for (Class<?> type = target.getClass(); type != null; type = type.getSuperclass()) {
            try { Field field = type.getDeclaredField(name); field.setAccessible(true); return (T)field.get(target); }
            catch (NoSuchFieldException missing) { }
        }
        throw new NoSuchFieldException(name);
    }
    static Object call(String name, Class<?>[] signature, Object... arguments) throws Exception {
        for (Class<?> type = storage.getClass(); type != null; type = type.getSuperclass()) {
            try {
                Method method = type.getDeclaredMethod(name, signature); method.setAccessible(true);
                return method.invoke(storage, arguments);
            } catch (NoSuchMethodException missing) { }
        }
        throw new NoSuchMethodException(name);
    }
    static Object noArgs(String name) throws Exception { return call(name, new Class<?>[0]); }
    static Object at(String name, long node) throws Exception { return call(name, new Class<?>[]{long.class}, node); }
    static void toggle(String name, long node, boolean flag) throws Exception { call(name, new Class<?>[]{long.class, boolean.class}, node, flag); }
    static DataLayer data(long node, boolean updating) throws Exception {
        return (DataLayer)call("getDataLayer", new Class<?>[]{long.class, boolean.class}, node, updating);
    }
    static int number(String[] fields, int index) { return Integer.parseInt(fields[index]); }
    static long section(String[] fields, int index) {
        return SectionPos.asLong(number(fields, index), number(fields, index + 1), number(fields, index + 2));
    }
    static long block(String[] fields, int index) {
        return BlockPos.asLong(number(fields, index), number(fields, index + 1), number(fields, index + 2));
    }
    static String key(long key) { return SectionPos.x(key) + "," + SectionPos.y(key) + "," + SectionPos.z(key); }
    static String keys(Collection<Long> keys) {
        List<String> values = new ArrayList<>();
        for (long key : keys) values.add(key(key));
        Collections.sort(values);
        return String.join(";", values);
    }
    static String repr(DataLayer data) throws Exception {
        if (data == null) return "-";
        boolean empty = data.isEmpty();
        if (data.isDefinitelyHomogenous()) return "U:" + data.get(0, 0, 0) + ":" + empty;
        byte[] digest = MessageDigest.getInstance("SHA-256").digest(data.copy().getData());
        return "A:" + HexFormat.of().formatHex(digest) + ":" + empty;
    }
    static void snapshot(String label) throws Exception {
        OUT.add("D|" + label + "|" + kind + "|" + noArgs("hasInconsistencies")
            + "|" + keys(field(storage, "changedSections")) + "|" + keys(field(storage, "sectionsAffectedByLightUpdates")));
        Map<Long, Byte> states = field(storage, "sectionStates");
        Map<Long, DataLayer> updating = field(field(storage, "updatingSectionData"), "map");
        Map<Long, DataLayer> visible = field(field(storage, "visibleSectionData"), "map");
        Map<Long, DataLayer> queued = field(storage, "queuedSections");
        Set<Long> nodes = new HashSet<>();
        nodes.addAll(states.keySet());
        nodes.addAll(updating.keySet());
        nodes.addAll(visible.keySet());
        nodes.addAll(queued.keySet());
        List<Long> ordered = new ArrayList<>(nodes);
        ordered.sort(Comparator.comparing(LightStorageProbe::key));
        for (long node : ordered) {
            OUT.add("S|" + label + "|" + key(node) + "|" + states.getOrDefault(node, (byte)0)
                + "|" + storage.getDebugSectionType(node) + "|" + repr(data(node, true))
                + "|" + repr(data(node, false)) + "|" + repr(queued.get(node))
                + "|" + repr(storage.getDataLayerData(node)));
        }
        for (var alias : ALIASES.entrySet()) OUT.add("A|" + label + "|" + alias.getKey() + "|" + repr(alias.getValue()));
        Collections.sort(recorder.updates);
        OUT.add("N|" + label + "|" + String.join(";", recorder.updates));
        recorder.updates.clear();
    }
    static void point(String[] fields) throws Exception {
        long pos = block(fields, 2);
        long node = SectionPos.blockToSection(pos);
        String raw = (boolean)at("storingLightForSection", node) ? at("getStoredLevel", pos).toString() : "-";
        String updating = storage instanceof Sky ? call("getLightValue", new Class<?>[]{long.class, boolean.class}, pos, true).toString() : raw;
        OUT.add("P|" + fields[1] + "|" + fields[2] + "," + fields[3] + "," + fields[4]
            + "|" + at("getLightValue", pos) + "|" + updating + "|" + raw);
    }
    static void top(String[] fields) throws Exception {
        long node = section(fields, 2);
        OUT.add("T|" + fields[1] + "|" + key(node) + "|" + at("getTopSectionY", SectionPos.getZeroNode(node))
            + "|" + noArgs("getBottomSectionY") + "|" + call("hasLightDataAtOrBelow", new Class<?>[]{int.class}, SectionPos.y(node))
            + "|" + at("isAboveData", node) + "|" + at("lightOnInSection", node));
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            if (line.isBlank() || line.startsWith("#")) continue;
            String[] f = line.split(" ");
            switch (f[0]) {
                case "new" -> {
                    kind = f[1]; recorder = new Recorder(); ALIASES.clear();
                    storage = kind.equals("B") ? new Block(recorder) : new Sky(recorder);
                }
                case "uniform" -> ALIASES.put(f[1], new DataLayer(number(f, 2)));
                case "bytes" -> ALIASES.put(f[1], new DataLayer(HexFormat.of().parseHex(f[2])));
                case "alias_set" -> ALIASES.get(f[1]).set(number(f, 2), number(f, 3), number(f, 4), number(f, 5));
                case "alias_fill" -> ALIASES.get(f[1]).fill(number(f, 2));
                case "queue" -> call("queueSectionData", new Class<?>[]{long.class, DataLayer.class}, section(f, 1), ALIASES.get(f[4]));
                case "clear" -> call("queueSectionData", new Class<?>[]{long.class, DataLayer.class}, section(f, 1), null);
                case "status" -> toggle("updateSectionStatus", section(f, 1), Boolean.parseBoolean(f[4]));
                case "enable" -> toggle("setLightEnabled", SectionPos.asLong(number(f, 1), 0, number(f, 2)), Boolean.parseBoolean(f[3]));
                case "retain" -> storage.retainData(SectionPos.asLong(number(f, 1), 0, number(f, 2)), Boolean.parseBoolean(f[3]));
                case "write" -> call("setStoredLevel", new Class<?>[]{long.class, int.class}, block(f, 1), number(f, 4));
                case "writable" -> ALIASES.put(f[4], (DataLayer)at("getDataLayerToWrite", section(f, 1)));
                case "keep" -> ALIASES.put(f[5], data(section(f, 1), f[4].equals("U")));
                case "mark" -> call("markNewInconsistencies", new Class<?>[]{LightEngine.class}, new Object[]{null});
                case "swap" -> noArgs("swapSectionMap");
                case "snapshot" -> snapshot(f[1]);
                case "point" -> point(f);
                case "top" -> top(f);
                default -> throw new AssertionError(line);
            }
        }
        Files.write(Path.of(args[1]), OUT);
    }
}


"##;

fn fixtures() -> String {
    let mut script = String::new();
    for domain in ["B", "S"] {
        writeln!(
            script,
            "new {domain}\nuniform implicit_zero 0\nuniform nonzero 4"
        )
        .unwrap();
        writeln!(script, "bytes allocated_zero {}", "00".repeat(LAYER_BYTES)).unwrap();
        writeln!(script, "queue 5 5 5 implicit_zero\nqueue 6 5 5 allocated_zero\nqueue 7 5 5 nonzero\nsnapshot {domain}_overlay_unsupported").unwrap();
        script.push_str("clear 5 5 5\nclear 6 5 5\nclear 7 5 5\nstatus 0 0 0 false\nmark\nswap\nwrite 2 2 2 6\nswap\nqueue 0 0 0 implicit_zero\n");
        writeln!(script, "snapshot {domain}_overlay_implicit_zero\nqueue 0 0 0 allocated_zero\nsnapshot {domain}_overlay_allocated_zero\nqueue 0 0 0 nonzero\nsnapshot {domain}_overlay_nonzero\nclear 0 0 0\nsnapshot {domain}_overlay_clear_falls_back").unwrap();
    }
    script.push_str(
        r#"
new B
snapshot block_empty
uniform unused 4
queue 5 5 5 unused
snapshot block_queued_unloaded
clear 5 5 5
snapshot block_queue_cleared
mark
snapshot block_queue_cleared_mark
status 0 0 0 false
snapshot block_initialized
status 0 0 0 false
snapshot block_repeated_status
mark
swap
snapshot block_visible_initial
keep 0 0 0 V old
write 7 7 7 11
snapshot block_interior_write
point block_write_visibility 7 7 7
write 0 0 0 12
write 15 15 15 13
snapshot block_boundary_writes
swap
snapshot block_visible_written
point block_written_visible 7 7 7
writable 0 0 0 mutable
alias_set mutable 2 2 2 6
snapshot block_writable_no_notify
swap
snapshot block_writable_visible
status 0 0 0 true
snapshot block_removal_pending
status 0 0 0 false
snapshot block_readded_before_mark
mark
swap
snapshot block_readded_visible

new B
uniform queued 5
queue 0 0 0 queued
status 0 0 0 false
write 1 1 1 9
snapshot queue_storage_write_before_mark
point queue_storage_write_previsible 1 1 1
mark
swap
snapshot queue_storage_write_visible
uniform replacement 7
queue 0 0 0 replacement
write 2 2 2 12
snapshot queue_overrides_pending_write
point queue_before_override 2 2 2
mark
snapshot queue_override_marked
point queue_override_not_swapped 2 2 2
swap
snapshot queue_override_visible
point queue_override_value 2 2 2
retain 0 0 true
status 0 0 0 true
snapshot retained_remove_pending
mark
swap
snapshot retained_after_remove
retain 0 0 false
snapshot retained_flag_released_queue_survives
status 0 0 0 false
mark
swap
snapshot retained_reloaded
status 0 0 0 true
mark
swap
snapshot released_remove_drops

new B
status 0 0 0 false
mark
swap
retain 0 0 true
uniform overriding 13
queue 0 0 0 overriding
status 0 0 0 true
mark
swap
snapshot retain_queued_wins_over_stored
clear 0 0 0
snapshot retain_explicit_queue_clear

new B
"#,
    );
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if (x, y, z) != (0, 0, 0) {
                    writeln!(script, "status {x} {y} {z} false").unwrap();
                }
            }
        }
    }
    script.push_str(
        r#"
snapshot neighbors_max_26
status 0 0 0 false
snapshot neighbors_max_26_with_data
status 0 0 0 false
snapshot neighbors_max_26_repeated
mark
swap
snapshot neighbors_all_visible
status 0 0 0 true
snapshot neighbors_center_light_only
"#,
    );
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if (x, y, z) != (0, 0, 0) {
                    writeln!(script, "status {x} {y} {z} true").unwrap();
                }
            }
        }
    }
    script.push_str(
        r#"
snapshot neighbors_all_removal_pending
mark
swap
snapshot neighbors_all_removed

new S
snapshot sky_empty
point sky_empty_disabled 0 0 0
top sky_empty_top 0 0 0
enable 0 0 true
point sky_empty_enabled 0 0 0
top sky_empty_enabled_top 0 0 0
status 0 3 0 false
snapshot sky_initialized_enabled
point sky_before_swap_stored 0 48 0
point sky_before_swap_above 0 80 0
top sky_top_initial 0 3 0
mark
swap
snapshot sky_visible_enabled
point sky_missing_below_uses_first 0 0 0
point sky_adjacent_disabled_above 16 80 0
point sky_above_enabled 0 96 0
enable 0 0 false
point sky_above_now_disabled 0 96 0
point sky_existing_now_disabled 0 48 0
top sky_disabled_top 0 3 0
enable 0 0 true
"#,
    );
    let pattern: Vec<_> = (0_usize..LAYER_BYTES)
        .map(|index| ((index * 71) ^ (index / 19)) as u8)
        .collect();
    writeln!(script, "bytes patterned {}", hex(&pattern)).unwrap();
    script.push_str(
        r#"
queue 0 2 0 patterned
mark
swap
snapshot sky_pattern_above_gap
point sky_gap_reads_y_zero 0 0 0
point sky_gap_reads_firstplane_odd 15 31 15
status 0 0 0 false
snapshot sky_inherits_first_plane
point sky_inherited_updating 15 0 15
top sky_lowest_expanded 0 -1 0
mark
swap
snapshot sky_inherited_visible
point sky_inherited_bottom 15 -16 15
point sky_inherited_top 15 15 15
keep 0 0 0 V inherited_snapshot
write 0 0 0 4
point sky_cow_before_swap 0 0 0
swap
snapshot sky_cow_visible
status 0 3 0 true
mark
snapshot sky_remove_top_before_swap
top sky_top_after_removal 0 3 0
point sky_top_remove_before_swap 0 64 0
swap
snapshot sky_remove_top_visible
point sky_top_remove_visible 0 64 0
status 0 0 0 true
mark
swap
snapshot sky_all_removed_lowest_persists
top sky_lowest_after_all_removed 0 -2 0
point sky_empty_after_remove 0 -16 0
"#,
    );
    script
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("{}\n", line.trim()))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}
fn repr(layer: Option<&DataLayer>) -> String {
    let Some(layer) = layer else {
        return "-".to_owned();
    };
    match layer.bytes() {
        None => format!("U:{}:{}", layer.get(0, 0, 0).unwrap(), layer.is_empty()),
        Some(bytes) => format!("A:{}:{}", hex(&Sha256::digest(bytes)), layer.is_empty()),
    }
}
fn key(section: LightSection) -> String {
    format!("{},{},{}", section.x, section.y, section.z)
}
fn keys(sections: &[LightSection]) -> String {
    let mut strings: Vec<_> = sections.iter().copied().map(key).collect();
    strings.sort();
    strings.join(";")
}
fn number(fields: &[&str], index: usize) -> i32 {
    fields[index].parse().unwrap()
}
fn section(fields: &[&str], index: usize) -> LightSection {
    LightSection {
        x: number(fields, index),
        y: number(fields, index + 1),
        z: number(fields, index + 2),
    }
}
fn block(fields: &[&str], index: usize) -> LightBlock {
    LightBlock {
        x: number(fields, index),
        y: number(fields, index + 1),
        z: number(fields, index + 2),
    }
}
fn column(fields: &[&str]) -> ChunkAddress {
    ChunkAddress {
        x: number(fields, 1),
        z: number(fields, 2),
    }
}
fn kind(kind: LightKind) -> &'static str {
    if kind == LightKind::Block { "B" } else { "S" }
}
fn section_type(value: SectionType) -> &'static str {
    match value {
        SectionType::Empty => "EMPTY",
        SectionType::LightOnly => "LIGHT_ONLY",
        SectionType::LightAndData => "LIGHT_AND_DATA",
    }
}
enum Alias {
    Input(DataLayer),
    Snapshot(LightSnapshot, LightSection),
    Writable(LightSection),
}
fn limits() -> StorageLimits {
    StorageLimits {
        max_sections: 2048,
        max_columns: 512,
        max_notifications: 4096,
        metadata_bytes: 16 * 1024 * 1024,
        layer_bytes: 16 * 1024 * 1024,
    }
}
fn observe(
    storage: &LightSectionStorage,
    candidates: &BTreeSet<LightSection>,
    aliases: &BTreeMap<String, Alias>,
    notifications: &mut Vec<String>,
    label: &str,
    output: &mut Vec<String>,
) {
    // The snapshot exposes getDataLayerData's queued-over-visible view. Keep it
    // only through this observation so later writes have their original COW
    // ownership. Java's S row remains the independent expected getter result.
    let data_snapshot = storage.data_snapshot().unwrap();
    assert_eq!(
        data_snapshot.kind(),
        storage.kind(),
        "{label} data snapshot kind"
    );
    let data_sections: BTreeSet<_> = data_snapshot.sections().collect();
    let expected_data_sections: BTreeSet<_> = candidates
        .iter()
        .copied()
        .filter(|&node| storage.data_layer_data(node).is_some())
        .collect();
    assert_eq!(
        data_sections, expected_data_sections,
        "{label} data snapshot section presence"
    );
    assert_eq!(
        data_snapshot.sections().count(),
        data_sections.len(),
        "{label} unique data snapshot sections"
    );
    output.push(format!(
        "D|{label}|{}|{}|{}",
        kind(storage.kind()),
        storage.has_inconsistencies(),
        keys(storage.affected_sections())
    ));
    let mut ordered: Vec<_> = candidates.iter().copied().collect();
    ordered.sort_by_key(|value| key(*value));
    for node in ordered {
        let state_type = storage.section_type(node);
        let up = storage.layer(node, true);
        let visible = storage.layer(node, false);
        let data = data_snapshot.layer(node);
        assert_eq!(
            repr(data),
            repr(storage.data_layer_data(node)),
            "{label} {} captured data getter",
            key(node)
        );
        if state_type == SectionType::Empty && up.is_none() && visible.is_none() && data.is_none() {
            continue;
        }
        let state = storage.neighbor_count(node)
            + if state_type == SectionType::LightAndData {
                32
            } else {
                0
            };
        output.push(format!(
            "S|{label}|{}|{state}|{}|{}|{}|{}",
            key(node),
            section_type(state_type),
            repr(up),
            repr(visible),
            repr(data)
        ));
    }
    for (name, alias) in aliases {
        let layer = match alias {
            Alias::Input(_) => continue,
            Alias::Snapshot(snapshot, section) => snapshot.layer(*section),
            Alias::Writable(section) => storage.layer(*section, true),
        };
        output.push(format!("A|{label}|{name}|{}", repr(layer)));
    }
    notifications.sort();
    output.push(format!("N|{label}|{}", notifications.join(";")));
    notifications.clear();
}

fn rust_trace(script: &str) -> Vec<String> {
    let mut storage = LightSectionStorage::new(LightKind::Block, limits()).unwrap();
    let mut aliases = BTreeMap::new();
    let mut candidates = BTreeSet::new();
    let mut notifications = Vec::new();
    let mut output = Vec::new();
    for line in script.lines() {
        let f: Vec<_> = line.split_whitespace().collect();
        match f[0] {
            "new" => {
                storage = LightSectionStorage::new(
                    if f[1] == "B" {
                        LightKind::Block
                    } else {
                        LightKind::Sky
                    },
                    limits(),
                )
                .unwrap();
                aliases.clear();
                candidates.clear();
                notifications.clear();
            }
            "uniform" => {
                aliases.insert(
                    f[1].to_owned(),
                    Alias::Input(DataLayer::uniform(number(&f, 2))),
                );
            }
            "bytes" => {
                let bytes: Vec<_> = f[2]
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                aliases.insert(
                    f[1].to_owned(),
                    Alias::Input(DataLayer::from_bytes(&bytes, LAYER_BYTES).unwrap()),
                );
            }
            "alias_set" => {
                let Alias::Writable(node) = aliases.get(f[1]).unwrap() else {
                    panic!("fixture must not mutate transferred inputs")
                };
                storage
                    .layer_to_write(*node)
                    .unwrap()
                    .unwrap()
                    .set(
                        number(&f, 2) as u8,
                        number(&f, 3) as u8,
                        number(&f, 4) as u8,
                        number(&f, 5),
                        LAYER_BYTES,
                    )
                    .unwrap();
            }
            "queue" => {
                let node = section(&f, 1);
                candidates.insert(node);
                let Alias::Input(layer) = aliases.get(f[4]).unwrap() else {
                    panic!("queue fixture must own input")
                };
                storage.queue_data(node, Some(layer)).unwrap();
            }
            "clear" => storage.queue_data(section(&f, 1), None).unwrap(),
            "status" => {
                let node = section(&f, 1);
                for x in -1..=1 {
                    for y in -1..=1 {
                        for z in -1..=1 {
                            candidates.insert(node.offset(x, y, z));
                        }
                    }
                }
                storage.update_section_status(node, f[4] == "true").unwrap();
            }
            "enable" => storage.set_enabled(column(&f), f[3] == "true").unwrap(),
            "retain" => storage.retain_data(column(&f), f[3] == "true").unwrap(),
            "write" => storage
                .set_stored_level(block(&f, 1), number(&f, 4) as u8)
                .unwrap(),
            "writable" => {
                let node = section(&f, 1);
                assert!(storage.layer_to_write(node).unwrap().is_some());
                aliases.insert(f[4].to_owned(), Alias::Writable(node));
            }
            "keep" => {
                assert_eq!(f[4], "V");
                aliases.insert(
                    f[5].to_owned(),
                    Alias::Snapshot(storage.snapshot(), section(&f, 1)),
                );
            }
            "mark" => storage.process_inconsistencies().unwrap(),
            "swap" => {
                storage.publish_visible().unwrap();
                let prefix = if storage.kind() == LightKind::Block {
                    "BLOCK"
                } else {
                    "SKY"
                };
                notifications.extend(
                    storage
                        .published_sections()
                        .iter()
                        .map(|node| format!("{prefix}:{}", key(*node))),
                );
                storage.clear_published_notifications();
            }
            "snapshot" => observe(
                &storage,
                &candidates,
                &aliases,
                &mut notifications,
                f[1],
                &mut output,
            ),
            "point" => {
                let point = block(&f, 2);
                let raw = storage
                    .stored_level(point)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_owned());
                let updating = if storage.kind() == LightKind::Sky {
                    storage.get_level(point, true).to_string()
                } else {
                    raw.clone()
                };
                output.push(format!(
                    "P|{}|{},{},{}|{}|{updating}|{raw}",
                    f[1],
                    point.x,
                    point.y,
                    point.z,
                    storage.get_level(point, false)
                ));
            }
            "top" => {
                let node = section(&f, 2);
                output.push(format!(
                    "T|{}|{}|{}|{}|{}|{}|{}",
                    f[1],
                    key(node),
                    storage.top_section_y(node.column()),
                    storage.bottom_section_y(),
                    storage.has_light_data_at_or_below(node.y),
                    storage.is_above_data(node),
                    storage.light_enabled(node.column())
                ));
            }
            _ => panic!("unexpected fixture operation {line}"),
        }
    }
    output
}

fn observable_java_rows(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<_> = line.split('|').collect();
            match f[0] {
                // Storage intentionally uses different change tracking and queue ownership.
                "D" => Some([f[0], f[1], f[2], f[3], f[5]].join("|")),
                "S" => Some([f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[8]].join("|")),
                "A" if !["old", "mutable", "inherited_snapshot"].contains(&f[2]) => None,
                _ => Some(line.to_owned()),
            }
        })
        .collect()
}

#[test]
#[ignore = "requires Java25 and locked jars via ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"]
fn support_queues_snapshots_notifications_and_sky_inheritance_match_actual_vanilla() {
    let jar = env::var_os("ARROW_VANILLA_SERVER_JAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
                    .expect("set ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"),
            )
            .join("artifacts/26.3-pre-2/server-26.3-pre-2.jar")
        });
    assert!(jar.is_file(), "missing server JAR: {}", jar.display());
    let classpath =
        env::join_paths([jar.clone(), jar.parent().unwrap().join("libraries/*")]).unwrap();
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-light-storage-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("LightStorageProbe.java");
    let input = directory.join("cases.txt");
    let output = directory.join("observations.txt");
    let script = fixtures();
    fs::write(&source, ORACLE).unwrap();
    fs::write(&input, &script).unwrap();
    let expected = rust_trace(&script);
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
        .arg("--class-path")
        .arg(classpath)
        .arg(&source)
        .arg(input)
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
    let actual = observable_java_rows(&fs::read_to_string(output).unwrap());
    assert!(
        directory
            .canonicalize()
            .unwrap()
            .starts_with(env::temp_dir().canonicalize().unwrap())
    );
    fs::remove_dir_all(&directory).unwrap();
    assert_eq!(actual.len(), expected.len(), "trace length");
    for (index, (java, rust)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(java, rust, "storage trace row {index}");
    }
    eprintln!(
        "Compared {} complete storage observation rows against actual Vanilla 26.3-pre-2",
        actual.len()
    );
}
