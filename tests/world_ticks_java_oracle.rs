//! Opt-in comparison with the actual locked Java scheduled-tick containers.
//! Set ARROW_VANILLA_SERVER_JAR directly, or ARROW_MC_JAVA_REFERENCE_ROOT to the
//! sibling Decompile root, then run this test with --ignored. Java 25 is required.
//! The embedded Java code is an independently authored public-API observer.

use std::{env, fmt::Write, fs, path::PathBuf, process::Command, time::SystemTime};

use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::ticks::{
    ScheduleOutcome, ScheduledTickOwner, TickDomain, TickLimits, TickPosition, TickPriority,
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
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.ticks.LevelChunkTicks;
import net.minecraft.world.ticks.LevelTicks;
import net.minecraft.world.ticks.ScheduledTick;
import net.minecraft.world.ticks.TickPriority;

class LiveTickOracle {
    static final String[] TYPES = new String[32];
    static final List<String> OUTPUT = new ArrayList<>();
    static Set<Long> eligible;
    static Set<Long> registered;
    static Map<Long, LevelChunkTicks<String>> blockContainers;
    static Map<Long, LevelChunkTicks<String>> fluidContainers;
    static LevelTicks<String> blocks;
    static LevelTicks<String> fluids;
    static long suborder;

    static void reset() {
        eligible = new HashSet<>();
        registered = new HashSet<>();
        blockContainers = new HashMap<>();
        fluidContainers = new HashMap<>();
        blocks = new LevelTicks<>(eligible::contains);
        fluids = new LevelTicks<>(eligible::contains);
        suborder = 0;
    }

    static LevelTicks<String> domain(String domain) { return domain.equals("B") ? blocks : fluids; }
    static BlockPos pos(int x) { return new BlockPos(x, 64, 0); }
    static void schedule(String domain, BlockPos position, int id, long time, int delay, int priority) {
        LevelTicks<String> ticks = domain(domain);
        String outcome = !registered.contains(ChunkPos.pack(position)) ? "MissingContainer"
            : ticks.hasScheduledTick(position, TYPES[id]) ? "Duplicate" : "Added";
        ticks.schedule(new ScheduledTick<>(TYPES[id], position, time + delay, TickPriority.byValue(priority), suborder++));
        OUTPUT.add("S|" + outcome + "|" + suborder);
    }

    static String queries(LevelTicks<String> ticks) {
        String result = "|" + ticks.count();
        for (int id = 1; id <= 3; id++) {
            result += "|" + ticks.hasScheduledTick(pos(id), TYPES[id]) + "|" + ticks.willTickThisTick(pos(id), TYPES[id]);
        }
        return result;
    }

    static void run(String name, String domain, long time, int cap, String action) {
        LevelTicks<String> ticks = domain(domain);
        OUTPUT.add("R|" + name + queries(ticks));
        ticks.tick(time, cap, (position, type) -> {
            int id = Integer.parseInt(type);
            OUTPUT.add("C|" + name + "|" + id + "|" + position.getX() + "|" + position.getY() + "|" + position.getZ() + queries(ticks));
            if (action.equals("reschedule") && id == 1) {
                schedule(domain, pos(1), 1, time, 0, 0);
                schedule(domain, pos(2), 2, time, 0, -1);
                schedule(domain, pos(3), 3, time, 0, -3);
                OUTPUT.add("Q|" + name + queries(ticks));
            }
            if (action.equals("blockfluid") && id == 1) {
                schedule("F", pos(2), 2, time, 0, -1);
                schedule("B", pos(3), 3, time, 0, 0);
                OUTPUT.add("Q|" + name + queries(ticks));
            }
            if (action.equals("fluidblock") && id == 2) {
                schedule("B", pos(4), 4, time, 0, -1);
                OUTPUT.add("Q|" + name + queries(ticks));
            }
        });
        OUTPUT.add("E|" + name + queries(ticks) + "|" + suborder);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        for (int id = 0; id < TYPES.length; id++) TYPES[id] = new String(Integer.toString(id));
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] f = line.split(" ");
            switch (f[0]) {
                case "new" -> reset();
                case "register" -> {
                    ChunkPos position = new ChunkPos(Integer.parseInt(f[1]), Integer.parseInt(f[2]));
                    long key = position.pack();
                    if (!registered.add(key)) throw new AssertionError("already registered");
                    if (Boolean.parseBoolean(f[3])) eligible.add(key); else eligible.remove(key);
                    blocks.addContainer(position, blockContainers.computeIfAbsent(key, unused -> new LevelChunkTicks<>()));
                    fluids.addContainer(position, fluidContainers.computeIfAbsent(key, unused -> new LevelChunkTicks<>()));
                }
                case "eligible" -> {
                    long key = new ChunkPos(Integer.parseInt(f[1]), Integer.parseInt(f[2])).pack();
                    if (Boolean.parseBoolean(f[3])) eligible.add(key); else eligible.remove(key);
                }
                case "detach" -> {
                    ChunkPos position = new ChunkPos(Integer.parseInt(f[1]), Integer.parseInt(f[2]));
                    registered.remove(position.pack());
                    blocks.removeContainer(position);
                    fluids.removeContainer(position);
                }
                case "s" -> schedule(f[1], new BlockPos(Integer.parseInt(f[2]), Integer.parseInt(f[3]), Integer.parseInt(f[4])),
                    Integer.parseInt(f[5]), Long.parseLong(f[6]), Integer.parseInt(f[7]), Integer.parseInt(f[8]));
                case "run" -> run(f[1], f[2], Long.parseLong(f[3]), Integer.parseInt(f[4]), f[5]);
                default -> throw new AssertionError("unknown command " + line);
            }
        }
        Files.write(Path.of(args[1]), OUTPUT);
    }
}
"#;

fn new_owner() -> ScheduledTickOwner {
    ScheduledTickOwner::new(
        32,
        32,
        TickLimits {
            max_chunks: 8,
            queued_per_chunk: 128,
            selected_per_phase: 256,
            allocation_bytes: 1024 * 1024,
        },
    )
    .unwrap()
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

fn schedule(
    owner: &mut ScheduledTickOwner,
    domain: TickDomain,
    position: TickPosition,
    id: u32,
    time: i64,
    delay: i32,
    priority: i32,
) -> String {
    let outcome = owner
        .schedule(
            domain,
            position,
            id,
            time,
            delay,
            TickPriority::from_value(priority),
        )
        .unwrap();
    let name = match outcome {
        ScheduleOutcome::Added => "Added",
        ScheduleOutcome::Duplicate => "Duplicate",
        ScheduleOutcome::MissingContainer => "MissingContainer",
    };
    format!("S|{name}|{}", owner.next_sub_tick_order())
}

fn queries(owner: &ScheduledTickOwner, domain: TickDomain) -> String {
    let mut result = format!("|{}", owner.queued_count(domain));
    for id in 1..=3 {
        write!(
            result,
            "|{}|{}",
            owner.has_scheduled(domain, position(id), id as u32),
            owner.will_tick_this_phase(domain, position(id), id as u32)
        )
        .unwrap();
    }
    result
}

fn rust_trace(script: &str) -> Vec<String> {
    let mut owner = new_owner();
    let mut output = Vec::new();
    for line in script.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let n = |index: usize| fields[index].parse::<i64>().unwrap();
        match fields[0] {
            "new" => owner = new_owner(),
            "register" => owner
                .register_chunk(
                    ChunkAddress {
                        x: n(1) as i32,
                        z: n(2) as i32,
                    },
                    fields[3].parse().unwrap(),
                )
                .unwrap(),
            "eligible" => owner
                .set_eligible(
                    ChunkAddress {
                        x: n(1) as i32,
                        z: n(2) as i32,
                    },
                    fields[3].parse().unwrap(),
                )
                .unwrap(),
            "detach" => owner
                .detach_chunk(ChunkAddress {
                    x: n(1) as i32,
                    z: n(2) as i32,
                })
                .unwrap(),
            "s" => output.push(schedule(
                &mut owner,
                domain(fields[1]),
                TickPosition {
                    x: n(2) as i32,
                    y: n(3) as i32,
                    z: n(4) as i32,
                },
                n(5) as u32,
                n(6),
                n(7) as i32,
                n(8) as i32,
            )),
            "run" => {
                let name = fields[1];
                let domain = domain(fields[2]);
                let time = n(3);
                let action = fields[5];
                output.push(format!("R|{name}{}", queries(&owner, domain)));
                owner.begin_phase(domain, time, n(4) as usize).unwrap();
                while let Some(tick) = owner.next_due().unwrap() {
                    output.push(format!(
                        "C|{name}|{}|{}|{}|{}{}",
                        tick.type_id,
                        tick.position.x,
                        tick.position.y,
                        tick.position.z,
                        queries(&owner, domain)
                    ));
                    if action == "reschedule" && tick.type_id == 1 {
                        for (id, priority) in [(1, 0), (2, -1), (3, -3)] {
                            output.push(schedule(
                                &mut owner,
                                domain,
                                position(id),
                                id as u32,
                                time,
                                0,
                                priority,
                            ));
                        }
                        output.push(format!("Q|{name}{}", queries(&owner, domain)));
                    }
                    if action == "blockfluid" && tick.type_id == 1 {
                        output.push(schedule(
                            &mut owner,
                            TickDomain::Fluid,
                            position(2),
                            2,
                            time,
                            0,
                            -1,
                        ));
                        output.push(schedule(
                            &mut owner,
                            TickDomain::Block,
                            position(3),
                            3,
                            time,
                            0,
                            0,
                        ));
                        output.push(format!("Q|{name}{}", queries(&owner, domain)));
                    }
                    if action == "fluidblock" && tick.type_id == 2 {
                        output.push(schedule(
                            &mut owner,
                            TickDomain::Block,
                            position(4),
                            4,
                            time,
                            0,
                            -1,
                        ));
                        output.push(format!("Q|{name}{}", queries(&owner, domain)));
                    }
                }
                owner.finish_phase().unwrap();
                output.push(format!(
                    "E|{name}{}|{}",
                    queries(&owner, domain),
                    owner.next_sub_tick_order()
                ));
            }
            _ => panic!("unknown fixture command: {line}"),
        }
    }
    output
}

fn fixtures() -> String {
    let mut script = String::from(
        "new\nregister 0 0 true\ns B 1 64 0 1 100 0 1\ns B 1 64 0 1 0 0 -3\ns B 1 64 0 2 100 0 0\ns B 2 64 0 1 100 0 0\nrun dedup_before B 99 20 none\nrun dedup B 100 20 none\n",
    );
    for (name, x) in [("same_chunk", 2), ("cross_chunk", 17)] {
        writeln!(script, "new\nregister 0 0 true\nregister 1 0 true\ns B 1 64 0 1 1 0 1\ns B {x} 64 0 2 9 0 -3\nrun {name} B 10 20 none").unwrap();
    }
    script.push_str("new\nregister 0 0 true\nregister 1 0 true\ns B 1 64 0 1 1 0 1\ns B 2 64 0 2 9 0 -3\ns B 17 64 0 3 5 0 0\nrun hidden_head B 10 20 none\n");
    script.push_str("new\nregister 0 0 true\ns B 1 64 0 1 5 0 0\ns B 2 64 0 2 5 0 0\nrun callbacks B 5 20 reschedule\nrun callback_next B 5 20 none\n");
    script.push_str("new\nregister 0 0 true\ns B 1 64 0 1 5 0 0\ns F 1 64 0 1 5 0 0\nrun block_phase B 5 20 blockfluid\nrun fluid_phase F 5 20 fluidblock\nrun next_block B 5 20 none\n");
    script.push_str("new\nregister 0 0 true\nregister 1 0 true\ns B 1 64 0 1 1 0 0\ns B 2 64 0 2 2 0 -1\ns B 17 64 0 3 1 0 -1\ns B 18 64 0 4 2 0 1\ns B 3 64 0 5 100 0 -3\nrun cap0 B 10 0 none\nrun cap2 B 10 2 none\nrun cap1 B 10 1 none\nrun rest B 10 20 none\nrun future B 100 20 none\n");
    script.push_str("new\nregister 0 0 false\ns B 1 64 0 1 5 0 0\nrun ineligible B 10 20 none\ndetach 0 0\ns B 2 64 0 2 5 0 -1\nrun detached B 10 20 none\nregister 0 0 false\nrun reattached_ineligible B 10 20 none\neligible 0 0 true\nrun eligible B 10 20 none\ns B 49 64 0 3 5 0 0\nregister 3 0 true\nrun missing_then_loaded B 10 20 none\n");
    script.push_str("new\nregister -1 -1 true\nregister -2 -2 true\ns B -1 -64 -1 1 100 -7 0\ns B -16 -64 -16 2 100 -7 0\ns B -17 -64 -17 3 100 -7 0\nrun negative_before B 92 20 none\nrun negative_due B 93 20 none\n");
    script.push_str(
        "new\nregister 0 0 true\nregister 1 0 true\nregister 2 0 true\nregister 3 0 true\n",
    );
    for index in 0..96 {
        let x = index % 4 * 16 + index / 4 % 16;
        let y = 64 + index / 64;
        let domain = if index % 3 == 0 { "F" } else { "B" };
        writeln!(
            script,
            "s {domain} {x} {y} 0 {} {} {} {}",
            index % 16,
            index % 11,
            index % 7 - 3,
            index % 7 - 3
        )
        .unwrap();
    }
    for time in [0, 3, 7, 20] {
        for domain in ["B", "F"] {
            writeln!(script, "run mixed_{time}_{domain}_cap {domain} {time} 7 none\nrun mixed_{time}_{domain}_rest {domain} {time} 256 none").unwrap();
        }
    }
    script
}

#[test]
#[ignore = "requires Java25 and locked jars via ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"]
fn live_scheduled_tick_trace_matches_actual_vanilla() {
    let jar = if let Some(jar) = env::var_os("ARROW_VANILLA_SERVER_JAR") {
        PathBuf::from(jar)
    } else {
        PathBuf::from(
            env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
                .expect("set ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"),
        )
        .join("artifacts/26.3-pre-2/server-26.3-pre-2.jar")
    };
    assert!(jar.is_file(), "missing server JAR: {}", jar.display());
    let libraries = jar.parent().unwrap().join("libraries/*");
    let classpath = env::join_paths([jar, libraries]).unwrap();
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-live-tick-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("LiveTickOracle.java");
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
    // Source-file mode compiles this independent driver against the actual JAR.
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
    let actual = fs::read_to_string(output).unwrap();
    assert!(
        directory
            .canonicalize()
            .unwrap()
            .starts_with(env::temp_dir().canonicalize().unwrap())
    );
    fs::remove_dir_all(&directory).unwrap();
    let actual: Vec<_> = actual.lines().map(str::to_owned).collect();
    assert_eq!(actual.len(), expected.len(), "trace length");
    for (index, (java, rust)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(java, rust, "trace row {index}");
    }
    eprintln!(
        "Compared {} schedule/callback/query trace rows against actual Vanilla 26.3-pre-2",
        actual.len()
    );
}
