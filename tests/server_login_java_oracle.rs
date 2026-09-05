//! Login field/packet comparisons with the actual pinned Java codecs.
use arrow_mc::server::{
    login::{AuthenticatedProfile, ProfileProperty, packet},
    packet::PacketWriter,
};
use std::{env, fs, path::Path, process::Command, time::SystemTime};

const JAVA: &str = r#"
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import com.google.common.collect.ImmutableMultimap;
import com.mojang.authlib.GameProfile;
import com.mojang.authlib.properties.*;
import io.netty.buffer.*;
import net.minecraft.network.*;
import net.minecraft.network.protocol.login.*;
import net.minecraft.network.chat.Component;

class LoginCodecOracle {
 static UUID uuid(String hex){var bytes=HexFormat.of().parseHex(hex);var b=java.nio.ByteBuffer.wrap(bytes);return new UUID(b.getLong(),b.getLong());}
 public static void main(String[]args)throws Exception{
  var output=System.out;System.setOut(System.err);net.minecraft.SharedConstants.tryDetectVersion();net.minecraft.server.Bootstrap.bootStrap();System.setOut(output);
  var requests=JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonArray();
  for(var element:requests){var request=element.getAsJsonObject();var buffer=new FriendlyByteBuf(Unpooled.buffer());
   try{
    String mode=request.get("mode").getAsString();
    if(mode.equals("decode")){
     buffer.writeBytes(HexFormat.of().parseHex(request.get("bytes").getAsString()));
     LoginProtocols.SERVERBOUND.codec().decode(buffer);
     if(buffer.isReadable())throw new IllegalArgumentException("trailing");
     System.out.println("OK");
    }else{
     if(mode.equals("hello")){LoginProtocols.CLIENTBOUND.codec().encode(buffer,new ClientboundHelloPacket("",HexFormat.of().parseHex(request.get("key").getAsString()),new byte[]{1,2,3,4},true));}
     else if(mode.equals("disconnect")){LoginProtocols.CLIENTBOUND.codec().encode(buffer,new ClientboundLoginDisconnectPacket(Component.literal(request.get("text").getAsString())));}
     else{
      var properties=ImmutableMultimap.<String,Property>builder();
      for(var item:request.getAsJsonArray("properties")){var p=item.getAsJsonObject();var name=p.get("name").getAsString();properties.put(name,new Property(name,p.get("value").getAsString(),p.has("signature")&&!p.get("signature").isJsonNull()?p.get("signature").getAsString():null));}
      var profile=new GameProfile(uuid(request.get("id").getAsString()),request.get("name").getAsString(),new PropertyMap(properties.build()));
      LoginProtocols.CLIENTBOUND.codec().encode(buffer,new ClientboundLoginFinishedPacket(profile,uuid(request.get("session").getAsString())));
     }
     byte[] encoded=new byte[buffer.readableBytes()];buffer.readBytes(encoded);System.out.println(HexFormat.of().formatHex(encoded));
    }
   }catch(Exception error){System.out.println("ERROR");}finally{buffer.release();}
  }
 }
}
"#;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT with locked server jars"]
fn login_packet_acceptance_and_outbound_bytes_match_official_codecs() {
    let mut requests = Vec::new();
    let mut expected = Vec::new();
    for name in [
        "",
        "Player!",
        "abcdefghijklmnop",
        "abcdefghijklmnopq",
        "한글",
        "😀😀😀😀😀😀😀😀",
    ] {
        let mut w = PacketWriter::new(1000);
        w.varint(0).unwrap();
        w.utf(name, 32767).unwrap();
        w.uuid([42; 16]).unwrap();
        let full = w.into_bytes();
        for end in 0..=full.len() {
            let bytes = &full[..end];
            requests.push(serde_json::json!({"mode":"decode","bytes":hex(bytes)}));
            expected.push(if packet::decode(bytes).is_ok() {
                "OK".into()
            } else {
                "ERROR".into()
            });
        }
    }
    for bytes in [
        vec![1, 0, 0],
        vec![1, 1, 42, 1, 7],
        vec![2, 0],
        vec![2, 0, 255],
        vec![3],
        vec![3, 0],
        vec![4, 0, 0],
        vec![4, 1, b'a', 1, 0],
        vec![5],
        vec![0x83, 0],
    ] {
        requests.push(serde_json::json!({"mode":"decode","bytes":hex(&bytes)}));
        expected.push(if packet::decode(&bytes).is_ok() {
            "OK".into()
        } else {
            "ERROR".into()
        });
    }
    for key in [vec![], vec![7; 162], vec![8; 512]] {
        requests.push(serde_json::json!({"mode":"hello","key":hex(&key)}));
        expected.push(hex(&packet::hello(&key, &[1, 2, 3, 4], 4096).unwrap()));
    }
    for text in ["reason", "한글 😀", "quote\"slash\\\nline"] {
        requests.push(serde_json::json!({"mode":"disconnect","text":text}));
        expected.push(hex(
            &packet::disconnect(serde_json::json!(text), 4096).unwrap()
        ));
    }
    for count in [0, 1, 2, 16, 17] {
        let properties: Vec<_> = (0..count)
            .map(|i| ProfileProperty {
                name: if i % 2 == 0 {
                    "textures".into()
                } else {
                    "other".into()
                },
                value: format!("value{i}"),
                signature: if i % 3 == 0 {
                    Some(format!("sig{i}"))
                } else {
                    None
                },
            })
            .collect();
        let jsonproperties: Vec<_> = properties
            .iter()
            .map(|p| serde_json::json!({"name":p.name,"value":p.value,"signature":p.signature}))
            .collect();
        let profile = AuthenticatedProfile {
            id: [7; 16],
            name: "Player!".into(),
            properties,
        };
        requests.push(serde_json::json!({"mode":"finished","id":hex(&profile.id),"name":profile.name,"properties":jsonproperties,"session":hex(&[9;16])}));
        expected.push(match packet::finished(&profile, [9; 16], 2 * 1024 * 1024) {
            Ok(bytes) => hex(&bytes),
            Err(_) => "ERROR".into(),
        });
    }
    let root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").unwrap();
    let artifacts = Path::new(&root).join("artifacts/26.3-pre-2");
    let cp = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        env::temp_dir().join(format!("arrow-login-codec-{}-{unique}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("LoginCodecOracle.java");
    let file = directory.join("cases.json");
    fs::write(&source, JAVA).unwrap();
    fs::write(&file, serde_json::to_vec(&requests).unwrap()).unwrap();
    let result = Command::new("java")
        .arg("--class-path")
        .arg(cp)
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
    let actual = String::from_utf8(result.stdout).unwrap();
    let lines: Vec<_> = actual.lines().collect();
    assert_eq!(lines.len(), expected.len());
    for (index, (actual, expected)) in lines.iter().zip(expected).enumerate() {
        assert_eq!(*actual, expected, "case{} {}", index, requests[index]);
    }
    eprintln!(
        "Matched {} actual-Java login packet codec cases",
        requests.len()
    );
}
