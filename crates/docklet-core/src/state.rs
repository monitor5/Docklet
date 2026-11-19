use serde::{Deserialize, Serialize};

/// Lifecycle of the Docklet engine (the dedicated WSL2 distro and its dockerd).
///
/// `WslMissing → NotInstalled → Stopped → Starting → Running → Stopping → Stopped`,
/// with `Error` reachable from any state. With idle stop, an unused engine
/// goes `Running → Sleeping` and the next Docker client takes it back
/// through `Starting` to `Running`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum EngineState {
    WslMissing,
    NotInstalled,
    Stopped,
    /// The distro is stopped and a pipe proxy with idle stop is listening:
    /// the first Docker client that connects starts the engine again. Never
    /// reported by a probe of the engine alone; see [`EngineState::seen_with`].
    Sleeping,
    Starting,
    Running,
    Stopping,
    Error(String),
}

/// The pipe proxy of this user, as far as it can be told from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// No proxy is running: nothing listens on the Docker pipe.
    Down,
    /// A proxy that only relays. It never starts or stops the engine.
    Plain,
    /// A proxy that stops the engine when it is unused and starts it again
    /// when a client connects.
    IdleStop,
}

impl ProxyMode {
    pub fn is_running(self) -> bool {
        self != Self::Down
    }
}

impl EngineState {
    /// True while a start or stop is in flight and further commands must wait.
    pub fn is_transitioning(&self) -> bool {
        matches!(self, Self::Starting | Self::Stopping)
    }

    /// The state as a Docker client meets it: what the engine itself reports
    /// (`engine::probe`) together with the pipe proxy in front of it.
    ///
    /// - A stopped distro behind an idle-stop proxy is asleep, not stopped:
    ///   Docker commands work and the first one wakes it.
    /// - Docker without any proxy cannot be reached from Windows yet, so it
    ///   still counts as starting.
    pub fn seen_with(self, proxy: ProxyMode) -> Self {
        match (self, proxy) {
            (Self::Stopped, ProxyMode::IdleStop) => Self::Sleeping,
            (Self::Running, ProxyMode::Down) => Self::Starting,
            (state, _) => state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXIES: [ProxyMode; 3] = [ProxyMode::Down, ProxyMode::Plain, ProxyMode::IdleStop];

    #[test]
    fn only_starting_and_stopping_are_transitions() {
        assert!(EngineState::Starting.is_transitioning());
        assert!(EngineState::Stopping.is_transitioning());
        assert!(!EngineState::Running.is_transitioning());
        assert!(!EngineState::Sleeping.is_transitioning());
        assert!(!EngineState::Error("boom".into()).is_transitioning());
    }

    #[test]
    fn a_stopped_distro_sleeps_only_behind_an_idle_stop_proxy() {
        assert_eq!(
            EngineState::Stopped.seen_with(ProxyMode::IdleStop),
            EngineState::Sleeping
        );
        // A plain proxy relays into a distro that is not there: stopped.
        assert_eq!(
            EngineState::Stopped.seen_with(ProxyMode::Plain),
            EngineState::Stopped
        );
        assert_eq!(
            EngineState::Stopped.seen_with(ProxyMode::Down),
            EngineState::Stopped
        );
    }

    #[test]
    fn docker_without_a_proxy_is_still_starting() {
        assert_eq!(
            EngineState::Running.seen_with(ProxyMode::Down),
            EngineState::Starting
        );
        for proxy in [ProxyMode::Plain, ProxyMode::IdleStop] {
            assert_eq!(EngineState::Running.seen_with(proxy), EngineState::Running);
        }
    }

    #[test]
    fn every_other_state_is_what_the_engine_reports() {
        let states = [
            EngineState::WslMissing,
            EngineState::NotInstalled,
            EngineState::Starting,
            EngineState::Stopping,
            EngineState::Error("boom".to_string()),
        ];
        for state in states {
            for proxy in PROXIES {
                assert_eq!(state.clone().seen_with(proxy), state, "{proxy:?}");
            }
        }
    }

    #[test]
    fn only_a_missing_proxy_is_not_running() {
        assert!(!ProxyMode::Down.is_running());
        assert!(ProxyMode::Plain.is_running());
        assert!(ProxyMode::IdleStop.is_running());
    }
}
