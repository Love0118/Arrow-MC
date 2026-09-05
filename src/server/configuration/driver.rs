//! Socket sequencing for configuration; spawn readiness belongs to the world
//! owner and is deliberately not represented by a placeholder boolean here.

use super::{
    ConfigurationSession, SessionError, SessionStage,
    packet::{self, Clientbound},
};
use crate::server::transport::{ConnectionTransport, TransportError};
use std::{fmt, time::Duration};
use tokio::{sync::watch, time::Instant};

#[derive(Debug)]
pub enum DriverError {
    Session(SessionError),
    Transport(TransportError),
}
impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for DriverError {}

/// The caller retains the authenticated profile and connection lease while
/// this future owns configuration progress. Shutdown/error closes both objects.
/// Read deadline events preserve partial transport/cipher progress; only the
/// explicit shutdown branch cancels a read and closes the connection.
pub async fn run(
    transport: &mut ConnectionTransport,
    session: &mut ConfigurationSession,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), DriverError> {
    let origin = Instant::now();
    let outcome = run_inner(transport, session, shutdown, origin).await;
    if let Err(DriverError::Session(error)) = &outcome {
        // The read may have been valid framing with invalid task data, so try
        // one correctly framed disconnect before releasing the connection.
        if let Ok(packet) = packet::encode(Clientbound::Disconnect(&error.to_string()), 4096) {
            let _ = transport.write_packet(&packet).await;
        }
    }
    session.close();
    transport.close();
    outcome
}

async fn run_inner(
    transport: &mut ConnectionTransport,
    session: &mut ConfigurationSession,
    shutdown: &mut watch::Receiver<bool>,
    origin: Instant,
) -> Result<(), DriverError> {
    loop {
        if session.stage() == SessionStage::Closed {
            return Err(DriverError::Session(SessionError::Unexpected(
                "configuration session is already closed",
            )));
        }
        if *shutdown.borrow() {
            return Ok(());
        }
        let now_ms = origin.elapsed().as_millis().min(u64::MAX as u128) as u64;
        session.tick(now_ms).map_err(DriverError::Session)?;
        if let Some(packet) = session
            .next_outbound(packet::DEFAULT_PACKET_LIMIT)
            .map_err(DriverError::Session)?
        {
            tokio::select! {
                result=transport.write_packet(&packet)=>result.map_err(DriverError::Transport)?,
                _=shutdown_requested(shutdown)=>return Ok(()),
            }
            // A fast peer may already have replied; apply the write boundary
            // first, before the next read observes that response.
            session.outbound_written().map_err(DriverError::Session)?;
            continue;
        }
        let deadline = origin + Duration::from_millis(session.next_keepalive_ms());
        let received = tokio::select! {
            result=transport.read_packet_until(deadline)=>result.map_err(DriverError::Transport)?,
            _=shutdown_requested(shutdown)=>return Ok(()),
        };
        if let Some(received) = received {
            let now_ms = origin.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let result = session.on_packet(received.bytes(), now_ms, packet::DEFAULT_PACKET_LIMIT);
            drop(received); // Return the worker byte lease before producing replies.
            result.map_err(DriverError::Session)?;
        }
    }
}

/// A non-shutdown notification must not cancel the in-flight I/O future selected
/// alongside this one; only true or sender closure completes this future.
async fn shutdown_requested(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}
