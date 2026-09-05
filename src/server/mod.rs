//! Java Edition status, login and configuration connection ownership.
//!
//! Verified configuration services enable real login; without them a matching
//! login receives an explicit disconnect. No world players are published yet.
//! One async task owns each connection's reads/writes. Tasks, input frames and
//! total admitted connection traffic are bounded without gameplay CPU work on
//! the I/O executor. The locked protocol behavior is documented in the tests.

pub mod access;
pub mod auth;
pub mod chunk_packet;
pub mod chunk_sender;
pub mod compression;
pub mod configuration;
pub mod configuration_data;
pub mod crypto;
pub mod login;
pub mod packet;
pub mod population;
mod protocol;
pub mod transport;

use protocol::{Handshake, TrafficBudget, read_frame, write_frame};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
    time::{Instant, timeout_at},
};

/// Shared resources for real login and configuration. Construct once before
/// listening; per-connection clones retain only shared handles and a shutdown
/// receiver. Player publication remains the world's responsibility.
#[derive(Clone)]
pub struct LoginServices {
    pub key: Arc<crypto::ServerKey>,
    pub auth: Arc<auth::AuthClient>,
    pub cpu: Arc<crate::runtime::CpuPool>,
    pub snapshot: Arc<configuration_data::ConfigurationSnapshot>,
    pub access: Arc<access::LoginAccess>,
    pub compression_threshold: i32,
    pub online_mode: bool,
    pub prevent_proxy_connections: bool,
    pub accepts_transfers: bool,
    /// Bounds expensive login/configuration sockets independently of status.
    pub max_login_connections: usize,
    pub shutdown: watch::Receiver<bool>,
}

pub const MINECRAFT_VERSION: &str = "26.3 Pre-Release 2";
pub const PROTOCOL_VERSION: i32 = 1_073_742_158;

/// Conservative processing admission per configured socket beyond CPU packet
/// leases, covering decoder/output temporaries. Not measured RSS; shared
/// snapshot, native TLS/crypto, stacks and allocator overhead are separate.
pub const LOGIN_PROCESSING_ALLOWANCE: usize = 32 * 1024 * 1024;

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
    login: Option<LoginServices>,
    login_slots: Arc<Semaphore>,
    population: population::ConnectionPopulation,
}

impl Server {
    /// Validates configuration and binds the actual TCP listener. Port zero is
    /// supported for OS-selected ephemeral ports used by integration tests.
    pub async fn bind(config: ServerConfig) -> io::Result<Self> {
        Self::bind_inner(config, None).await
    }

    /// Enables genuine login/configuration against a verified data snapshot.
    /// Authentication defaults are chosen by the supplied services; the CLI
    /// selects online mode unless offline mode is explicitly requested.
    pub async fn bind_with_login(
        config: ServerConfig,
        services: LoginServices,
    ) -> io::Result<Self> {
        if services.max_login_connections == 0
            || services.max_login_connections > Semaphore::MAX_PERMITS
            || services
                .max_login_connections
                .checked_mul(LOGIN_PROCESSING_ALLOWANCE)
                .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid login connection admission limit",
            ));
        }
        Self::bind_inner(config, Some(services)).await
    }

    async fn bind_inner(config: ServerConfig, login: Option<LoginServices>) -> io::Result<Self> {
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
        let population = population::ConnectionPopulation::new(config.max_connections)?;
        let login_slots = Arc::new(Semaphore::new(
            login
                .as_ref()
                .map_or(0, |services| services.max_login_connections),
        ));
        Ok(Self {
            listener,
            shared: Arc::new(Shared {
                config,
                status,
                login_unavailable,
                outdated_client,
                incompatible,
                transfers_disabled,
                login,
                login_slots,
                population,
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
        let mut maintenance = tokio::time::interval(Duration::from_millis(50));
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                _ = maintenance.tick() => self.shared.population.maintain(),
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
                    let lease = match self.shared.population.try_admit() {
                        Ok(Some(lease)) => lease,
                        Ok(None) => { drop(stream); continue; }
                        Err(error) => break Err(error),
                    };
                    let shared = Arc::clone(&self.shared);
                    let stop = shutdown.clone();
                    connections.spawn(async move {
                        // Malformed input, timeouts and peer I/O failures close
                        // only this connection; they are not listener failures.
                        let _ = connection(stream, &shared, lease, stop).await;
                    });
                }
            }
        };
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        result
    }
}

async fn connection(
    mut stream: TcpStream,
    shared: &Shared,
    lease: population::ConnectionLease,
    shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut bytes = [0; MAX_HANDSHAKE_FRAME_BYTES];
    let mut budget = TrafficBudget::new(shared.config.max_connection_bytes);
    let deadline = Instant::now() + shared.config.connection_timeout;
    let length = timeout_at(
        deadline,
        read_frame(
            &mut stream,
            &mut bytes,
            MAX_HANDSHAKE_FRAME_BYTES,
            &mut budget,
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake deadline exceeded"))??;
    let handshake = Handshake::parse(&bytes[..length])?;
    match handshake.intention {
        1 => {}
        2 | 3 => {
            let accepts_transfers = shared
                .login
                .as_ref()
                .is_some_and(|services| services.accepts_transfers);
            let reason = if handshake.intention == 3 && !accepts_transfers {
                &shared.transfers_disabled
            } else if handshake.protocol != PROTOCOL_VERSION {
                if handshake.protocol < 754 {
                    &shared.outdated_client
                } else {
                    &shared.incompatible
                }
            } else {
                if let Some(services) = &shared.login {
                    // Login/configuration residency is bounded independently of
                    // the finite status traffic/deadline policy.
                    let _login_slot = match Arc::clone(&shared.login_slots).try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => return Ok(()),
                    };
                    let remote = stream.peer_addr()?.ip();
                    let mut services = services.clone();
                    services.shutdown = shutdown;
                    let transport = transport::ConnectionTransport::new(
                        stream,
                        Arc::clone(&services.cpu),
                        transport::TransportLimits::default(),
                    );
                    return login::session::run_login(
                        transport,
                        &services,
                        lease,
                        handshake.intention == 3,
                        remote,
                    )
                    .await
                    .map_err(io::Error::other);
                }
                &shared.login_unavailable
            };
            timeout_at(deadline, write_frame(&mut stream, reason, &mut budget))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "handshake response deadline exceeded",
                    )
                })??;
            return Ok(());
        }
        _ => unreachable!("handshake parser validates intentions"),
    }
    timeout_at(
        deadline,
        status(&mut stream, shared, &mut bytes, &mut budget),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "status deadline exceeded"))?
}

async fn status(
    stream: &mut TcpStream,
    shared: &Shared,
    bytes: &mut [u8],
    budget: &mut TrafficBudget,
) -> io::Result<()> {
    let mut requested = false;
    loop {
        let length = read_frame(stream, bytes, 13, budget).await?;
        let (packet, consumed) =
            crate::wire::read_varint(&bytes[..length]).map_err(protocol::invalid)?;
        match (packet, &bytes[consumed..length]) {
            (0, []) if !requested => {
                requested = true;
                write_frame(stream, &shared.status, budget).await?;
            }
            (1, payload) if payload.len() == 8 => {
                let mut pong = [0; 10];
                pong[0] = 9;
                pong[1] = 1;
                pong[2..].copy_from_slice(payload);
                write_frame(stream, &pong, budget).await?;
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
