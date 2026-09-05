//! Bounded, shared session verification. No access token or client UUID is trusted.
//!
//! The caller publishes a returned profile only if its login generation is still
//! current. Dropping this future cancels network/body work; it does not publish.

use super::login::{AuthenticatedProfile, ProfileProperty};
use reqwest::{Client, Url, redirect::Policy};
use serde::{
    Deserialize, Deserializer,
    de::{self, SeqAccess, Visitor},
};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

/// Official discovery document's session/verify endpoint, checked 2026-09-05.
pub const SESSION_VERIFY_URL: &str = "https://sessionserver.mojang.com/session/minecraft/hasJoined";
pub const MAX_AUTH_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct AuthLimits {
    pub max_in_flight: usize,
    pub max_response_bytes: usize,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// Absolute request deadline, including headers and the complete body.
    pub request_timeout: Duration,
}

impl Default for AuthLimits {
    fn default() -> Self {
        Self {
            max_in_flight: 16,
            max_response_bytes: MAX_AUTH_RESPONSE_BYTES,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    InvalidConfig,
    InvalidRequest,
    Busy,
    Cancelled,
    Timeout,
    Transport,
    HttpStatus { status: u16, unavailable: bool },
    BodyTooLarge,
    InvalidProfile,
    AllocationFailed,
}

impl AuthError {
    pub fn is_unavailable(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Transport
                | Self::HttpStatus {
                    unavailable: true,
                    ..
                }
        )
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpStatus { status, .. } => {
                write!(f, "session verification HTTP status {status}")
            }
            error => f.write_str(match error {
                Self::InvalidConfig => "invalid authentication limits or endpoint",
                Self::InvalidRequest => "invalid session verification request",
                Self::Busy => "authentication request limit reached",
                Self::Cancelled => "authentication cancelled",
                Self::Timeout => "authentication timed out",
                Self::Transport => "authentication transport failed",
                Self::BodyTooLarge => "authentication response exceeds byte limit",
                Self::InvalidProfile => "invalid authentication profile",
                Self::AllocationFailed => "authentication allocation failed",
                Self::HttpStatus { .. } => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for AuthError {}

#[derive(Clone)]
pub struct AuthClient {
    client: Client,
    endpoint: Url,
    requests: Arc<Semaphore>,
    limits: AuthLimits,
}

impl AuthClient {
    pub fn new(limits: AuthLimits) -> Result<Self, AuthError> {
        Self::build(
            Url::parse(SESSION_VERIFY_URL).map_err(|_| AuthError::InvalidConfig)?,
            limits,
        )
    }

    /// Explicit loopback-only endpoint for local integration tests. Production
    /// configuration uses `new`; this cannot send plaintext to a remote host.
    #[doc(hidden)]
    pub fn for_loopback_tests(address: SocketAddr, limits: AuthLimits) -> Result<Self, AuthError> {
        if !address.ip().is_loopback() {
            return Err(AuthError::InvalidConfig);
        }
        let endpoint = Url::parse(&format!("http://{address}/session/minecraft/hasJoined"))
            .map_err(|_| AuthError::InvalidConfig)?;
        Self::build(endpoint, limits)
    }

    fn build(endpoint: Url, limits: AuthLimits) -> Result<Self, AuthError> {
        if limits.max_in_flight == 0
            || limits.max_in_flight > 1024
            || limits.max_response_bytes == 0
            || limits.max_response_bytes > MAX_AUTH_RESPONSE_BYTES
            || limits.connect_timeout.is_zero()
            || limits.read_timeout.is_zero()
            || limits.request_timeout.is_zero()
        {
            return Err(AuthError::InvalidConfig);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .http1_only()
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(Duration::from_secs(30))
            .connect_timeout(limits.connect_timeout)
            .read_timeout(limits.read_timeout)
            .timeout(limits.request_timeout)
            .build()
            .map_err(|_| AuthError::InvalidConfig)?;
        Ok(Self {
            client,
            endpoint,
            requests: Arc::new(Semaphore::new(limits.max_in_flight)),
            limits,
        })
    }

    /// Admission happens before URL/body allocation and never creates a waiting
    /// queue. The slot covers body parsing as well as I/O. Each slot permits a
    /// bounded response and at most 16 wire-bounded property strings; HTTP/TLS
    /// library buffers and returned profile retention are separate owner costs.
    pub async fn has_joined(
        &self,
        username: &str,
        server_hash: &str,
        ip: Option<IpAddr>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Option<AuthenticatedProfile>, AuthError> {
        if *cancel.borrow() {
            return Err(AuthError::Cancelled);
        }
        if username.len() > 16
            || !username.bytes().all(|byte| (33..=126).contains(&byte))
            || !valid_hash(server_hash)
        {
            return Err(AuthError::InvalidRequest);
        }
        let _permit = self.requests.try_acquire().map_err(|_| AuthError::Busy)?;
        tokio::select! {
            biased;
            () = cancelled(cancel) => Err(AuthError::Cancelled),
            result = self.request(username, server_hash, ip) => result,
        }
    }

    async fn request(
        &self,
        username: &str,
        server_hash: &str,
        ip: Option<IpAddr>,
    ) -> Result<Option<AuthenticatedProfile>, AuthError> {
        let mut url = self.endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("username", username)
                .append_pair("serverId", server_hash);
            if let Some(ip) = ip {
                query.append_pair("ip", &ip.to_string());
            }
        }
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(http_error)?;
        let status = response.status().as_u16();
        if status == 204 {
            return Ok(None);
        }
        if response
            .content_length()
            .is_some_and(|n| n > self.limits.max_response_bytes as u64)
        {
            return Err(AuthError::BodyTooLarge);
        }
        let mut body = Vec::new();
        body.try_reserve_exact(self.limits.max_response_bytes)
            .map_err(|_| AuthError::AllocationFailed)?;
        while let Some(chunk) = response.chunk().await.map_err(http_error)? {
            if chunk.len() > self.limits.max_response_bytes - body.len() {
                return Err(AuthError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        if !(200..300).contains(&status) {
            let named_rejection = serde_json::from_slice::<ServiceError>(&body)
                .ok()
                .and_then(|error| error.error)
                .is_some_and(|name| {
                    [
                        "ForbiddenOperationException",
                        "multiplayer.access.banned",
                        "FORCED_USERNAME_CHANGE",
                        "InsufficientPrivilegesException",
                    ]
                    .iter()
                    .any(|known| name.eq_ignore_ascii_case(known))
                });
            return Err(AuthError::HttpStatus {
                status,
                unavailable: status >= 500 && !named_rejection,
            });
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        parse_profile(&body, username)
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() || cancel.changed().await.is_err() {
            return;
        }
    }
}

fn http_error(error: reqwest::Error) -> AuthError {
    if error.is_timeout() {
        AuthError::Timeout
    } else {
        AuthError::Transport
    }
}

fn valid_hash(hash: &str) -> bool {
    let digits = hash.strip_prefix('-').unwrap_or(hash);
    !digits.is_empty() && digits.len() <= 40 && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Deserialize)]
struct ServiceError {
    error: Option<String>,
}

#[derive(Deserialize)]
struct ResponseProfile {
    id: Option<String>,
    #[serde(default, deserialize_with = "properties")]
    properties: Vec<ProfileProperty>,
}

#[derive(Deserialize)]
struct ResponseProperty {
    name: String,
    value: String,
    signature: Option<String>,
}

fn properties<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ProfileProperty>, D::Error> {
    struct Properties;
    impl<'de> Visitor<'de> for Properties {
        type Value = Vec<ProfileProperty>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("up to 16 profile properties")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_seq(self)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            if sequence.size_hint().is_some_and(|n| n > 16) {
                return Err(de::Error::custom("too many properties"));
            }
            let mut result = Vec::new();
            while let Some(property) = sequence.next_element::<ResponseProperty>()? {
                if result.len() == 16
                    || property.name.encode_utf16().count() > 64
                    || property.value.encode_utf16().count() > 32767
                    || property
                        .signature
                        .as_ref()
                        .is_some_and(|s| s.encode_utf16().count() > 1024)
                {
                    return Err(de::Error::custom("profile property exceeds wire limits"));
                }
                result.push(ProfileProperty {
                    name: property.name,
                    value: property.value,
                    signature: property.signature,
                });
            }
            Ok(result)
        }
    }
    deserializer.deserialize_option(Properties)
}

fn parse_profile(
    body: &[u8],
    requested_name: &str,
) -> Result<Option<AuthenticatedProfile>, AuthError> {
    let response: Option<ResponseProfile> =
        serde_json::from_slice(body).map_err(|_| AuthError::InvalidProfile)?;
    let Some(response) = response else {
        return Ok(None);
    };
    let Some(id) = response.id else {
        return Ok(None);
    };
    let id = parse_uuid(&id).ok_or(AuthError::InvalidProfile)?;
    // Authlib 10 creates the GameProfile with the queried name, not a response
    // name field. UUID/properties come only from the trusted HTTPS response.
    Ok(Some(AuthenticatedProfile {
        id,
        name: requested_name.to_owned(),
        properties: response.properties,
    }))
}

fn parse_uuid(value: &str) -> Option<[u8; 16]> {
    let bytes = value.as_bytes();
    if bytes.len() != 32 && bytes.len() != 36 {
        return None;
    }
    let mut hex = [0; 32];
    let mut count = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if bytes.len() == 36 && [8, 13, 18, 23].contains(&index) {
            if byte != b'-' {
                return None;
            }
        } else {
            *hex.get_mut(count)? = byte;
            count += 1;
        }
    }
    let mut uuid = [0; 16];
    for (target, pair) in uuid.iter_mut().zip(hex.chunks_exact(2)) {
        let upper = char::from(pair[0]).to_digit(16)? as u8;
        let lower = char::from(pair[1]).to_digit(16)? as u8;
        *target = upper * 16 + lower;
    }
    Some(uuid)
}
