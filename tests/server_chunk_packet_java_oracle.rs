//! Actual 26.3-pre-2 registry-bound packet encoding against authored semantic inputs.
//! Opt in with ARROW_MC_JAVA_REFERENCE_ROOT and --ignored; Java 25 is required.
//! The Java observer constructs official packet objects. It does not copy codec
//! bodies, instantiate a world, or use Rust packet bytes to construct expectations.

use arrow_mc::nbt::{Compound, NbtString, Tag};
use arrow_mc::server::chunk_packet::{
    self, BlockEntity, ChunkWithLight, HeightmapEntry, LightData, LightUpdate, Limits,
};
use arrow_mc::world::{heightmap::HeightmapKind, preparation::ChunkAddress};
use std::{env, fs, io::Read, path::Path, process::Command, time::SystemTime};

const JAVA: &str = r#"
import io.netty.buffer.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.nbt.*;
import net.minecraft.network.*;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.Packet;
import net.minecraft.network.protocol.game.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.block.entity.BlockEntityType;
import net.minecraft.world.level.chunk.DataLayer;
import net.minecraft.world.level.levelgen.Heightmap;
import java.io.*;
import java.nio.file.*;
import java.util.*;

class ChunkPacketCrossOracle {
    static StreamCodec<ByteBuf,Packet<? super ClientGamePacketListener>> codec;
    static byte[] blob(DataInputStream in) throws Exception {
        int length=in.readInt();return length<0?null:in.readNBytes(length);
    }
    static void blob(DataOutputStream out,byte[] bytes) throws Exception {
        out.writeInt(bytes==null?-1:bytes.length);if(bytes!=null)out.write(bytes);
    }
    static byte[] light(DataInputStream in) throws Exception {
        return switch(in.readInt()) {
            case 0 -> blob(in);
            case 1 -> new DataLayer(in.readInt()).copy().getData();
            default -> throw new AssertionError("unknown light fixture");
        };
    }
    static CompoundTag typed() {
        var tag=new CompoundTag();
        tag.putByte("a",(byte)-5);tag.putShort("b",(short)300);tag.putInt("c",-123456);tag.putLong("d",0x0123456789abcdefL);
        tag.putFloat("e",1.25f);tag.putDouble("f",-2.5);tag.putByteArray("g",new byte[]{0,1,-1});tag.putString("h","A\u0000\ud83d\ude00");
        var list=new ListTag();list.add(IntTag.valueOf(3));list.add(IntTag.valueOf(-4));tag.put("i",list);
        var nested=new CompoundTag();nested.putString("n","nested");tag.put("j",nested);
        tag.putIntArray("k",new int[]{1,-2});tag.putLongArray("l",new long[]{Long.MIN_VALUE,Long.MAX_VALUE});
        return tag;
    }
    static Packet<? super ClientGamePacketListener> packet(DataInputStream in) throws Exception {
        return switch(in.readInt()) {
            case 0 -> ClientboundChunkBatchStartPacket.INSTANCE;
            case 1 -> new ClientboundChunkBatchFinishedPacket(in.readInt());
            case 2 -> new ClientboundForgetLevelChunkPacket(new ChunkPos(in.readInt(),in.readInt()));
            case 3 -> new ClientboundSetChunkCacheCenterPacket(in.readInt(),in.readInt());
            case 4 -> new ClientboundSetChunkCacheRadiusPacket(in.readInt());
            case 5 -> chunk(in);
            default -> throw new AssertionError("unknown packet");
        };
    }
    static ClientboundLevelChunkWithLightPacket chunk(DataInputStream in) throws Exception {
        int x=in.readInt(),z=in.readInt();
        Map<Heightmap.Types,long[]> maps=new LinkedHashMap<>();
        int heightmaps=in.readInt();
        for(int i=0;i<heightmaps;i++) {
            Heightmap.Types type=Heightmap.Types.values()[in.readInt()];
            long[] words=new long[in.readInt()];for(int j=0;j<words.length;j++)words[j]=in.readLong();
            maps.put(type,words);
        }
        byte[] sections=blob(in);
        int entityCount=in.readInt();
        List<Object> entities=new ArrayList<>();
        var entityClass=Class.forName("net.minecraft.network.protocol.game.ClientboundLevelChunkPacketData$BlockEntityInfo");
        var entityConstructor=entityClass.getDeclaredConstructor(byte.class,short.class,BlockEntityType.class,Optional.class);
        entityConstructor.setAccessible(true);
        for(int i=0;i<entityCount;i++) {
            byte packedXZ=in.readByte();short y=in.readShort();
            var type=BuiltInRegistries.BLOCK_ENTITY_TYPE.byIdOrThrow(in.readInt());
            Optional<CompoundTag> tag=switch(in.readInt()) {
                case 0 -> Optional.empty();case 1 -> Optional.of(new CompoundTag());case 2 -> Optional.of(typed());
                default -> throw new AssertionError("unknown tag fixture");
            };
            entities.add(entityConstructor.newInstance(packedXZ,y,type,tag));
        }
        BitSet[] masks=new BitSet[4];for(int i=0;i<4;i++)masks[i]=BitSet.valueOf(blob(in));
        List<byte[]> sky=new ArrayList<>(),block=new ArrayList<>();
        int skyCount=in.readInt();for(int i=0;i<skyCount;i++)sky.add(light(in));
        int blockCount=in.readInt();for(int i=0;i<blockCount;i++)block.add(light(in));
        var dataConstructor=ClientboundLevelChunkPacketData.class.getDeclaredConstructor(Map.class,byte[].class,List.class);
        dataConstructor.setAccessible(true);
        var data=dataConstructor.newInstance(maps,sections,entities);
        return new ClientboundLevelChunkWithLightPacket(x,z,data,new ClientboundLightUpdatePacketData(masks[0],masks[1],masks[2],masks[3],sky,block));
    }
    static byte[] encode(byte[] semantic) {
        ByteBuf out=Unpooled.buffer();
        try {codec.encode(out,packet(new DataInputStream(new ByteArrayInputStream(semantic))));return ByteBufUtil.getBytes(out);}
        catch(Exception rejected){return null;}
        finally{out.release();}
    }
    static boolean decode(byte[] bytes) {
        if(bytes==null)return false;
        ByteBuf in=Unpooled.wrappedBuffer(bytes);
        try {codec.decode(in);return !in.isReadable();}
        catch(Exception rejected){return false;}
        finally{in.release();}
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();Bootstrap.bootStrap();
        if(!SharedConstants.getCurrentVersion().id().equals("26.3-pre-2"))throw new AssertionError("wrong reference");
        var registries=RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
        codec=GameProtocols.CLIENTBOUND_TEMPLATE.bind(RegistryFriendlyByteBuf.decorator(registries)).codec();
        try(var in=new DataInputStream(new BufferedInputStream(Files.newInputStream(Path.of(args[0]))));
            var out=new DataOutputStream(new BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            out.writeInt(BuiltInRegistries.BLOCK_ENTITY_TYPE.size());
            int cases=in.readInt();
            for(int i=0;i<cases;i++) {
                byte[] semantic=blob(in),rust=blob(in);
                blob(out,encode(semantic));out.writeBoolean(decode(rust));
            }
        }
    }
}
"#;

fn position(x: i32, z: i32) -> ChunkAddress {
    ChunkAddress { x, z }
}

fn typed_tag() -> Tag {
    let mut nested = Compound::new();
    nested
        .insert(NbtString::from("n"), Tag::String(NbtString::from("nested")))
        .unwrap();
    let fields = [
        ("a", Tag::Byte(-5)),
        ("b", Tag::Short(300)),
        ("c", Tag::Int(-123456)),
        ("d", Tag::Long(0x0123_4567_89ab_cdef)),
        ("e", Tag::Float(1.25)),
        ("f", Tag::Double(-2.5)),
        ("g", Tag::ByteArray(vec![0, 1, -1])),
        ("h", Tag::String(NbtString::from("A\0😀"))),
        ("i", Tag::List(vec![Tag::Int(3), Tag::Int(-4)])),
        ("j", Tag::Compound(nested)),
        ("k", Tag::IntArray(vec![1, -2])),
        ("l", Tag::LongArray(vec![i64::MIN, i64::MAX])),
    ];
    let mut tag = Compound::new();
    for (name, value) in fields {
        tag.insert(NbtString::from(name), value).unwrap();
    }
    Tag::Compound(tag)
}

#[derive(Clone, Default)]
struct ChunkValue {
    x: i32,
    z: i32,
    maps: Vec<(HeightmapKind, Vec<u64>)>,
    sections: Vec<u8>,
    entities: Vec<(u8, i16, u32, u32)>,
    masks: [Vec<u8>; 4],
    updates: [Vec<LightValue>; 2],
}

#[derive(Clone)]
enum LightValue {
    Bytes(Vec<u8>),
    Uniform(u8),
}

impl LightValue {
    fn borrowed(&self) -> LightUpdate<'_> {
        match self {
            Self::Bytes(bytes) => LightUpdate::Bytes(bytes),
            Self::Uniform(value) => LightUpdate::Uniform(*value),
        }
    }
}

fn complex() -> ChunkValue {
    ChunkValue {
        x: -123456,
        z: 987654,
        maps: vec![
            (
                HeightmapKind::WorldSurface,
                vec![0x0102_0304_0506_0708, 1 << 63],
            ),
            (HeightmapKind::MotionBlockingNoLeaves, vec![u64::MAX]),
        ],
        sections: vec![0, 1, 2, 3, 255],
        entities: vec![
            (0xf2, -64, 1, 2),
            (8, i16::MAX, 0, 0),
            (0x80, i16::MIN, 7, 1),
        ],
        masks: [vec![0x81, 1, 0], vec![0, 0, 0x80], vec![2], vec![0]],
        updates: [
            vec![
                LightValue::Bytes(vec![0x12, 0x34]),
                LightValue::Bytes(vec![]),
            ],
            vec![LightValue::Bytes(vec![255])],
        ],
    }
}

fn put_int(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn put_blob(bytes: &mut Vec<u8>, value: &[u8]) {
    put_int(bytes, value.len() as i32);
    bytes.extend_from_slice(value);
}
fn read_int(bytes: &mut &[u8]) -> i32 {
    let mut value = [0; 4];
    bytes.read_exact(&mut value).unwrap();
    i32::from_be_bytes(value)
}
fn read_blob(bytes: &mut &[u8]) -> Option<Vec<u8>> {
    let count = read_int(bytes);
    if count < 0 {
        return None;
    }
    let mut value = vec![0; count as usize];
    bytes.read_exact(&mut value).unwrap();
    Some(value)
}

impl ChunkValue {
    fn semantic(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for word in [5, self.x, self.z, self.maps.len() as i32] {
            put_int(&mut out, word);
        }
        for (kind, words) in &self.maps {
            put_int(&mut out, i32::from(kind.id()));
            put_int(&mut out, words.len() as i32);
            for word in words {
                out.extend_from_slice(&word.to_be_bytes());
            }
        }
        put_blob(&mut out, &self.sections);
        put_int(&mut out, self.entities.len() as i32);
        for (xz, y, type_id, tag) in &self.entities {
            out.push(*xz);
            out.extend_from_slice(&y.to_be_bytes());
            put_int(&mut out, *type_id as i32);
            put_int(&mut out, *tag as i32);
        }
        for mask in &self.masks {
            put_blob(&mut out, mask);
        }
        for updates in &self.updates {
            put_int(&mut out, updates.len() as i32);
            for update in updates {
                match update {
                    LightValue::Bytes(bytes) => {
                        put_int(&mut out, 0);
                        put_blob(&mut out, bytes);
                    }
                    LightValue::Uniform(value) => {
                        put_int(&mut out, 1);
                        put_int(&mut out, i32::from(*value));
                    }
                }
            }
        }
        out
    }
    fn encoded(&self) -> Option<Vec<u8>> {
        let maps: Vec<_> = self
            .maps
            .iter()
            .map(|(kind, words)| HeightmapEntry { kind: *kind, words })
            .collect();
        let tags: Vec<_> = self
            .entities
            .iter()
            .map(|entry| match entry.3 {
                0 => None,
                1 => Some(Tag::Compound(Compound::new())),
                2 => Some(typed_tag()),
                _ => unreachable!(),
            })
            .collect();
        let entities: Vec<_> = self
            .entities
            .iter()
            .zip(&tags)
            .map(|((packed_xz, y, type_id, _), tag)| BlockEntity {
                packed_xz: *packed_xz,
                y: *y,
                type_id: *type_id,
                update_tag: tag.as_ref(),
            })
            .collect();
        let sky: Vec<_> = self.updates[0].iter().map(LightValue::borrowed).collect();
        let block: Vec<_> = self.updates[1].iter().map(LightValue::borrowed).collect();
        let packet = ChunkWithLight {
            position: position(self.x, self.z),
            heightmaps: &maps,
            sections: &self.sections,
            block_entities: &entities,
            light: LightData {
                sky_mask: &self.masks[0],
                block_mask: &self.masks[1],
                empty_sky_mask: &self.masks[2],
                empty_block_mask: &self.masks[3],
                sky_updates: &sky,
                block_updates: &block,
            },
        };
        let limits = Limits::default();
        let length = chunk_packet::encoded_len(&packet, 49, limits);
        let encoded = chunk_packet::encode(&packet, 49, limits);
        assert_eq!(length.is_ok(), encoded.is_ok());
        if let (Ok(length), Ok(encoded)) = (length, encoded) {
            assert_eq!(length, encoded.len());
            Some(encoded)
        } else {
            None
        }
    }
}

fn from_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn complex_packet_matches_captured_java_constructor_bytes() {
    // Actual registry-bound official-JAR constructor observation, 2026-09-05.
    let expected = from_hex(concat!(
        "2dfffe1dc0000f1206020102010203040506070880000000000000000501ffffffffffffffff0500010203ff03f2ffc0010a",
        "01000161fb02000162012c03000163fffe1dc0040001640123456789abcdef050001653fa0000006000166c004000000000000",
        "07000167000000030001ff08000168000941c080eda0bdedb88009000169030000000200000003fffffffc0a00016a0800016e",
        "00066e6573746564000b00016b0000000200000001fffffffe0c00016c0000000280000000000000007fffffffffffffff00",
        "087fff0000808000070a000281010300008001020002021234000101ff"
    ));
    assert_eq!(complex().encoded().unwrap(), expected);
}

struct Case {
    name: String,
    semantic: Vec<u8>,
    encoded: Option<Vec<u8>>,
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut small = |name: String, fields: &[i32], bytes: &[u8]| {
        let mut semantic = Vec::new();
        for field in fields {
            put_int(&mut semantic, *field);
        }
        cases.push(Case {
            name,
            semantic,
            encoded: Some(bytes.to_vec()),
        });
    };
    small("start".into(), &[0], chunk_packet::batch_start().as_bytes());
    for value in [0, 1, 2, 127, 128, 32, -1, i32::MIN, i32::MAX] {
        small(
            format!("finish-{value}"),
            &[1, value],
            chunk_packet::batch_finished(value).as_bytes(),
        );
        small(
            format!("radius-{value}"),
            &[4, value],
            chunk_packet::cache_radius(value).as_bytes(),
        );
        let pos = position(value, value.wrapping_neg());
        small(
            format!("forget-{value}"),
            &[2, pos.x, pos.z],
            chunk_packet::forget(pos).as_bytes(),
        );
        small(
            format!("center-{value}"),
            &[3, pos.x, pos.z],
            chunk_packet::cache_center(pos).as_bytes(),
        );
    }
    let mut chunks = vec![
        ("empty".into(), ChunkValue::default()),
        ("complex".into(), complex()),
    ];
    let mut reverse = complex();
    reverse.maps.reverse();
    chunks.push(("heightmap-order".into(), reverse));
    let all = ChunkValue {
        maps: HeightmapKind::ALL
            .iter()
            .rev()
            .map(|kind| (*kind, vec![]))
            .collect(),
        ..ChunkValue::default()
    };
    chunks.push(("all-heightmap-kinds-empty-arrays".into(), all));
    for length in [0, 1, 2047, 2048, 2049] {
        let mut chunk = ChunkValue::default();
        chunk.updates[0].push(LightValue::Bytes(vec![0xab; length]));
        chunks.push((format!("sky-array-{length}"), chunk.clone()));
        chunk.updates.swap(0, 1);
        chunks.push((format!("block-array-{length}"), chunk));
    }
    for length in [127, 128, 2097152, 2097153] {
        chunks.push((
            format!("section-size-{length}"),
            ChunkValue {
                sections: vec![0x5a; length],
                ..ChunkValue::default()
            },
        ));
    }
    for type_id in [0, 1, 7, 48, 49, u32::MAX] {
        for tag in [0, 1, 2] {
            chunks.push((
                format!("entity-type-{type_id}-tag-{tag}"),
                ChunkValue {
                    entities: vec![(0xff, -1, type_id, tag)],
                    ..ChunkValue::default()
                },
            ));
        }
    }
    for length in [0, 1, 8, 9, 128] {
        chunks.push((
            format!("mask-canonical-{length}"),
            ChunkValue {
                masks: std::array::from_fn(|index| {
                    let mut mask = vec![0; length];
                    if length > 0 && index < 3 {
                        mask[0] = 0x81;
                    }
                    mask
                }),
                ..ChunkValue::default()
            },
        ));
    }
    for value in (0..=15).chain([16, 31, 127, 128, 255]) {
        for domain in 0..2 {
            let mut chunk = ChunkValue::default();
            chunk.updates[domain].push(LightValue::Uniform(value));
            // Raw updates deliberately include uniform zero as data; a live
            // producer determines empty masks from DataLayer before this codec.
            chunk.masks[domain].push(1);
            chunks.push((format!("uniform-{domain}-{value}"), chunk));
        }
    }
    let mut mixed = complex();
    mixed.updates[0] = vec![
        LightValue::Uniform(3),
        LightValue::Bytes(vec![0x12, 0x34]),
        LightValue::Uniform(255),
    ];
    mixed.updates[1] = vec![LightValue::Bytes(vec![]), LightValue::Uniform(0)];
    chunks.push(("mixed-uniform-and-byte-updates".into(), mixed));
    for (name, value) in chunks {
        cases.push(Case {
            name,
            semantic: value.semantic(),
            encoded: value.encoded(),
        });
    }
    cases
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with official server jars"]
fn semantic_packet_values_match_actual_java_constructors_and_codecs() {
    let cases = cases();
    let mut input = Vec::new();
    put_int(&mut input, cases.len() as i32);
    for case in &cases {
        put_blob(&mut input, &case.semantic);
        if let Some(encoded) = &case.encoded {
            put_blob(&mut input, encoded);
        } else {
            put_int(&mut input, -1);
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
        "arrow-chunk-packet-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("ChunkPacketCrossOracle.java");
    let file = directory.join("input.bin");
    let output = directory.join("output.bin");
    fs::write(&source, JAVA).unwrap();
    fs::write(&file, input).unwrap();
    let result = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(file)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = fs::read(output).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut observed = output.as_slice();
    assert_eq!(read_int(&mut observed), 49);
    for case in &cases {
        let java = read_blob(&mut observed);
        let mut accepted = [0];
        observed.read_exact(&mut accepted).unwrap();
        assert_eq!(case.encoded, java, "{}", case.name);
        assert_eq!(
            accepted[0] != 0,
            case.encoded.is_some(),
            "Java decoder: {}",
            case.name
        );
    }
    assert!(observed.is_empty());
    eprintln!(
        "Actual Java chunk/cache/batch packet constructor cases: {}",
        cases.len()
    );
}
