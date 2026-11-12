//! Platform-neutral Docklet logic. Everything here builds and tests on Linux.

pub mod state;
pub mod wsl_output;

pub use state::EngineState;
