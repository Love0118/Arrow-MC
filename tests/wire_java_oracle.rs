//! Opt-in comparison against the actual pinned Vanilla server classes.
//!
//! Set `ARROW_MC_JAVA_REFERENCE_ROOT` to the sibling `Decompile` directory, then
//! run `cargo test --test wire_java_oracle -- --ignored`. Java must support the
//! locked server's class version. No server code or jars are stored in this repo.

use arrow_mc::wire::{
    DecodeError, read_varint, read_varlong, varint_len, varlong_len, write_varint, write_varlong,
};
use std::{env, fmt::Write, fs, process::Command, time::SystemTime};

const ORACLE: &str = r#"
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import net.minecraft.network.VarInt;
import net.minecraft.network.VarLong;

class WireOracle {
    public static void main(String[] args) throws Exception {
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String data = line.substring(2);
            char operation = line.charAt(0);
            ByteBuf buffer = Unpooled.buffer();
            try {
                if (operation == 'i' || operation == 'l') {
                    int length;
                    if (operation == 'i') {
                        int value = Integer.parseInt(data);
                        length = VarInt.getByteSize(value);
                        VarInt.write(buffer, value);
                    } else {
                        long value = Long.parseLong(data);
                        length = VarLong.getByteSize(value);
                        VarLong.write(buffer, value);
                    }
                    byte[] bytes = new byte[buffer.readableBytes()];
                    buffer.readBytes(bytes);
                    System.out.println(HexFormat.of().formatHex(bytes) + ":" + length);
                } else {
                    buffer.writeBytes(HexFormat.of().parseHex(data));
                    try {
                        long value = operation == 'I' ? VarInt.read(buffer) : VarLong.read(buffer);
                        System.out.println("ok:" + value + ":" + buffer.readerIndex());
                    } catch (IndexOutOfBoundsException error) {
                        System.out.println("incomplete");
                    } catch (RuntimeException error) {
                        if (!error.getMessage().equals("VarInt too big") &&
                            !error.getMessage().equals("VarLong too big")) throw error;
                        System.out.println("too_long");
                    }
                }
            } finally {
                buffer.release();
            }
        }
    }
}
"#;

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn add_encode_case(cases: &mut Vec<(String, String)>, value: i64) {
    let mut output = [0; 10];
    let length = write_varint(value as i32, &mut output).unwrap();
    cases.push((
        format!("i:{}", value as i32),
        format!("{}:{}", hex(&output[..length]), varint_len(value as i32)),
    ));
    let length = write_varlong(value, &mut output).unwrap();
    cases.push((
        format!("l:{value}"),
        format!("{}:{}", hex(&output[..length]), varlong_len(value)),
    ));
}

fn add_decode_case(cases: &mut Vec<(String, String)>, bytes: &[u8]) {
    let expected = match read_varint(bytes) {
        Ok((value, length)) => format!("ok:{value}:{length}"),
        Err(DecodeError::Incomplete) => "incomplete".into(),
        Err(DecodeError::TooLong) => "too_long".into(),
    };
    cases.push((format!("I:{}", hex(bytes)), expected));
    let expected = match read_varlong(bytes) {
        Ok((value, length)) => format!("ok:{value}:{length}"),
        Err(DecodeError::Incomplete) => "incomplete".into(),
        Err(DecodeError::TooLong) => "too_long".into(),
    };
    cases.push((format!("L:{}", hex(bytes)), expected));
}

#[test]
#[ignore = "requires Java and ARROW_MC_JAVA_REFERENCE_ROOT with locked Vanilla jars"]
fn matches_locked_java_varints() {
    let reference_root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
        .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to the sibling Decompile directory");
    let artifacts = std::path::Path::new(&reference_root).join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();

    let mut cases = Vec::new();
    for value in [
        0,
        1,
        -1,
        -2,
        i64::MIN,
        i64::MAX,
        i32::MIN as i64,
        i32::MAX as i64,
    ] {
        add_encode_case(&mut cases, value);
    }
    for shift in 0..63 {
        let boundary = 1_i64 << shift;
        for value in [boundary - 1, boundary, boundary + 1, -boundary] {
            add_encode_case(&mut cases, value);
        }
    }
    let mut random = 0x48fa_27e9_a57f_e328_u64;
    for _ in 0..2_048 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        add_encode_case(&mut cases, random as i64);
        add_decode_case(&mut cases, &random.to_le_bytes());
    }
    for length in 0..=12 {
        add_decode_case(&mut cases, &vec![0x80; length]);
        add_decode_case(&mut cases, &vec![0xff; length]);
        for terminal in 0..=127 {
            let mut bytes = vec![0x80; length];
            bytes.push(terminal);
            add_decode_case(&mut cases, &bytes);
            // A valid integer must stop before unrelated trailing input.
            bytes.extend_from_slice(&[0xff; 12]);
            add_decode_case(&mut cases, &bytes);
        }
    }

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-wire-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("WireOracle.java");
    let input = directory.join("input.txt");
    fs::write(&source, ORACLE).unwrap();
    let input_text = cases
        .iter()
        .map(|(input, _)| input.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&input, input_text).unwrap();
    let execution = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(input)
        .output();
    fs::remove_dir_all(&directory).unwrap();
    let execution = execution.expect("Java must be installed and available on PATH");
    assert!(
        execution.status.success(),
        "Java oracle failed: {}",
        String::from_utf8_lossy(&execution.stderr)
    );
    let output = String::from_utf8(execution.stdout).unwrap();
    let results: Vec<_> = output.lines().collect();
    assert_eq!(results.len(), cases.len(), "oracle response count");
    for ((input, expected), actual) in cases.iter().zip(results) {
        assert_eq!(actual, expected, "Java disagreement for {input}");
    }
    eprintln!(
        "Compared {} cases with actual Vanilla 26.3-pre-2 VarInt/VarLong classes",
        cases.len()
    );
}
