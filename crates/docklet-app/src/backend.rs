//! Engine control and the streams together: a start subscribes to Docker's
//! events, a stop closes the streams first.

use std::io;
use std::sync::Arc;

use tokio::runtime::Handle;

use crate::control::{EngineControl, EngineHost, EngineStatus, SystemHost};
use crate::streams::{StreamSink, Streams};

/// What a shell keeps for as long as it runs, window or not.
///
/// `status`, `install`, `start` and `stop` block, on `wsl.exe` for the most
/// part: call them from a worker thread. The streams deliver to the sink on
/// worker threads of `runtime`.
pub struct Backend {
    control: EngineControl,
    streams: Streams,
}

impl Backend {
    /// A backend for the engine on `host`, whose streams run on `runtime`
    /// and deliver to `sink`.
    pub fn new(host: Arc<dyn EngineHost>, runtime: Handle, sink: Arc<dyn StreamSink>) -> Self {
        Self {
            control: EngineControl::new(host.clone()),
            streams: Streams::new(runtime, host, sink),
        }
    }

    /// A backend for this user's real engine, whose pipe proxy is this same
    /// executable in `--proxy` mode.
    pub fn system(runtime: Handle, sink: Arc<dyn StreamSink>) -> Self {
        Self::new(Arc::new(SystemHost::new()), runtime, sink)
    }

    pub fn control(&self) -> &EngineControl {
        &self.control
    }

    pub fn streams(&self) -> &Streams {
        &self.streams
    }

    /// See `EngineControl::status`. Blocks on `wsl.exe`.
    pub fn status(&self) -> io::Result<EngineStatus> {
        self.control.status()
    }

    /// See `EngineControl::install`. Blocks.
    pub fn install(&self, base: Option<String>, progress: &mut dyn FnMut(&str)) -> io::Result<()> {
        self.control.install(base, progress)
    }

    /// Starts the distro, dockerd and the pipe proxy, then subscribes to
    /// Docker's events if there is a window. Wakes the engine if it sleeps.
    /// Blocks until the engine answers.
    pub fn start(&self) -> io::Result<()> {
        self.control.start()?;
        self.streams.watch_events();
        Ok(())
    }

    /// Stops the pipe proxy and the distro. Blocks. The streams go first,
    /// so the UI is not told that the engine disconnected when it was asked
    /// to stop.
    pub fn stop(&self) -> io::Result<()> {
        self.control.stop(|| self.streams.stop_all())
    }
}

#[cfg(test)]
mod tests {
    use docklet_core::{EngineState, ProxyMode};

    use super::*;
    use crate::control::testing::FakeHost;
    use crate::policy::WindowState;
    use crate::streams::DockerEvent;

    struct Quiet;

    impl StreamSink for Quiet {
        fn docker_event(&self, _event: DockerEvent) {}
    }

    fn backend(host: Arc<FakeHost>) -> (Backend, tokio::runtime::Runtime) {
        // Never driven: the tasks the backend spawns do not run.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let backend = Backend::new(host, runtime.handle().clone(), Arc::new(Quiet));
        (backend, runtime)
    }

    #[test]
    fn a_stop_ends_the_streams_inside_the_transition() {
        let host = FakeHost::new(EngineState::Running, ProxyMode::Plain);
        let (backend, _runtime) = backend(host.clone());
        backend.streams().set_window(WindowState::Visible);
        backend.streams().watch_events();
        backend.stop().unwrap();
        assert_eq!(host.calls(), ["stop_proxy", "stop_distro"]);
        // The subscription is gone until a list view asks for it again; the
        // window is still there.
        assert_eq!(backend.streams().window(), WindowState::Visible);
        assert_eq!(backend.status().unwrap().state, EngineState::Stopped);
    }

    #[test]
    fn a_start_that_fails_subscribes_to_nothing() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::Down);
        *host.start_result.lock().unwrap() = Some("boom".to_string());
        let (backend, _runtime) = backend(host);
        backend.streams().set_window(WindowState::Visible);
        assert_eq!(backend.start().unwrap_err().to_string(), "boom");
    }

    #[test]
    fn a_start_without_a_window_starts_the_engine_all_the_same() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::Down);
        let (backend, _runtime) = backend(host.clone());
        backend.start().unwrap();
        assert_eq!(host.calls(), ["start_distro", "spawn_proxy"]);
        assert_eq!(backend.status().unwrap().state, EngineState::Running);
    }

    #[test]
    fn an_install_goes_through_the_supervisor() {
        let host = FakeHost::new(EngineState::NotInstalled, ProxyMode::Down);
        let (backend, _runtime) = backend(host);
        let mut steps = 0;
        backend.install(None, &mut |_| steps += 1).unwrap();
        assert_eq!(steps, 1);
        assert_eq!(backend.control().in_flight(), None);
    }
}
