//! Docker Engine client wrapper (bollard). Filled in by milestones M2 and M3.

/// Named pipe that the Docklet proxy exposes to the GUI and the Windows
/// `docker` CLI.
pub const ENGINE_PIPE: &str = r"\\.\pipe\docklet_engine";
