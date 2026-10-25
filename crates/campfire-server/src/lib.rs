//! Public in-process server composition API.
#[cfg(feature = "arena")]
pub use campfire_arena::ArenaModule;
pub use campfire_runtime::{Campfire, Config, ConnectionLimits, RuntimeOptions};

pub mod cli;
mod connection_limits;
pub mod dashboard;
pub use connection_limits::connection_limits_from_env;
