//! `wsl.exe` runner, distro registry lookup and keepalive.
//!
//! Filled in by milestone M1. The crate exists now so the workspace layout and
//! CI are stable from the start.

/// Name of the dedicated WSL2 distro that hosts the Docker Engine.
pub const DISTRO_NAME: &str = "docklet";
