//! Platform-neutral Docklet logic. Everything here builds and tests on Linux.

pub mod disk;
pub mod docker_client;
pub mod doctor;
pub mod engine;
pub mod idle;
pub mod settings;
pub mod skills;
pub mod state;
pub mod system;
pub mod wsl;
pub mod wsl_output;
pub mod wslconfig;

pub use state::{EngineState, ProxyMode};
pub use system::system_program;
