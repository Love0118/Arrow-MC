use arrow_mc::{
    runtime::{CpuPool, CpuPoolConfig},
    server::{
        compression::{CompressionLimits, CompressionScratch, CompressionState},
        crypto::{CipherPair, DecryptCipher, EncryptCipher},
        transport::{ConnectionTransport, TransportError, TransportLimits},
    },
};
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, sleep, timeout},
};

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}
fn pool() -> Arc<CpuPool> {
    Arc::new(
        CpuPool::new(CpuPoolConfig {
            workers: 2,
            max_jobs: 8,
            buffer_bytes: 64 * 1024 * 1024,
        })
        .unwrap(),
    )
}
async fn pair(limits: TransportLimits) -> (ConnectionTransport, TcpStream, Arc<CpuPool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    );
    let cpu = pool();
    (
        ConnectionTransport::new(server.unwrap().0, Arc::clone(&cpu), limits),
        client.unwrap(),
        cpu,
    )
}
fn encode(packet: &[u8], threshold: i32, cipher: Option<&mut EncryptCipher>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut allocation = 32 * 1024 * 1024;
    CompressionState::new(threshold)
        .encode_frame(
            packet,
            &mut CompressionScratch::default(),
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    if let Some(cipher) = cipher {
        cipher.encrypt_in_place(&mut output).unwrap();
    }
    output
}
async fn read(
    stream: &mut TcpStream,
    threshold: i32,
    mut cipher: Option<&mut DecryptCipher>,
) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let mut frame = Vec::new();
        let mut length = 0;
        for index in 0..3 {
            let mut byte = [stream.read_u8().await.unwrap()];
            if let Some(cipher) = cipher.as_mut() {
                cipher.decrypt_in_place(&mut byte).unwrap();
            }
            frame.push(byte[0]);
            length |= usize::from(byte[0] & 127) << (index * 7);
            if byte[0] & 128 == 0 {
                break;
            }
        }
        let prefix = frame.len();
        frame.resize(prefix + length, 0);
        stream.read_exact(&mut frame[prefix..]).await.unwrap();
        if let Some(cipher) = cipher.as_mut() {
            cipher.decrypt_in_place(&mut frame[prefix..]).unwrap();
        }
        let mut input = frame.as_slice();
        let mut output = Vec::new();
        let mut allocation = 32 * 1024 * 1024;
        CompressionState::new(threshold)
            .decode_frame(
                &mut input,
                &mut CompressionScratch::default(),
                &mut output,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        assert!(input.is_empty());
        output
    })
    .await
    .unwrap()
}
async fn closed(stream: &mut TcpStream) {
    let result = timeout(Duration::from_secs(2), stream.read_u8())
        .await
        .unwrap();
    assert!(result.is_err());
}

#[test]
fn encrypted_partial_reads_survive_timers_and_allow_outbound_keepalive() {
    run(async {
        let (mut transport, mut client, cpu) = pair(TransportLimits::default()).await;
        let secret = [42; 16];
        transport.enable_encryption(secret).unwrap();
        let (mut encrypt, mut decrypt) = CipherPair::new(secret).unwrap().into_parts();
        let payload = vec![7; 300];
        let frame = encode(&payload, -1, Some(&mut encrypt));
        client.write_all(&frame[..1]).await.unwrap();
        assert!(
            transport
                .read_packet_until(Instant::now() + Duration::from_millis(10))
                .await
                .unwrap()
                .is_none()
        );
        assert!(transport.is_open());
        transport.write_packet(b"\x04keepalive").await.unwrap();
        assert_eq!(
            read(&mut client, -1, Some(&mut decrypt)).await,
            b"\x04keepalive"
        );
        client.write_all(&frame[1..80]).await.unwrap();
        assert!(
            transport
                .read_packet_until(Instant::now() + Duration::from_millis(10))
                .await
                .unwrap()
                .is_none()
        );
        assert!(transport.set_compression(256).await.is_err());
        assert!(transport.is_open());
        client.write_all(&frame[80..]).await.unwrap();
        let packet = transport.read_packet().await.unwrap();
        assert_eq!(packet.bytes(), payload);
        drop(packet);
        let next = encode(b"\x01next", -1, Some(&mut encrypt));
        client.write_all(&next).await.unwrap();
        let packet = transport.read_packet().await.unwrap();
        assert_eq!(packet.bytes(), b"\x01next");
        drop(packet);
        transport.close();
        closed(&mut client).await;
        drop(transport);
        assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
    });
}

#[test]
fn encrypted_compression_transition_preserves_mode_and_cipher_boundaries() {
    run(async {
        let (mut transport, mut client, _cpu) = pair(TransportLimits::default()).await;
        let secret = [11; 16];
        transport.enable_encryption(secret).unwrap();
        let (mut encrypt, mut decrypt) = CipherPair::new(secret).unwrap().into_parts();
        transport.write_packet(b"\x01hello").await.unwrap();
        transport.set_compression(128).await.unwrap();
        transport.write_packet(&vec![8; 300]).await.unwrap();
        assert_eq!(
            read(&mut client, -1, Some(&mut decrypt)).await,
            b"\x01hello"
        );
        assert_eq!(read(&mut client, -1, Some(&mut decrypt)).await, [3, 128, 1]);
        assert_eq!(
            read(&mut client, 128, Some(&mut decrypt)).await,
            vec![8; 300]
        );
        let combined = [
            encode(b"\x03", 128, Some(&mut encrypt)),
            encode(&vec![5; 500], 128, Some(&mut encrypt)),
        ]
        .concat();
        client.write_all(&combined).await.unwrap();
        let first = transport.read_packet().await.unwrap();
        assert_eq!(first.bytes(), [3]);
        drop(first);
        let second = transport.read_packet().await.unwrap();
        assert_eq!(second.bytes(), vec![5; 500]);
    });
}

#[test]
fn cancelled_read_future_retains_progress_and_idle_deadline_still_closes() {
    run(async {
        let (mut transport, mut client, _cpu) = pair(TransportLimits {
            read_idle_timeout: Duration::from_millis(80),
            ..TransportLimits::default()
        })
        .await;
        client.write_all(&[0x82]).await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), transport.read_packet())
                .await
                .is_err()
        );
        assert!(transport.is_open());
        client.write_all(&[0, 1]).await.unwrap();
        assert!(
            transport
                .read_packet_until(Instant::now() + Duration::from_millis(10))
                .await
                .unwrap()
                .is_none()
        );
        client.write_all(&[2]).await.unwrap();
        let packet = transport.read_packet().await.unwrap();
        assert_eq!(packet.bytes(), [1, 2]);
        drop(packet);
        sleep(Duration::from_millis(90)).await;
        assert!(matches!(
            transport
                .read_packet_until(Instant::now() + Duration::from_millis(1))
                .await,
            Err(TransportError::Timeout)
        ));
        assert!(!transport.is_open());
        closed(&mut client).await;
    });
}

#[test]
fn malformed_or_closed_read_cannot_reuse_socket() {
    run(async {
        for input in [&[0][..], &[0x80, 0x80, 0x80][..]] {
            let (mut transport, mut client, _) = pair(TransportLimits::default()).await;
            client.write_all(input).await.unwrap();
            assert!(matches!(
                transport.read_packet().await,
                Err(TransportError::FrameLength)
            ));
            assert!(!transport.is_open());
            closed(&mut client).await;
        }
        let (mut transport, mut client, _) = pair(TransportLimits::default()).await;
        client.shutdown().await.unwrap();
        assert!(
            matches!(transport.read_packet().await,Err(TransportError::Io(error)) if error.kind()==io::ErrorKind::UnexpectedEof)
        );
        assert!(!transport.is_open());
    });
}
