//! Public in-process server composition API.
#[cfg(feature = "arena")]
pub use campfire_arena::ArenaModule;
pub use campfire_runtime::{Campfire, Config};

pub mod cli;
