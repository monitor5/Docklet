//! Engine lifecycle requests from the tray and the window.
//!
//! The lifecycle itself is in `docklet-app`: its supervisor lets one
//! transition run and turns the others away, whoever asks. What is left
//! here is finding the state and knowing whether there is a window.

use std::io;

use docklet_app::EngineStatus;
use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::streams;

/// The engine state as the UI should see it, with what the screens around
/// it need. Blocks on `wsl.exe`.
pub fn status(app: &AppHandle) -> io::Result<EngineStatus> {
    app.state::<AppState>().backend.status()
}

/// Installs the distro from the published rootfs for `base`, or for the
/// default base. `progress` receives one line per step. Blocks.
pub fn install(
    app: &AppHandle,
    base: Option<String>,
    progress: &mut dyn FnMut(&str),
) -> io::Result<()> {
    app.state::<AppState>().backend.install(base, progress)
}

/// Starts the distro, dockerd and the pipe proxy, then subscribes to Docker's
/// events. Wakes the engine if it sleeps. Blocks until the engine answers.
pub fn start(app: &AppHandle) -> io::Result<()> {
    streams::sync_window(app);
    app.state::<AppState>().backend.start()?;
    // The start takes a while, and the window may have been opened
    // meanwhile.
    streams::watch_events(app);
    Ok(())
}

/// Stops the pipe proxy and the distro. Blocks.
pub fn stop(app: &AppHandle) -> io::Result<()> {
    app.state::<AppState>().backend.stop()
}
