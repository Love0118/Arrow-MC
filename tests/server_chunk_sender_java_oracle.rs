//! Opt-in comparison with locked Java sender feedback and dependency selection.
//! Set ARROW_MC_JAVA_REFERENCE_ROOT and run this test with --ignored (Java 25).
//! Java rate/ACK observations call the actual PlayerChunkSender. Selection uses
//! its actual pending set plus the bundled Guava collector and ChunkPos API;
//! it does not instantiate a world or claim full socket/server integration.

use arrow_mc::server::chunk_sender::{
    ChunkDeliveryQueue, ChunkSender, DeliveryLimits, SendReadyChunk, SenderLimits,
};
use arrow_mc::world::preparation::ChunkAddress;
use std::{env, fs, io::Read, path::Path, process::Command, time::SystemTime};

const JAVA: &str = r#"
import com.google.common.collect.Comparators;
import it.unimi.dsi.fastutil.longs.LongSet;
import net.minecraft.SharedConstants;
import net.minecraft.server.network.PlayerChunkSender;
import net.minecraft.world.level.ChunkPos;
import java.io.*;
import java.lang.reflect.Field;
import java.nio.file.*;
import java.util.*;

class ChunkSenderCrossOracle {
    static Object field(PlayerChunkSender sender, String name) throws Exception {
        Field field=PlayerChunkSender.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(sender);
    }
    static void set(PlayerChunkSender sender, String name, Object value) throws Exception {
        Field field=PlayerChunkSender.class.getDeclaredField(name);
        field.setAccessible(true);
        field.set(sender,value);
    }
    static void state(DataOutputStream out, PlayerChunkSender sender) throws Exception {
        out.writeInt(Float.floatToRawIntBits((Float)field(sender,"desiredChunksPerTick")));
        out.writeInt(Float.floatToRawIntBits((Float)field(sender,"batchQuota")));
        out.writeInt((Integer)field(sender,"unacknowledgedBatches"));
        out.writeInt((Integer)field(sender,"maxUnacknowledgedBatches"));
    }
    static void assertMaximumRateTrim(LongSet pending, Comparator<Long> order) throws Exception {
        var type=Class.forName("com.google.common.collect.TopKSelector");
        var factory=type.getDeclaredMethod("least",int.class,Comparator.class);
        var offer=type.getDeclaredMethod("offer",Object.class);
        var size=type.getDeclaredField("bufferSize");
        factory.setAccessible(true);offer.setAccessible(true);size.setAccessible(true);
        var selector=factory.invoke(null,64,order);
        boolean trimmed=false;
        for(long key:pending.longStream().toArray()) {
            int before=size.getInt(selector);
            offer.invoke(selector,Long.valueOf(key));
            if(before==127 && size.getInt(selector)==64) trimmed=true;
        }
        if(!trimmed) throw new AssertionError("maximum-rate fixture did not exercise trim");
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference");
        try (var in=new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[0]))));
             var out=new DataOutputStream(new BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            if(in.readInt()==1) {
                PlayerChunkSender sender=null;
                int commands=in.readInt();
                for(int i=0;i<commands;i++) {
                    switch(in.readInt()) {
                        case 0 -> {
                            sender=new PlayerChunkSender(false);
                            set(sender,"desiredChunksPerTick",Float.intBitsToFloat(in.readInt()));
                            set(sender,"batchQuota",Float.intBitsToFloat(in.readInt()));
                            set(sender,"unacknowledgedBatches",in.readInt());
                            set(sender,"maxUnacknowledgedBatches",in.readInt());
                        }
                        case 1 -> sender.onChunkBatchReceivedByClient(Float.intBitsToFloat(in.readInt()));
                        case 2 -> sender.sendNextChunks(null);
                        default -> throw new AssertionError("unknown operation");
                    }
                    state(out,sender);
                }
                return;
            }
            int cases=in.readInt();
            for(int i=0;i<cases;i++) {
                boolean memory=in.readBoolean();
                float desired=Float.intBitsToFloat(in.readInt());
                var center=new ChunkPos(in.readInt(),in.readInt());
                var sender=new PlayerChunkSender(memory);
                sender.onChunkBatchReceivedByClient(desired);
                // Empty pending set exercises the actual tick budget without needing a world.
                sender.sendNextChunks(null);
                state(out,sender);
                LongSet pending=(LongSet)field(sender,"pendingChunks");
                int positions=in.readInt();
                for(int j=0;j<positions;j++) pending.add(ChunkPos.pack(in.readInt(),in.readInt()));
                int removals=in.readInt();
                for(int j=0;j<removals;j++) pending.remove(ChunkPos.pack(in.readInt(),in.readInt()));
                int quota=((Float)field(sender,"batchQuota")).intValue();
                Comparator<Long> byDistance=Comparator.comparingInt(center::distanceSquared);
                if(!memory && pending.size()==4225 && quota==64) assertMaximumRateTrim(pending,byDistance);
                List<Long> selected=(!memory && pending.size()>quota)
                    ? pending.stream().collect(Comparators.least(quota,byDistance))
                    : pending.longStream().boxed().sorted(byDistance).toList();
                out.writeInt(selected.size());
                for(long key:selected) { out.writeInt(ChunkPos.getX(key)); out.writeInt(ChunkPos.getZ(key)); }
            }
        }
    }
}
"#;

#[derive(Clone)]
struct SelectionCase {
    memory: bool,
    desired: f32,
    center: ChunkAddress,
    insertion: Vec<ChunkAddress>,
    removals: Vec<ChunkAddress>,
}

fn coordinate(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}

fn put_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn read_u32(input: &mut &[u8]) -> u32 {
    let mut value = [0; 4];
    input.read_exact(&mut value).unwrap();
    u32::from_be_bytes(value)
}

fn oracle(input: &[u8]) -> Vec<u8> {
    let reference =
        env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set ARROW_MC_JAVA_REFERENCE_ROOT");
    let artifacts = Path::new(&reference).join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-chunk-sender-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("ChunkSenderCrossOracle.java");
    let file = directory.join("input.bin");
    let output = directory.join("output.bin");
    fs::write(&source, JAVA).unwrap();
    fs::write(&file, input).unwrap();
    let run = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(file)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let observed = fs::read(output).unwrap();
    // Only the freshly created, uniquely named temporary oracle directory is removed.
    fs::remove_dir_all(directory).unwrap();
    observed
}

fn selection_cases() -> Vec<SelectionCase> {
    let minimal = [(1, -1), (-2, 0), (0, -2), (0, -1), (1, 2), (-1, -3)]
        .map(|(x, z)| coordinate(x, z))
        .to_vec();
    let grid: Vec<_> = (-4..=4)
        .flat_map(|x| (-4..=4).map(move |z| coordinate(x, z)))
        .collect();
    let limits = vec![
        coordinate(0, 0),
        coordinate(46_341, 0),
        coordinate(65_536, 0),
        coordinate(32_768, 32_768),
        coordinate(i32::MIN, i32::MAX),
        coordinate(1_000_000, 1_000_000),
        coordinate(-1, 0),
        coordinate(1, 0),
    ];
    let mut cases = Vec::new();
    for memory in [false, true] {
        for desired in [1.0, 2.0, 3.0, 7.0, 8.0, 9.0, 10.0, 31.0, 64.0] {
            for insertion in [&minimal, &grid, &limits] {
                cases.push(SelectionCase {
                    memory,
                    desired,
                    center: coordinate(0, 0),
                    insertion: insertion.clone(),
                    removals: Vec::new(),
                });
            }
        }
    }
    let mut random = 0x62f4_e9aa_1847_0001u64;
    for variant in 0..32 {
        let mut insertion = grid.clone();
        for index in (1..insertion.len()).rev() {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            insertion.swap(index, (random as usize) % (index + 1));
        }
        cases.push(SelectionCase {
            memory: variant % 4 == 0,
            desired: (variant % 16 + 1) as f32,
            center: coordinate(variant % 3 - 1, variant % 5 - 2),
            insertion,
            removals: Vec::new(),
        });
    }
    let growth: Vec<_> = (0..100).map(|x| coordinate(x, x % 7 - 3)).collect();
    for memory in [false, true] {
        for count in [23, 24, 25, 48, 49, 96, 97, 100] {
            for removed in [0, 1, 52, 53, 76, 77, 88, 89, 100] {
                if removed > count {
                    continue;
                }
                let mut insertion = growth[..count].to_vec();
                // A duplicate must leave physical table history unchanged.
                insertion.push(growth[0]);
                cases.push(SelectionCase {
                    memory,
                    desired: 9.0,
                    center: coordinate(0, 0),
                    insertion,
                    removals: growth[..removed].to_vec(),
                });
            }
        }
    }
    for bits in [
        0,
        0x8000_0000,
        1,
        0x3c23_d709,
        0x3c23_d70a,
        0x3c23_d70b,
        0x3eaa_aaab,
        0x4280_0001,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0001,
        0xffc0_0001,
    ] {
        cases.push(SelectionCase {
            memory: false,
            desired: f32::from_bits(bits),
            center: coordinate(i32::MAX, i32::MIN),
            insertion: limits.clone(),
            removals: Vec::new(),
        });
    }
    // More than 2*k offers are required to exercise trim at the maximum rate.
    // This footprint also checks logical hash growth for a full radius-32 view.
    let full_view: Vec<_> = (-32..=32)
        .flat_map(|x| (-32..=32).map(move |z| coordinate(x, z)))
        .collect();
    for desired in [1.0, 3.0, 16.0, 32.0, 64.0] {
        cases.push(SelectionCase {
            memory: false,
            desired,
            center: coordinate(0, 0),
            insertion: full_view.clone(),
            removals: Vec::new(),
        });
    }
    cases
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with official server jars"]
fn candidate_order_matches_actual_fastutil_guava_and_chunk_distance() {
    let cases = selection_cases();
    let mut input = Vec::new();
    put_i32(&mut input, 0);
    put_i32(&mut input, cases.len() as i32);
    for case in &cases {
        input.push(u8::from(case.memory));
        input.extend_from_slice(&case.desired.to_bits().to_be_bytes());
        put_i32(&mut input, case.center.x);
        put_i32(&mut input, case.center.z);
        put_i32(&mut input, case.insertion.len() as i32);
        for position in &case.insertion {
            put_i32(&mut input, position.x);
            put_i32(&mut input, position.z);
        }
        put_i32(&mut input, case.removals.len() as i32);
        for position in &case.removals {
            put_i32(&mut input, position.x);
            put_i32(&mut input, position.z);
        }
    }
    let observations = oracle(&input);
    let mut observed = observations.as_slice();
    for (index, case) in cases.iter().enumerate() {
        let desired = read_u32(&mut observed);
        let quota = read_u32(&mut observed);
        let outstanding = read_u32(&mut observed);
        let maximum = read_u32(&mut observed);
        let count = read_u32(&mut observed);
        let expected: Vec<_> = (0..count)
            .map(|_| {
                coordinate(
                    read_u32(&mut observed) as i32,
                    read_u32(&mut observed) as i32,
                )
            })
            .collect();
        let mut sender = ChunkSender::new(
            case.memory,
            SenderLimits {
                max_pending: case.insertion.len().max(1),
                control_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        sender.acknowledge(case.desired);
        for position in &case.insertion {
            sender.mark_pending(*position).unwrap();
        }
        let mut delivery = ChunkDeliveryQueue::new(DeliveryLimits {
            max_groups: 1,
            max_bytes: 4096,
        })
        .unwrap();
        for position in &case.removals {
            sender.drop_chunk(*position, false, &mut delivery).unwrap();
        }
        let plan = sender.begin_tick(0, case.center).unwrap();
        assert_eq!(plan.candidates(), expected, "case {index}");
        let stats = sender.stats();
        assert_eq!(
            stats.desired_chunks_per_tick.to_bits(),
            desired,
            "case {index}"
        );
        assert_eq!(stats.batch_quota.to_bits(), quota, "case {index}");
        assert_eq!(
            stats.unacknowledged_batches as u32, outstanding,
            "case {index}"
        );
        assert_eq!(
            stats.max_unacknowledged_batches as u32, maximum,
            "case {index}"
        );
    }
    assert!(observed.is_empty());
    eprintln!("Actual Java dependency selection: {} cases", cases.len());
}

fn new_sender(memory: bool) -> ChunkSender {
    ChunkSender::new(
        memory,
        SenderLimits {
            max_pending: 256,
            control_bytes: 1024 * 1024,
        },
    )
    .unwrap()
}

fn snapshot(sender: &ChunkSender) -> [u32; 4] {
    let stats = sender.stats();
    [
        stats.desired_chunks_per_tick.to_bits(),
        stats.batch_quota.to_bits(),
        u32::from(stats.unacknowledged_batches),
        u32::from(stats.max_unacknowledged_batches),
    ]
}

fn admit(sender: &mut ChunkSender, tick: u64, all: bool) {
    let mut delivery = ChunkDeliveryQueue::new(DeliveryLimits {
        max_groups: 1,
        max_bytes: 16384,
    })
    .unwrap();
    let mut plan = sender.begin_tick(tick, coordinate(0, 0)).unwrap();
    assert!(!plan.candidates().is_empty());
    let ready: Vec<_> = plan
        .candidates()
        .iter()
        .enumerate()
        .map(|(index, position)| {
            (all || index == 0).then_some(SendReadyChunk {
                position: *position,
                packet_bytes: &[1],
            })
        })
        .collect();
    plan.try_admit(&mut delivery, &ready).unwrap();
    // Completed ordered delivery is separate from client feedback.
    while delivery.front_packet().is_some() {
        delivery.packet_written().unwrap();
    }
}

fn observe(
    commands: &mut Vec<Vec<u32>>,
    expected: &mut Vec<[u32; 4]>,
    command: Vec<u32>,
    sender: &ChunkSender,
) {
    commands.push(command);
    expected.push(snapshot(sender));
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with official server jars"]
fn feedback_and_tick_float_states_match_actual_player_chunk_sender() {
    let mut commands = Vec::new();
    let mut expected = Vec::new();
    let feedback = [
        0,
        0x8000_0000,
        1,
        0x3c23_d709,
        0x3c23_d70a,
        0x3c23_d70b,
        0x3eaa_aaab,
        0x3f00_0000,
        0x4280_0000,
        0x4280_0001,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0001,
        0xffc0_0001,
        0x7f7f_ffff,
    ];
    for requested in feedback {
        for outstanding in [0, 1, 2, 10] {
            let mut sender = new_sender(false);
            sender.acknowledge(9.0);
            for x in 0..128 {
                sender.mark_pending(coordinate(x, 0)).unwrap();
            }
            for tick in 0..outstanding {
                admit(&mut sender, tick, false);
            }
            // Independently specified reachable checkpoint: one ready chunk per
            // nine-slot tick leaves quota eight, one ACK outstanding per batch.
            // Java reflection seeds this checkpoint; subsequent methods are real.
            let quota = if outstanding == 0 { 1.0f32 } else { 8.0f32 };
            assert_eq!(
                snapshot(&sender),
                [9.0f32.to_bits(), quota.to_bits(), outstanding as u32, 10]
            );
            observe(
                &mut commands,
                &mut expected,
                vec![0, 9.0f32.to_bits(), quota.to_bits(), outstanding as u32, 10],
                &sender,
            );
            let _ = sender.begin_tick(100, coordinate(0, 0)).unwrap();
            observe(&mut commands, &mut expected, vec![2], &sender);
            for round in 0..2 {
                sender.acknowledge(f32::from_bits(requested));
                observe(&mut commands, &mut expected, vec![1, requested], &sender);
                let _ = sender.begin_tick(101 + round, coordinate(0, 0)).unwrap();
                observe(&mut commands, &mut expected, vec![2], &sender);
            }
        }
    }

    let mut sender = new_sender(false);
    sender.acknowledge(0.01);
    sender.mark_pending(coordinate(0, 0)).unwrap();
    admit(&mut sender, 0, false);
    assert_eq!(snapshot(&sender), [0.01f32.to_bits(), 0, 1, 10]);
    observe(
        &mut commands,
        &mut expected,
        vec![0, 0.01f32.to_bits(), 0, 1, 10],
        &sender,
    );
    for tick in 1..=105 {
        let _ = sender.begin_tick(tick, coordinate(0, 0)).unwrap();
        observe(&mut commands, &mut expected, vec![2], &sender);
        if tick == 100 {
            assert_eq!(sender.stats().batch_quota.to_bits(), 0x3f7f_fff5);
        }
        if tick == 101 {
            assert_eq!(sender.stats().batch_quota.to_bits(), 1.0f32.to_bits());
        }
    }

    let mut sender = new_sender(true);
    sender.acknowledge(3.0);
    for x in 0..20 {
        sender.mark_pending(coordinate(x, 0)).unwrap();
    }
    admit(&mut sender, 0, true);
    assert_eq!(
        snapshot(&sender),
        [3.0f32.to_bits(), (-17.0f32).to_bits(), 1, 10]
    );
    observe(
        &mut commands,
        &mut expected,
        vec![0, 3.0f32.to_bits(), (-17.0f32).to_bits(), 1, 10],
        &sender,
    );
    for tick in 1..=8 {
        let _ = sender.begin_tick(tick, coordinate(0, 0)).unwrap();
        observe(&mut commands, &mut expected, vec![2], &sender);
    }

    let mut input = Vec::new();
    put_i32(&mut input, 1);
    put_i32(&mut input, commands.len() as i32);
    for command in &commands {
        for word in command {
            input.extend_from_slice(&word.to_be_bytes());
        }
    }
    let observations = oracle(&input);
    let mut observed = observations.as_slice();
    for (index, expected) in expected.iter().enumerate() {
        let actual = std::array::from_fn(|_| read_u32(&mut observed));
        assert_eq!(*expected, actual, "feedback/tick checkpoint {index}");
    }
    assert!(observed.is_empty());
    eprintln!(
        "Actual Java sender feedback/tick checkpoints: {}",
        expected.len()
    );
}
