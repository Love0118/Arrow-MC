//! End-to-end public Server API: TCP handshake, real crypto, a local session
//! service, compression and configuration. No real account/auth request is used.
#[path = "common/configuration_fixture.rs"]
mod fixture;

use arrow_mc::{
    runtime::{CpuPool, CpuPoolConfig},
    server::{
        LoginServices, PROTOCOL_VERSION, Server, ServerConfig,
        access::LoginAccess,
        auth::{AuthClient, AuthLimits},
        compression::{CompressionLimits, CompressionScratch, CompressionState},
        crypto::{CipherPair, ServerKey},
        packet::{PacketReader, PacketWriter},
    },
};
use openssl::{encrypt::Encrypter, pkey::PKey, rsa::Padding};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};

struct Client {
    stream: TcpStream,
    state: CompressionState,
    scratch: CompressionScratch,
    cipher: Option<CipherPair>,
}

impl Client {
    async fn send(&mut self, packet: &[u8]) {
        let mut bytes = Vec::new();
        let mut allocation = 16 * 1024 * 1024;
        self.state
            .encode_frame(
                packet,
                &mut self.scratch,
                &mut bytes,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        if let Some(cipher) = &mut self.cipher {
            cipher.encrypt_in_place(&mut bytes).unwrap();
        }
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn receive(&mut self) -> Vec<u8> {
        let mut frame = Vec::new();
        let mut length = 0;
        for index in 0..3 {
            let mut byte = [self.stream.read_u8().await.unwrap()];
            if let Some(cipher) = &mut self.cipher {
                cipher.decrypt_in_place(&mut byte).unwrap();
            }
            frame.push(byte[0]);
            length |= usize::from(byte[0] & 127) << (index * 7);
            if byte[0] & 128 == 0 {
                assert!(length <= 0x1f_ffff);
                let prefix = frame.len();
                frame.resize(prefix + length, 0);
                self.stream.read_exact(&mut frame[prefix..]).await.unwrap();
                if let Some(cipher) = &mut self.cipher {
                    cipher.decrypt_in_place(&mut frame[prefix..]).unwrap();
                }
                let mut input = frame.as_slice();
                let mut bytes = Vec::new();
                let mut allocation = 16 * 1024 * 1024;
                self.state
                    .decode_frame(
                        &mut input,
                        &mut self.scratch,
                        &mut bytes,
                        CompressionLimits::default(),
                        &mut allocation,
                    )
                    .unwrap();
                assert!(input.is_empty());
                return bytes;
            }
        }
        panic!("invalid frame prefix");
    }

    async fn start_login(&mut self, port: u16) {
        let mut handshake = PacketWriter::new(1024);
        handshake.varint(0).unwrap();
        handshake.varint(PROTOCOL_VERSION).unwrap();
        handshake.utf("localhost", 255).unwrap();
        handshake.unsigned_short(port).unwrap();
        handshake.varint(2).unwrap();
        self.send(handshake.as_bytes()).await;
        // The finite status deadline/traffic limit must not leak into login.
        tokio::time::sleep(Duration::from_millis(260)).await;
        let mut hello = PacketWriter::new(128);
        hello.varint(0).unwrap();
        hello.utf("ArrowTest", 16).unwrap();
        hello.uuid([0xff; 16]).unwrap(); // Untrusted client identity.
        self.send(hello.as_bytes()).await;
        let challenge = self.receive().await;
        let mut reader = PacketReader::new(&challenge);
        assert_eq!(reader.varint().unwrap(), 1);
        assert_eq!(reader.utf(20).unwrap(), "");
        let public = PKey::public_key_from_der(reader.bytes(1024).unwrap()).unwrap();
        let token = reader.bytes(4).unwrap();
        assert!(reader.boolean().unwrap());
        reader.finish().unwrap();
        let secret = [0x23; 16];
        let mut encryption = Encrypter::new(&public).unwrap();
        encryption.set_rsa_padding(Padding::PKCS1).unwrap();
        let mut encrypted_secret = [0; 128];
        let mut encrypted_token = [0; 128];
        assert_eq!(
            encryption.encrypt(&secret, &mut encrypted_secret).unwrap(),
            128
        );
        assert_eq!(
            encryption.encrypt(token, &mut encrypted_token).unwrap(),
            128
        );
        let mut key = PacketWriter::new(300);
        key.varint(1).unwrap();
        key.bytes(&encrypted_secret, 128).unwrap();
        key.bytes(&encrypted_token, 128).unwrap();
        self.send(key.as_bytes()).await;
        self.cipher = Some(CipherPair::new(secret).unwrap());
    }
}

async fn run_case(verified: bool) {
    let fixture = fixture::Fixture::new();
    let auth_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth_address = auth_listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let (mut socket, _) = auth_listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192);
            request.push(socket.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /session/minecraft/hasJoined?"));
        assert!(request.contains("username=ArrowTest"));
        assert!(request.contains("serverId="));
        assert!(request.contains("ip=127.0.0.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let body = if verified {
            serde_json::json!({"id":"1234567890abcdef1234567890abcdef", "name":"DifferentCanonicalName",
                "properties":[{"name":"textures","value":"a".repeat(512),"signature":"signature"}]}).to_string()
        } else {
            String::new()
        };
        let status = if verified { "200 OK" } else { "204 No Content" };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let cpu = Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 16,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    );
    let (stop, shutdown) = watch::channel(false);
    let services = LoginServices {
        key: Arc::new(ServerKey::generate().unwrap()),
        auth: Arc::new(
            AuthClient::for_loopback_tests(auth_address, AuthLimits::default()).unwrap(),
        ),
        cpu: Arc::clone(&cpu),
        snapshot: Arc::new(fixture.load().unwrap()),
        access: Arc::new(LoginAccess::new(20)),
        online_mode: true,
        prevent_proxy_connections: true,
        accepts_transfers: false,
        compression_threshold: 256,
        max_login_connections: 2,
        shutdown: shutdown.clone(),
    };
    let server = Server::bind_with_login(
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            connection_timeout: Duration::from_millis(200),
            max_connection_bytes: 256,
            ..ServerConfig::default()
        },
        services,
    )
    .await
    .unwrap();
    let address = server.local_addr().unwrap();
    let task = tokio::spawn(server.run(shutdown));
    let mut client = Client {
        stream: TcpStream::connect(address).await.unwrap(),
        state: CompressionState::new(-1),
        scratch: CompressionScratch::default(),
        cipher: None,
    };
    client.start_login(address.port()).await;
    let packet = client.receive().await;
    let mut reader = PacketReader::new(&packet);
    if !verified {
        assert_eq!(reader.varint().unwrap(), 0);
        let json = reader.utf(262144).unwrap();
        assert!(json.contains("unverified_username"));
        reader.finish().unwrap();
    } else {
        assert_eq!(reader.varint().unwrap(), 3);
        assert_eq!(reader.varint().unwrap(), 256);
        reader.finish().unwrap();
        client.state = CompressionState::new(256);
        let packet = client.receive().await;
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.varint().unwrap(), 2);
        assert_eq!(
            reader.uuid().unwrap(),
            [
                0x12, 0x34, 0x56, 0x78, 0x90, 0xab, 0xcd, 0xef, 0x12, 0x34, 0x56, 0x78, 0x90, 0xab,
                0xcd, 0xef
            ]
        );
        assert_eq!(reader.utf(16).unwrap(), "ArrowTest");
        assert_eq!(reader.varint().unwrap(), 1);
        assert_eq!(reader.utf(64).unwrap(), "textures");
        assert_eq!(reader.utf(32767).unwrap(), "a".repeat(512));
        assert!(reader.boolean().unwrap());
        assert_eq!(reader.utf(1024).unwrap(), "signature");
        assert_eq!(reader.uuid().unwrap()[6] >> 4, 4);
        reader.finish().unwrap();
        client.send(&[3]).await; // Terminal LoginAcknowledged.
        for expected in [1, 13, 15] {
            let packet = client.receive().await;
            assert_eq!(PacketReader::new(&packet).varint().unwrap(), expected);
        }
        client.send(&[7, 0]).await; // No known packs: full contents fallback.
        for expected_registry in arrow_mc::server::configuration_data::REQUIRED_REGISTRIES {
            let packet = client.receive().await;
            let mut reader = PacketReader::new(&packet);
            assert_eq!(reader.varint().unwrap(), 7);
            assert_eq!(reader.identifier().unwrap(), expected_registry);
            assert_eq!(reader.varint().unwrap(), 1);
            assert_eq!(reader.identifier().unwrap(), "test:synthetic");
            assert!(reader.boolean().unwrap());
            assert_eq!(reader.remaining_bytes(2).unwrap(), [10, 0]);
        }
        let tags = client.receive().await;
        assert_eq!(PacketReader::new(&tags).varint().unwrap(), 14);
        assert!(
            timeout(Duration::from_millis(100), client.receive())
                .await
                .is_err(),
            "FinishConfiguration must await actual spawn readiness"
        );
    }
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    mock.await.unwrap();
    assert_eq!(cpu.stats().in_flight, 0);
    drop(client);
    Arc::try_unwrap(cpu).ok().unwrap().shutdown().unwrap();
}

#[test]
fn real_online_login_reaches_encrypted_configuration_with_authenticated_identity() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            timeout(Duration::from_secs(8), run_case(true))
                .await
                .unwrap();
        });
}

#[test]
fn unverified_online_login_gets_encrypted_disconnect_without_offline_fallback() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            timeout(Duration::from_secs(8), run_case(false))
                .await
                .unwrap();
        });
}
