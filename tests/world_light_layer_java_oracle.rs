//! Opt-in DataLayer comparison with the locked Java server JAR.
//! Set ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT and use --ignored.
//! Requires Java 25. This original observer calls the real DataLayer API without
//! starting a server or reproducing any Vanilla implementation bodies.

use std::{env, fmt::Write, fs, path::PathBuf, process::Command, time::SystemTime};

use arrow_mc::world::lighting::layer::{DataLayer, LAYER_BYTES};

const DEFAULTS: &[i32] = &[
    i32::MIN,
    i32::MIN + 1,
    -65_537,
    -256,
    -17,
    -16,
    -1,
    0,
    1,
    7,
    15,
    16,
    17,
    31,
    127,
    128,
    255,
    256,
    257,
    i32::MAX,
];

const ORACLE: &str = r#"
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.world.level.chunk.DataLayer;

class LightLayerOracle {
    static final List<String> OUTPUT = new ArrayList<>();
    static final DataLayer[] LAYERS = new DataLayer[3];
    static int number(String[] fields, int index) { return Integer.parseInt(fields[index]); }
    static void observe(String[] fields) {
        String label = fields[1];
        DataLayer layer = LAYERS[number(fields, 2)];
        boolean uniform = layer.isDefinitelyHomogenous();
        StringBuilder header = new StringBuilder("D|" + label + "|" + layer.isEmpty() + "|" + uniform);
        for (int index = 3; index < fields.length; index++) {
            header.append('|').append(layer.isDefinitelyFilledWith(number(fields, index)));
        }
        header.append('|').append(uniform ? 0 : layer.getData().length);
        OUTPUT.add(header.toString());
        for (int y = 0; y < 16; y++) {
            for (int z = 0; z < 16; z++) {
                StringBuilder row = new StringBuilder("G|" + label + "|" + y + "|" + z);
                for (int x = 0; x < 16; x++) row.append('|').append(layer.get(x, y, z));
                OUTPUT.add(row.toString());
            }
        }
        OUTPUT.add("B|" + label + "|" + (uniform ? "-" : HexFormat.of().formatHex(layer.getData())));
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        if (!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2")) throw new AssertionError("wrong reference version");
        if (DataLayer.SIZE != 2048) throw new AssertionError("changed layer size");
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] fields = line.split(" ");
            int slot = fields[0].equals("D") ? -1 : number(fields, 1);
            switch (fields[0]) {
                case "U" -> LAYERS[slot] = new DataLayer(number(fields, 2));
                case "B" -> LAYERS[slot] = new DataLayer(HexFormat.of().parseHex(fields[2]));
                case "S" -> LAYERS[slot].set(number(fields, 2), number(fields, 3), number(fields, 4), number(fields, 5));
                case "F" -> LAYERS[slot].fill(number(fields, 2));
                case "M" -> LAYERS[slot].getData();
                case "C" -> LAYERS[slot] = LAYERS[number(fields, 2)].copy();
                case "D" -> observe(fields);
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

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn observe(layer: &DataLayer, fields: &[&str], output: &mut Vec<String>) {
    let label = fields[1];
    let mut header = format!(
        "D|{label}|{}|{}",
        layer.is_empty(),
        layer.is_definitely_homogeneous()
    );
    for index in 3..fields.len() {
        write!(header, "|{}", layer.is_filled_with(number(fields, index))).unwrap();
    }
    write!(header, "|{}", layer.heap_bytes()).unwrap();
    output.push(header);
    for y in 0..16 {
        for z in 0..16 {
            let mut row = format!("G|{label}|{y}|{z}");
            for x in 0..16 {
                write!(row, "|{}", layer.get(x, y, z).unwrap()).unwrap();
            }
            output.push(row);
        }
    }
    output.push(format!(
        "B|{label}|{}",
        layer.bytes().map(hex).unwrap_or_else(|| "-".to_owned())
    ));
}

fn rust_trace(script: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut layers = [
        DataLayer::uniform(0),
        DataLayer::uniform(0),
        DataLayer::uniform(0),
    ];
    for line in script.lines() {
        let fields: Vec<_> = line.split(' ').collect();
        if fields[0] == "D" {
            observe(&layers[number(&fields, 2) as usize], &fields, &mut output);
            continue;
        }
        let slot = number(&fields, 1) as usize;
        match fields[0] {
            "U" => layers[slot] = DataLayer::uniform(number(&fields, 2)),
            "B" => {
                let bytes: Vec<_> = fields[2]
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                layers[slot] = DataLayer::from_bytes(&bytes, LAYER_BYTES).unwrap();
            }
            "S" => layers[slot]
                .set(
                    number(&fields, 2) as u8,
                    number(&fields, 3) as u8,
                    number(&fields, 4) as u8,
                    number(&fields, 5),
                    LAYER_BYTES,
                )
                .unwrap(),
            "F" => layers[slot].fill(number(&fields, 2)),
            "M" => {
                layers[slot].materialize(LAYER_BYTES).unwrap();
            }
            "C" => {
                layers[slot] = layers[number(&fields, 2) as usize]
                    .try_copy(LAYER_BYTES)
                    .unwrap();
            }
            _ => panic!("unknown fixture: {line}"),
        }
    }
    output
}

fn dump(script: &mut String, label: &str, slot: usize) {
    write!(script, "D {label} {slot}").unwrap();
    for value in DEFAULTS {
        write!(script, " {value}").unwrap();
    }
    script.push('\n');
}

fn fixtures() -> String {
    let mut script = String::new();
    for (index, value) in DEFAULTS.iter().enumerate() {
        writeln!(script, "U 0 {value}").unwrap();
        dump(&mut script, &format!("uniform_{index}"), 0);
        script.push_str("C 1 0\nM 0\n");
        dump(&mut script, &format!("materialized_{index}"), 0);
        dump(&mut script, &format!("uniform_copy_{index}"), 1);
        // Both nibble positions, adjacent rows, and all coordinate extremes.
        for (x, y, z, replacement) in [
            (0, 0, 0, -17),
            (1, 0, 0, 256),
            (15, 0, 0, i32::MIN),
            (0, 0, 1, 31),
            (0, 1, 0, i32::MAX),
            (15, 15, 15, 16),
        ] {
            writeln!(script, "S 0 {x} {y} {z} {replacement}").unwrap();
        }
        dump(&mut script, &format!("mutated_{index}"), 0);
        script.push_str("C 2 0\nS 0 5 6 7 1\nS 2 6 7 8 -1\n");
        dump(&mut script, &format!("original_after_copy_{index}"), 0);
        dump(&mut script, &format!("allocated_copy_{index}"), 2);
        dump(
            &mut script,
            &format!("lazy_copy_still_unchanged_{index}"),
            1,
        );
        writeln!(script, "F 0 {value}").unwrap();
        dump(&mut script, &format!("filled_{index}"), 0);
        script.push_str("M 0\nM 0\n");
        dump(&mut script, &format!("filled_materialized_{index}"), 0);
        // set itself must materialize a lazy non-nibble default correctly.
        script.push_str("S 1 3 4 5 7\n");
        dump(&mut script, &format!("set_materialized_{index}"), 1);
    }

    // Every input byte value, with a pattern that also changes between y slabs.
    let pattern: Vec<u8> = (0..LAYER_BYTES)
        .map(|index| (index * 73 + index / 128 * 19) as u8)
        .collect();
    writeln!(script, "B 0 {}", hex(&pattern)).unwrap();
    dump(&mut script, "from_pattern", 0);
    script.push_str("C 1 0\n");
    // Revisit every coordinate through different axis orders. Observations
    // after each slab expose writes leaking into unrelated coordinates.
    for pass in 0..2 {
        for slab in 0..16 {
            for row in 0..16 {
                for column in 0..16 {
                    let (x, y, z) = if pass == 0 {
                        (column, slab, row)
                    } else {
                        (slab, row, column)
                    };
                    let value = if pass == 0 {
                        (x * 3 + y * 5 + z * 7) - 128
                    } else {
                        DEFAULTS[(x + y * 3 + z * 7) as usize % DEFAULTS.len()]
                    };
                    writeln!(script, "S 0 {x} {y} {z} {value}").unwrap();
                }
            }
            dump(&mut script, &format!("all_coordinates_{pass}_{slab}"), 0);
        }
    }
    dump(&mut script, "pattern_copy_unchanged", 1);
    for byte in [0, 0x11, 0xff] {
        writeln!(script, "B 0 {}", hex(&[byte; LAYER_BYTES])).unwrap();
        dump(&mut script, &format!("allocated_uniform_{byte}"), 0);
        script.push_str("C 1 0\n");
        dump(&mut script, &format!("allocated_uniform_copy_{byte}"), 1);
        script.push_str("F 0 0\n");
        dump(&mut script, &format!("fill_zero_{byte}"), 0);
        dump(&mut script, &format!("copy_after_fill_zero_{byte}"), 1);
    }
    script
}

#[test]
#[ignore = "requires Java25 and locked jars via ARROW_VANILLA_SERVER_JAR or ARROW_MC_JAVA_REFERENCE_ROOT"]
fn layer_values_storage_transitions_and_copies_match_actual_vanilla() {
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
        "arrow-mc-light-layer-oracle-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("LightLayerOracle.java");
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
        "Compared {} DataLayer metadata/coordinate/raw-byte rows against actual Vanilla 26.3-pre-2",
        actual.len()
    );
}
