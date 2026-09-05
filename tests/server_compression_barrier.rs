use arrow_mc::server::compression::{
    CompressionError, CompressionLimits, CompressionScratch, CompressionState,
};
use std::{io, task::Poll, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    time::{sleep, timeout},
};

async fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(1024).unwrap();
    let connect = socket.connect(listener.local_addr().unwrap());
    let (client, server) = tokio::join!(connect, listener.accept());
    (server.unwrap().0, client.unwrap())
}

async fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        let mut frame = Vec::new();
        let mut length = 0;
        for index in 0..3 {
            let byte = stream.read_u8().await.unwrap();
            frame.push(byte);
            length |= usize::from(byte & 127) << (index * 7);
            if byte & 128 == 0 {
                let prefix = frame.len();
                frame.resize(prefix + length, 0);
                stream.read_exact(&mut frame[prefix..]).await.unwrap();
                return frame;
            }
        }
        panic!("invalid transition frame");
    })
    .await
    .unwrap()
}

fn decode(state: &CompressionState, frame: &[u8], scratch: &mut CompressionScratch) -> Vec<u8> {
    let mut input = frame;
    let mut output = Vec::new();
    let mut allocation = 1_000_000;
    state
        .decode_frame(
            &mut input,
            scratch,
            &mut output,
            CompressionLimits::default(),
            &mut allocation,
        )
        .unwrap();
    assert!(input.is_empty());
    output
}

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

async fn peer_closed(client: &mut TcpStream) {
    timeout(Duration::from_secs(3), async {
        let mut buffer = [0; 8192];
        let mut received = 0;
        loop {
            match client.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    received += count;
                    assert!(received < 16 * 1024 * 1024);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("peer close: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[test]
fn dropping_an_unpolled_write_future_closes_socket_without_changing_mode() {
    run(async {
        let (server, mut client) = pair().await;
        let mut state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        let mut allocation = 1_000_000;
        let transition = state
            .prepare_threshold(
                256,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        drop(transition.write_threshold(server));
        assert_eq!(state.threshold().unwrap(), None);
        peer_closed(&mut client).await;
    });
}

#[test]
fn ordered_write_barrier_changes_mode_between_real_tcp_frames() {
    run(async {
        let (mut server, mut client) = pair().await;
        let mut state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        let mut allocation = 1_000_000;
        let mut before = Vec::new();
        state
            .encode_frame(
                b"\0before",
                &mut scratch,
                &mut before,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        server.write_all(&before).await.unwrap();
        let transition = state
            .prepare_threshold(
                16,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        // Preparation borrows connection state, but worker scratch is already
        // available to another connection while the transition awaits its write.
        let other = CompressionState::new(0);
        let mut worker_result = Vec::new();
        other
            .encode_frame(
                b"another connection",
                &mut scratch,
                &mut worker_result,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        server = transition.write_threshold(server).await.unwrap();
        assert_eq!(state.threshold().unwrap(), Some(16));
        let after = vec![42; 257];
        let mut after_frame = Vec::new();
        state
            .encode_frame(
                &after,
                &mut scratch,
                &mut after_frame,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        server.write_all(&after_frame).await.unwrap();

        assert_eq!(read_frame(&mut client).await, before);
        assert_eq!(read_frame(&mut client).await, vec![2, 3, 16]);
        assert_eq!(
            decode(&state, &read_frame(&mut client).await, &mut scratch),
            after
        );

        server = state
            .prepare_threshold(
                -1,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap()
            .write_threshold(server)
            .await
            .unwrap();
        let old_mode = CompressionState::new(16);
        assert_eq!(
            decode(&old_mode, &read_frame(&mut client).await, &mut scratch),
            vec![3, 255, 255, 255, 255, 15]
        );
        assert_eq!(state.threshold().unwrap(), None);
        let mut plain = Vec::new();
        state
            .encode_frame(
                b"\0plain",
                &mut scratch,
                &mut plain,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        server.write_all(&plain).await.unwrap();
        assert_eq!(read_frame(&mut client).await, plain);
    });
}

#[test]
fn pure_preparation_errors_and_unstarted_guards_keep_old_state() {
    let mut state = CompressionState::new(-1);
    let mut scratch = CompressionScratch::default();
    let mut allocation = 0;
    assert!(matches!(
        state.prepare_threshold(
            256,
            &mut scratch,
            CompressionLimits::default(),
            &mut allocation
        ),
        Err(CompressionError::AllocationLimit)
    ));
    assert_eq!(state.threshold().unwrap(), None);
    allocation = 1_000_000;
    drop(
        state
            .prepare_threshold(
                256,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap(),
    );
    assert_eq!(state.threshold().unwrap(), None);
    let limits = CompressionLimits {
        max_frame_body_bytes: 1,
        ..CompressionLimits::default()
    };
    assert!(matches!(
        state.prepare_threshold(256, &mut scratch, limits, &mut allocation),
        Err(CompressionError::FrameTooLarge)
    ));
    assert_eq!(state.threshold().unwrap(), None);
}

#[test]
fn failed_transition_write_poison_prevents_any_further_codec_use() {
    run(async {
        let (mut server, mut client) = pair().await;
        server.shutdown().await.unwrap();
        let mut state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        let mut allocation = 1_000_000;
        let transition = state
            .prepare_threshold(
                256,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        assert!(matches!(
            transition.write_threshold(server).await,
            Err(CompressionError::Io(_))
        ));
        assert!(matches!(
            state.threshold(),
            Err(CompressionError::UnusableState)
        ));
        let mut output = Vec::new();
        assert!(matches!(
            state.encode_frame(
                b"a",
                &mut scratch,
                &mut output,
                CompressionLimits::default(),
                &mut allocation
            ),
            Err(CompressionError::UnusableState)
        ));
        let mut input = &[1, 0][..];
        assert!(matches!(
            state.decode_frame(
                &mut input,
                &mut scratch,
                &mut output,
                CompressionLimits::default(),
                &mut allocation
            ),
            Err(CompressionError::UnusableState)
        ));
        peer_closed(&mut client).await;
    });
}

#[test]
fn cancellation_during_a_blocked_tcp_write_leaves_state_poisoned() {
    run(async {
        let (server, mut client) = pair().await;
        server.set_nodelay(true).unwrap();
        let filler = [0u8; 65536];
        // Saturate the peer's small receive window and local send buffer with
        // earlier traffic. This is a transport fault test, not a fake login.
        let mut total = 0usize;
        timeout(Duration::from_secs(2), server.writable())
            .await
            .unwrap()
            .unwrap();
        for _ in 0..3 {
            loop {
                match server.try_write(&filler) {
                    Ok(count) => {
                        total += count;
                        assert!(total < 16 * 1024 * 1024);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    other => panic!("filling send buffer: {other:?}"),
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
        assert!(total > 0);
        let mut state = CompressionState::new(-1);
        let mut scratch = CompressionScratch::default();
        let mut allocation = 1_000_000;
        let transition = state
            .prepare_threshold(
                256,
                &mut scratch,
                CompressionLimits::default(),
                &mut allocation,
            )
            .unwrap();
        let mut pending = Box::pin(transition.write_threshold(server));
        std::future::poll_fn(|context| {
            use std::future::Future;
            assert!(matches!(pending.as_mut().poll(context), Poll::Pending));
            Poll::Ready(())
        })
        .await;
        drop(pending);
        assert!(matches!(
            state.threshold(),
            Err(CompressionError::UnusableState)
        ));
        peer_closed(&mut client).await;
    });
}
