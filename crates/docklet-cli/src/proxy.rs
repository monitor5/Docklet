//! The background pipe proxy (`docklet proxy`), started by `docklet start`.

#[cfg(windows)]
mod imp {
    use crate::failure::Failure;
    use docklet_proxy::{PipeProxyConfig, RelayCommand, control};
    use std::io;
    use std::time::{Duration, Instant};

    pub fn is_running() -> bool {
        control::is_running()
    }

    /// Starts the proxy as a detached process unless one is already running.
    pub fn ensure_running() -> io::Result<()> {
        if is_running() {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        docklet_wsl::spawn_detached(&format!("\"{}\" proxy", exe.display()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if is_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(io::Error::other("the pipe proxy did not start"))
    }

    /// Stops the proxy and waits for it to exit. True when none is left.
    pub fn stop() -> bool {
        control::stop()
    }

    /// `docklet proxy [--trace] [--byte]`: runs the proxy in the foreground.
    pub fn serve(trace: bool, byte_mode: bool) -> Result<(), Failure> {
        let config = PipeProxyConfig {
            pipe_name: docklet_proxy::ENGINE_PIPE.to_string(),
            message_mode: !byte_mode,
            relay: RelayCommand::wsl_relay(docklet_wsl::DISTRO),
            trace,
        };
        if is_running() {
            return Err(Failure::failed("a pipe proxy is already running"));
        }
        Ok(docklet_proxy::serve(config)?)
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::failure::Failure;
    use std::io;

    pub fn is_running() -> bool {
        false
    }

    pub fn ensure_running() -> io::Result<()> {
        Err(io::Error::other("the docklet engine only runs on Windows"))
    }

    pub fn stop() -> bool {
        true
    }

    pub fn serve(_trace: bool, _byte_mode: bool) -> Result<(), Failure> {
        Err(Failure::failed("the docklet engine only runs on Windows"))
    }
}

pub use imp::{ensure_running, is_running, serve, stop};

pub const DOCKER_HOST: &str = docklet_proxy::ENGINE_DOCKER_HOST;
