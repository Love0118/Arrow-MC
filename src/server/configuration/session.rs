use super::packet::{self, ClientInformation, Clientbound, Serverbound};
use crate::server::{configuration_data::ConfigurationSnapshot, packet::PacketError};
use std::{fmt, sync::Arc};

const KEEP_ALIVE_INTERVAL_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStage {
    Initializing,
    AwaitingKnownPacks,
    SendingRegistries,
    SendingTags,
    AwaitingSpawn,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingWrite {
    Prefix,
    Registry,
    Tags,
    KeepAlive { i64_id: i64, at_ms: u64 },
}

#[derive(Debug)]
pub enum SessionError {
    Packet(PacketError),
    Unexpected(&'static str),
    KeepAliveTimeout,
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packet(error) => error.fmt(f),
            Self::Unexpected(message) => f.write_str(message),
            Self::KeepAliveTimeout => f.write_str("configuration keepalive timed out"),
        }
    }
}
impl std::error::Error for SessionError {}
impl From<PacketError> for SessionError {
    fn from(value: PacketError) -> Self {
        Self::Packet(value)
    }
}

/// Connection-owned state. It serializes one packet at a time from one shared
/// immutable snapshot, and advances publication only after the write completes.
/// There is deliberately no synthetic spawn-ready flag or FinishConfiguration.
pub struct ConfigurationSession {
    snapshot: Arc<ConfigurationSnapshot>,
    brand: String,
    stage: SessionStage,
    prefix_index: usize,
    registry_index: usize,
    packs_matched: bool,
    pending_write: Option<PendingWrite>,
    queued_keepalive: Option<(i64, u64)>,
    pending_keepalive: Option<i64>,
    keepalive_time: u64,
    latency: i32,
    client_information: ClientInformation,
}

impl ConfigurationSession {
    pub fn new(snapshot: Arc<ConfigurationSnapshot>, brand: String, now_ms: u64) -> Self {
        Self {
            snapshot,
            brand,
            stage: SessionStage::Initializing,
            prefix_index: 0,
            registry_index: 0,
            packs_matched: false,
            pending_write: None,
            queued_keepalive: None,
            pending_keepalive: None,
            keepalive_time: now_ms,
            latency: 0,
            client_information: ClientInformation::default(),
        }
    }
    pub fn stage(&self) -> SessionStage {
        self.stage
    }
    pub fn client_information(&self) -> &ClientInformation {
        &self.client_information
    }
    pub fn latency(&self) -> i32 {
        self.latency
    }
    pub fn next_keepalive_ms(&self) -> u64 {
        self.keepalive_time.saturating_add(KEEP_ALIVE_INTERVAL_MS)
    }
    pub fn snapshot(&self) -> &ConfigurationSnapshot {
        &self.snapshot
    }
    pub fn known_packs_matched(&self) -> bool {
        self.packs_matched
    }
    pub fn close(&mut self) {
        self.stage = SessionStage::Closed;
        self.pending_write = None;
        self.queued_keepalive = None;
        self.pending_keepalive = None;
    }

    pub fn next_outbound(
        &mut self,
        max_packet_bytes: usize,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        if self.pending_write.is_some() {
            return Err(SessionError::Unexpected(
                "previous configuration write is not complete",
            ));
        }
        if self.stage == SessionStage::Closed {
            return Ok(None);
        }
        if let Some((id, at_ms)) = self.queued_keepalive {
            let bytes = packet::encode(Clientbound::KeepAlive(id), max_packet_bytes)?;
            self.pending_write = Some(PendingWrite::KeepAlive { i64_id: id, at_ms });
            return Ok(Some(bytes));
        }
        let (bytes, pending) = match self.stage {
            SessionStage::Initializing => {
                let value = match self.prefix_index {
                    0 => Clientbound::Brand(&self.brand),
                    1 => Clientbound::EnabledFeatures(self.snapshot.features()),
                    _ => Clientbound::SelectKnownPacks(self.snapshot.known_packs()),
                };
                (
                    packet::encode(value, max_packet_bytes)?,
                    PendingWrite::Prefix,
                )
            }
            SessionStage::SendingRegistries => {
                let response = if self.packs_matched {
                    self.snapshot.known_packs()
                } else {
                    &[]
                };
                let negotiated = self.snapshot.negotiate_known_packs(response);
                let registry = &self.snapshot.registries()[self.registry_index];
                (
                    packet::encode(
                        Clientbound::Registry {
                            registry,
                            negotiated: &negotiated,
                        },
                        max_packet_bytes,
                    )?,
                    PendingWrite::Registry,
                )
            }
            SessionStage::SendingTags => (
                packet::encode(Clientbound::UpdateTags(&self.snapshot), max_packet_bytes)?,
                PendingWrite::Tags,
            ),
            SessionStage::AwaitingKnownPacks
            | SessionStage::AwaitingSpawn
            | SessionStage::Closed => return Ok(None),
        };
        self.pending_write = Some(pending);
        Ok(Some(bytes))
    }

    pub fn outbound_written(&mut self) -> Result<(), SessionError> {
        match self.pending_write.take().ok_or(SessionError::Unexpected(
            "no configuration write is pending",
        ))? {
            PendingWrite::Prefix => {
                self.prefix_index += 1;
                if self.prefix_index == 3 {
                    self.stage = SessionStage::AwaitingKnownPacks;
                }
            }
            PendingWrite::Registry => {
                self.registry_index += 1;
                if self.registry_index == self.snapshot.registries().len() {
                    self.stage = SessionStage::SendingTags;
                }
            }
            PendingWrite::Tags => self.stage = SessionStage::AwaitingSpawn,
            PendingWrite::KeepAlive { i64_id, at_ms } => {
                self.queued_keepalive = None;
                self.pending_keepalive = Some(i64_id);
                self.keepalive_time = at_ms;
            }
        }
        Ok(())
    }

    pub fn on_packet(
        &mut self,
        input: &[u8],
        now_ms: u64,
        max_packet_bytes: usize,
    ) -> Result<(), SessionError> {
        if self.stage == SessionStage::Closed {
            return Err(SessionError::Unexpected(
                "configuration connection is closed",
            ));
        }
        let packet = match packet::decode(input, max_packet_bytes) {
            Ok(packet) => packet,
            Err(error) => {
                self.close();
                return Err(error.into());
            }
        };
        let result = match packet {
            Serverbound::ClientInformation(information) => {
                self.client_information = information;
                Ok(())
            }
            Serverbound::SelectKnownPacks(packs) => {
                if self.stage != SessionStage::AwaitingKnownPacks {
                    Err(SessionError::Unexpected(
                        "known pack response does not match the current configuration task",
                    ))
                } else {
                    self.packs_matched = packs.matches(self.snapshot.known_packs())?;
                    self.stage = SessionStage::SendingRegistries;
                    Ok(())
                }
            }
            Serverbound::KeepAlive(id) => {
                if self.pending_keepalive == Some(id) {
                    let elapsed = now_ms.saturating_sub(self.keepalive_time) as i32;
                    self.latency = self.latency.wrapping_mul(3).wrapping_add(elapsed) / 4;
                    self.pending_keepalive = None;
                    Ok(())
                } else {
                    Err(SessionError::KeepAliveTimeout)
                }
            }
            Serverbound::CookieResponse { .. } => {
                Err(SessionError::Unexpected("unexpected cookie response"))
            }
            Serverbound::FinishConfiguration => Err(SessionError::Unexpected(
                "finish configuration received before the actual spawn prerequisite",
            )),
            Serverbound::AcceptCodeOfConduct => Err(SessionError::Unexpected(
                "code of conduct acceptance has no active task",
            )),
            Serverbound::ResourcePack { action, .. } if action.is_terminal() => Err(
                SessionError::Unexpected("terminal resource pack response has no active task"),
            ),
            // Vanilla's default common listener ignores these; custom click is
            // debug-log only in the locked dedicated MinecraftServer.
            Serverbound::ResourcePack { .. }
            | Serverbound::Pong(_)
            | Serverbound::CustomPayload { .. }
            | Serverbound::CustomClick { .. } => Ok(()),
        };
        if result.is_err() {
            self.close();
        }
        result
    }

    pub fn tick(&mut self, now_ms: u64) -> Result<(), SessionError> {
        if self.stage == SessionStage::Closed {
            return Ok(());
        }
        if now_ms.saturating_sub(self.keepalive_time) >= KEEP_ALIVE_INTERVAL_MS {
            if self.pending_keepalive.is_some() {
                self.close();
                return Err(SessionError::KeepAliveTimeout);
            }
            if self.queued_keepalive.is_none() {
                self.queued_keepalive = Some((now_ms as i64, now_ms));
            }
        }
        Ok(())
    }
}
