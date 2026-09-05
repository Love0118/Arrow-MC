//! Local Authlib hasJoined oracle. Both implementations call loopback mocks only.
use arrow_mc::server::auth::{AuthClient, AuthError, AuthLimits};
use std::{env, fs, net::Ipv4Addr, path::Path, process::Command, time::SystemTime};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
};

const ORACLE: &str = r#"
import com.sun.net.httpserver.HttpServer;
import java.net.*;
import java.util.*;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Supplier;
import com.google.gson.Gson;
import com.mojang.authlib.services.MinecraftServicesDiscoveryService;
import com.mojang.authlib.services.response.discovery.DiscoveryResponse;
import com.mojang.authlib.exceptions.AuthenticationUnavailableException;

class SessionAuthOracle {
  public static void main(String[] args) throws Exception {
    int[] statuses={200,204,200,200,403,503,500};
    String[] bodies={"{\"id\":\"b50ad385829d3141a2167e7d7539ba7f\",\"name\":\"IGNORED\",\"properties\":[{\"name\":\"textures\",\"value\":\"value\",\"signature\":\"signature\"}]}","","null","{}","{}","{}","{\"error\":\"ForbiddenOperationException\"}"};
    var index=new AtomicInteger();
    var server=HttpServer.create(new InetSocketAddress(InetAddress.getByName("127.0.0.1"),0),0);
    server.createContext("/verify",exchange->{
      int i=index.getAndIncrement();
      byte[] body=bodies[i].getBytes(java.nio.charset.StandardCharsets.UTF_8);
      exchange.getResponseHeaders().add("Content-Type","application/json");
      exchange.sendResponseHeaders(statuses[i],statuses[i]==204?-1:body.length);
      if(statuses[i]!=204) exchange.getResponseBody().write(body);
      exchange.close();
    });
    server.start();
    try {
      String uri="http://"+server.getAddress().getAddress().getHostAddress()+":"+server.getAddress().getPort()+"/verify";
      String json="{\"environment\":\"test\",\"product\":\"minecraft\",\"discovery\":{\"session\":{\"endpoints\":{\"verify\":{\"uri\":\""+uri+"\"}}}}}";
      var discovery=new Gson().fromJson(json,DiscoveryResponse.class);
      var ctor=MinecraftServicesDiscoveryService.class.getDeclaredConstructor(Proxy.class,boolean.class,Supplier.class);
      ctor.setAccessible(true);
      var service=ctor.newInstance(Proxy.NO_PROXY,false,(Supplier<DiscoveryResponse>)()->discovery).createMinecraftSessionService();
      for(int i=0;i<statuses.length;i++) {
        try {
          var response=service.hasJoinedServer("Requested", "-123",null);
          if(response==null) System.out.println("CASE|"+i+"|none");
          else {
            var profile=response.profile();
            var property=profile.properties().get("textures").iterator().next();
            System.out.println("CASE|"+i+"|"+profile.id()+"|"+profile.name()+"|"+property.value()+"|"+property.signature());
          }
        } catch(AuthenticationUnavailableException unavailable) { System.out.println("CASE|"+i+"|unavailable"); }
      }
    } finally { server.stop(0); }
  }
}
"#;

#[tokio::test]
#[ignore = "requires Java25 and local ARROW_MC_JAVA_REFERENCE_ROOT; uses loopback HTTP only"]
async fn has_joined_matches_local_authlib_profile_and_status_semantics() {
    let root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT").expect("set local Decompile root");
    let artifacts = Path::new(&root).join("artifacts/26.3-pre-2");
    let cp = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();
    let dir = env::temp_dir().join(format!(
        "arrow-auth-oracle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let source = dir.join("SessionAuthOracle.java");
    fs::write(&source, ORACLE).unwrap();
    let output = Command::new("java")
        .arg("--class-path")
        .arg(cp)
        .arg(&source)
        .output()
        .unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let java = String::from_utf8(output.stdout).unwrap();
    let observed: Vec<_> = java
        .lines()
        .filter(|line| line.starts_with("CASE|"))
        .collect();
    assert_eq!(observed.len(), 7, "{java}");
    let scenarios = [
        (
            200,
            r#"{"id":"b50ad385829d3141a2167e7d7539ba7f","name":"IGNORED","properties":[{"name":"textures","value":"value","signature":"signature"}]}"#,
        ),
        (204, ""),
        (200, "null"),
        (200, "{}"),
        (403, "{}"),
        (503, "{}"),
        (500, r#"{"error":"ForbiddenOperationException"}"#),
    ];
    for (index, (status, body)) in scenarios.into_iter().enumerate() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client =
            AuthClient::for_loopback_tests(listener.local_addr().unwrap(), AuthLimits::default())
                .unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let (_cancel, mut cancel) = watch::channel(false);
        let result = client
            .has_joined("Requested", "-123", None, &mut cancel)
            .await;
        let result = match result {
            Ok(Some(profile)) => {
                let hex: String = profile
                    .id
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                let uuid = format!(
                    "{}-{}-{}-{}-{}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..]
                );
                format!(
                    "{uuid}|{}|{}|{}",
                    profile.name,
                    profile.properties[0].value,
                    profile.properties[0].signature.as_ref().unwrap()
                )
            }
            Ok(None)
            | Err(AuthError::HttpStatus {
                unavailable: false, ..
            }) => "none".into(),
            Err(error) if error.is_unavailable() => "unavailable".into(),
            other => panic!("unexpected Rust result {other:?}"),
        };
        assert_eq!(observed[index], format!("CASE|{index}|{result}"));
    }
}
