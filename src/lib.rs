//! Foundations for the Arrow MC Java Edition server.
//!
//! Game features are added only after their data and execution prerequisites
//! have independent compatibility evidence against the locked Vanilla version.

#[cfg(test)]
extern crate self as arrow_mc;

pub mod nbt;
pub mod runtime;
pub mod server;
pub mod snbt;
pub mod wire;
pub mod world;

mod unicode_names;
