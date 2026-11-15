//! Engine lifecycle: the one place that installs, starts and stops the engine.
//!
//! The tray and the window can ask for a start or a stop at the same moment.
//! Every request goes through the [`Supervisor`] in the app state, which lets
//! one transition run and turns the others away.

use std::fmt;
use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use docklet_core::{EngineState, ProxyMode, engine};
use docklet_proxy::control;
use docklet_wsl::{Engine, RootfsSource};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

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

/// The engine state as the UI should see it. Blocks on `wsl.exe`.
///
/// The engine counts as running only when the pipe proxy is up, because that
/// is what the UI itself talks to. A stopped distro behind an idle-stop proxy
/// is `Sleeping`: any Docker client would wake it, so the UI must not be one
/// until the user asks. Telling the proxies apart costs no `wsl.exe` call.
pub fn status() -> io::Result<EngineState> {
    let state = Engine::system()?.status();
    Ok(state.seen_with(control::mode()))
}

/// True when a Docker API connection would be answered without starting
/// anything: the pipe proxy is up and the distro runs. Blocks on `wsl.exe`.
///
/// Asked before the event stream is reopened, because trying is not harmless.
/// A plain proxy starts `wsl.exe -d docklet` for every connection, and that
/// boots a distro the user stopped from outside (`wsl --shutdown`). An
/// idle-stop proxy starts a sleeping engine for every connection, and the
/// window would keep an engine awake that nobody uses. A sleeping engine is
/// therefore not reachable in this sense, whichever proxy runs.
pub fn reachable() -> bool {
    control::is_running()
        && Engine::system().is_ok_and(|engine| {
            let running = engine.wsl().running();
            running.is_ok_and(|names| names.iter().any(|name| name == docklet_wsl::DISTRO))
        })
}

/// Installs the distro from the published rootfs for `base`, or for the
/// default base. `progress` receives one line per step. Blocks.
pub fn install(
    app: &AppHandle,
    base: Option<String>,
    progress: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let state = app.state::<AppState>();
    state.supervisor.run(Transition::Installing, || {
        let base = base.unwrap_or_else(|| engine::bases()[0].to_string());
        Engine::system()?.install(&RootfsSource::Base(base), progress)
    })
}

/// Starts the distro, dockerd and the pipe proxy, then subscribes to Docker's
/// events. Wakes the engine if it sleeps. Blocks until the engine answers.
pub fn start(app: &AppHandle) -> io::Result<()> {
    let state = app.state::<AppState>();
    state.supervisor.run(Transition::Starting, start_engine)?;
    state.streams.watch_events(app);
    Ok(())
}

/// Stops the pipe proxy and the distro. Blocks.
pub fn stop(app: &AppHandle) -> io::Result<()> {
    let state = app.state::<AppState>();
    state.supervisor.run(Transition::Stopping, || {
        // The streams go first, so the UI is not told that the engine
        // disconnected when it was asked to stop.
        state.streams.stop_all();
        stop_engine()
    })
}

fn start_engine() -> io::Result<()> {
    // An idle-stop proxy owns the engine's lifetime, so it is asked for the
    // engine the way a Docker client asks: with a request through the pipe.
    // It starts the distro if it has to. Started behind its back, the distro
    // would be up with the proxy still believing it stopped, and nothing
    // would stop it again.
    if control::mode() == ProxyMode::IdleStop
        && docklet_proxy::ping(docklet_proxy::PING_TIMEOUT).is_ok()
    {
        return Ok(());
    }
    // No such proxy, or it could not bring the engine up: starting it here
    // reports why not.
    Engine::system()?.start()?;
    if control::is_running() {
        return Ok(());
    }
    // The proxy is this same executable in `--proxy` mode, detached so the
    // Windows docker CLI keeps working when the GUI exits.
    let exe = std::env::current_exe()?;
    docklet_wsl::spawn_detached(&format!("\"{}\" --proxy", exe.display()))?;
    for _ in 0..100 {
        if control::is_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::other("the pipe proxy did not start"))
}

fn stop_engine() -> io::Result<()> {
    // Waits for the proxy to exit, so a start right after this stop does not
    // mistake the leaving proxy for a running one.
    control::stop();
    Engine::system()?.stop()
}

#[cfg(test)]
mod tests {
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
}
