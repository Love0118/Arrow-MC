//! Connection-owned Java Edition login protocol and authenticated handoff data.

pub mod packet;
pub mod session;

/// The profile returned by session authentication, or explicitly constructed
/// by the configured offline-mode branch. The client Hello UUID is not proof
/// of identity and is never copied into this profile by the login session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedProfile {
    pub id: [u8; 16],
    pub name: String,
    pub properties: Vec<ProfileProperty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

/// Emitted only after LoginFinished has been written and the matching terminal
/// LoginAcknowledged has been consumed. This is configuration admission, not
/// permission to send FinishConfiguration or publish a player in the world.
#[derive(Debug)]
pub struct LoginAccepted {
    pub profile: AuthenticatedProfile,
    pub session_id: [u8; 16],
    pub transferred: bool,
}
