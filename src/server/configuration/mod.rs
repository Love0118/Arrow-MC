//! Configuration-phase packets and ordered registry synchronization.
//!
//! The default path uses the verified local snapshot. It stops at the real
//! spawn prerequisite: no API manufactures a FinishConfiguration/Play handoff.

mod driver;
pub mod packet;
mod session;

pub use driver::{DriverError, run};
pub use session::{ConfigurationSession, SessionError, SessionStage};
