#[path = "common/configuration_fixture.rs"]
mod configuration_fixture;
use arrow_mc::{
    nbt,
    runtime::{CpuPool, CpuPoolConfig},
    server::{
        compression::{CompressionLimits, CompressionScratch, CompressionState},
        configuration::{self, ConfigurationSession, SessionStage},
        packet::{PacketReader, PacketWriter},
        transport::{ConnectionTransport, TransportLimits},
    },
};
use configuration_fixture::{Fixture, core};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}
async fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    );
    (server.unwrap().0, client.unwrap())
}
async fn frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut length = 0;
    for index in 0..3 {
        let byte = stream.read_u8().await.unwrap();
        bytes.push(byte);
        length |= usize::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            let prefix = bytes.len();
            bytes.resize(prefix + length, 0);
            stream.read_exact(&mut bytes[prefix..]).await.unwrap();
            return bytes;
        }
    }
    panic!("invalid frame prefix")
}
async fn read(
    stream: &mut TcpStream,
    state: &CompressionState,
    scratch: &mut CompressionScratch,
) -> Vec<u8> {
    let bytes = timeout(Duration::from_secs(3), frame(stream))
        .await
        .unwrap();
    decode(&bytes, state, scratch)
}
fn decode(bytes: &[u8], state: &CompressionState, scratch: &mut CompressionScratch) -> Vec<u8> {
    let mut input = bytes;
    let mut output = Vec::new();
    let mut budget = 16 * 1024 * 1024;
    state
        .decode_frame(
            &mut input,
            scratch,
            &mut output,
            CompressionLimits::default(),
            &mut budget,
        )
        .unwrap();
    assert!(input.is_empty());
    output
}
async fn send(
    stream: &mut TcpStream,
    state: &CompressionState,
    scratch: &mut CompressionScratch,
    packet: &[u8],
) {
    let mut frame = Vec::new();
    let mut budget = 16 * 1024 * 1024;
    state
        .encode_frame(
            packet,
            scratch,
            &mut frame,
            CompressionLimits::default(),
            &mut budget,
        )
        .unwrap();
    stream.write_all(&frame).await.unwrap();
}
fn pool() -> Arc<CpuPool> {
    Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 16,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    )
}

async fn exchange(
    snapshot: Arc<arrow_mc::server::configuration_data::ConfigurationSnapshot>,
    threshold: i32,
    accept_known: bool,
) {
    let cpu = pool();
    let (server, mut client) = pair().await;
    let (worker_shutdown, mut shutdown) = watch::channel(false);
    let server_cpu = Arc::clone(&cpu);
    let expected_snapshot = Arc::clone(&snapshot);
    let task = tokio::spawn(async move {
        let mut transport =
            ConnectionTransport::new(server, server_cpu, TransportLimits::default());
        if threshold >= 0 {
            transport.set_compression(threshold).await.unwrap();
        }
        let mut session = ConfigurationSession::new(snapshot, "Arrow MC".into(), 0);
        let result = configuration::run(&mut transport, &mut session, &mut shutdown).await;
        (result, session.stage(), transport.is_open())
    });
    let mut scratch = CompressionScratch::default();
    let state = CompressionState::new(threshold);
    if threshold >= 0 {
        let transition = read(&mut client, &CompressionState::new(-1), &mut scratch).await;
        let mut reader = PacketReader::new(&transition);
        assert_eq!(reader.varint().unwrap(), 3);
        assert_eq!(reader.varint().unwrap(), threshold);
        reader.finish().unwrap();
    }
    for expected in [1, 13, 15] {
        let bytes = read(&mut client, &state, &mut scratch).await;
        assert_eq!(bytes[0], expected);
    }
    let mut reply = PacketWriter::new(256);
    reply.varint(7).unwrap();
    reply.varint(i32::from(accept_known)).unwrap();
    if accept_known {
        let pack = core();
        reply.utf(&pack.namespace, 32767).unwrap();
        reply.utf(&pack.id, 32767).unwrap();
        reply.utf(&pack.version, 32767).unwrap();
    }
    send(&mut client, &state, &mut scratch, reply.as_bytes()).await;
    let negotiated = expected_snapshot.negotiate_known_packs(if accept_known {
        expected_snapshot.known_packs()
    } else {
        &[]
    });
    for registry in expected_snapshot.registries() {
        let bytes = read(&mut client, &state, &mut scratch).await;
        let mut header = PacketReader::new(&bytes);
        assert_eq!(header.varint().unwrap(), 7);
        assert_eq!(header.identifier().unwrap(), registry.id());
        assert_eq!(header.varint().unwrap(), registry.entries().len() as i32);
        let mut remainder = &bytes[header.position()..];
        for entry in registry.entries() {
            let mut fields = PacketReader::new(remainder);
            assert_eq!(fields.identifier().unwrap(), entry.id());
            let present = fields.boolean().unwrap();
            let expected = negotiated.entry_contents(entry);
            assert_eq!(present, expected.is_some());
            remainder = &remainder[fields.position()..];
            if let Some(expected) = expected {
                let before = remainder;
                let _ = nbt::read_network(&mut remainder, nbt::Limits::default()).unwrap();
                let consumed = before.len() - remainder.len();
                assert_eq!(&before[..consumed], expected);
            }
        }
        assert!(remainder.is_empty());
    }
    let bytes = read(&mut client, &state, &mut scratch).await;
    let mut reader = PacketReader::new(&bytes);
    assert_eq!(reader.varint().unwrap(), 14);
    assert_eq!(
        reader.varint().unwrap(),
        expected_snapshot.tags().len() as i32
    );
    for registry in expected_snapshot.tags() {
        assert_eq!(reader.identifier().unwrap(), registry.registry());
        assert_eq!(reader.varint().unwrap(), registry.tags().len() as i32);
        for tag in registry.tags() {
            assert_eq!(reader.identifier().unwrap(), tag.id());
            assert_eq!(reader.varint().unwrap(), tag.members().len() as i32);
            for member in tag.members() {
                assert_eq!(reader.varint().unwrap(), *member);
            }
        }
    }
    reader.finish().unwrap();
    assert!(
        timeout(Duration::from_millis(40), client.read_u8())
            .await
            .is_err(),
        "FinishConfiguration must not be fabricated"
    );
    worker_shutdown.send(true).unwrap();
    let (result, stage, open) = timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_ok());
    assert_eq!(stage, SessionStage::Closed);
    assert!(!open);
    assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    drop(cpu);
}
#[test]
fn actual_tcp_configuration_sends_snapshot_order_and_stays_before_play() {
    run(async {
        let fixture = Fixture::new();
        let snapshot = Arc::new(fixture.load().unwrap());
        for threshold in [-1, 256] {
            for accept_known in [false, true] {
                exchange(Arc::clone(&snapshot), threshold, accept_known).await;
            }
        }
    });
}

#[test]
#[ignore = "requires verified local official configuration and an independently recorded manifest digest"]
fn actual_official_configuration_all_entries_and_tags_cross_tcp() {
    run(async {
        use arrow_mc::server::configuration_data::{
            ConfigurationSnapshot, ExpectedReference, LoadLimits, PackFingerprint,
            REFERENCE_PROTOCOL, REFERENCE_VERSION, parse_sha256,
        };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("Decompile/bootstrap/26.3-pre-2");
        let jar = parse_sha256("18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a")
            .unwrap();
        let manifest = std::env::var("ARROW_CONFIGURATION_MANIFEST_SHA256").unwrap_or_else(|_| {
            "105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198".into()
        });
        let packs = [PackFingerprint {
            id: "vanilla".into(),
            version: REFERENCE_VERSION.into(),
            sha256: jar,
        }];
        let expected = ExpectedReference {
            expected_manifest_sha256: parse_sha256(&manifest).unwrap(),
            minecraft_version: REFERENCE_VERSION,
            protocol: REFERENCE_PROTOCOL,
            source_jar_sha256: jar,
            source_jar_bytes: 26_649_663,
            selected_packs: &packs,
        };
        let snapshot =
            Arc::new(ConfigurationSnapshot::load(&root, &expected, LoadLimits::default()).unwrap());
        assert_eq!(
            snapshot
                .registries()
                .iter()
                .map(|registry| registry.entries().len())
                .sum::<usize>(),
            432
        );
        for accept_known in [false, true] {
            exchange(Arc::clone(&snapshot), 256, accept_known).await;
        }
    });
}

#[test]
fn real_idle_client_receives_keepalive_without_abandoning_partial_input() {
    run(async {
        let fixture = Fixture::new();
        let snapshot = Arc::new(fixture.load().unwrap());
        let cpu = pool();
        let (server, mut client) = pair().await;
        let (stop, mut shutdown) = watch::channel(false);
        let server_cpu = Arc::clone(&cpu);
        let task = tokio::spawn(async move {
            let mut transport =
                ConnectionTransport::new(server, server_cpu, TransportLimits::default());
            let mut session = ConfigurationSession::new(snapshot, "Arrow MC".into(), 0);
            configuration::run(&mut transport, &mut session, &mut shutdown).await
        });
        let state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        for _ in 0..3 {
            let _ = read(&mut client, &state, &mut scratch).await;
        }
        // A two-byte known-pack response frame is split before its body finishes.
        // The configuration timer must still send its challenge at fifteen seconds.
        client.write_all(&[2, 7]).await.unwrap();
        stop.send(false).unwrap();
        let bytes = timeout(Duration::from_secs(18), frame(&mut client))
            .await
            .unwrap();
        let packet = decode(&bytes, &state, &mut scratch);
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.varint().unwrap(), 4);
        let challenge = reader.long().unwrap();
        reader.finish().unwrap();
        client.write_all(&[0]).await.unwrap();
        let mut response = PacketWriter::new(16);
        response.varint(4).unwrap();
        response.long(challenge).unwrap();
        send(&mut client, &state, &mut scratch, response.as_bytes()).await;
        for _ in 0..32 {
            assert_eq!(read(&mut client, &state, &mut scratch).await[0], 7);
        }
        assert_eq!(read(&mut client, &state, &mut scratch).await[0], 14);
        stop.send(true).unwrap();
        assert!(
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
        assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    });
}

#[test]
fn premature_finish_on_socket_disconnects_in_configuration_protocol() {
    run(async {
        let fixture = Fixture::new();
        let snapshot = Arc::new(fixture.load().unwrap());
        let cpu = pool();
        let (server, mut client) = pair().await;
        let (_stop, mut shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut transport = ConnectionTransport::new(server, cpu, TransportLimits::default());
            let mut session = ConfigurationSession::new(snapshot, "Arrow MC".into(), 0);
            configuration::run(&mut transport, &mut session, &mut shutdown).await
        });
        let state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        for _ in 0..3 {
            let _ = read(&mut client, &state, &mut scratch).await;
        }
        send(&mut client, &state, &mut scratch, &[3]).await;
        let disconnect = read(&mut client, &state, &mut scratch).await;
        assert_eq!(disconnect[0], 2);
        let mut reason = &disconnect[1..];
        assert!(matches!(
            nbt::read_network(&mut reason, nbt::Limits::default()).unwrap(),
            nbt::Tag::String(_)
        ));
        assert!(
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    });
}
