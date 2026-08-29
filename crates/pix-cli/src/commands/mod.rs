//! Command-group implementations. `main.rs` owns argument parsing and
//! dispatch; each group module owns its human menu, headless actions, and
//! JSON envelopes.

pub(crate) mod device;
pub(crate) mod pi;
pub(crate) mod pi_bridge;
pub(crate) mod relay;
pub(crate) mod session;
pub(crate) mod setup;
pub(crate) mod shared;
pub(crate) mod update;
pub(crate) mod workspace;
