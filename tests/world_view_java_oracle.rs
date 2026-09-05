//! Opt-in chunk tracking comparison with the locked Java server JAR.
//! Set ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT and use --ignored.
//! Requires Java 25. The observer calls public APIs on original small inputs; no
//! Vanilla implementation bodies, decompiled sources, or generated assets follow.

use std::{env, fmt::Write, fs, path::PathBuf, process::Command, time::SystemTime};

use arrow_mc::world::preparation::ChunkAddress;
use arrow_mc::world::view::{
    TrackingView, ViewChange, ViewDifference, ViewDistance, is_within_distance,
};

const ORACLE: &str = r#"
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.server.level.ChunkMap;
import net.minecraft.server.level.ChunkTrackingView;
import net.minecraft.world.level.ChunkPos;

class TrackingViewOracle {
    static final List<String> OUTPUT = new ArrayList<>();
    static int number(String[] fields, int index) { return Integer.parseInt(fields[index]); }
    static ChunkTrackingView view(String[] fields, int offset) {
        int distance = number(fields, offset + 2);
        return distance == 0 ? ChunkTrackingView.EMPTY
            : ChunkTrackingView.of(new ChunkPos(number(fields, offset), number(fields, offset + 1)), distance);
    }
    static void grid(String[] fields) {
        int centerX = number(fields, 2), centerZ = number(fields, 3), distance = number(fields, 4);
        int extent = distance + 3;
        ChunkTrackingView view = view(fields, 2);
        for (boolean neighbors : new boolean[] {false, true}) {
            for (int x = centerX - extent; x <= centerX + extent; x++) {
                StringBuilder row = new StringBuilder("G|" + fields[1] + "|" + neighbors + "|" + x + "|");
                for (int z = centerZ - extent; z <= centerZ + extent; z++) {
                    boolean member = ChunkTrackingView.isWithinDistance(centerX, centerZ, distance, x, z, neighbors);
                    if (member != view.contains(x, z, neighbors)) throw new AssertionError("instance/static membership");
                    boolean convenience = neighbors ? view.contains(x, z) : view.isInViewDistance(x, z);
                    if (member != convenience) throw new AssertionError("default membership");
                    row.append(member ? '1' : '0');
                }
                OUTPUT.add(row.toString());
            }
        }
    }
    static void point(String[] fields) {
        int centerX = number(fields, 2), centerZ = number(fields, 3), distance = number(fields, 4);
        int x = number(fields, 5), z = number(fields, 6);
        // Membership has no iterator: extreme int coordinates are safe to query.
        // Positioned scans with overflowing bounds are deliberately never run.
        for (boolean neighbors : new boolean[] {false, true}) {
            boolean member = ChunkTrackingView.isWithinDistance(centerX, centerZ, distance, x, z, neighbors);
            if (!neighbors && member != ChunkTrackingView.isInViewDistance(centerX, centerZ, distance, x, z)) {
                throw new AssertionError("static default membership");
            }
            OUTPUT.add("P|" + fields[1] + "|" + neighbors + "|" + member);
        }
    }
    static void difference(String[] fields) {
        StringBuilder row = new StringBuilder("D|" + fields[1] + "|");
        ChunkTrackingView.difference(view(fields, 2), view(fields, 5),
            position -> row.append("E,").append(position.x()).append(',').append(position.z()).append(';'),
            position -> row.append("L,").append(position.x()).append(',').append(position.z()).append(';'));
        OUTPUT.add(row.toString());
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        if (ChunkMap.MIN_VIEW_DISTANCE != 2 || ChunkMap.MAX_VIEW_DISTANCE != 32) throw new AssertionError("changed view bounds");
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] fields = line.split(" ");
            switch (fields[0]) {
                case "G" -> grid(fields);
                case "P" -> point(fields);
                case "D" -> difference(fields);
                default -> throw new AssertionError(line);
            }
        }
        Files.write(Path.of(args[1]), OUTPUT);
    }
}
"#;

fn number(fields: &[&str], index: usize) -> i32 {
    fields[index].parse().unwrap()
}

fn chunk(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}

fn distance(value: i32) -> ViewDistance {
    let distance = ViewDistance::server(value);
    assert_eq!(
        i32::from(distance.get()),
        value,
        "oracle radius is in range"
    );
    distance
}

fn view(fields: &[&str], offset: usize) -> TrackingView {
    let radius = number(fields, offset + 2);
    if radius == 0 {
        TrackingView::EMPTY
    } else {
        TrackingView::positioned(
            chunk(number(fields, offset), number(fields, offset + 1)),
            distance(radius),
        )
        .unwrap()
    }
}

fn rust_trace(script: &str) -> Vec<String> {
    let mut output = Vec::new();
    for line in script.lines() {
        let fields: Vec<_> = line.split(' ').collect();
        match fields[0] {
            "G" => {
                let center = chunk(number(&fields, 2), number(&fields, 3));
                let radius = distance(number(&fields, 4));
                let extent = i32::from(radius.get()) + 3;
                let positioned = view(&fields, 2);
                for neighbors in [false, true] {
                    for x in center.x - extent..=center.x + extent {
                        let mut row = format!("G|{}|{neighbors}|{x}|", fields[1]);
                        for z in center.z - extent..=center.z + extent {
                            let query = chunk(x, z);
                            let member = is_within_distance(center, radius, query, neighbors);
                            let convenience = if neighbors {
                                positioned.contains(query)
                            } else {
                                positioned.is_in_view_distance(query)
                            };
                            assert_eq!(member, convenience, "instance membership: {line} {x},{z}");
                            row.push(if member { '1' } else { '0' });
                        }
                        output.push(row);
                    }
                }
            }
            "P" => {
                let center = chunk(number(&fields, 2), number(&fields, 3));
                let radius = distance(number(&fields, 4));
                let query = chunk(number(&fields, 5), number(&fields, 6));
                for neighbors in [false, true] {
                    let member = is_within_distance(center, radius, query, neighbors);
                    output.push(format!("P|{}|{neighbors}|{member}", fields[1]));
                }
            }
            "D" => {
                let mut row = format!("D|{}|", fields[1]);
                for change in ViewDifference::new(view(&fields, 2), view(&fields, 5)) {
                    let (kind, position) = match change {
                        ViewChange::Enter(position) => ('E', position),
                        ViewChange::Leave(position) => ('L', position),
                    };
                    write!(row, "{kind},{},{};", position.x, position.z).unwrap();
                }
                output.push(row);
            }
            _ => panic!("unknown fixture: {line}"),
        }
    }
    output
}

fn fixtures() -> String {
    let mut script = String::new();
    // Every supported radius, every point through three chunks beyond the
    // radius, both neighbor modes, and the complete initial iteration order.
    for radius in 2..=32 {
        writeln!(script, "G radius_{radius} -17 29 {radius}").unwrap();
        writeln!(script, "D enter_{radius} 0 0 0 -17 29 {radius}").unwrap();
    }
    script.push_str("D empty 0 0 0 0 0 0\n");
    for radius in [2, 3, 8, 16, 31, 32] {
        writeln!(script, "D leave_{radius} -17 29 {radius} 0 0 0").unwrap();
        writeln!(script, "D equal_{radius} -17 29 {radius} -17 29 {radius}").unwrap();
        writeln!(script, "D east_{radius} -17 29 {radius} -16 29 {radius}").unwrap();
        writeln!(script, "D west_{radius} -17 29 {radius} -18 29 {radius}").unwrap();
        writeln!(
            script,
            "D diagonal_{radius} -17 29 {radius} -18 30 {radius}"
        )
        .unwrap();
        // Square bounds touch at 2r+2. Probe both directions: westward
        // touching scans enter before leaving, while disjoint scans leave
        // before entering regardless of the destination coordinates.
        let touching_x = -17 + radius * 2 + 2;
        writeln!(
            script,
            "D touch_{radius} -17 29 {radius} {touching_x} 29 {radius}"
        )
        .unwrap();
        writeln!(
            script,
            "D separate_{radius} -17 29 {radius} {} 29 {radius}",
            touching_x + 1
        )
        .unwrap();
        let touching_west = -17 - radius * 2 - 2;
        writeln!(
            script,
            "D touch_west_{radius} -17 29 {radius} {touching_west} 29 {radius}"
        )
        .unwrap();
        writeln!(
            script,
            "D separate_west_{radius} -17 29 {radius} {} 29 {radius}",
            touching_west - 1
        )
        .unwrap();
        writeln!(
            script,
            "D distant_{radius} -17 29 {radius} -100000 200000 {radius}"
        )
        .unwrap();
    }
    for (previous, next) in [(2, 3), (3, 2), (2, 32), (32, 2), (31, 32), (32, 31)] {
        writeln!(
            script,
            "D resize_{previous}_{next} -17 29 {previous} -17 29 {next}"
        )
        .unwrap();
        writeln!(
            script,
            "D move_resize_{previous}_{next} -17 29 {previous} -16 28 {next}"
        )
        .unwrap();
    }
    // Both nearest safe edges are iterated; a maximum bound of i32::MAX
    // would cause Java's loop increment to wrap and is excluded by the API.
    for radius in [2, 32] {
        let low = i32::MIN + radius + 1;
        let high = i32::MAX - radius - 2;
        writeln!(script, "D low_enter_{radius} 0 0 0 {low} {low} {radius}").unwrap();
        writeln!(script, "D high_enter_{radius} 0 0 0 {high} {high} {radius}").unwrap();
        writeln!(
            script,
            "D low_move_{radius} {low} {low} {radius} {} {} {radius}",
            low + 1,
            low + 1
        )
        .unwrap();
        writeln!(
            script,
            "D high_move_{radius} {high} {high} {radius} {} {} {radius}",
            high - 1,
            high - 1
        )
        .unwrap();
    }
    // This Cartesian product catches subtraction wrapping, Math.abs(MIN),
    // subtraction after abs, and mixed extreme/near-center axes. It never
    // constructs or iterates a positioned view at these arbitrary centers.
    let extrema = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
    let mut point_index = 0;
    for radius in [2, 3, 31, 32] {
        for center_x in extrema {
            for center_z in [0, i32::MIN, i32::MAX] {
                for x in extrema {
                    for z in [0, i32::MIN, i32::MAX] {
                        writeln!(
                            script,
                            "P extreme_{point_index} {center_x} {center_z} {radius} {x} {z}"
                        )
                        .unwrap();
                        point_index += 1;
                    }
                }
            }
        }
    }
    script
}

#[test]
#[ignore = "requires Java25 and locked jars via ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"]
fn membership_and_ordered_differences_match_actual_vanilla() {
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
        "arrow-mc-tracking-view-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("TrackingViewOracle.java");
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
        "Compared {} membership/ordered-view-difference trace rows against actual Vanilla 26.3-pre-2",
        actual.len()
    );
}
