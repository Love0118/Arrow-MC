//! Local, opt-in calls to the pinned Java crypto API; no services or accounts.
use arrow_mc::server::crypto::{CipherPair, ServerKey, login_digest, offline_uuid};
use std::{env, fs, path::Path, process::Command, time::SystemTime};

const ORACLE: &str = r#"
import java.nio.file.*;
import java.util.*;
import java.math.BigInteger;
import javax.crypto.*;
import javax.crypto.spec.SecretKeySpec;
import java.security.*;
import java.security.spec.X509EncodedKeySpec;
import net.minecraft.util.Crypt;
import net.minecraft.core.UUIDUtil;

class LoginCryptoOracle {
  public static void main(String[] args) throws Exception {
    var hex = HexFormat.of();
    var publicKey = KeyFactory.getInstance("RSA").generatePublic(new X509EncodedKeySpec(Files.readAllBytes(Path.of(args[0]))));
    byte[] secret = new byte[16];
    for (int i=0;i<16;i++) secret[i]=(byte)(i*13);
    byte[] challenge = new byte[]{-1,0,7,63};
    var aes = new SecretKeySpec(secret,"AES");
    System.out.println("digest|"+new BigInteger(Crypt.digestData("",publicKey,aes)).toString(16));
    var rsa = Cipher.getInstance("RSA");
    rsa.init(Cipher.ENCRYPT_MODE,publicKey);
    System.out.println("secret|"+hex.formatHex(rsa.doFinal(secret)));
    byte[] encodedChallenge;
    int attempts=0;
    do { encodedChallenge=rsa.doFinal(challenge); if(++attempts>10000) throw new AssertionError("RSA leading zero sample"); }
    while(encodedChallenge[0]!=0);
    System.out.println("challenge|"+hex.formatHex(Arrays.copyOfRange(encodedChallenge,1,encodedChallenge.length)));
    var local = Crypt.generateKeyPair();
    rsa.init(Cipher.ENCRYPT_MODE,local.getPublic());
    byte[] trimmed;
    do { trimmed=rsa.doFinal(challenge); } while(trimmed[0]!=0);
    rsa.init(Cipher.DECRYPT_MODE,local.getPrivate());
    if(!Arrays.equals(challenge,rsa.doFinal(Arrays.copyOfRange(trimmed,1,trimmed.length)))) throw new AssertionError("short JCE ciphertext");
    byte[] plain = new byte[1027];
    for(int i=0;i<plain.length;i++) plain[i]=(byte)i;
    for(int split:new int[]{0,1,3,16,17,1026,1027}) {
      var cipher=Crypt.getCipher(Cipher.ENCRYPT_MODE,aes);
      byte[] first=cipher.update(plain,0,split), last=cipher.doFinal(plain,split,plain.length-split);
      byte[] combined=new byte[plain.length];
      if(first!=null) System.arraycopy(first,0,combined,0,first.length);
      System.arraycopy(last,0,combined,first==null?0:first.length,last.length);
      System.out.println("cipher"+split+"|"+hex.formatHex(combined));
    }
    for(String name:new String[]{"","Notch","notch","Arrow!","\uD83D\uDE00"})
      System.out.println("uuid|"+UUIDUtil.createOfflinePlayerUUID(name).toString().replace("-",""));
  }
}
"#;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|bytes| u8::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap())
        .collect()
}

#[test]
#[ignore = "requires Java25 and ARROW_MC_JAVA_REFERENCE_ROOT; no external auth requests"]
fn matches_locked_java_crypto_and_short_rsa_inputs() {
    let root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set local Decompile root");
    let artifacts = Path::new(&root).join("artifacts/26.3-pre-2");
    let cp = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let dir = env::temp_dir().join(format!(
        "arrow-login-crypto-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let key = ServerKey::generate().unwrap();
    let source = dir.join("LoginCryptoOracle.java");
    let public = dir.join("public.der");
    fs::write(&source, ORACLE).unwrap();
    fs::write(&public, key.public_key_der()).unwrap();
    let output = Command::new("java")
        .arg("--class-path")
        .arg(cp)
        .arg(&source)
        .arg(&public)
        .output()
        .unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let secret: [u8; 16] = std::array::from_fn(|i| (i * 13) as u8);
    assert_eq!(
        lines[0],
        format!(
            "digest|{}",
            login_digest(&secret, key.public_key_der()).unwrap()
        )
    );
    let encrypted_secret = unhex(lines[1].split_once('|').unwrap().1);
    let encrypted_challenge = unhex(lines[2].split_once('|').unwrap().1);
    assert_eq!(encrypted_challenge.len(), 127);
    let result = key
        .verify_key_response(&encrypted_secret, &encrypted_challenge, [255, 0, 7, 63])
        .unwrap();
    assert_eq!(result.shared_secret, secret);
    for (line, split) in lines[3..10].iter().zip([0, 1, 3, 16, 17, 1026, 1027]) {
        let mut bytes: Vec<_> = (0..1027).map(|i| i as u8).collect();
        let mut pair = CipherPair::new(secret).unwrap();
        pair.encrypt_in_place(&mut bytes[..split]).unwrap();
        pair.encrypt_in_place(&mut bytes[split..]).unwrap();
        assert_eq!(*line, format!("cipher{split}|{}", hex(&bytes)));
    }
    for (line, name) in lines[10..]
        .iter()
        .zip(["", "Notch", "notch", "Arrow!", "😀"])
    {
        assert_eq!(*line, format!("uuid|{}", hex(&offline_uuid(name).unwrap())));
    }
    assert_eq!(lines.len(), 15);
}
