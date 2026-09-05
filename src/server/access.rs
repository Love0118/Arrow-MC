//! Bounded, in-memory dedicated-server admission checks.
//!
//! Behavioral references: 26.3-pre-2 `PlayerList.canPlayerLogin`,
//! `DedicatedPlayerList`, `ServerOpListEntry`, and `BanListEntry`. The policy is
//! independently implemented with typed UUID/IP keys. File persistence, command
//! permissions, and operator permission levels are outside this module.
//!
//! Expired bans are ignored directly. This avoids the reference's separate
//! contains/get expiry race. IPs are compared as addresses, without reproducing
//! its SocketAddress string splitting (which mishandles IPv6).

use super::login::AuthenticatedProfile;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::IpAddr,
    time::{SystemTime, UNIX_EPOCH},
};

/// Bounds on retained policy entries and each ban's UTF-8 text.
///
/// The entry bound applies separately to each of the four collections,
/// including expired bans until `purge_expired` removes them. Hash tables retain
/// their bounded high-water capacity; a check never scans all bans or mutates it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessLimits {
    pub max_entries_per_list: usize,
    pub max_reason_bytes: usize,
    pub max_expiration_bytes: usize,
}

impl Default for AccessLimits {
    fn default() -> Self {
        Self {
            max_entries_per_list: 65_536,
            max_reason_bytes: 4096,
            max_expiration_bytes: 128,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessError {
    EntryLimit,
    ReasonLimit,
    ExpirationLimit,
    AllocationFailed,
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EntryLimit => "login access list entry limit exceeded",
            Self::ReasonLimit => "ban reason byte limit exceeded",
            Self::ExpirationLimit => "ban expiration display byte limit exceeded",
            Self::AllocationFailed => "unable to reserve login access list storage",
        })
    }
}

impl std::error::Error for AccessError {}

#[derive(Clone, Debug)]
struct Ban {
    reason: Option<Box<str>>,
    expiration: Option<(i128, Box<str>)>,
}

impl Ban {
    fn new(
        reason: Option<&str>,
        expiration: Option<(SystemTime, &str)>,
        limits: AccessLimits,
    ) -> Result<Self, AccessError> {
        if reason.is_some_and(|text| text.len() > limits.max_reason_bytes) {
            return Err(AccessError::ReasonLimit);
        }
        if expiration.is_some_and(|(_, text)| text.len() > limits.max_expiration_bytes) {
            return Err(AccessError::ExpirationLimit);
        }
        Ok(Self {
            reason: reason.map(Box::from),
            expiration: expiration.map(|(time, text)| (epoch_millis(time), Box::from(text))),
        })
    }

    fn active_at(&self, now_millis: i128) -> bool {
        self.expiration
            .as_ref()
            .is_none_or(|(expires, _)| *expires >= now_millis)
    }

    fn rejection(&self, ip_ban: bool) -> Value {
        let reason = match self.reason.as_deref() {
            Some(text) => json!({"text": text}),
            None => json!({"translate": "multiplayer.disconnect.banned.reason.default"}),
        };
        let mut component = json!({
            "translate": if ip_ban {
                "multiplayer.disconnect.banned_ip.reason"
            } else {
                "multiplayer.disconnect.banned.reason"
            },
            "with": [reason]
        });
        if let Some((_, display)) = &self.expiration {
            component["extra"] = json!([{
                "translate": if ip_ban {
                    "multiplayer.disconnect.banned_ip.expiration"
                } else {
                    "multiplayer.disconnect.banned.expiration"
                },
                "with": [display.as_ref()]
            }]);
        }
        component
    }
}

/// Admission uses authenticated UUIDs, never names or the client Hello UUID.
///
/// Operators bypass the whitelist; only an operator whose stored flag is true
/// bypasses capacity. Neither privilege bypasses a profile or IP ban. The caller
/// owns the actual player count and must serialize capacity admission with world
/// membership changes; this policy does not reserve a player slot.
#[derive(Clone, Debug)]
pub struct LoginAccess {
    max_players: usize,
    limits: AccessLimits,
    whitelist_enabled: bool,
    profile_bans: HashMap<[u8; 16], Ban>,
    ip_bans: HashMap<IpAddr, Ban>,
    whitelist: HashSet<[u8; 16]>,
    operators: HashMap<[u8; 16], bool>,
}

impl LoginAccess {
    /// Empty ban/allow/operator lists with whitelist enforcement disabled.
    pub fn new(max_players: usize) -> Self {
        Self::with_limits(max_players, AccessLimits::default())
    }

    pub fn with_limits(max_players: usize, limits: AccessLimits) -> Self {
        Self {
            max_players,
            limits,
            whitelist_enabled: false,
            profile_bans: HashMap::new(),
            ip_bans: HashMap::new(),
            whitelist: HashSet::new(),
            operators: HashMap::new(),
        }
    }

    pub fn set_whitelist_enabled(&mut self, enabled: bool) {
        self.whitelist_enabled = enabled;
    }

    /// Adds or replaces a UUID ban. `None` reason means the translated default;
    /// `Some("")` remains a literal empty reason. `None` expiration is permanent.
    ///
    /// The caller supplies the display text for the same expiration instant in
    /// Vanilla's `yyyy-MM-dd 'at' HH:mm:ss z` format and configured timezone. This
    /// module bounds and preserves that text; it does not format or validate a
    /// timezone. Expiration comparisons use Java's millisecond precision.
    pub fn set_profile_ban(
        &mut self,
        id: [u8; 16],
        reason: Option<&str>,
        expiration: Option<(SystemTime, &str)>,
    ) -> Result<(), AccessError> {
        let new_entry = !self.profile_bans.contains_key(&id);
        self.check_entry_limit(self.profile_bans.len(), new_entry)?;
        let ban = Ban::new(reason, expiration, self.limits)?;
        if new_entry {
            self.profile_bans
                .try_reserve(1)
                .map_err(|_| AccessError::AllocationFailed)?;
        }
        self.profile_bans.insert(id, ban);
        Ok(())
    }

    /// Adds or replaces an exact IPv4/IPv6 ban; no CIDR, hostname, or port.
    /// IPv4-mapped IPv6 addresses share their IPv4 key, so a dual-stack listener
    /// cannot accidentally bypass the same client's configured IPv4 ban.
    /// Reason and expiration have the same contract as `set_profile_ban`.
    pub fn set_ip_ban(
        &mut self,
        address: IpAddr,
        reason: Option<&str>,
        expiration: Option<(SystemTime, &str)>,
    ) -> Result<(), AccessError> {
        let address = address.to_canonical();
        let new_entry = !self.ip_bans.contains_key(&address);
        self.check_entry_limit(self.ip_bans.len(), new_entry)?;
        let ban = Ban::new(reason, expiration, self.limits)?;
        if new_entry {
            self.ip_bans
                .try_reserve(1)
                .map_err(|_| AccessError::AllocationFailed)?;
        }
        self.ip_bans.insert(address, ban);
        Ok(())
    }

    pub fn remove_profile_ban(&mut self, id: [u8; 16]) -> bool {
        self.profile_bans.remove(&id).is_some()
    }

    pub fn remove_ip_ban(&mut self, address: IpAddr) -> bool {
        self.ip_bans.remove(&address.to_canonical()).is_some()
    }

    pub fn set_whitelisted(&mut self, id: [u8; 16], allowed: bool) -> Result<(), AccessError> {
        if !allowed {
            self.whitelist.remove(&id);
            return Ok(());
        }
        if !self.whitelist.contains(&id) {
            self.check_entry_limit(self.whitelist.len(), true)?;
            self.whitelist
                .try_reserve(1)
                .map_err(|_| AccessError::AllocationFailed)?;
            self.whitelist.insert(id);
        }
        Ok(())
    }

    /// `Some(flag)` adds/replaces an operator's capacity bypass setting;
    /// `None` removes the operator. Permission levels belong to command policy.
    pub fn set_operator(
        &mut self,
        id: [u8; 16],
        bypasses_capacity: Option<bool>,
    ) -> Result<(), AccessError> {
        let Some(bypasses_capacity) = bypasses_capacity else {
            self.operators.remove(&id);
            return Ok(());
        };
        let new_entry = !self.operators.contains_key(&id);
        self.check_entry_limit(self.operators.len(), new_entry)?;
        if new_entry {
            self.operators
                .try_reserve(1)
                .map_err(|_| AccessError::AllocationFailed)?;
        }
        self.operators.insert(id, bypasses_capacity);
        Ok(())
    }

    /// Explicit owner-side maintenance; expiry never requires a full scan on
    /// login. Returns the number of removed bans and frees their text storage.
    pub fn purge_expired(&mut self, now: SystemTime) -> usize {
        let now = epoch_millis(now);
        let before = self.profile_bans.len() + self.ip_bans.len();
        self.profile_bans.retain(|_, ban| ban.active_at(now));
        self.ip_bans.retain(|_, ban| ban.active_at(now));
        before - self.profile_bans.len() - self.ip_bans.len()
    }

    pub fn check(
        &self,
        profile: &AuthenticatedProfile,
        address: IpAddr,
        world_players: usize,
    ) -> Option<Value> {
        self.check_at(profile, address, world_players, SystemTime::now())
    }

    /// Deterministic form of `check`, sharing one wall-clock sample for both ban
    /// lists. A ban expires strictly after its expiration millisecond, matching
    /// `Date.before`; equality is still banned.
    pub fn check_at(
        &self,
        profile: &AuthenticatedProfile,
        address: IpAddr,
        world_players: usize,
        now: SystemTime,
    ) -> Option<Value> {
        let now = epoch_millis(now);
        if let Some(ban) = self
            .profile_bans
            .get(&profile.id)
            .filter(|ban| ban.active_at(now))
        {
            return Some(ban.rejection(false));
        }
        let operator = self.operators.get(&profile.id);
        if self.whitelist_enabled && operator.is_none() && !self.whitelist.contains(&profile.id) {
            return Some(json!({"translate": "multiplayer.disconnect.not_whitelisted"}));
        }
        if let Some(ban) = self
            .ip_bans
            .get(&address.to_canonical())
            .filter(|ban| ban.active_at(now))
        {
            return Some(ban.rejection(true));
        }
        if world_players >= self.max_players && !operator.copied().unwrap_or(false) {
            return Some(json!({"translate": "multiplayer.disconnect.server_full"}));
        }
        None
    }

    fn check_entry_limit(&self, len: usize, adding: bool) -> Result<(), AccessError> {
        if adding && len >= self.limits.max_entries_per_list {
            Err(AccessError::EntryLimit)
        } else {
            Ok(())
        }
    }
}

fn epoch_millis(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i128,
        Err(error) => {
            let duration = error.duration();
            -(duration.as_millis() as i128)
                - i128::from(!duration.subsec_nanos().is_multiple_of(1_000_000))
        }
    }
}
