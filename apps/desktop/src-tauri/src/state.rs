//! State shared by the Tauri commands and the tray.

use docklet_app::Backend;
use tauri::AppHandle;

use crate::streams;

/// Engine control and the streams, which live in `docklet-app` so the
/// native shell runs the same code.
pub struct AppState {
    pub backend: Backend,
}

impl AppState {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            backend: streams::backend(app),
        }
    }
}
