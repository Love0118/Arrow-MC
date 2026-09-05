use arrow_mc::server::{MINECRAFT_VERSION, PROTOCOL_VERSION, Server, ServerConfig};
use serde_json::Value;
use std::{io, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

struct Running {
    address: SocketAddr,
    stop: watch::Sender<bool>,
    task: JoinHandle<io::Result<()>>,
}

impl Running {
    async fn start(mut config: ServerConfig) -> Self {
        config.bind = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(config).await.unwrap();
        let address = server.local_addr().unwrap();
        let (stop, receiver) = watch::channel(false);
        let task = tokio::spawn(server.run(receiver));
        Self {
            address,
            stop,
            task,
        }
    }

    async fn client(&self) -> TcpStream {
        TcpStream::connect(self.address).await.unwrap()
    }

    async fn stop(self) {
        self.stop.send(true).unwrap();
        timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn varint(value: i32) -> Vec<u8> {
    let mut value = value as u32;
    let mut bytes = Vec::new();
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            return bytes;
        }
    }
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(body.len() as i32);
    bytes.extend_from_slice(body);
    bytes
}

fn handshake(protocol: i32, host: &[u8], port: u16, intention: i32) -> Vec<u8> {
    let mut body = vec![0];
    body.extend(varint(protocol));
    body.extend(varint(host.len() as i32));
    body.extend_from_slice(host);
    body.extend_from_slice(&port.to_be_bytes());
    body.extend(varint(intention));
    frame(&body)
}

fn ping(value: i64) -> Vec<u8> {
    let mut body = vec![1];
    body.extend_from_slice(&value.to_be_bytes());
    frame(&body)
}

async fn packet(stream: &mut TcpStream) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let mut length = 0;
        for shift in (0..21).step_by(7) {
            let byte = stream.read_u8().await.unwrap();
            length |= usize::from(byte & 127) << shift;
            if byte & 128 == 0 {
                assert!((1..=100_000).contains(&length));
                let mut output = vec![0; length];
                stream.read_exact(&mut output).await.unwrap();
                return output;
            }
        }
        panic!("invalid outbound frame prefix");
    })
    .await
    .unwrap()
}

fn json(body: &[u8]) -> Value {
    assert_eq!(body[0], 0);
    let mut length = 0;
    for (index, &byte) in body[1..].iter().enumerate() {
        length |= usize::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            let payload = &body[index + 2..];
            assert_eq!(payload.len(), length);
            return serde_json::from_slice(payload).unwrap();
        }
    }
    panic!("missing JSON length");
}

async fn closed(stream: &mut TcpStream) {
    match timeout(Duration::from_secs(2), stream.read_u8())
        .await
        .unwrap()
    {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("expected closed connection, got {result:?}"),
    }
}

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

#[test]
fn fragmented_handshake_status_and_negative_ping_use_actual_tcp() {
    run(async {
        let description = "Arrow \"test\" \\ 한글 😀\nsecond line";
        let server = Running::start(ServerConfig {
            description: description.into(),
            max_players: 64,
            ..ServerConfig::default()
        })
        .await;
        let mut client = server.client().await;
        for byte in handshake(PROTOCOL_VERSION, b"localhost", 25565, 1) {
            client.write_all(&[byte]).await.unwrap();
            tokio::task::yield_now().await;
        }
        client.write_all(&[1, 0]).await.unwrap();
        let status = json(&packet(&mut client).await);
        assert_eq!(status["description"], description);
        assert_eq!(status["players"]["max"], 64);
        assert_eq!(status["players"]["online"], 0);
        assert_eq!(status["version"]["name"], MINECRAFT_VERSION);
        assert_eq!(status["version"]["protocol"], PROTOCOL_VERSION);
        assert!(status.get("favicon").is_none());
        client.write_all(&ping(i64::MIN)).await.unwrap();
        assert_eq!(
            packet(&mut client).await,
            [&[1][..], &i64::MIN.to_be_bytes()].concat()
        );
        closed(&mut client).await;
        server.stop().await;
    });
}

#[test]
fn coalesced_frames_preserve_status_then_pong_order() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        let mut client = server.client().await;
        let mut request = handshake(-1, b"localhost", 0, 1);
        request.extend_from_slice(&[1, 0]);
        request.extend(ping(0x0102_0304_0506_0708));
        client.write_all(&request).await.unwrap();
        assert_eq!(
            json(&packet(&mut client).await)["version"]["protocol"],
            PROTOCOL_VERSION
        );
        assert_eq!(packet(&mut client).await, vec![1, 1, 2, 3, 4, 5, 6, 7, 8]);
        closed(&mut client).await;
        server.stop().await;
    });
}

#[test]
fn ping_before_status_and_duplicate_status_follow_vanilla() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        let mut client = server.client().await;
        let mut request = handshake(1, b"", 65535, 1);
        request.extend(ping(-7));
        client.write_all(&request).await.unwrap();
        assert_eq!(
            packet(&mut client).await,
            [&[1][..], &(-7i64).to_be_bytes()].concat()
        );
        closed(&mut client).await;
        let mut client = server.client().await;
        let mut request = handshake(PROTOCOL_VERSION, b"localhost", 25565, 1);
        request.extend_from_slice(&[1, 0]);
        client.write_all(&request).await.unwrap();
        assert_eq!(json(&packet(&mut client).await)["players"]["online"], 0);
        client.write_all(&[1, 0]).await.unwrap();
        closed(&mut client).await;
        server.stop().await;
    });
}

#[test]
fn malformed_frames_packets_and_trailing_data_are_closed() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        for bytes in [
            vec![0],
            vec![0x80, 0x80, 0x80],
            vec![0xff, 0xff, 0x7f],
            vec![1, 1],
            vec![1, 0x80],
            handshake(PROTOCOL_VERSION, b"a", 1, 0),
        ] {
            let mut client = server.client().await;
            client.write_all(&bytes).await.unwrap();
            closed(&mut client).await;
        }
        let normal = handshake(PROTOCOL_VERSION, b"a", 1, 1);
        let mut trailing = normal[1..].to_vec();
        trailing.push(42);
        let mut client = server.client().await;
        client.write_all(&frame(&trailing)).await.unwrap();
        closed(&mut client).await;
        for bad_status in [
            vec![2, 0, 99],
            vec![1, 2],
            vec![8, 1, 0, 0, 0, 0, 0, 0, 0],
            vec![10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            let mut client = server.client().await;
            client.write_all(&normal).await.unwrap();
            client.write_all(&bad_status).await.unwrap();
            closed(&mut client).await;
        }
        let mut client = server.client().await;
        client.write_all(&[0x80]).await.unwrap();
        client.shutdown().await.unwrap();
        closed(&mut client).await;
        server.stop().await;
    });
}

#[test]
fn java_hostname_limits_include_replacement_decoding_and_surrogate_pairs() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        let cases = [
            (vec![b'a'; 255], true),
            (vec![b'a'; 256], false),
            ("한".repeat(255).into_bytes(), true),
            ("한".repeat(256).into_bytes(), false),
            (("😀".repeat(127) + "a").into_bytes(), true),
            ("😀".repeat(128).into_bytes(), false),
            ([0xed, 0xa0, 0x80].repeat(255), true),
            ([vec![b'a'; 254], vec![0xe1, 0x80]].concat(), true),
            ([vec![b'a'; 255], vec![0xe1, 0x80]].concat(), false),
            (
                [vec![b'a'; 252], vec![0xf0, 0x80, 0x80, 0x80]].concat(),
                false,
            ),
        ];
        for (hostname, accepted) in cases {
            let mut client = server.client().await;
            let mut bytes = handshake(PROTOCOL_VERSION, &hostname, 65535, 1);
            bytes.extend_from_slice(&[1, 0]);
            client.write_all(&bytes).await.unwrap();
            if accepted {
                assert_eq!(json(&packet(&mut client).await)["players"]["online"], 0);
            } else {
                closed(&mut client).await;
            }
        }
        server.stop().await;
    });
}

#[test]
fn noncanonical_varints_remain_valid_within_their_limits() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        let mut client = server.client().await;
        // Java accepts upper unused bits in byte five and leading zero groups.
        let mut body = vec![0x80, 0x80, 0x80, 0x80, 0x70];
        body.extend(varint(PROTOCOL_VERSION));
        body.extend_from_slice(&[0x80, 0x00, 0, 0, 0x81, 0x00]);
        let mut data = vec![(body.len() as u8) | 0x80, 0x80, 0];
        data.extend(body);
        data.extend_from_slice(&[5, 0x80, 0x80, 0x80, 0x80, 0x70]);
        client.write_all(&data).await.unwrap();
        assert_eq!(json(&packet(&mut client).await)["players"]["online"], 0);
        server.stop().await;
    });
}

#[test]
fn login_and_transfer_disconnects_are_in_login_json_state() {
    run(async {
        let server = Running::start(ServerConfig::default()).await;
        for (version, intent, expected) in [
            (753, 2, "multiplayer.disconnect.outdated_client"),
            (754, 2, "multiplayer.disconnect.incompatible"),
            (
                PROTOCOL_VERSION + 1,
                2,
                "multiplayer.disconnect.incompatible",
            ),
            (1, 3, "multiplayer.disconnect.transfers_disabled"),
        ] {
            let mut client = server.client().await;
            client
                .write_all(&handshake(version, b"localhost", 25565, intent))
                .await
                .unwrap();
            let reason = json(&packet(&mut client).await);
            assert_eq!(reason["translate"], expected);
            if intent == 2 {
                assert_eq!(reason["with"][0], MINECRAFT_VERSION);
            }
            closed(&mut client).await;
        }
        let mut client = server.client().await;
        client
            .write_all(&handshake(PROTOCOL_VERSION, b"localhost", 25565, 2))
            .await
            .unwrap();
        assert!(
            json(&packet(&mut client).await)
                .as_str()
                .unwrap()
                .contains("not implemented")
        );
        closed(&mut client).await;
        server.stop().await;
    });
}

#[test]
fn traffic_budget_reserves_entire_outbound_packets() {
    run(async {
        let normal = Running::start(ServerConfig::default()).await;
        let hello = handshake(PROTOCOL_VERSION, b"localhost", 25565, 1);
        let mut client = normal.client().await;
        client
            .write_all(&[hello.as_slice(), &[1, 0]].concat())
            .await
            .unwrap();
        let status_bytes = frame(&packet(&mut client).await).len();
        normal.stop().await;
        for allowance in [hello.len() - 1, hello.len() + 2 + status_bytes - 1] {
            let server = Running::start(ServerConfig {
                max_connection_bytes: allowance,
                ..ServerConfig::default()
            })
            .await;
            let mut client = server.client().await;
            client
                .write_all(&[hello.as_slice(), &[1, 0]].concat())
                .await
                .unwrap();
            closed(&mut client).await;
            server.stop().await;
        }
        for (allowance, pong_expected) in [
            (hello.len() + 2 + status_bytes + 20, true),
            (hello.len() + 2 + status_bytes + 19, false),
        ] {
            let server = Running::start(ServerConfig {
                max_connection_bytes: allowance,
                ..ServerConfig::default()
            })
            .await;
            let mut client = server.client().await;
            client
                .write_all(&[hello.as_slice(), &[1, 0], &ping(-1)].concat())
                .await
                .unwrap();
            packet(&mut client).await;
            if pong_expected {
                assert_eq!(
                    packet(&mut client).await,
                    [&[1][..], &(-1i64).to_be_bytes()].concat()
                );
            }
            closed(&mut client).await;
            server.stop().await;
        }
    });
}

#[test]
fn deadline_and_connection_cap_release_sockets_and_tasks() {
    run(async {
        let server = Running::start(ServerConfig {
            max_connections: 1,
            connection_timeout: Duration::from_millis(100),
            ..ServerConfig::default()
        })
        .await;
        let mut first = server.client().await;
        first.write_all(&[0x80]).await.unwrap();
        sleep(Duration::from_millis(20)).await;
        let mut rejected = server.client().await;
        closed(&mut rejected).await;
        // A partial frame cannot retain its slot forever.
        closed(&mut first).await;
        let mut recovered = server.client().await;
        recovered
            .write_all(&[handshake(PROTOCOL_VERSION, b"a", 1, 1), vec![1, 0]].concat())
            .await
            .unwrap();
        assert_eq!(json(&packet(&mut recovered).await)["players"]["online"], 0);
        server.stop().await;
        closed(&mut recovered).await;
    });
}

#[test]
fn invalid_configuration_is_rejected_before_listening() {
    run(async {
        for config in [
            ServerConfig {
                max_connections: 0,
                ..ServerConfig::default()
            },
            ServerConfig {
                max_connection_bytes: 0,
                ..ServerConfig::default()
            },
            ServerConfig {
                connection_timeout: Duration::ZERO,
                ..ServerConfig::default()
            },
            ServerConfig {
                max_players: u32::MAX,
                ..ServerConfig::default()
            },
            ServerConfig {
                description: "\n".repeat(32767),
                ..ServerConfig::default()
            },
        ] {
            assert!(Server::bind(config).await.is_err());
        }
    });
}
