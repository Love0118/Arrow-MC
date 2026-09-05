//! Actual-JAR bidirectional compression compatibility, opt in with
//! ARROW_MC_JAVA_REFERENCE_ROOT and cargo test --test server_compression_java_oracle -- --ignored.

use arrow_mc::server::compression::{
    CompressionLimits, CompressionScratch, CompressionState, MAX_FRAME_BODY_BYTES,
    MAX_UNCOMPRESSED_BYTES,
};
use std::{
    env, fs,
    io::{Cursor, Read},
    path::Path,
    process::Command,
    time::SystemTime,
};

const JAVA: &str = r#"
import java.io.*;
import java.nio.file.*;
import io.netty.buffer.*;
import io.netty.channel.embedded.EmbeddedChannel;
import net.minecraft.network.*;

class CompressionCrossOracle {
    static void blob(DataOutputStream out, byte[] bytes) throws Exception {
        if (bytes == null) out.writeInt(-1);
        else { out.writeInt(bytes.length); out.write(bytes); }
    }
    static byte[] bytes(ByteBuf value) {
        if (value == null) return null;
        try { byte[] result = new byte[value.readableBytes()]; value.readBytes(result); return result; }
        finally { value.release(); }
    }
    static byte[] encode(int threshold, byte[] payload) {
        EmbeddedChannel channel = threshold < 0
            ? new EmbeddedChannel(new Varint21LengthFieldPrepender())
            : new EmbeddedChannel(new Varint21LengthFieldPrepender(), new CompressionEncoder(threshold));
        try { channel.writeOutbound(Unpooled.wrappedBuffer(payload)); return bytes(channel.readOutbound()); }
        catch (Exception error) { return null; }
        finally { try { channel.finishAndReleaseAll(); } catch (Exception ignored) {} }
    }
    static byte[] decode(int threshold, byte[] frame) {
        EmbeddedChannel channel = threshold < 0
            ? new EmbeddedChannel(new Varint21FrameDecoder(null))
            : new EmbeddedChannel(new Varint21FrameDecoder(null), new CompressionDecoder(threshold, true));
        try { channel.writeInbound(Unpooled.wrappedBuffer(frame)); return bytes(channel.readInbound()); }
        catch (Exception error) { return null; }
        finally { try { channel.finishAndReleaseAll(); } catch (Exception ignored) {} }
    }
    public static void main(String[] args) throws Exception {
        try (var in = new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[0]))));
             var out = new DataOutputStream(new BufferedOutputStream(System.out))) {
            int count = in.readInt();
            for (int i = 0; i < count; i++) {
                int threshold = in.readInt();
                byte[] payload = new byte[in.readInt()]; in.readFully(payload);
                int frameLength = in.readInt();
                byte[] frame = frameLength < 0 ? null : new byte[frameLength];
                if (frame != null) in.readFully(frame);
                blob(out, encode(threshold, payload));
                blob(out, frame == null ? null : decode(threshold, frame));
            }
        }
    }
}
"#;

fn random(length: usize) -> Vec<u8> {
    let mut state = 0x19ca_8701_3432_1169u64;
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 56) as u8
        })
        .collect()
}

fn blob(cursor: &mut Cursor<Vec<u8>>) -> Option<Vec<u8>> {
    let mut length = [0; 4];
    cursor.read_exact(&mut length).unwrap();
    let length = i32::from_be_bytes(length);
    if length < 0 {
        return None;
    }
    let mut value = vec![0; length as usize];
    cursor.read_exact(&mut value).unwrap();
    Some(value)
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with official server jars"]
fn rust_and_java_compressors_decode_each_others_packets() {
    let mut cases = Vec::new();
    for threshold in [-1, 0, 1, 128, 256, 1024, i32::MAX] {
        for length in [1, 2, 127, 128, 255, 256, 257, 4096, 65536] {
            cases.push((threshold, vec![42; length]));
            cases.push((threshold, random(length)));
        }
    }
    for (threshold, payload) in [
        (-1, vec![42; MAX_FRAME_BODY_BYTES]),
        (-1, vec![42; MAX_FRAME_BODY_BYTES + 1]),
        (0, vec![42; MAX_UNCOMPRESSED_BYTES]),
        (0, vec![42; MAX_UNCOMPRESSED_BYTES + 1]),
        (0, random(MAX_FRAME_BODY_BYTES + 4096)),
        (i32::MAX, vec![42; MAX_FRAME_BODY_BYTES - 1]),
        (i32::MAX, vec![42; MAX_FRAME_BODY_BYTES]),
    ] {
        cases.push((threshold, payload));
    }
    let mut input = (cases.len() as u32).to_be_bytes().to_vec();
    let mut frames = Vec::new();
    let mut scratch = CompressionScratch::default();
    for (threshold, payload) in &cases {
        input.extend_from_slice(&threshold.to_be_bytes());
        input.extend_from_slice(&(payload.len() as i32).to_be_bytes());
        input.extend_from_slice(payload);
        let mut frame = Vec::new();
        let mut allocation = 64 * 1024 * 1024;
        let result = CompressionState::new(*threshold).encode_frame(
            payload,
            &mut scratch,
            &mut frame,
            CompressionLimits::default(),
            &mut allocation,
        );
        if result.is_ok() {
            input.extend_from_slice(&(frame.len() as i32).to_be_bytes());
            input.extend_from_slice(&frame);
            frames.push(Some(frame));
        } else {
            input.extend_from_slice(&(-1i32).to_be_bytes());
            frames.push(None);
        }
    }
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
        "arrow-compression-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("CompressionCrossOracle.java");
    let file = directory.join("input.bin");
    fs::write(&source, JAVA).unwrap();
    fs::write(&file, input).unwrap();
    let result = Command::new("java")
        .arg("-Xmx512m")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(file)
        .output();
    fs::remove_dir_all(directory).unwrap();
    let result = result.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut observations = Cursor::new(result.stdout);
    let mut passed = 0;
    for ((threshold, payload), rust_frame) in cases.iter().zip(frames) {
        let java_frame = blob(&mut observations);
        let java_decoded = blob(&mut observations);
        assert_eq!(
            java_frame.is_some(),
            rust_frame.is_some(),
            "frame acceptance: threshold {threshold} length {}",
            payload.len()
        );
        if let Some(java_frame) = java_frame {
            let mut remaining = java_frame.as_slice();
            let mut output = Vec::new();
            let mut allocation = 64 * 1024 * 1024;
            CompressionState::new(*threshold)
                .decode_frame(
                    &mut remaining,
                    &mut scratch,
                    &mut output,
                    CompressionLimits::default(),
                    &mut allocation,
                )
                .unwrap();
            assert!(remaining.is_empty());
            assert_eq!(&output, payload);
            assert_eq!(java_decoded.as_ref(), Some(payload));
            passed += 1;
        } else {
            assert!(java_decoded.is_none());
        }
    }
    assert_eq!(
        observations.position() as usize,
        observations.get_ref().len()
    );
    eprintln!(
        "Actual Java: {} cases, {passed} accepted bidirectional payloads, {} matching size rejections",
        cases.len(),
        cases.len() - passed
    );
}
