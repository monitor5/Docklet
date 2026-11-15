//! State shared by the Tauri commands and the tray.

use crate::streams::Streams;
use crate::supervisor::Supervisor;

#[derive(Default)]
pub struct AppState {
    pub supervisor: Supervisor,
    pub streams: Streams,
}
