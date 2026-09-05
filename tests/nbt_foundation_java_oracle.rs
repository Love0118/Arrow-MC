//! Actual locked server API comparisons. Run explicitly with
//! `ARROW_MC_JAVA_REFERENCE_ROOT=<Decompile>` and
//! `cargo test --test nbt_foundation_java_oracle -- --ignored --nocapture`.
//! Test trees and probes are authored here; no server implementation is copied.

use arrow_mc::nbt::{
    Compound, NbtString, Tag,
    predicate::{CompareBudget, CompareLimits},
};
use std::{env, fs, path::Path, process::Command, time::SystemTime};

const JAVA: &str = r#"
import java.io.*;
import java.nio.file.*;
import net.minecraft.nbt.*;

@SuppressWarnings("removal")
class NbtFoundationOracle {
    static Tag readTag(DataInputStream input) throws Exception {
        int id = input.readUnsignedByte();
        return switch (id) {
            case 255 -> null;
            case 0 -> EndTag.INSTANCE;
            case 1 -> ByteTag.valueOf(input.readByte());
            case 2 -> ShortTag.valueOf(input.readShort());
            case 3 -> IntTag.valueOf(input.readInt());
            case 4 -> LongTag.valueOf(input.readLong());
            case 5 -> new FloatTag(Float.intBitsToFloat(input.readInt()));
            case 6 -> new DoubleTag(Double.longBitsToDouble(input.readLong()));
            case 7 -> { byte[] a = new byte[input.readInt()]; input.readFully(a); yield new ByteArrayTag(a); }
            case 8 -> StringTag.valueOf(readString(input));
            case 9 -> {
                int size = input.readInt();
                ListTag list = new ListTag();
                for (int i = 0; i < size; i++) list.add(readTag(input));
                yield list;
            }
            case 10 -> {
                int size = input.readInt();
                CompoundTag compound = new CompoundTag();
                for (int i = 0; i < size; i++) compound.put(readString(input), readTag(input));
                yield compound;
            }
            case 11 -> {
                int[] a = new int[input.readInt()];
                for (int i = 0; i < a.length; i++) a[i] = input.readInt();
                yield new IntArrayTag(a);
            }
            case 12 -> {
                long[] a = new long[input.readInt()];
                for (int i = 0; i < a.length; i++) a[i] = input.readLong();
                yield new LongArrayTag(a);
            }
            default -> throw new IllegalArgumentException("tag " + id);
        };
    }

    static String readString(DataInputStream input) throws Exception {
        char[] chars = new char[input.readInt()];
        for (int i = 0; i < chars.length; i++) chars[i] = input.readChar();
        return new String(chars);
    }

    public static void main(String[] args) throws Exception {
        try (var input = new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[1]))));
             var output = new DataOutputStream(new BufferedOutputStream(System.out))) {
            int count = input.readInt();
            for (int i = 0; i < count; i++) {
                Tag left = readTag(input);
                if (args[0].equals("numeric")) {
                    NumericTag numeric = (NumericTag) left;
                    output.writeByte(numeric.byteValue());
                    output.writeShort(numeric.shortValue());
                    output.writeInt(numeric.intValue());
                    output.writeLong(numeric.longValue());
                    output.writeInt(Float.floatToIntBits(numeric.floatValue()));
                    output.writeLong(Double.doubleToLongBits(numeric.doubleValue()));
                } else {
                    Tag right = readTag(input);
                    output.writeBoolean(NbtUtils.compareNbt(left, right, false));
                    output.writeBoolean(NbtUtils.compareNbt(left, right, true));
                    output.writeBoolean(left == null ? right == null : left.equals(right));
                }
            }
            if (input.read() != -1) throw new AssertionError("unused input");
        }
    }
}
"#;

fn run_oracle(mode: &str, input: &[u8]) -> Vec<u8> {
    let reference = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
        .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to Decompile");
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
        "arrow-nbt-foundation-{mode}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("NbtFoundationOracle.java");
    let cases = directory.join("cases.bin");
    fs::write(&source, JAVA).unwrap();
    fs::write(&cases, input).unwrap();
    let execution = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(mode)
        .arg(cases)
        .output();
    fs::remove_dir_all(&directory).unwrap();
    let execution = execution.expect("Java must be on PATH");
    assert!(
        execution.status.success(),
        "{}",
        String::from_utf8_lossy(&execution.stderr)
    );
    execution.stdout
}

fn string_bytes(value: &NbtString, output: &mut Vec<u8>) {
    output.extend_from_slice(&(value.as_utf16().len() as u32).to_be_bytes());
    for unit in value.as_utf16() {
        output.extend_from_slice(&unit.to_be_bytes());
    }
}

// A test-only direct-constructor format preserves signed zero and NaN payloads.
// Vanilla's ordinary binary reader/valueOf zero normalization is not part of
// NumericTag conversions or compareNbt on already constructed tags.
fn tag_bytes(tag: &Tag, output: &mut Vec<u8>) {
    output.push(tag.id());
    match tag {
        Tag::End => {}
        Tag::Byte(value) => output.push(*value as u8),
        Tag::Short(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Int(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Long(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Float(value) => output.extend_from_slice(&value.to_bits().to_be_bytes()),
        Tag::Double(value) => output.extend_from_slice(&value.to_bits().to_be_bytes()),
        Tag::String(value) => string_bytes(value, output),
        Tag::List(values) => {
            output.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                tag_bytes(value, output);
            }
        }
        Tag::Compound(values) => {
            output.extend_from_slice(&(values.entries().len() as u32).to_be_bytes());
            for entry in values.entries() {
                string_bytes(&entry.name, output);
                tag_bytes(&entry.value, output);
            }
        }
        Tag::ByteArray(values) => {
            output.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                output.push(*value as u8);
            }
        }
        Tag::IntArray(values) => {
            output.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
        Tag::LongArray(values) => {
            output.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
}

fn numeric_output(tag: &Tag) -> Vec<u8> {
    let mut output = Vec::new();
    output.push(tag.as_byte().unwrap() as u8);
    output.extend_from_slice(&tag.as_short().unwrap().to_be_bytes());
    output.extend_from_slice(&tag.as_int().unwrap().to_be_bytes());
    output.extend_from_slice(&tag.as_long().unwrap().to_be_bytes());
    let float = tag.as_float().unwrap();
    let double = tag.as_double().unwrap();
    // Java's public numeric equality/wire representations canonicalize NaNs.
    // Cross-width payload propagation is not portable, and is not asserted.
    let float_bits = if float.is_nan() {
        0x7fc0_0000
    } else {
        float.to_bits()
    };
    let double_bits = if double.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        double.to_bits()
    };
    output.extend_from_slice(&float_bits.to_be_bytes());
    output.extend_from_slice(&double_bits.to_be_bytes());
    output
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with locked server jars"]
fn numeric_conversions_match_actual_vanilla() {
    let mut cases = Vec::new();
    cases.extend((i8::MIN..=i8::MAX).map(Tag::Byte));
    cases.extend((i16::MIN..=i16::MAX).map(Tag::Short));
    for bit in 0..64 {
        let pivot = 1_i64.wrapping_shl(bit);
        for integer in [
            pivot.wrapping_sub(1),
            pivot,
            pivot.wrapping_add(1),
            pivot.wrapping_neg(),
        ] {
            cases.push(Tag::Int(integer as i32));
            cases.push(Tag::Long(integer));
            for float_bits in [
                (integer as f32).to_bits().wrapping_sub(1),
                (integer as f32).to_bits(),
                (integer as f32).to_bits().wrapping_add(1),
            ] {
                cases.push(Tag::Float(f32::from_bits(float_bits)));
            }
            for double_bits in [
                (integer as f64).to_bits().wrapping_sub(1),
                (integer as f64).to_bits(),
                (integer as f64).to_bits().wrapping_add(1),
            ] {
                cases.push(Tag::Double(f64::from_bits(double_bits)));
            }
        }
    }
    for bits in [
        0,
        0x8000_0000,
        1,
        0x8000_0001,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0x7f80_0001,
        0xff80_0001,
        0xcf00_0001,
    ] {
        cases.push(Tag::Float(f32::from_bits(bits)));
    }
    for bits in [
        0,
        0x8000_0000_0000_0000,
        1,
        0x8000_0000_0000_0001,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0x7ff0_0000_0000_0001,
        0xfff0_0000_0000_0001,
        0xc1e0_0000_0000_0001,
        0x3690_0000_0000_0000,
        0x3690_0000_0000_0001,
        0x47ef_ffff_f000_0000,
        0x47ef_ffff_efff_ffff,
    ] {
        cases.push(Tag::Double(f64::from_bits(bits)));
    }
    cases.push(Tag::Long(4_611_686_293_305_294_849));
    cases.push(Tag::Long(-4_611_686_293_305_294_849));
    let mut random = 0x6830_2ad5_4b87_3315_u64;
    for _ in 0..8192 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        cases.extend([
            Tag::Int(random as i32),
            Tag::Long(random as i64),
            Tag::Float(f32::from_bits(random as u32)),
            Tag::Double(f64::from_bits(random)),
        ]);
    }
    let mut input = (cases.len() as u32).to_be_bytes().to_vec();
    for tag in &cases {
        tag_bytes(tag, &mut input);
    }
    let output = run_oracle("numeric", &input);
    assert_eq!(output.len(), cases.len() * 27);
    for (tag, expected) in cases.iter().zip(output.chunks_exact(27)) {
        assert_eq!(numeric_output(tag), expected, "{tag:?}");
    }
    eprintln!(
        "Actual Vanilla: {} NumericTags, {} primitive conversions matched",
        cases.len(),
        cases.len() * 6
    );
}

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    let mut compound = Compound::new();
    for (key, value) in entries {
        compound.insert(key.into(), value).unwrap();
    }
    Tag::Compound(compound)
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with locked server jars"]
fn predicate_and_exact_equality_match_actual_vanilla() {
    let mut cases = vec![
        Tag::End,
        Tag::Byte(1),
        Tag::Short(1),
        Tag::Int(1),
        Tag::Long(1),
        Tag::Int(2),
        Tag::Float(1.0),
        Tag::Double(1.0),
        Tag::Float(0.0),
        Tag::Float(-0.0),
        Tag::Double(0.0),
        Tag::Double(-0.0),
        Tag::Float(f32::NAN),
        Tag::Float(f32::from_bits(0xff80_0001)),
        Tag::Double(f64::NAN),
        Tag::Double(f64::from_bits(0xfff0_0000_0000_0001)),
        Tag::String("".into()),
        Tag::String("x".into()),
        Tag::String("xy".into()),
        Tag::String(NbtString::from_utf16(vec![0xd800])),
        Tag::ByteArray(vec![]),
        Tag::ByteArray(vec![1]),
        Tag::ByteArray(vec![1, 2]),
        Tag::ByteArray(vec![2, 1]),
        Tag::IntArray(vec![]),
        Tag::IntArray(vec![1]),
        Tag::IntArray(vec![1, 2]),
        Tag::IntArray(vec![2, 1]),
        Tag::LongArray(vec![]),
        Tag::LongArray(vec![1]),
        Tag::LongArray(vec![1, 2]),
        Tag::LongArray(vec![2, 1]),
        Tag::List(vec![]),
        compound(vec![]),
    ];
    for tag in cases.clone().into_iter().skip(1) {
        cases.push(Tag::List(vec![tag.clone()]));
        cases.push(Tag::List(vec![tag.clone(), tag.clone()]));
        cases.push(Tag::List(vec![Tag::Int(2), tag.clone()]));
        cases.push(compound(vec![("a", tag.clone())]));
        cases.push(compound(vec![("a", tag), ("b", Tag::Int(2))]));
    }
    let partial_compound = compound(vec![("x", Tag::Int(1))]);
    let extended_compound = compound(vec![("x", Tag::Int(1)), ("y", Tag::Int(2))]);
    for leaf in [partial_compound, extended_compound] {
        let nested = Tag::List(vec![Tag::List(vec![leaf])]);
        cases.push(nested.clone());
        cases.push(compound(vec![("a", nested)]));
    }
    let mut input = (((cases.len() + 1) * (cases.len() + 1)) as u32)
        .to_be_bytes()
        .to_vec();
    let values: Vec<_> = std::iter::once(None)
        .chain(cases.iter().map(Some))
        .collect();
    for &expected in &values {
        for &actual in &values {
            if let Some(tag) = expected {
                tag_bytes(tag, &mut input);
            } else {
                input.push(255);
            }
            if let Some(tag) = actual {
                tag_bytes(tag, &mut input);
            } else {
                input.push(255);
            }
        }
    }
    let output = run_oracle("predicate", &input);
    assert_eq!(output.len(), values.len() * values.len() * 3);
    let mut observations = output.chunks_exact(3);
    for &expected in &values {
        for &actual in &values {
            let java = observations.next().unwrap();
            let mut budget = CompareBudget::new(CompareLimits::default());
            assert_eq!(
                budget.compare(expected, actual, false).unwrap(),
                java[0] != 0,
                "strict expected={expected:?} actual={actual:?}"
            );
            assert_eq!(
                budget.compare(expected, actual, true).unwrap(),
                java[1] != 0,
                "partial expected={expected:?} actual={actual:?}"
            );
            let exact = match (expected, actual) {
                (Some(expected), Some(actual)) => budget.equal(expected, actual).unwrap(),
                (None, None) => true,
                _ => false,
            };
            assert_eq!(
                exact,
                java[2] != 0,
                "exact expected={expected:?} actual={actual:?}"
            );
        }
    }
    eprintln!(
        "Actual Vanilla: {} nullable pairs, {} strict/partial/equality comparisons matched",
        values.len() * values.len(),
        values.len() * values.len() * 3
    );
}
