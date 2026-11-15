//! Engine lifecycle: the one place that installs, starts and stops the engine.
//!
//! A tray and a window can ask for a start or a stop at the same moment.
//! Every request goes through the [`Supervisor`] of an [`EngineControl`],
//! which lets one transition run and turns the others away.
//!
//! Everything here blocks, on `wsl.exe` for the most part: call it from a
//! worker thread, never from the thread that runs the message loop.

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use docklet_core::{EngineState, ProxyMode, engine};
use docklet_wsl::{Engine, RootfsSource};

/// A change of engine state that takes a while.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Installing,
    Starting,
    Stopping,
}

impl fmt::Display for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Installing => "being installed",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
        })
    }
}

/// Lets one engine transition run at a time.
///
/// A request that arrives while another transition is in flight fails at
/// once. Queueing it instead would run a stale request much later, for
/// example a stop that was clicked while a slow start was still going.
#[derive(Default)]
pub struct Supervisor {
    in_flight: Mutex<Option<Transition>>,
}

impl Supervisor {
    /// Runs `work` as the only transition in flight.
    pub fn run<T>(
        &self,
        transition: Transition,
        work: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let _in_flight = self.begin(transition)?;
        work()
    }

    /// The transition that runs now, if any.
    pub fn in_flight(&self) -> Option<Transition> {
        *self.slot()
    }

    fn begin(&self, transition: Transition) -> io::Result<InFlight<'_>> {
        let mut in_flight = self.slot();
        if let Some(running) = *in_flight {
            let busy = format!("the engine is already {running}");
            return Err(io::Error::other(busy));
        }
        *in_flight = Some(transition);
        Ok(InFlight(self))
    }

    fn slot(&self) -> MutexGuard<'_, Option<Transition>> {
        let slot = self.in_flight.lock();
        // The slot holds a plain value, so it is still good after a panic
        // elsewhere poisoned the lock.
        slot.unwrap_or_else(PoisonError::into_inner)
    }
}

/// Marks the transition as over when dropped, also when the work panicked.
struct InFlight<'a>(&'a Supervisor);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        *self.0.slot() = None;
    }
}

/// The machine the engine lives on: WSL, the distro and the pipe proxy.
///
/// [`SystemHost`] is the real one. The trait is the seam that lets the
/// decisions of [`EngineControl`] be tested without `wsl.exe`.
pub trait EngineHost: Send + Sync + 'static {
    /// What the engine itself reports, the pipe proxy not considered.
    /// Blocks on `wsl.exe`; starts nothing.
    fn probe(&self) -> io::Result<EngineState>;
    /// Which pipe proxy runs, if any. Cheap: no process is started.
    fn proxy_mode(&self) -> ProxyMode;
    /// Whether the distro is running. Blocks on `wsl.exe`; starts nothing.
    fn distro_running(&self) -> bool;
    /// Asks Docker for an answer through the pipe, which makes an idle-stop
    /// proxy start a sleeping engine. Blocks until it answered.
    fn wake(&self) -> io::Result<()>;
    /// Starts the distro and waits for dockerd. Blocks.
    fn start_distro(&self) -> io::Result<()>;
    /// Starts the pipe proxy as a process that outlives this one.
    fn spawn_proxy(&self) -> io::Result<()>;
    /// Ends the pipe proxy and waits until it is gone.
    fn stop_proxy(&self);
    /// Stops the distro. Blocks.
    fn stop_distro(&self) -> io::Result<()>;
    /// Installs the distro from the published rootfs for `base`. `progress`
    /// receives one line per step. Blocks.
    fn install(&self, base: &str, progress: &mut dyn FnMut(&str)) -> io::Result<()>;
    /// Whether the user's `.wslconfig` turns on mirrored networking.
    fn mirrored_networking(&self) -> bool;
}

/// The pipe proxy's control calls, which `docklet-proxy` only has on Windows.
#[cfg(windows)]
mod proxy {
    pub use docklet_proxy::control::{mode, stop};
}

/// Nothing serves the engine pipe outside Windows.
#[cfg(not(windows))]
mod proxy {
    use docklet_core::ProxyMode;

    pub fn mode() -> ProxyMode {
        ProxyMode::Down
    }

    pub fn stop() -> bool {
        true
    }
}

/// The real WSL, distro and pipe proxy of this user.
#[derive(Debug, Clone, Default)]
pub struct SystemHost {
    proxy_command: Option<String>,
}

impl SystemHost {
    /// A host whose pipe proxy is this same executable in `--proxy` mode.
    pub fn new() -> Self {
        Self::default()
    }

    /// A host whose pipe proxy is started with `command_line`, for a shell
    /// that has no `--proxy` mode of its own (`"C:\...\docklet.exe" proxy`).
    pub fn with_proxy_command(command_line: impl Into<String>) -> Self {
        Self {
            proxy_command: Some(command_line.into()),
        }
    }
}

impl EngineHost for SystemHost {
    fn probe(&self) -> io::Result<EngineState> {
        Ok(Engine::system()?.status())
    }

    fn proxy_mode(&self) -> ProxyMode {
        proxy::mode()
    }

    fn distro_running(&self) -> bool {
        Engine::system().is_ok_and(|engine| {
            let running = engine.wsl().running();
            running.is_ok_and(|names| names.iter().any(|name| name == docklet_wsl::DISTRO))
        })
    }

    fn wake(&self) -> io::Result<()> {
        docklet_proxy::ping(docklet_proxy::PING_TIMEOUT)
    }

    fn start_distro(&self) -> io::Result<()> {
        Engine::system()?.start()
    }

    fn spawn_proxy(&self) -> io::Result<()> {
        // Detached, so the Windows docker CLI keeps working when the app
        // exits.
        let command_line = match &self.proxy_command {
            Some(command_line) => command_line.clone(),
            None => format!("\"{}\" --proxy", std::env::current_exe()?.display()),
        };
        docklet_wsl::spawn_detached(&command_line)
    }

    fn stop_proxy(&self) {
        proxy::stop();
    }

    fn stop_distro(&self) -> io::Result<()> {
        Engine::system()?.stop()
    }

    fn install(&self, base: &str, progress: &mut dyn FnMut(&str)) -> io::Result<()> {
        Engine::system()?.install(&RootfsSource::Base(base.to_string()), progress)
    }

    fn mirrored_networking(&self) -> bool {
        docklet_wsl::mirrored_networking()
    }
}

/// What a status poll answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    pub state: EngineState,
    /// The bases an install can pick from, default first.
    pub bases: Vec<&'static str>,
    /// The user's `.wslconfig` turns on mirrored networking, under which
    /// published ports may not reach Windows.
    pub mirrored_networking: bool,
}

/// How often and how long a started pipe proxy is waited for.
const PROXY_START_CHECKS: u32 = 100;
const PROXY_START_PAUSE: Duration = Duration::from_millis(100);

/// True when a Docker API connection would be answered without starting
/// anything: the pipe proxy is up and the distro runs. Blocks on `wsl.exe`.
///
/// Asked before a stream is opened again, because trying is not harmless. A
/// plain proxy starts `wsl.exe -d docklet` for every connection, and that
/// boots a distro the user stopped from outside (`wsl --shutdown`). An
/// idle-stop proxy starts a sleeping engine for every connection, and the
/// window would keep an engine awake that nobody uses. A sleeping engine is
/// therefore not reachable in this sense, whichever proxy runs.
pub fn reachable(host: &dyn EngineHost) -> bool {
    host.proxy_mode().is_running() && host.distro_running()
}

/// Installs, starts and stops the engine, one transition at a time.
pub struct EngineControl {
    host: Arc<dyn EngineHost>,
    supervisor: Supervisor,
}

impl EngineControl {
    pub fn new(host: Arc<dyn EngineHost>) -> Self {
        Self {
            host,
            supervisor: Supervisor::default(),
        }
    }

    pub fn host(&self) -> &Arc<dyn EngineHost> {
        &self.host
    }

    /// The transition this process runs now, if any.
    pub fn in_flight(&self) -> Option<Transition> {
        self.supervisor.in_flight()
    }

    /// The engine state as the UI should see it. Blocks on `wsl.exe`.
    ///
    /// The engine counts as running only when the pipe proxy is up, because
    /// that is what the UI itself talks to. A stopped distro behind an
    /// idle-stop proxy is `Sleeping`: any Docker client would wake it, so
    /// the UI must not be one until the user asks. Telling the proxies apart
    /// costs no `wsl.exe` call.
    pub fn state(&self) -> io::Result<EngineState> {
        let state = self.host.probe()?;
        Ok(state.seen_with(self.host.proxy_mode()))
    }

    /// The state with what the screens around it need. Blocks on `wsl.exe`.
    pub fn status(&self) -> io::Result<EngineStatus> {
        Ok(EngineStatus {
            state: self.state()?,
            bases: engine::bases(),
            mirrored_networking: self.host.mirrored_networking(),
        })
    }

    /// See [`reachable`].
    pub fn reachable(&self) -> bool {
        reachable(self.host.as_ref())
    }

    /// Installs the distro from the published rootfs for `base`, or for the
    /// default base. `progress` receives one line per step. Blocks.
    pub fn install(&self, base: Option<String>, progress: &mut dyn FnMut(&str)) -> io::Result<()> {
        self.supervisor.run(Transition::Installing, || {
            let base = base.unwrap_or_else(|| engine::bases()[0].to_string());
            self.host.install(&base, progress)
        })
    }

    /// Starts the distro, dockerd and the pipe proxy. Wakes the engine if it
    /// sleeps. Blocks until the engine answers.
    pub fn start(&self) -> io::Result<()> {
        self.supervisor
            .run(Transition::Starting, || self.start_engine())
    }

    /// Stops the pipe proxy and the distro. Blocks. `before` runs first,
    /// inside the transition: it is where the streams are closed, so the UI
    /// is not told that the engine disconnected when it was asked to stop.
    pub fn stop(&self, before: impl FnOnce()) -> io::Result<()> {
        self.supervisor.run(Transition::Stopping, || {
            before();
            // Waits for the proxy to exit, so a start right after this stop
            // does not mistake the leaving proxy for a running one.
            self.host.stop_proxy();
            self.host.stop_distro()
        })
    }

    fn start_engine(&self) -> io::Result<()> {
        let host = &self.host;
        // An idle-stop proxy owns the engine's lifetime, so it is asked for
        // the engine the way a Docker client asks: with a request through
        // the pipe. It starts the distro if it has to. Started behind its
        // back, the distro would be up with the proxy still believing it
        // stopped, and nothing would stop it again.
        if host.proxy_mode() == ProxyMode::IdleStop && host.wake().is_ok() {
            return Ok(());
        }
        // No such proxy, or it could not bring the engine up: starting it
        // here reports why not.
        host.start_distro()?;
        if host.proxy_mode().is_running() {
            return Ok(());
        }
        host.spawn_proxy()?;
        for _ in 0..PROXY_START_CHECKS {
            if host.proxy_mode().is_running() {
                return Ok(());
            }
            std::thread::sleep(PROXY_START_PAUSE);
        }
        Err(io::Error::other("the pipe proxy did not start"))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;

    use super::*;

    /// An engine that is whatever the test says, and that records what was
    /// asked of it.
    pub(crate) struct FakeHost {
        pub state: Mutex<EngineState>,
        pub proxy: Mutex<ProxyMode>,
        pub distro_running: Mutex<bool>,
        pub wake_result: Mutex<Option<String>>,
        pub start_result: Mutex<Option<String>>,
        /// The proxy mode a spawned proxy shows up as.
        pub spawned_proxy: Mutex<ProxyMode>,
        /// When set, the next start of the distro waits until the sender
        /// says so or goes away: a start that takes a while.
        pub start_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        pub calls: Mutex<Vec<&'static str>>,
    }

    impl FakeHost {
        pub fn new(state: EngineState, proxy: ProxyMode) -> Arc<Self> {
            Arc::new(Self {
                distro_running: Mutex::new(state == EngineState::Running),
                state: Mutex::new(state),
                proxy: Mutex::new(proxy),
                wake_result: Mutex::new(None),
                start_result: Mutex::new(None),
                spawned_proxy: Mutex::new(ProxyMode::Plain),
                start_gate: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
            })
        }

        pub fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }

        fn outcome(result: &Mutex<Option<String>>) -> io::Result<()> {
            match result.lock().unwrap().clone() {
                Some(message) => Err(io::Error::other(message)),
                None => Ok(()),
            }
        }
    }

    impl EngineHost for FakeHost {
        fn probe(&self) -> io::Result<EngineState> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn proxy_mode(&self) -> ProxyMode {
            *self.proxy.lock().unwrap()
        }

        fn distro_running(&self) -> bool {
            *self.distro_running.lock().unwrap()
        }

        fn wake(&self) -> io::Result<()> {
            self.record("wake");
            Self::outcome(&self.wake_result)
        }

        fn start_distro(&self) -> io::Result<()> {
            self.record("start_distro");
            let gate = self.start_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                // Bounded, so a test that forgets the gate ends anyway.
                let _ = gate.recv_timeout(Duration::from_secs(10));
            }
            Self::outcome(&self.start_result)?;
            *self.state.lock().unwrap() = EngineState::Running;
            *self.distro_running.lock().unwrap() = true;
            Ok(())
        }

        fn spawn_proxy(&self) -> io::Result<()> {
            self.record("spawn_proxy");
            *self.proxy.lock().unwrap() = *self.spawned_proxy.lock().unwrap();
            Ok(())
        }

        fn stop_proxy(&self) {
            self.record("stop_proxy");
            *self.proxy.lock().unwrap() = ProxyMode::Down;
        }

        fn stop_distro(&self) -> io::Result<()> {
            self.record("stop_distro");
            *self.state.lock().unwrap() = EngineState::Stopped;
            *self.distro_running.lock().unwrap() = false;
            Ok(())
        }

        fn install(&self, base: &str, progress: &mut dyn FnMut(&str)) -> io::Result<()> {
            self.record("install");
            progress(&format!("Installing {base}"));
            *self.state.lock().unwrap() = EngineState::Stopped;
            Ok(())
        }

        fn mirrored_networking(&self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeHost;
    use super::*;

    /// The error a start request gets while `in_flight` is running.
    fn refusal(in_flight: Transition) -> String {
        let supervisor = Supervisor::default();
        let outcome = supervisor.run(in_flight, || {
            let refused = supervisor.run(Transition::Starting, || Ok(()));
            Ok(refused.unwrap_err().to_string())
        });
        outcome.unwrap()
    }

    #[test]
    fn a_second_request_is_refused_while_one_is_in_flight() {
        assert_eq!(
            refusal(Transition::Starting),
            "the engine is already starting"
        );
        assert_eq!(
            refusal(Transition::Stopping),
            "the engine is already stopping"
        );
        assert_eq!(
            refusal(Transition::Installing),
            "the engine is already being installed"
        );
    }

    #[test]
    fn the_refused_work_does_not_run() {
        let supervisor = Supervisor::default();
        let mut ran = false;
        let outcome = supervisor.run(Transition::Stopping, || {
            let refused = supervisor.run(Transition::Starting, || {
                ran = true;
                Ok(())
            });
            Ok(refused.is_err())
        });
        assert!(outcome.unwrap());
        assert!(!ran);
    }

    #[test]
    fn the_next_request_runs_once_the_first_is_over() {
        let supervisor = Supervisor::default();
        supervisor.run(Transition::Starting, || Ok(())).unwrap();
        let boom = || Err::<(), _>(io::Error::other("boom"));
        let failed = supervisor.run(Transition::Stopping, boom);
        assert_eq!(failed.unwrap_err().to_string(), "boom");
        let third = supervisor.run(Transition::Starting, || Ok(7));
        assert_eq!(third.unwrap(), 7);
    }

    #[test]
    fn the_transition_in_flight_can_be_asked_for() {
        let supervisor = Supervisor::default();
        assert_eq!(supervisor.in_flight(), None);
        let seen = supervisor.run(Transition::Stopping, || Ok(supervisor.in_flight()));
        assert_eq!(seen.unwrap(), Some(Transition::Stopping));
        assert_eq!(supervisor.in_flight(), None);
    }

    #[test]
    fn a_stopped_distro_behind_an_idle_stop_proxy_is_asleep() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::IdleStop);
        let control = EngineControl::new(host);
        assert_eq!(control.state().unwrap(), EngineState::Sleeping);
        // Looking at it asked for nothing that would wake it.
        let status = control.status().unwrap();
        assert_eq!(status.state, EngineState::Sleeping);
        assert!(!status.bases.is_empty());
    }

    #[test]
    fn docker_without_a_proxy_still_counts_as_starting() {
        let host = FakeHost::new(EngineState::Running, ProxyMode::Down);
        let control = EngineControl::new(host);
        assert_eq!(control.state().unwrap(), EngineState::Starting);
    }

    #[test]
    fn a_sleeping_engine_is_not_reachable_whichever_proxy_runs() {
        for proxy in [ProxyMode::Plain, ProxyMode::IdleStop] {
            let host = FakeHost::new(EngineState::Stopped, proxy);
            assert!(!EngineControl::new(host).reachable());
        }
        let no_proxy = FakeHost::new(EngineState::Running, ProxyMode::Down);
        assert!(!EngineControl::new(no_proxy).reachable());
        let up = FakeHost::new(EngineState::Running, ProxyMode::Plain);
        assert!(EngineControl::new(up).reachable());
    }

    #[test]
    fn a_sleeping_engine_is_woken_through_its_proxy() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::IdleStop);
        let control = EngineControl::new(host.clone());
        control.start().unwrap();
        // Not started behind the proxy's back.
        assert_eq!(host.calls(), ["wake"]);
    }

    #[test]
    fn an_idle_stop_proxy_that_cannot_wake_the_engine_gets_a_direct_start() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::IdleStop);
        *host.wake_result.lock().unwrap() = Some("no answer".to_string());
        let control = EngineControl::new(host.clone());
        control.start().unwrap();
        // The proxy is up already, so none is started.
        assert_eq!(host.calls(), ["wake", "start_distro"]);
    }

    #[test]
    fn a_start_without_a_proxy_starts_the_distro_and_then_the_proxy() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::Down);
        let control = EngineControl::new(host.clone());
        control.start().unwrap();
        assert_eq!(host.calls(), ["start_distro", "spawn_proxy"]);
        assert_eq!(control.state().unwrap(), EngineState::Running);
    }

    #[test]
    fn a_start_behind_a_plain_proxy_only_starts_the_distro() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::Plain);
        let control = EngineControl::new(host.clone());
        control.start().unwrap();
        assert_eq!(host.calls(), ["start_distro"]);
    }

    #[test]
    fn a_distro_that_does_not_start_reports_why_and_starts_no_proxy() {
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::Down);
        *host.start_result.lock().unwrap() = Some("dockerd did not become ready".to_string());
        let control = EngineControl::new(host.clone());
        let error = control.start().unwrap_err();
        assert_eq!(error.to_string(), "dockerd did not become ready");
        assert_eq!(host.calls(), ["start_distro"]);
        // The failed start is over: the next request is not turned away.
        assert_eq!(control.in_flight(), None);
    }

    #[test]
    fn a_stop_closes_the_streams_first_then_the_proxy_then_the_distro() {
        let host = FakeHost::new(EngineState::Running, ProxyMode::IdleStop);
        let control = EngineControl::new(host.clone());
        let mut order = Vec::new();
        control
            .stop(|| {
                // Nothing was stopped yet, and the transition is in flight.
                order.push(host.calls().len());
                assert_eq!(control.in_flight(), Some(Transition::Stopping));
            })
            .unwrap();
        assert_eq!(order, [0]);
        assert_eq!(host.calls(), ["stop_proxy", "stop_distro"]);
        assert_eq!(control.state().unwrap(), EngineState::Stopped);
    }

    #[test]
    fn an_install_reports_its_steps_and_uses_the_default_base() {
        let host = FakeHost::new(EngineState::NotInstalled, ProxyMode::Down);
        let control = EngineControl::new(host.clone());
        let mut steps = Vec::new();
        control
            .install(None, &mut |step| steps.push(step.to_string()))
            .unwrap();
        assert_eq!(steps, [format!("Installing {}", engine::bases()[0])]);
        control
            .install(Some("alpine".to_string()), &mut |step| {
                steps.push(step.to_string())
            })
            .unwrap();
        assert_eq!(steps[1], "Installing alpine");
    }

    #[test]
    fn a_request_during_a_transition_is_refused_without_touching_the_engine() {
        let host = FakeHost::new(EngineState::Running, ProxyMode::Plain);
        let control = EngineControl::new(host.clone());
        let refused = control.stop(|| {
            let error = control.start().unwrap_err();
            assert_eq!(error.to_string(), "the engine is already stopping");
        });
        refused.unwrap();
        assert!(!host.calls().contains(&"start_distro"));
        assert!(!host.calls().contains(&"wake"));
    }
}
