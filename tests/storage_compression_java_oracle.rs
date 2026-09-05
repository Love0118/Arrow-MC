//! Complete-stream comparison with pinned RegionFileVersion codecs. This is
//! deliberately separate from NbtIo's earlier stopping point in a chunk reader.
use arrow_mc::world::storage::compression::{CompressionKind, StorageDecoder};
use std::{env, fs, path::Path, process::Command, time::SystemTime};

const ORACLE: &str = r#"
import java.io.*;
import java.util.*;
import net.minecraft.world.level.chunk.storage.RegionFileVersion;
class StorageCompressionOracle {
  static final HexFormat HEX=HexFormat.of();
  static byte[] encode(int kind,byte[] data)throws Exception{
    var output=new ByteArrayOutputStream();
    try(var encoder=RegionFileVersion.fromId(kind).wrap(output)){encoder.write(data);}
    return output.toByteArray();
  }
  static void test(String name,int kind,byte[] input){
    String result;
    try(var decoded=RegionFileVersion.fromId(kind).wrap(new ByteArrayInputStream(input))){result=HEX.formatHex(decoded.readAllBytes());}
    catch(Exception error){result="ERROR";}
    System.out.println("CASE|"+name+"|"+kind+"|"+HEX.formatHex(input)+"|"+result);
  }
  public static void main(String[] args)throws Exception{
    for(int kind=1;kind<=4;kind++)for(int size:new int[]{0,1,31,8192,65537}){
      byte[] data=new byte[size];int state=11;
      for(int i=0;i<size;i++){state=state*1664525+1013904223;data[i]=(byte)(state>>>24);}
      byte[] encoded=encode(kind,data);
      test("valid-"+size,kind,encoded);
      test("short-"+size,kind,Arrays.copyOf(encoded,Math.max(0,encoded.length-1)));
      byte[] bad=encoded.clone();if(bad.length>0)bad[bad.length-1]^=1;test("tailbit-"+size,kind,bad);
      byte[] suffix=Arrays.copyOf(encoded,encoded.length+4);Arrays.fill(suffix,encoded.length,suffix.length,(byte)7);test("suffix-"+size,kind,suffix);
    }
    byte[] repetitive=new byte[131073];Arrays.fill(repetitive,(byte)3);
    test("lz4-multiple-compressed-blocks",4,encode(4,repetitive));
    byte[] compressedBlock=encode(4,repetitive);
    int blockLength=java.nio.ByteBuffer.wrap(compressedBlock).order(java.nio.ByteOrder.LITTLE_ENDIAN).getInt(9);
    for(byte extra:new byte[]{0,1,15,16}){
      byte[] extended=new byte[compressedBlock.length+1];
      System.arraycopy(compressedBlock,0,extended,0,21+blockLength);
      extended[21+blockLength]=extra;
      System.arraycopy(compressedBlock,21+blockLength,extended,22+blockLength,compressedBlock.length-21-blockLength);
      java.nio.ByteBuffer.wrap(extended).order(java.nio.ByteOrder.LITTLE_ENDIAN).putInt(9,blockLength+1);
      test("lz4-extra-compressed-byte-"+extra,4,extended);
    }
    byte[] a=encode(1,new byte[]{1,2}),b=encode(1,new byte[]{3,4});
    byte[] joined=Arrays.copyOf(a,a.length+b.length);System.arraycopy(b,0,joined,a.length,b.length);test("gzip-members",1,joined);
    for(int suffixSize:new int[]{1,2,9,10}){
      byte[] incomplete=Arrays.copyOf(a,a.length+suffixSize);System.arraycopy(b,0,incomplete,a.length,suffixSize);test("gzip-incomplete-header-"+suffixSize,1,incomplete);
    }
  }
}
"#;
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

#[test]
#[ignore = "requires Java25 and local ARROW_MC_JAVA_REFERENCE_ROOT"]
fn complete_streams_match_locked_java_codecs() {
    let root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set local Decompile root");
    let artifacts = Path::new(&root).join("artifacts/26.3-pre-2");
    let cp = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let dir = env::temp_dir().join(format!(
        "arrow-storage-codec-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let source = dir.join("StorageCompressionOracle.java");
    fs::write(&source, ORACLE).unwrap();
    let result = Command::new("java")
        .arg("--class-path")
        .arg(cp)
        .arg(source)
        .output()
        .unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    let mut count = 0;
    let mut decoder = StorageDecoder::new();
    for line in text.lines().filter(|line| line.starts_with("CASE|")) {
        let fields: Vec<_> = line.split('|').collect();
        let kind = CompressionKind::try_from(fields[2].parse::<u8>().unwrap()).unwrap();
        let input = unhex(fields[3]);
        let mut output = Vec::with_capacity(200_000);
        let actual = decoder.decompress(kind, &input, &mut output, 200_000);
        if fields[4] == "ERROR" {
            assert!(
                actual.is_err(),
                "{} {kind:?}: Java error, Rust accepted",
                fields[1]
            );
        } else {
            assert!(actual.is_ok(), "{} {kind:?}: {actual:?}", fields[1]);
            assert_eq!(output, unhex(fields[4]), "{} {kind:?}", fields[1]);
        }
        count += 1;
    }
    assert_eq!(count, 90);
    println!("Compared {count} complete streams against official RegionFileVersion");
}
