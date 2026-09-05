use arrow_mc::server::packet::{PacketReader, PacketWriter};
use arrow_mc::wire::write_varint;
use std::{env, fs, io::Read, path::Path, process::Command, time::SystemTime};

// Independently written API driver: no reference method bodies or generated data.
const JAVA: &str = r#"
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.UUID;
import io.netty.buffer.*;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.network.Utf8String;
import net.minecraft.resources.Identifier;

class PacketFieldsOracle {
    public static void main(String[] args) throws Exception {
        try (var input = new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[0]))));
             var output = new DataOutputStream(new BufferedOutputStream(System.out))) {
            int count = input.readInt();
            for (int index = 0; index < count; index++) {
                int operation = input.readUnsignedByte();
                int limit = input.readInt();
                byte[] bytes = input.readNBytes(input.readInt());
                ByteBuf buffer = Unpooled.buffer();
                byte[] result;
                try {
                    switch (operation) {
                        case 0 -> {
                            buffer.writeBytes(bytes);
                            result = Utf8String.read(buffer, limit).getBytes(StandardCharsets.UTF_8);
                        }
                        case 1 -> {
                            Utf8String.write(buffer, new String(bytes, StandardCharsets.UTF_8), limit);
                            result = ByteBufUtil.getBytes(buffer);
                        }
                        case 2 -> {
                            buffer.writeBytes(bytes);
                            result = Identifier.parse(Utf8String.read(buffer, 32767)).toString().getBytes(StandardCharsets.UTF_8);
                        }
                        case 3 -> {
                            Identifier value = Identifier.parse(new String(bytes, StandardCharsets.UTF_8));
                            Identifier.STREAM_CODEC.encode(buffer, value);
                            result = ByteBufUtil.getBytes(buffer);
                        }
                        case 4 -> {
                            var values = new DataInputStream(new ByteArrayInputStream(bytes));
                            var fields = new FriendlyByteBuf(buffer);
                            fields.writeBoolean(values.readBoolean());
                            fields.writeByte(values.readByte());
                            fields.writeShort(values.readShort());
                            fields.writeInt(values.readInt());
                            fields.writeLong(values.readLong());
                            fields.writeFloat(Float.intBitsToFloat(values.readInt()));
                            fields.writeDouble(Double.longBitsToDouble(values.readLong()));
                            fields.writeUUID(new UUID(values.readLong(), values.readLong()));
                            result = ByteBufUtil.getBytes(buffer);
                        }
                        default -> throw new AssertionError(operation);
                    }
                } catch (RuntimeException rejected) {
                    result = null;
                } finally {
                    buffer.release();
                }
                output.writeInt(result == null ? -1 : result.length);
                if (result != null) output.write(result);
            }
        }
    }
}
"#;

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum Operation {
    ReadUtf,
    WriteUtf,
    ReadIdentifier,
    WriteIdentifier,
    WriteScalars,
}

struct Case {
    operation: Operation,
    limit: usize,
    input: Vec<u8>,
}

fn string_wire(bytes: &[u8]) -> Vec<u8> {
    let mut prefix = [0; 5];
    let length = write_varint(i32::try_from(bytes.len()).unwrap(), &mut prefix).unwrap();
    let mut wire = prefix[..length].to_vec();
    wire.extend_from_slice(bytes);
    wire
}

fn read_cases(cases: &mut Vec<Case>, bytes: &[u8], limits: &[usize]) {
    for &limit in limits {
        cases.push(Case {
            operation: Operation::ReadUtf,
            limit,
            input: string_wire(bytes),
        });
    }
}

fn utf_read_cases(cases: &mut Vec<Case>) {
    read_cases(cases, &[], &[0, 1, 2]);
    for first in 0..=255 {
        read_cases(cases, &[first], &[0, 1, 2]);
        for second in 0..=255 {
            read_cases(cases, &[first, second], &[0, 1, 2]);
        }
    }
    let boundaries = [
        0, 0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xdf, 0xed, 0xff,
    ];
    for first in [0xe0, 0xe1, 0xec, 0xed, 0xee, 0xef] {
        for second in 0..=255 {
            for third in boundaries {
                read_cases(cases, &[first, second, third], &[1, 2, 3]);
            }
        }
    }
    for first in [0xf0, 0xf1, 0xf3, 0xf4, 0xf5] {
        for second in boundaries {
            for third in boundaries {
                read_cases(cases, &[first, second, third], &[1, 2, 3]);
                for fourth in [0, 0x7f, 0x80, 0xbf, 0xc0, 0xff] {
                    read_cases(cases, &[first, second, third, fourth], &[1, 2, 3, 4]);
                }
            }
        }
    }
    for fragment in [
        &[0xed, 0xa0][..],
        &[0xed, 0xa0, 0x80],
        &[0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80],
        &[0xe0, 0x80, 0xaf],
        &[0xf0, 0x90, 0x80],
        &[0x80, 0xc0, 0xc2],
    ] {
        for prefix in ["", "A", "¢", "가", "😀"] {
            for suffix in ["", "Z", "¢", "가", "😀"] {
                let mut bytes = prefix.as_bytes().to_vec();
                bytes.extend_from_slice(fragment);
                bytes.extend_from_slice(suffix.as_bytes());
                read_cases(cases, &bytes, &[0, 1, 2, 3, 4, 5, 6, 12]);
            }
        }
    }
    let mut state = 0x8f5c_a937_12b4_d60eu64;
    for index in 0..8192 {
        let length = index % 65;
        let bytes: Vec<_> = (0..length)
            .map(|_| (random(&mut state) >> 56) as u8)
            .collect();
        read_cases(cases, &bytes, &[length / 3, length / 2, length]);
    }
    // Malformed prefixes and truncated payloads exercise rejection independently
    // of replacement decoding. Failure cursor behavior is deliberately excluded.
    for wire in [
        vec![],
        vec![0x80],
        vec![0x80; 6],
        vec![0xff, 0xff, 0xff, 0xff, 0x0f],
        vec![3, b'a'],
        vec![0x80, 0],
        vec![0x81, 0, b'a'],
    ] {
        for limit in [0, 1, 32767] {
            cases.push(Case {
                operation: Operation::ReadUtf,
                limit,
                input: wire.clone(),
            });
        }
    }
}

fn random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
}

fn write_utf_cases(cases: &mut Vec<Case>, value: &str) {
    let length = value.encode_utf16().count();
    let mut limits = vec![0, length.saturating_sub(1), length, length + 1];
    limits.sort_unstable();
    limits.dedup();
    for limit in limits {
        cases.push(Case {
            operation: Operation::WriteUtf,
            limit,
            input: value.as_bytes().to_vec(),
        });
    }
}

fn utf_write_cases(cases: &mut Vec<Case>) {
    write_utf_cases(cases, "");
    for value in [
        "\0",
        "hello",
        "\u{7f}\u{80}\u{7ff}\u{800}",
        "가😀\0¢",
        "\u{d7ff}\u{e000}\u{ffff}\u{10000}\u{10ffff}",
    ] {
        write_utf_cases(cases, value);
    }
    for length in [
        1, 2, 42, 43, 63, 64, 127, 128, 129, 5461, 5462, 8191, 8192, 16383, 16384, 32766, 32767,
        32768,
    ] {
        for character in ["x", "¢", "가", "😀"] {
            write_utf_cases(cases, &character.repeat(length));
        }
    }
    let mut state = 0xadc1_83e7_5219_6bf0u64;
    for index in 0..1024 {
        let value: String = (0..index % 33)
            .filter_map(|_| char::from_u32((random(&mut state) % 0x11_0000) as u32))
            .collect();
        write_utf_cases(cases, &value);
    }
}

fn identifier_cases(cases: &mut Vec<Case>) {
    let mut values: Vec<String> = [
        "",
        ":",
        "stone",
        ":stone",
        "minecraft:",
        "custom:",
        ".:a",
        "..:a",
        "...:a",
        "minecraft:..",
        "a:b:c",
        "namespace:path/./../x",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for character in (0..=255).filter_map(char::from_u32).chain([
        '가',
        '😀',
        '\u{d7ff}',
        '\u{e000}',
        '\u{ffff}',
        '\u{10ffff}',
    ]) {
        values.extend([
            character.to_string(),
            format!("a{character}z:path"),
            format!("namespace:a{character}z"),
            format!(":{character}"),
        ]);
    }
    for length in [32756, 32757, 32758, 32766, 32767, 32768] {
        let path = "x".repeat(length);
        values.extend([
            path.clone(),
            format!(":{path}"),
            format!("a:{path}"),
            format!("minecraft:{path}"),
        ]);
    }
    for value in values {
        cases.push(Case {
            operation: Operation::ReadIdentifier,
            limit: 32767,
            input: string_wire(value.as_bytes()),
        });
        cases.push(Case {
            operation: Operation::WriteIdentifier,
            limit: 32767,
            input: value.into_bytes(),
        });
    }
}

fn scalar_cases(cases: &mut Vec<Case>) {
    for (float, double) in [
        (0u32, 0u64),
        (0x8000_0000, 0x8000_0000_0000_0000),
        (1, 1),
        (0x7f80_0000, 0xfff0_0000_0000_0000),
        (0x7fc0_4321, 0x7ff8_1234_5678_9abc),
        (0xffc0_abcd, 0xfff9_abcd_0123_4567),
    ] {
        let mut input = vec![1, 0x80];
        input.extend_from_slice(&i16::MIN.to_be_bytes());
        input.extend_from_slice(&(-123_456_789i32).to_be_bytes());
        input.extend_from_slice(&i64::MIN.to_be_bytes());
        input.extend_from_slice(&float.to_be_bytes());
        input.extend_from_slice(&double.to_be_bytes());
        input.extend_from_slice(&0x0011_2233_4455_6677_8899_aabb_ccdd_eeffu128.to_be_bytes());
        cases.push(Case {
            operation: Operation::WriteScalars,
            limit: 0,
            input,
        });
    }
}

fn rust_result(case: &Case) -> Option<Vec<u8>> {
    // Resource admission is tested separately; this oracle isolates wire parity.
    let mut writer = PacketWriter::new(1 << 20);
    match case.operation {
        Operation::ReadUtf => PacketReader::new(&case.input)
            .utf(case.limit)
            .ok()
            .map(String::into_bytes),
        Operation::ReadIdentifier => PacketReader::new(&case.input)
            .identifier()
            .ok()
            .map(String::into_bytes),
        Operation::WriteUtf => {
            writer
                .utf(std::str::from_utf8(&case.input).unwrap(), case.limit)
                .ok()?;
            Some(writer.into_bytes())
        }
        Operation::WriteIdentifier => {
            writer
                .identifier(std::str::from_utf8(&case.input).unwrap())
                .ok()?;
            Some(writer.into_bytes())
        }
        Operation::WriteScalars => {
            let bytes = &case.input;
            writer.boolean(bytes[0] != 0).unwrap();
            writer.byte(bytes[1] as i8).unwrap();
            writer
                .short(i16::from_be_bytes(bytes[2..4].try_into().unwrap()))
                .unwrap();
            writer
                .int(i32::from_be_bytes(bytes[4..8].try_into().unwrap()))
                .unwrap();
            writer
                .long(i64::from_be_bytes(bytes[8..16].try_into().unwrap()))
                .unwrap();
            writer
                .float(f32::from_bits(u32::from_be_bytes(
                    bytes[16..20].try_into().unwrap(),
                )))
                .unwrap();
            writer
                .double(f64::from_bits(u64::from_be_bytes(
                    bytes[20..28].try_into().unwrap(),
                )))
                .unwrap();
            writer.uuid(bytes[28..44].try_into().unwrap()).unwrap();
            Some(writer.into_bytes())
        }
    }
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with locked Vanilla jars"]
fn matches_locked_java_packet_strings_identifiers_and_scalars() {
    let reference = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
        .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to the sibling Decompile directory");
    let artifacts = Path::new(&reference).join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let mut cases = Vec::new();
    utf_read_cases(&mut cases);
    utf_write_cases(&mut cases);
    identifier_cases(&mut cases);
    scalar_cases(&mut cases);
    let mut input = u32::try_from(cases.len()).unwrap().to_be_bytes().to_vec();
    for case in &cases {
        input.push(case.operation as u8);
        input.extend_from_slice(&i32::try_from(case.limit).unwrap().to_be_bytes());
        input.extend_from_slice(&i32::try_from(case.input.len()).unwrap().to_be_bytes());
        input.extend_from_slice(&case.input);
    }
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-packet-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("PacketFieldsOracle.java");
    let input_path = directory.join("input.bin");
    fs::write(&source, JAVA).unwrap();
    fs::write(&input_path, input).unwrap();
    let execution = Command::new("java")
        .arg("-Xmx512m")
        .arg("--class-path")
        .arg(classpath)
        .arg(&source)
        .arg(&input_path)
        .output();
    fs::remove_file(source).unwrap();
    fs::remove_file(input_path).unwrap();
    fs::remove_dir(&directory).unwrap();
    let execution = execution.expect("Java25 must be installed and available on PATH");
    assert!(
        execution.status.success(),
        "Java oracle failed: {}",
        String::from_utf8_lossy(&execution.stderr)
    );
    let mut output = execution.stdout.as_slice();
    for (index, case) in cases.iter().enumerate() {
        let mut length = [0; 4];
        output
            .read_exact(&mut length)
            .expect("missing Java oracle result length");
        let length = i32::from_be_bytes(length);
        let actual = if length == -1 {
            None
        } else {
            assert!(length >= 0, "invalid Java oracle result length {length}");
            let mut bytes = vec![0; length as usize];
            output
                .read_exact(&mut bytes)
                .expect("truncated Java oracle result");
            Some(bytes)
        };
        assert_eq!(
            actual,
            rust_result(case),
            "Java disagreement at case {index}: {:?}, limit {}, input length {}, prefix {:02x?}",
            case.operation,
            case.limit,
            case.input.len(),
            &case.input[..case.input.len().min(24)]
        );
    }
    assert!(output.is_empty(), "unexpected trailing Java oracle results");
    eprintln!(
        "Compared {} packet field cases with actual Vanilla 26.3-pre-2 classes",
        cases.len()
    );
}
