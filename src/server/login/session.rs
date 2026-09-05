//! Pure login state and tick boundaries owned by the connection driver.
//!
//! CPU/auth results are admitted only in their expected state. This module does
//! not fabricate authentication, configuration data, duplicate-player removal,
//! or world readiness; the driver supplies completed prerequisite results.

use super::{AuthenticatedProfile, LoginAccepted};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginPhase {
    Hello,
    Key,
    CheckingKey,
    Authenticating,
    Verifying,
    WaitingForDuplicate,
    ReadyToFinish,
    WritingFinished,
    AwaitingAcknowledgement,
    Accepted,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    UnexpectedPacket,
    InvalidName,
    SlowLogin,
    Closed,
}

impl fmt::Display for SessionError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::UnexpectedPacket => "unexpected login state transition",
            Self::InvalidName => "invalid login player name",
            Self::SlowLogin => "login exceeded 600 listener ticks",
            Self::Closed => "login session is closed",
        })
    }
}
impl std::error::Error for SessionError {}

pub struct LoginSession {
    phase: LoginPhase,
    requested_name: Option<String>,
    profile: Option<AuthenticatedProfile>,
    session_id: Option<[u8; 16]>,
    ticks: u16,
    transferred: bool,
}

impl LoginSession {
    pub fn new(transferred: bool) -> Self {
        Self {
            phase: LoginPhase::Hello,
            requested_name: None,
            profile: None,
            session_id: None,
            ticks: 0,
            transferred,
        }
    }

    pub fn phase(&self) -> LoginPhase {
        self.phase
    }
    pub fn name(&self) -> Option<&str> {
        self.requested_name.as_deref()
    }
    pub fn profile(&self) -> Option<&AuthenticatedProfile> {
        self.profile.as_ref()
    }
    pub fn close(&mut self) {
        self.phase = LoginPhase::Closed;
    }

    /// Current Vanilla accepts empty names and any ASCII 33..126 through 16
    /// UTF-16 units. Authentication may subsequently reject an empty account.
    pub fn receive_hello(&mut self, name: String) -> Result<(), SessionError> {
        self.require(LoginPhase::Hello)?;
        if name.len() > 16 || !name.bytes().all(|byte| (33..127).contains(&byte)) {
            return Err(SessionError::InvalidName);
        }
        self.requested_name = Some(name);
        self.phase = LoginPhase::Key;
        Ok(())
    }

    pub fn begin_key_verification(&mut self) -> Result<(), SessionError> {
        self.require(LoginPhase::Key)?;
        self.phase = LoginPhase::CheckingKey;
        Ok(())
    }

    /// Called only after crypto validation and encryption activation succeed.
    pub fn key_verified(&mut self) -> Result<(), SessionError> {
        self.require(LoginPhase::CheckingKey)?;
        self.phase = LoginPhase::Authenticating;
        Ok(())
    }

    pub fn authenticated(&mut self, profile: AuthenticatedProfile) -> Result<(), SessionError> {
        self.require(LoginPhase::Authenticating)?;
        self.profile = Some(profile);
        self.phase = LoginPhase::Verifying;
        Ok(())
    }

    /// Only the explicitly configured offline branch calls this with the
    /// separately derived name UUID, never the UUID supplied in Hello.
    pub fn offline_profile(&mut self, profile: AuthenticatedProfile) -> Result<(), SessionError> {
        self.require(LoginPhase::Key)?;
        self.profile = Some(profile);
        self.phase = LoginPhase::Verifying;
        Ok(())
    }

    pub fn admitted(
        &mut self,
        session_id: [u8; 16],
        duplicate_pending: bool,
    ) -> Result<(), SessionError> {
        self.require(LoginPhase::Verifying)?;
        self.session_id = Some(session_id);
        self.phase = if duplicate_pending {
            LoginPhase::WaitingForDuplicate
        } else {
            LoginPhase::ReadyToFinish
        };
        Ok(())
    }

    pub fn duplicate_removed(&mut self) -> Result<(), SessionError> {
        self.require(LoginPhase::WaitingForDuplicate)?;
        self.phase = LoginPhase::ReadyToFinish;
        Ok(())
    }

    pub fn begin_finished_write(&mut self) -> Result<[u8; 16], SessionError> {
        self.require(LoginPhase::ReadyToFinish)?;
        self.phase = LoginPhase::WritingFinished;
        self.session_id.ok_or(SessionError::UnexpectedPacket)
    }

    pub fn finished_written(&mut self) -> Result<(), SessionError> {
        self.require(LoginPhase::WritingFinished)?;
        self.phase = LoginPhase::AwaitingAcknowledgement;
        Ok(())
    }

    pub fn acknowledge(&mut self) -> Result<LoginAccepted, SessionError> {
        self.require(LoginPhase::AwaitingAcknowledgement)?;
        let profile = self.profile.take().ok_or(SessionError::UnexpectedPacket)?;
        let session_id = self.session_id.ok_or(SessionError::UnexpectedPacket)?;
        self.phase = LoginPhase::Accepted;
        Ok(LoginAccepted {
            profile,
            session_id,
            transferred: self.transferred,
        })
    }

    /// Invoke after that listener tick's verification/duplicate work. Exactly
    /// 600 calls succeed; the 601st fails, matching `tick++ == 600`.
    pub fn tick(&mut self) -> Result<(), SessionError> {
        if self.phase == LoginPhase::Closed {
            return Err(SessionError::Closed);
        }
        if self.phase == LoginPhase::Accepted {
            return Ok(());
        }
        if self.ticks == 600 {
            self.close();
            return Err(SessionError::SlowLogin);
        }
        self.ticks += 1;
        Ok(())
    }

    fn require(&self, phase: LoginPhase) -> Result<(), SessionError> {
        if self.phase == LoginPhase::Closed {
            Err(SessionError::Closed)
        } else if self.phase != phase {
            Err(SessionError::UnexpectedPacket)
        } else {
            Ok(())
        }
    }
}

use crate::{
    runtime::{AdmissionError, LoginKeyJobError, PacketJobOutput},
    server::{
        LoginServices,
        auth::AuthError,
        configuration::{self, ConfigurationSession},
        crypto::{self, CryptoError},
        packet::PacketError,
        population::ConnectionLease,
        transport::{ConnectionTransport, TransportError},
    },
};
use std::{future::Future, net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};

#[derive(Debug)]
pub enum LoginError {
    Session(SessionError),
    Packet(PacketError),
    Transport(TransportError),
    Crypto(CryptoError),
    Admission(AdmissionError),
    KeyWorker(LoginKeyJobError),
    Auth(AuthError),
    Configuration(configuration::DriverError),
    Io(std::io::Error),
    Rejected(serde_json::Value),
    Shutdown,
}

impl fmt::Display for LoginError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => error.fmt(output),
            Self::Packet(error) => error.fmt(output),
            Self::Transport(error) => error.fmt(output),
            Self::Crypto(error) => error.fmt(output),
            Self::Admission(error) => error.fmt(output),
            Self::KeyWorker(error) => error.fmt(output),
            Self::Auth(error) => error.fmt(output),
            Self::Configuration(error) => error.fmt(output),
            Self::Io(error) => error.fmt(output),
            Self::Rejected(_) => output.write_str("login rejected"),
            Self::Shutdown => output.write_str("server shutdown during login"),
        }
    }
}
impl std::error::Error for LoginError {}

/// Real online login (or the explicit offline configuration branch). The
/// population lease lives through configuration; no player is registered here.
pub async fn run_login(
    mut transport: ConnectionTransport,
    services: &LoginServices,
    lease: ConnectionLease,
    transferred: bool,
    remote: IpAddr,
) -> Result<(), LoginError> {
    let mut session = LoginSession::new(transferred);
    let mut clock = LoginClock {
        next: Instant::now() + Duration::from_millis(50),
    };
    let mut shutdown = services.shutdown.clone();
    let result = login_flow(
        &mut transport,
        services,
        &lease,
        &mut session,
        &mut clock,
        &mut shutdown,
        remote,
    )
    .await;
    if let Err(error) = &result {
        // A post-ack configuration error belongs to its new protocol; do not
        // inject a login JSON packet into the configuration byte stream.
        if session.phase() != LoginPhase::Accepted && transport.is_open() {
            let reason = match error {
                LoginError::Rejected(reason) => reason.clone(),
                LoginError::Session(SessionError::SlowLogin) => {
                    serde_json::json!({"translate":"multiplayer.disconnect.slow_login"})
                }
                LoginError::Auth(error) if error.is_unavailable() => {
                    serde_json::json!({"translate":"multiplayer.disconnect.authservers_down"})
                }
                LoginError::Auth(_) => {
                    serde_json::json!({"translate":"multiplayer.disconnect.unverified_username"})
                }
                LoginError::Shutdown => {
                    serde_json::json!({"translate":"multiplayer.disconnect.server_shutdown"})
                }
                _ => serde_json::json!({"translate":"multiplayer.disconnect.invalid_packet"}),
            };
            if let Ok(packet) = super::packet::disconnect(reason, 1024 * 1024) {
                let _ = transport.write_packet(&packet).await;
            }
        }
        session.close();
        transport.close();
    }
    drop(lease);
    result
}

async fn login_flow(
    transport: &mut ConnectionTransport,
    services: &LoginServices,
    lease: &ConnectionLease,
    session: &mut LoginSession,
    clock: &mut LoginClock,
    shutdown: &mut watch::Receiver<bool>,
    remote: IpAddr,
) -> Result<(), LoginError> {
    // A validated, actually loaded snapshot is required before any successful
    // login publication. The configuration session still waits for real spawn.
    let mut configuration =
        ConfigurationSession::new(Arc::clone(&services.snapshot), "Arrow MC".into(), 0);
    let received = clock.receive(transport, session, shutdown).await?;
    let name = match super::packet::decode(received.bytes()).map_err(LoginError::Packet)? {
        super::packet::LoginPacket::Hello {
            name,
            claimed_id: _,
        } => name,
        super::packet::LoginPacket::QueryAnswer { .. }
        | super::packet::LoginPacket::CookieResponse { .. } => return Err(unexpected_query()),
        _ => return Err(LoginError::Session(SessionError::UnexpectedPacket)),
    };
    drop(received);
    session.receive_hello(name).map_err(LoginError::Session)?;
    if services.online_mode {
        let challenge = services.key.challenge().map_err(LoginError::Crypto)?;
        let hello = super::packet::hello(services.key.public_key_der(), &challenge, 4096)
            .map_err(LoginError::Packet)?;
        clock
            .wait(session, shutdown, async {
                transport
                    .write_packet(&hello)
                    .await
                    .map_err(LoginError::Transport)
            })
            .await?;
        drop(hello);
        let response = clock.receive(transport, session, shutdown).await?;
        let (encrypted_secret, encrypted_challenge) = match super::packet::decode(response.bytes())
            .map_err(LoginError::Packet)?
        {
            super::packet::LoginPacket::Key {
                encrypted_secret,
                encrypted_challenge,
            } => (encrypted_secret, encrypted_challenge),
            super::packet::LoginPacket::QueryAnswer { .. }
            | super::packet::LoginPacket::CookieResponse { .. } => return Err(unexpected_query()),
            _ => return Err(LoginError::Session(SessionError::UnexpectedPacket)),
        };
        if encrypted_secret.is_empty()
            || encrypted_challenge.is_empty()
            || encrypted_secret.len() > 128
            || encrypted_challenge.len() > 128
        {
            return Err(LoginError::Crypto(CryptoError::InvalidKeyResponse));
        }
        let mut pending = services
            .cpu
            .try_reserve_login_key(Arc::clone(&services.key), challenge)
            .map_err(LoginError::Admission)?;
        let destination = pending.encrypted_secret_mut();
        destination.fill(0);
        destination[128 - encrypted_secret.len()..].copy_from_slice(encrypted_secret);
        let destination = pending.encrypted_challenge_mut();
        destination.fill(0);
        destination[128 - encrypted_challenge.len()..].copy_from_slice(encrypted_challenge);
        drop(response);
        session
            .begin_key_verification()
            .map_err(LoginError::Session)?;
        let task = pending.submit().map_err(LoginError::Admission)?;
        let verified = clock
            .wait(session, shutdown, async {
                task.wait().await.map_err(LoginError::KeyWorker)
            })
            .await?;
        transport
            .enable_encryption(verified.secret().shared_secret)
            .map_err(LoginError::Transport)?;
        session.key_verified().map_err(LoginError::Session)?;
        let server_hash = verified.secret().server_hash.clone();
        // The shared CPU slot/buffers are no longer needed during HTTP wait.
        // The digest is bounded to forty hex digits plus an optional sign.
        drop(verified);
        let username = session.name().unwrap().to_owned();
        let mut auth_cancel = shutdown.clone();
        let address = services.prevent_proxy_connections.then_some(remote);
        let authenticated = {
            let mut authentication = std::pin::pin!(services.auth.has_joined(
                &username,
                &server_hash,
                address,
                &mut auth_cancel
            ));
            loop {
                if *shutdown.borrow() {
                    return Err(LoginError::Shutdown);
                }
                tokio::select! {
                    biased;
                    incoming = transport.read_packet_until(clock.next) => {
                        if let Some(incoming) = incoming.map_err(LoginError::Transport)? {
                            return Err(match super::packet::decode(incoming.bytes()).map_err(LoginError::Packet)? {
                                super::packet::LoginPacket::QueryAnswer { .. } | super::packet::LoginPacket::CookieResponse { .. } => unexpected_query(),
                                _ => LoginError::Session(SessionError::UnexpectedPacket),
                            });
                        }
                        clock.tick(session)?;
                    }
                    profile = &mut authentication => break profile.map_err(LoginError::Auth)?,
                    changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Err(LoginError::Shutdown); } }
                }
            }
        };
        let profile = authenticated.ok_or_else(|| {
            LoginError::Rejected(
                serde_json::json!({"translate":"multiplayer.disconnect.unverified_username"}),
            )
        })?;
        session
            .authenticated(profile)
            .map_err(LoginError::Session)?;
    } else {
        let name = session.name().unwrap().to_owned();
        let profile = AuthenticatedProfile {
            id: crypto::offline_uuid(&name).map_err(LoginError::Crypto)?,
            name,
            properties: Vec::new(),
        };
        session
            .offline_profile(profile)
            .map_err(LoginError::Session)?;
    }

    // The current server has no published world players; configuring sockets
    // are not a fake player population. Real owner admission extends this point.
    if let Some(reason) = services.access.check(session.profile().unwrap(), remote, 0) {
        return Err(LoginError::Rejected(reason));
    }
    if services.compression_threshold >= 0 {
        clock
            .wait(session, shutdown, async {
                transport
                    .set_compression(services.compression_threshold)
                    .await
                    .map_err(LoginError::Transport)
            })
            .await?;
    }
    session
        .admitted(lease.session_uuid().map_err(LoginError::Io)?, false)
        .map_err(LoginError::Session)?;
    let session_id = session
        .begin_finished_write()
        .map_err(LoginError::Session)?;
    let finished =
        super::packet::finished(session.profile().unwrap(), session_id, 2 * 1024 * 1024 - 1)
            .map_err(LoginError::Packet)?;
    clock
        .wait(session, shutdown, async {
            transport
                .write_packet(&finished)
                .await
                .map_err(LoginError::Transport)
        })
        .await?;
    drop(finished);
    session.finished_written().map_err(LoginError::Session)?;
    let acknowledged = clock.receive(transport, session, shutdown).await?;
    match super::packet::decode(acknowledged.bytes()).map_err(LoginError::Packet)? {
        super::packet::LoginPacket::Acknowledged => {}
        super::packet::LoginPacket::QueryAnswer { .. }
        | super::packet::LoginPacket::CookieResponse { .. } => return Err(unexpected_query()),
        _ => return Err(LoginError::Session(SessionError::UnexpectedPacket)),
    }
    drop(acknowledged);
    let accepted = session.acknowledge().map_err(LoginError::Session)?;
    let result = configuration::run(transport, &mut configuration, shutdown)
        .await
        .map_err(LoginError::Configuration);
    drop(accepted);
    result
}

fn unexpected_query() -> LoginError {
    LoginError::Rejected(
        serde_json::json!({"translate":"multiplayer.disconnect.unexpected_query_response"}),
    )
}

struct LoginClock {
    next: Instant,
}

impl LoginClock {
    fn tick(&mut self, session: &mut LoginSession) -> Result<(), LoginError> {
        session.tick().map_err(LoginError::Session)?;
        self.next += Duration::from_millis(50);
        Ok(())
    }

    async fn receive(
        &mut self,
        transport: &mut ConnectionTransport,
        session: &mut LoginSession,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<PacketJobOutput, LoginError> {
        loop {
            if *shutdown.borrow() {
                return Err(LoginError::Shutdown);
            }
            tokio::select! {
                biased;
                packet = transport.read_packet_until(self.next) => {
                    if let Some(packet) = packet.map_err(LoginError::Transport)? { return Ok(packet); }
                    self.tick(session)?;
                }
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Err(LoginError::Shutdown); } }
            }
        }
    }

    async fn wait<T>(
        &mut self,
        session: &mut LoginSession,
        shutdown: &mut watch::Receiver<bool>,
        future: impl Future<Output = Result<T, LoginError>>,
    ) -> Result<T, LoginError> {
        let mut future = std::pin::pin!(future);
        loop {
            if *shutdown.borrow() {
                return Err(LoginError::Shutdown);
            }
            tokio::select! {
                biased;
                result = &mut future => return result,
                _ = sleep_until(self.next) => self.tick(session)?,
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Err(LoginError::Shutdown); } }
            }
        }
    }
}
