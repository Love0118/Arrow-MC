//! Opt-in saved-tick and area-operation comparison with the locked Java JAR.
//! Set ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT and use --ignored.
//! Requires Java 25. The embedded observer uses public APIs and original small
//! inputs; it includes no Vanilla/JDK/fastutil implementation bodies or assets.

use std::{env, fmt::Write, fs, path::PathBuf, process::Command, time::SystemTime};

use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::ticks::{
    SavedTick, ScheduledTickOwner, TickBounds, TickDomain, TickLimits, TickPosition, TickPriority,
};

const ORACLE: &str = r#"
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Vec3i;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.ticks.LevelChunkTicks;
import net.minecraft.world.ticks.LevelTicks;
import net.minecraft.world.ticks.SavedTick;
import net.minecraft.world.ticks.ScheduledTick;
import net.minecraft.world.ticks.TickPriority;

class PersistenceTickOracle {
    static final String[] TYPES = new String[256];
    static final List<String> OUTPUT = new ArrayList<>();
    static World source;
    static World destination;
    static class World {
        final Set<Long> eligible = new HashSet<>();
        final Set<Long> registered = new HashSet<>();
        final Map<Long, LevelChunkTicks<String>> blocks = new HashMap<>();
        final Map<Long, LevelChunkTicks<String>> fluids = new HashMap<>();
        final LevelTicks<String> blockTicks = new LevelTicks<>(eligible::contains);
        final LevelTicks<String> fluidTicks = new LevelTicks<>(eligible::contains);
        long counter;
        LevelTicks<String> ticks(String domain) { return domain.equals("B") ? blockTicks : fluidTicks; }
        Map<Long, LevelChunkTicks<String>> chunks(String domain) { return domain.equals("B") ? blocks : fluids; }
        void register(ChunkPos chunk, boolean active) {
            long key = chunk.pack();
            if (!registered.add(key)) throw new AssertionError("duplicate registration");
            setEligible(chunk, active);
            blockTicks.addContainer(chunk, blocks.computeIfAbsent(key, unused -> new LevelChunkTicks<>()));
            fluidTicks.addContainer(chunk, fluids.computeIfAbsent(key, unused -> new LevelChunkTicks<>()));
        }
        void setEligible(ChunkPos chunk, boolean active) {
            if (active) eligible.add(chunk.pack()); else eligible.remove(chunk.pack());
        }
    }
    static World world(String name) { return name.equals("S") ? source : destination; }
    static int number(String[] fields, int index) { return Integer.parseInt(fields[index]); }
    static long time(String[] fields, int index) { return Long.parseLong(fields[index]); }
    static BlockPos position(int x) { return new BlockPos(x, 64, 0); }
    static BoundingBox bounds(int first, int last) { return new BoundingBox(first, 64, 0, last, 64, 0); }
    static List<SavedTick<String>> saved(String encoded) {
        List<SavedTick<String>> result = new ArrayList<>();
        if (!encoded.equals("_")) for (String entry : encoded.split(";")) {
            String[] f = entry.split(",");
            result.add(new SavedTick<>(TYPES[number(f, 0)],
                new BlockPos(number(f, 1), number(f, 2), number(f, 3)), number(f, 4), TickPriority.byValue(number(f, 5))));
        }
        return result;
    }
    static String describe(SavedTick<String> tick) {
        return tick.type() + "," + tick.pos().getX() + "," + tick.pos().getY() + "," + tick.pos().getZ()
            + "," + tick.delay() + "," + tick.priority().getValue();
    }
    static void pack(String label, World world, String domain, int x, int z, long now) {
        List<String> values = world.chunks(domain).get(ChunkPos.pack(x, z)).pack(now).stream()
            .map(PersistenceTickOracle::describe).toList();
        OUTPUT.add("P|" + label + "|" + String.join(";", values));
    }
    static void status(String label, World world, String domain) {
        LevelTicks<String> ticks = world.ticks(domain);
        String row = "Q|" + label + "|" + ticks.count() + "|" + world.counter;
        for (int id = 1; id <= 4; id++) row += "|" + ticks.hasScheduledTick(position(id), TYPES[id])
            + "|" + ticks.willTickThisTick(position(id), TYPES[id]);
        OUTPUT.add(row);
    }
    static void schedule(World world, String domain, BlockPos pos, int id, long now, int delay, int priority) {
        LevelTicks<String> ticks = world.ticks(domain);
        String outcome = !world.registered.contains(ChunkPos.pack(pos)) ? "MissingContainer"
            : ticks.hasScheduledTick(pos, TYPES[id]) ? "Duplicate" : "Added";
        ticks.schedule(new ScheduledTick<>(TYPES[id], pos, now + delay,
            TickPriority.byValue(priority), world.counter++));
        OUTPUT.add("S|" + outcome + "|" + world.counter);
    }
    static void action(String action, String label, World world, String domain, int id) {
        if (action.equals("query")) { status(label + "_each", world, domain); return; }
        if (id != 1) return;
        LevelTicks<String> ticks = world.ticks(domain);
        if (action.equals("readmit")) {
            status(label + "_before_readmit", world, domain);
            schedule(world, domain, position(1), 1, 101, 0, -1);
            status(label + "_after_readmit", world, domain);
        }
        if (action.startsWith("clear")) {
            if (action.equals("clear_query")) status(label + "_before_clear", world, domain);
            ticks.clearArea(bounds(1, 2));
            ticks.clearArea(bounds(4, 4));
            status(label + "_after_clear", world, domain);
            destination.ticks(domain).copyAreaFrom(ticks, bounds(1, 4), new Vec3i(32, 0, 0));
            pack(label + "_copied", destination, domain, 2, 0, 0);
        }
        if (action.equals("copy")) {
            schedule(world, domain, position(1), 1, 2, 0, -3);
            destination.ticks(domain).copyAreaFrom(ticks, bounds(1, 3), new Vec3i(32, 3, 1));
            pack(label + "_copied", destination, domain, 2, 0, 0);
            status(label + "_destination", destination, domain);
        }
        if (action.equals("self")) ticks.copyArea(bounds(1, 2), new Vec3i(0, 0, 0));
    }
    static void run(String label, World world, String domain, long now, int cap, String action) {
        LevelTicks<String> ticks = world.ticks(domain);
        // Public snapshots describe the entries before collection removes them.
        // Callback selection itself is performed exclusively by the actual JAR.
        List<ScheduledTick<String>> descriptions = new ArrayList<>();
        for (long key : world.registered) world.chunks(domain).get(key).getAll().forEach(descriptions::add);
        descriptions.sort(ScheduledTick.DRAIN_ORDER);
        OUTPUT.add("R|" + label + "|" + ticks.count() + "|" + world.counter);
        ticks.tick(now, cap, (pos, type) -> {
            ScheduledTick<String> tick = descriptions.stream()
                .filter(value -> value.type() == type && value.pos().equals(pos)).findFirst().orElseThrow();
            descriptions.remove(tick);
            OUTPUT.add("C|" + label + "|" + type + "|" + pos.getX() + "|" + pos.getY() + "|" + pos.getZ()
                + "|" + tick.triggerTick() + "|" + tick.priority().getValue() + "|" + tick.subTickOrder()
                + "|" + ticks.count() + "|" + world.counter);
            action(action, label, world, domain, Integer.parseInt(type));
        });
        OUTPUT.add("E|" + label + "|" + ticks.count() + "|" + world.counter);
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        for (int id = 0; id < TYPES.length; id++) TYPES[id] = new String(Integer.toString(id));
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] f = line.split(" ");
            switch (f[0]) {
                case "new" -> { source = new World(); destination = new World(); }
                case "load" -> {
                    World world = world(f[1]);
                    ChunkPos chunk = new ChunkPos(number(f, 2), number(f, 3));
                    world.blocks.put(chunk.pack(), new LevelChunkTicks<>(SavedTick.filterTickListForChunk(saved(f[4]), chunk)));
                    world.fluids.put(chunk.pack(), new LevelChunkTicks<>(SavedTick.filterTickListForChunk(saved(f[5]), chunk)));
                }
                case "register" -> world(f[1]).register(new ChunkPos(number(f, 2), number(f, 3)), Boolean.parseBoolean(f[4]));
                case "eligible" -> world(f[1]).setEligible(new ChunkPos(number(f, 2), number(f, 3)), Boolean.parseBoolean(f[4]));
                case "detach" -> {
                    World world = world(f[1]);
                    ChunkPos chunk = new ChunkPos(number(f, 2), number(f, 3));
                    world.registered.remove(chunk.pack());
                    world.blockTicks.removeContainer(chunk);
                    world.fluidTicks.removeContainer(chunk);
                }
                case "unpack" -> {
                    World world = world(f[1]);
                    long key = ChunkPos.pack(number(f, 2), number(f, 3));
                    world.blocks.get(key).unpack(time(f, 4));
                    world.fluids.get(key).unpack(time(f, 4));
                }
                case "pack" -> pack(f[1], world(f[2]), f[3], number(f, 4), number(f, 5), time(f, 6));
                case "status" -> status(f[1], world(f[2]), f[3]);
                case "s" -> schedule(world(f[1]), f[2], new BlockPos(number(f, 3), number(f, 4), number(f, 5)),
                    number(f, 6), time(f, 7), number(f, 8), number(f, 9));
                case "run" -> run(f[1], world(f[2]), f[3], time(f, 4), number(f, 5), f[6]);
                case "clear" -> world(f[1]).ticks(f[2]).clearArea(bounds(number(f, 3), number(f, 4)));
                case "copy" -> {
                    World to = world(f[1]), from = world(f[2]);
                    BoundingBox box = bounds(number(f, 4), number(f, 5));
                    Vec3i offset = new Vec3i(number(f, 6), number(f, 7), number(f, 8));
                    if (to == from) to.ticks(f[3]).copyArea(box, offset);
                    else to.ticks(f[3]).copyAreaFrom(from.ticks(f[3]), box, offset);
                }
                default -> throw new AssertionError("unknown input " + line);
            }
        }
        Files.write(Path.of(args[1]), OUTPUT);
    }
}
"#;

fn new_owner() -> ScheduledTickOwner {
    ScheduledTickOwner::new(
        256,
        256,
        TickLimits {
            max_chunks: 16,
            queued_per_chunk: 64,
            selected_per_phase: 128,
            allocation_bytes: 4 * 1024 * 1024,
        },
    )
    .unwrap()
}

fn world(name: &str) -> usize {
    match name {
        "S" => 0,
        "D" => 1,
        _ => panic!("unknown world"),
    }
}

fn domain(name: &str) -> TickDomain {
    match name {
        "B" => TickDomain::Block,
        "F" => TickDomain::Fluid,
        _ => panic!("unknown domain"),
    }
}

fn position(x: i32) -> TickPosition {
    TickPosition { x, y: 64, z: 0 }
}

fn bounds(first: i32, last: i32) -> TickBounds {
    TickBounds {
        min: position(first),
        max: position(last),
    }
}

fn saved(encoded: &str) -> Vec<SavedTick> {
    if encoded == "_" {
        return Vec::new();
    }
    encoded
        .split(';')
        .map(|entry| {
            let fields: Vec<i32> = entry.split(',').map(|part| part.parse().unwrap()).collect();
            SavedTick {
                type_id: fields[0] as u32,
                position: TickPosition {
                    x: fields[1],
                    y: fields[2],
                    z: fields[3],
                },
                delay: fields[4],
                priority: TickPriority::from_value(fields[5]),
            }
        })
        .collect()
}

fn pack(
    label: &str,
    owner: &mut ScheduledTickOwner,
    domain: TickDomain,
    x: i32,
    z: i32,
    now: i64,
) -> String {
    let mut values = Vec::with_capacity(64);
    owner
        .pack_chunk(ChunkAddress { x, z }, domain, now, &mut values)
        .unwrap();
    let values: Vec<_> = values
        .iter()
        .map(|tick| {
            format!(
                "{},{},{},{},{},{}",
                tick.type_id,
                tick.position.x,
                tick.position.y,
                tick.position.z,
                tick.delay,
                tick.priority as i32
            )
        })
        .collect();
    format!("P|{label}|{}", values.join(";"))
}

fn status(label: &str, owner: &mut ScheduledTickOwner, domain: TickDomain) -> String {
    let mut row = format!(
        "Q|{label}|{}|{}",
        owner.queued_count(domain),
        owner.next_sub_tick_order()
    );
    for id in 1..=4 {
        let has = owner.has_scheduled(domain, position(id), id as u32);
        let will = owner.will_tick_this_phase(domain, position(id), id as u32);
        write!(row, "|{has}|{will}").unwrap();
    }
    row
}

fn schedule(
    owner: &mut ScheduledTickOwner,
    domain: TickDomain,
    tick: SavedTick,
    now: i64,
) -> String {
    let outcome = owner
        .schedule(
            domain,
            tick.position,
            tick.type_id,
            now,
            tick.delay,
            tick.priority,
        )
        .unwrap();
    format!("S|{outcome:?}|{}", owner.next_sub_tick_order())
}

fn copy(
    owners: &mut [ScheduledTickOwner; 2],
    to: usize,
    from: usize,
    domain: TickDomain,
    bounds: TickBounds,
    offset: TickPosition,
) {
    if to == from {
        owners[to].copy_area(domain, bounds, offset).unwrap();
    } else {
        let [source, destination] = owners;
        if to == 1 {
            destination
                .copy_area_from(source, domain, bounds, offset)
                .unwrap();
        } else {
            source
                .copy_area_from(destination, domain, bounds, offset)
                .unwrap();
        }
    }
}

fn action(
    action: &str,
    label: &str,
    owners: &mut [ScheduledTickOwner; 2],
    selected: usize,
    domain: TickDomain,
    id: u32,
    output: &mut Vec<String>,
) {
    if action == "query" {
        output.push(status(
            &format!("{label}_each"),
            &mut owners[selected],
            domain,
        ));
        return;
    }
    if id != 1 {
        return;
    }
    if action == "readmit" {
        output.push(status(
            &format!("{label}_before_readmit"),
            &mut owners[selected],
            domain,
        ));
        output.push(schedule(
            &mut owners[selected],
            domain,
            SavedTick {
                position: position(1),
                type_id: 1,
                delay: 0,
                priority: TickPriority::High,
            },
            101,
        ));
        output.push(status(
            &format!("{label}_after_readmit"),
            &mut owners[selected],
            domain,
        ));
    }
    if action.starts_with("clear") {
        if action == "clear_query" {
            output.push(status(
                &format!("{label}_before_clear"),
                &mut owners[selected],
                domain,
            ));
        }
        owners[selected].clear_area(domain, bounds(1, 2)).unwrap();
        owners[selected].clear_area(domain, bounds(4, 4)).unwrap();
        output.push(status(
            &format!("{label}_after_clear"),
            &mut owners[selected],
            domain,
        ));
        copy(
            owners,
            1,
            selected,
            domain,
            bounds(1, 4),
            TickPosition { x: 32, y: 0, z: 0 },
        );
        output.push(pack(
            &format!("{label}_copied"),
            &mut owners[1],
            domain,
            2,
            0,
            0,
        ));
    }
    if action == "copy" {
        output.push(schedule(
            &mut owners[selected],
            domain,
            SavedTick {
                position: position(1),
                type_id: 1,
                delay: 0,
                priority: TickPriority::ExtremelyHigh,
            },
            2,
        ));
        copy(
            owners,
            1,
            selected,
            domain,
            bounds(1, 3),
            TickPosition { x: 32, y: 3, z: 1 },
        );
        output.push(pack(
            &format!("{label}_copied"),
            &mut owners[1],
            domain,
            2,
            0,
            0,
        ));
        output.push(status(
            &format!("{label}_destination"),
            &mut owners[1],
            domain,
        ));
    }
    if action == "self" {
        owners[selected]
            .copy_area(domain, bounds(1, 2), TickPosition { x: 0, y: 0, z: 0 })
            .unwrap();
    }
}

fn rust_trace(script: &str) -> Vec<String> {
    let mut owners = [new_owner(), new_owner()];
    let mut output = Vec::new();
    for line in script.lines() {
        let f: Vec<_> = line.split_whitespace().collect();
        let n = |index: usize| f[index].parse::<i64>().unwrap();
        match f[0] {
            "new" => owners = [new_owner(), new_owner()],
            "load" => owners[world(f[1])]
                .load_pending_chunk(
                    ChunkAddress {
                        x: n(2) as i32,
                        z: n(3) as i32,
                    },
                    &saved(f[4]),
                    &saved(f[5]),
                )
                .unwrap(),
            "register" => owners[world(f[1])]
                .register_chunk(
                    ChunkAddress {
                        x: n(2) as i32,
                        z: n(3) as i32,
                    },
                    f[4].parse().unwrap(),
                )
                .unwrap(),
            "eligible" => owners[world(f[1])]
                .set_eligible(
                    ChunkAddress {
                        x: n(2) as i32,
                        z: n(3) as i32,
                    },
                    f[4].parse().unwrap(),
                )
                .unwrap(),
            "detach" => owners[world(f[1])]
                .detach_chunk(ChunkAddress {
                    x: n(2) as i32,
                    z: n(3) as i32,
                })
                .unwrap(),
            "unpack" => owners[world(f[1])]
                .unpack_chunk(
                    ChunkAddress {
                        x: n(2) as i32,
                        z: n(3) as i32,
                    },
                    n(4),
                )
                .unwrap(),
            "pack" => output.push(pack(
                f[1],
                &mut owners[world(f[2])],
                domain(f[3]),
                n(4) as i32,
                n(5) as i32,
                n(6),
            )),
            "status" => output.push(status(f[1], &mut owners[world(f[2])], domain(f[3]))),
            "s" => output.push(schedule(
                &mut owners[world(f[1])],
                domain(f[2]),
                SavedTick {
                    position: TickPosition {
                        x: n(3) as i32,
                        y: n(4) as i32,
                        z: n(5) as i32,
                    },
                    type_id: n(6) as u32,
                    delay: n(8) as i32,
                    priority: TickPriority::from_value(n(9) as i32),
                },
                n(7),
            )),
            "clear" => {
                owners[world(f[1])]
                    .clear_area(domain(f[2]), bounds(n(3) as i32, n(4) as i32))
                    .unwrap();
            }
            "copy" => copy(
                &mut owners,
                world(f[1]),
                world(f[2]),
                domain(f[3]),
                bounds(n(4) as i32, n(5) as i32),
                TickPosition {
                    x: n(6) as i32,
                    y: n(7) as i32,
                    z: n(8) as i32,
                },
            ),
            "run" => {
                let selected = world(f[2]);
                let domain = domain(f[3]);
                let label = f[1];
                output.push(format!(
                    "R|{label}|{}|{}",
                    owners[selected].queued_count(domain),
                    owners[selected].next_sub_tick_order()
                ));
                owners[selected]
                    .begin_phase(domain, n(4), n(5) as usize)
                    .unwrap();
                while let Some(tick) = owners[selected].next_due().unwrap() {
                    output.push(format!(
                        "C|{label}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                        tick.type_id,
                        tick.position.x,
                        tick.position.y,
                        tick.position.z,
                        tick.trigger_tick,
                        tick.priority as i32,
                        tick.sub_tick_order,
                        owners[selected].queued_count(domain),
                        owners[selected].next_sub_tick_order()
                    ));
                    action(
                        f[6],
                        label,
                        &mut owners,
                        selected,
                        domain,
                        tick.type_id,
                        &mut output,
                    );
                }
                owners[selected].finish_phase().unwrap();
                output.push(format!(
                    "E|{label}|{}|{}",
                    owners[selected].queued_count(domain),
                    owners[selected].next_sub_tick_order()
                ));
            }
            _ => panic!("unknown fixture: {line}"),
        }
    }
    output
}

fn fixtures() -> String {
    let mut script = String::from(
        "new\nload S 0 0 1,1,64,0,0,0;1,1,64,0,5,0;2,2,64,0,-4,0;99,16,64,0,7,0 3,3,64,0,1,0;3,3,64,0,2,0\nregister S 0 0 true\nstatus pending S B\npack pending_block S B 0 0 100\npack pending_fluid S F 0 0 100\ns S B 1 64 0 1 0 0 -3\ns S B 4 64 0 4 105 0 1\npack mixed_pending_live S B 0 0 100\nclear S B 1 4\npack clear_keeps_pending S B 0 0 100\nunpack S 0 0 100\nstatus unpacked S B\npack unpacked S B 0 0 102\nunpack S 0 0 500\npack unpack_twice S B 0 0 102\nrun negative_first S B 100 1 none\nrun duplicate_first S B 100 1 readmit\nrun readmitted S B 101 20 none\nrun duplicate_remaining S B 105 20 none\nrun fluid_negative_orders S F 105 20 none\n",
    );
    for distinct in [1, 2, 4] {
        let entries = (0..24)
            .map(|index| {
                let id = index % distinct + 1;
                format!("{id},{id},64,0,0,0")
            })
            .collect::<Vec<_>>()
            .join(";");
        writeln!(script, "new\nload S 0 0 {entries} _\nregister S 0 0 true\nunpack S 0 0 100\nrun repeated_{distinct}_identities S B 100 64 query").unwrap();
    }
    script.push_str("new\n");
    let filtered = "1,-17,64,0,1,0;2,-16,64,0,2,0;3,-1,64,0,3,0;4,0,64,0,4,0;5,15,64,0,5,0;6,16,64,0,6,0;4,0,64,0,7,0";
    for x in -2..=1 {
        writeln!(script, "load S {x} 0 {filtered} _\npack filter_{x} S B {x} 0 0\nunpack S {x} 0 0\nregister S {x} 0 true").unwrap();
    }
    script.push_str("run filtered_positions S B 20 20 none\nnew\nregister S 0 0 true\ns S B 1 64 0 1 2147483651 0 0\ns S B 2 64 0 2 -2147483651 0 0\ns S B 3 64 0 3 -9223372036854775808 0 0\npack narrow_delays S B 0 0 0\npack subtraction_wrap S B 0 0 9223372036854775807\n");
    for (label, now, delay) in [("add_wrap", i64::MAX, 1), ("subtract_wrap", i64::MIN, -1)] {
        writeln!(script, "new\nload S 0 0 1,1,64,0,{delay},0 _\nregister S 0 0 true\nunpack S 0 0 {now}\npack {label} S B 0 0 0\nrun {label} S B 9223372036854775807 20 none").unwrap();
    }
    for history in [
        "fresh",
        "cap0",
        "cap0_twice",
        "cap1",
        "detach",
        "eligibility",
    ] {
        script.push_str("new\n");
        for (id, x) in [62, 63, 114, 124, 164, 191].into_iter().enumerate() {
            writeln!(
                script,
                "load S {x} 0 {},{},64,0,0,0 _\nregister S {x} 0 true\nunpack S {x} 0 100",
                id + 1,
                x * 16
            )
            .unwrap();
        }
        match history {
            "cap0" | "cap0_twice" => {
                writeln!(script, "run {history}_zero S B 100 0 none").unwrap();
                if history == "cap0_twice" { script.push_str("run cap0_twice_zero_again S B 100 0 none\n"); }
            }
            "cap1" => script.push_str("run cap1_first S B 100 1 none\n"),
            "detach" => script.push_str("detach S 62 0\nregister S 62 0 true\n"),
            "eligibility" => script.push_str("eligible S 62 0 false\neligible S 114 0 false\nrun eligibility_partial S B 100 0 none\neligible S 62 0 true\neligible S 114 0 true\n"),
            _ => {}
        }
        writeln!(script, "run history_{history} S B 100 20 none").unwrap();
    }
    script.push_str("new\n");
    let coordinates = [
        (51, -83),
        (-24, -89),
        (-122, 7),
        (-104, -78),
        (-110, -17),
        (64, -64),
        (-91, 119),
        (-118, 10),
        (61, 7),
    ];
    for (id, (x, z)) in coordinates.into_iter().enumerate() {
        writeln!(
            script,
            "load S {x} {z} {},{},64,{},0,0 _\nregister S {x} {z} {}\nunpack S {x} {z} 100",
            id + 1,
            x * 16,
            z * 16,
            x & 3 != 0
        )
        .unwrap();
    }
    script.push_str("run wrapped_history_zero S B 100 0 none\n");
    for (x, z) in coordinates {
        writeln!(script, "eligible S {x} {z} true").unwrap();
    }
    script.push_str("run wrapped_history_rest S B 100 20 none\nnew\nregister S -1 0 true\nregister S 0 0 true\nregister S 1 0 true\ns S B -1 64 0 1 5 0 0\ns S B 1 64 0 2 10 0 0\ns S B 2 64 0 3 20 0 0\ns S B 17 64 0 4 5 0 0\nclear S B -1 1\nrun clear_head_10 S B 10 20 none\nrun clear_head_20 S B 20 20 none\n");
    for action in ["clear_none", "clear_query"] {
        writeln!(script, "new\nregister S 0 0 true\nregister D 2 0 true\ns S B 1 64 0 1 10 0 0\ns S B 2 64 0 2 10 0 0\ns S B 3 64 0 3 10 0 0\ns S B 4 64 0 4 100 0 0\nrun {action} S B 10 20 {action}\nstatus {action}_cleaned S B\nrun {action}_copied D B 10 20 none").unwrap();
    }
    script.push_str("new\nregister S 0 0 true\nregister D 2 0 true\nregister D 3 0 true\n");
    for _ in 0..4 {
        script.push_str("s S B 160 64 0 8 0 0 0\n");
    }
    script.push_str("s S B 1 64 0 1 5 0 0\ns S B 2 64 0 2 5 0 0\ns S B 3 64 0 3 100 0 -1\ns S B 8 64 0 4 5 0 1\n");
    for _ in 0..12 {
        script.push_str("s D B 160 64 0 8 0 0 0\n");
    }
    script.push_str("s D B 49 67 1 5 5 0 0\nrun copy_sets S B 5 2 copy\ncopy D S B 1 3 32 3 1\npack copy_after_cleanup D B 2 0 0\nrun copy_destination_due D B 5 20 none\nrun copy_source_remaining S B 100 20 none\nrun copy_destination_future D B 100 20 none\nstatus copy_counter S B\nstatus destination_counter D B\nnew\nregister S 0 0 true\ns S B 1 64 0 1 5 0 0\ns S B 2 64 0 1 10 0 -1\ncopy S S B 1 2 1 0 0\npack self_overlap S B 0 0 0\ncopy S S B 1 3 0 0 0\npack self_zero_queued S B 0 0 0\nrun self_overlap_due S B 20 20 none\nnew\nregister S 0 0 true\ns S B 1 64 0 1 5 0 0\ns S B 2 64 0 2 5 0 0\nrun self_callback S B 5 20 self\npack self_next_queue S B 0 0 0\ns S B 3 64 0 3 5 0 0\nrun self_next_collect S B 5 20 none\nnew\nload S 0 0 1,1,64,0,0,0 _\nregister S 0 0 true\nregister D 2 0 true\ncopy D S B 1 1 32 0 0\npack excludes_pending D B 2 0 0\nunpack S 0 0 10\ncopy D S B 1 1 64 0 0\nstatus missing_destination D B\nrun pending_source S B 10 20 none\n");
    script.push_str("new\nload S 0 0 1,1,64,0,0,0;2,2,64,0,0,0 _\nregister S 0 0 true\nregister D 2 0 true\nunpack S 0 0 10\ncopy D S B 1 2 32 0 0\nstatus negative_copy_counter D B\ns D B 35 64 0 3 10 0 0\npack negative_copy S B 0 0 0\nrun negative_copy_tie D B 10 20 none\nrun negative_copy_source S B 10 20 none\n");
    script
}

#[test]
#[ignore = "requires Java25 and locked jars via ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"]
fn saved_tick_and_area_trace_matches_actual_vanilla() {
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
    let libraries = jar.parent().unwrap().join("libraries/*");
    let classpath = env::join_paths([jar, libraries]).unwrap();
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-persistence-tick-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("PersistenceTickOracle.java");
    let input = directory.join("input.txt");
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
    let actual: Vec<_> = fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        directory
            .canonicalize()
            .unwrap()
            .starts_with(env::temp_dir().canonicalize().unwrap())
    );
    fs::remove_dir_all(&directory).unwrap();
    assert_eq!(actual.len(), expected.len(), "trace length");
    for (index, (java, rust)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(java, rust, "trace row {index}");
    }
    eprintln!(
        "Compared {} saved-tick/order-history/area trace rows against actual Vanilla 26.3-pre-2",
        actual.len()
    );
}
