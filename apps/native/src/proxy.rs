//! `docklet-native.exe --proxy`: serve the Docker pipe and nothing else.
//!
//! `docklet_app::SystemHost` starts the pipe proxy as this executable in
//! this mode, detached, so the Windows Docker CLI keeps working when the
//! window's process exits. No window, no tray icon, and not the single
//! instance: the proxy runs next to the app.

use std::io;

/// Serves the pipe like `docklet proxy` does: with idle stop, as the setting
/// shared with the CLI says (`docklet config set idle-stop`). Returns when
/// the proxy is told to stop.
pub fn serve() -> io::Result<()> {
    let config = docklet_proxy::PipeProxyConfig {
        pipe_name: docklet_core::engine::ENGINE_PIPE.to_string(),
        message_mode: true,
        relay: docklet_proxy::RelayCommand::wsl_relay(docklet_wsl::DISTRO),
        trace: false,
    };
    let data_dir = docklet_wsl::data_dir()?;
    let engine = docklet_wsl::Engine::system()?;
    docklet_proxy::serve_configured(config, &data_dir, engine)
}
