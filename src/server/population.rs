//! Admission and the session UUID shared by the current connection population.
//!
//! Status, login and configuration sockets all retain one lease. The UUID is
//! created only when login needs it and cleared by connection maintenance when
//! no lease remains. A new accept before that phase preserves the current UUID.
//! This is connection lifetime accounting, not a list of players in the world.

use std::{
    io,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Clone)]
pub struct ConnectionPopulation {
    state: Arc<Mutex<State>>,
}

struct State {
    limit: usize,
    active: usize,
    next_id: u64,
    session_uuid: Option<[u8; 16]>,
}

/// Keep this non-cloneable lease with the socket owner through every protocol.
pub struct ConnectionLease {
    id: u64,
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ConnectionPopulation {
    pub fn new(limit: usize) -> io::Result<Self> {
        if limit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "connection limit must be nonzero",
            ));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                limit,
                active: 0,
                next_id: 0,
                session_uuid: None,
            })),
        })
    }

    /// No task or per-connection buffers should be created before admission.
    /// `None` means capacity is occupied; exhausted identities are an error.
    pub fn try_admit(&self) -> io::Result<Option<ConnectionLease>> {
        let mut state = lock(&self.state);
        if state.active == state.limit {
            return Ok(None);
        }
        let id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("connection identities exhausted"))?;
        state.next_id = id;
        state.active += 1;
        Ok(Some(ConnectionLease {
            id,
            state: Arc::clone(&self.state),
        }))
    }

    pub fn active(&self) -> usize {
        lock(&self.state).active
    }

    /// Called after pending accepts and disconnected owners are accounted for.
    /// The network owner drives this phase; individual socket drops do not reset
    /// the UUID between maintenance ticks.
    pub fn maintain(&self) {
        let mut state = lock(&self.state);
        if state.active == 0 {
            state.session_uuid = None;
        }
    }
}

impl ConnectionLease {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn session_uuid(&self) -> io::Result<[u8; 16]> {
        let mut state = lock(&self.state);
        if let Some(uuid) = state.session_uuid {
            return Ok(uuid);
        }
        let uuid = super::crypto::random_uuid().map_err(io::Error::other)?;
        state.session_uuid = Some(uuid);
        Ok(uuid)
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        state.active -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_socket_retains_session_until_entire_population_is_empty() {
        let population = ConnectionPopulation::new(3).unwrap();
        let status = population.try_admit().unwrap().unwrap();
        assert!(lock(&population.state).session_uuid.is_none());
        let login = population.try_admit().unwrap().unwrap();
        let uuid = login.session_uuid().unwrap();
        assert_eq!(uuid[6] >> 4, 4);
        assert_eq!(uuid[8] >> 6, 2);
        drop(login);
        let replacement = population.try_admit().unwrap().unwrap();
        assert_eq!(replacement.session_uuid().unwrap(), uuid);
        drop(replacement);
        drop(status);
        assert_eq!(population.active(), 0);
        assert_eq!(lock(&population.state).session_uuid, Some(uuid));
        population.maintain();
        assert!(lock(&population.state).session_uuid.is_none());
    }

    #[test]
    fn bounds_live_leases_and_never_reuses_connection_identity() {
        assert!(ConnectionPopulation::new(0).is_err());
        let population = ConnectionPopulation::new(1).unwrap();
        let first = population.try_admit().unwrap().unwrap();
        let first_id = first.id();
        assert!(population.try_admit().unwrap().is_none());
        drop(first);
        let second = population.try_admit().unwrap().unwrap();
        assert!(second.id() > first_id);
        drop(second);
        lock(&population.state).next_id = u64::MAX;
        assert!(population.try_admit().is_err());
        assert_eq!(population.active(), 0);
    }

    #[test]
    fn overlapping_threads_share_one_lazy_session_uuid() {
        let population = ConnectionPopulation::new(9).unwrap();
        let guard = population.try_admit().unwrap().unwrap();
        let expected = guard.session_uuid().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let lease = population.try_admit().unwrap().unwrap();
                scope.spawn(move || assert_eq!(lease.session_uuid().unwrap(), expected));
            }
        });
        assert_eq!(population.active(), 1);
        drop(guard);
        population.maintain();
        assert!(lock(&population.state).session_uuid.is_none());
    }

    #[test]
    fn reconnect_before_maintenance_keeps_the_previous_session() {
        let population = ConnectionPopulation::new(1).unwrap();
        let first = population.try_admit().unwrap().unwrap();
        let uuid = first.session_uuid().unwrap();
        drop(first);
        let next = population.try_admit().unwrap().unwrap();
        population.maintain();
        assert_eq!(next.session_uuid().unwrap(), uuid);
        drop(next);
        population.maintain();
        assert!(lock(&population.state).session_uuid.is_none());
    }
}
