//! Runnable Java Edition handshake, status and ping transport.
//!
//! Login/gameplay are not implemented. Matching-version login receives an
//! explicit login-state disconnect; status polling reports zero online players.
//! One async task owns each connection's reads/writes. Tasks, input frames and
//! total admitted connection traffic are bounded without gameplay CPU work on
//! the I/O executor. The locked protocol behavior is documented in the tests.

pub mod compression;
pub mod configuration_data;
mod protocol;

use protocol::{Handshake, TrafficBudget, read_frame, write_frame};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

pub const MINECRAFT_VERSION: &str = "26.3 Pre-Release 2";
pub const PROTOCOL_VERSION: i32 = 1_073_742_158;

/// All valid packets in the implemented inbound states fit this bound: a
/// handshake with every VarInt at five bytes and 255 three-byte UTF-16 units.
pub const MAX_HANDSHAKE_FRAME_BYTES: usize = 5 + 5 + 5 + 255 * 3 + 2 + 5;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub description: String,
    pub max_players: u32,
    pub max_connections: usize,
    /// An absolute deadline for the entire finite status exchange. It does not
    /// reset on each incoming byte, preventing a slow peer retaining a task.
    pub connection_timeout: Duration,
    /// Total admitted application bytes, including both directions and framing.
    /// A full declared frame is reserved before reading/allocating its payload.
    pub max_connection_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 25565),
            description: "Arrow MC — server foundations (login unavailable)".into(),
            max_players: 20,
            max_connections: 256,
            connection_timeout: Duration::from_secs(30),
            max_connection_bytes: 256 * 1024,
        }
    }
}

pub struct Server {
    listener: TcpListener,
    shared: Arc<Shared>,
}

struct Shared {
    config: ServerConfig,
    status: Vec<u8>,
    login_unavailable: Vec<u8>,
    outdated_client: Vec<u8>,
    incompatible: Vec<u8>,
    transfers_disabled: Vec<u8>,
}

impl Server {
    /// Validates configuration and binds the actual TCP listener. Port zero is
    /// supported for OS-selected ephemeral ports used by integration tests.
    pub async fn bind(config: ServerConfig) -> io::Result<Self> {
        if config.max_connections == 0
            || config.max_connection_bytes == 0
            || config.connection_timeout.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connection limits and deadline must be nonzero",
            ));
        }
        if config.max_players > i32::MAX as u32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "max players exceeds signed protocol integer range",
            ));
        }
        if config.description.encode_utf16().count() > 32767 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "server description exceeds status string bound",
            ));
        }
        let status = protocol::json_packet(
            serde_json::json!({
            "description": config.description,
                "players": {"max": config.max_players, "online": 0},
                "version": {"name": MINECRAFT_VERSION, "protocol": PROTOCOL_VERSION}
            }),
            32767,
        )?;
        let login_unavailable = protocol::json_packet(
            serde_json::json!("Arrow MC: login and gameplay are not implemented yet."),
            262144,
        )?;
        let outdated_client = protocol::json_packet(
            serde_json::json!({"translate": "multiplayer.disconnect.outdated_client", "with": [MINECRAFT_VERSION]}),
            262144,
        )?;
        let incompatible = protocol::json_packet(
            serde_json::json!({"translate": "multiplayer.disconnect.incompatible", "with": [MINECRAFT_VERSION]}),
            262144,
        )?;
        let transfers_disabled = protocol::json_packet(
            serde_json::json!({"translate": "multiplayer.disconnect.transfers_disabled"}),
            262144,
        )?;
        let listener = TcpListener::bind(config.bind).await?;
        Ok(Self {
            listener,
            shared: Arc::new(Shared {
                config,
                status,
                login_unavailable,
                outdated_client,
                incompatible,
                transfers_disabled,
            }),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Runs until shutdown becomes true or its sender disappears. Excess
    /// accepted sockets are immediately closed before spawning a task. Shutdown
    /// aborts and joins every connection so no socket/task outlives this method.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let mut connections = JoinSet::new();
        let result = 'serving: loop {
            if *shutdown.borrow() {
                break Ok(());
            }
            // Completed task records count towards the bound until removed.
            // Drain them before admission to avoid rejecting reusable slots.
            while let Some(result) = connections.try_join_next() {
                if let Err(error) = result {
                    break 'serving Err(io::Error::other(error));
                }
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break Ok(()); }
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result { break Err(io::Error::other(error)); }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = match accepted { Ok(value) => value, Err(error) => break Err(error) };
                    if connections.len() >= self.shared.config.max_connections {
                        drop(stream);
                        continue;
                    }
                    let shared = Arc::clone(&self.shared);
                    connections.spawn(async move {
                        // Malformed input, timeouts and peer I/O failures close
                        // only this connection; they are not listener failures.
                        let _ = timeout(shared.config.connection_timeout, connection(stream, &shared)).await;
                    });
                }
            }
        };
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        result
    }
}

async fn connection(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut bytes = [0; MAX_HANDSHAKE_FRAME_BYTES];
    let mut budget = TrafficBudget::new(shared.config.max_connection_bytes);
    let length = read_frame(
        &mut stream,
        &mut bytes,
        MAX_HANDSHAKE_FRAME_BYTES,
        &mut budget,
    )
    .await?;
    let handshake = Handshake::parse(&bytes[..length])?;
    match handshake.intention {
        1 => {}
        2 | 3 => {
            let reason = if handshake.intention == 3 {
                &shared.transfers_disabled
            } else if handshake.protocol != PROTOCOL_VERSION {
                if handshake.protocol < 754 {
                    &shared.outdated_client
                } else {
                    &shared.incompatible
                }
            } else {
                &shared.login_unavailable
            };
            write_frame(&mut stream, reason, &mut budget).await?;
            return Ok(());
        }
        _ => unreachable!("handshake parser validates intentions"),
    }
    let mut requested = false;
    loop {
        let length = read_frame(&mut stream, &mut bytes, 13, &mut budget).await?;
        let (packet, consumed) =
            crate::wire::read_varint(&bytes[..length]).map_err(protocol::invalid)?;
        match (packet, &bytes[consumed..length]) {
            (0, []) if !requested => {
                requested = true;
                write_frame(&mut stream, &shared.status, &mut budget).await?;
            }
            (1, payload) if payload.len() == 8 => {
                let mut pong = [0; 10];
                pong[0] = 9;
                pong[1] = 1;
                pong[2..].copy_from_slice(payload);
                write_frame(&mut stream, &pong, &mut budget).await?;
                return Ok(());
            }
            _ => {
                return Err(protocol::invalid(
                    "unexpected or trailing status packet data",
                ));
            }
        }
    }
}
