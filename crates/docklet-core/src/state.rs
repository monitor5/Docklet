use serde::{Deserialize, Serialize};

/// Lifecycle of the Docklet engine (the dedicated WSL2 distro and its dockerd).
///
/// `WslMissing → NotInstalled → Stopped → Starting → Running → Stopping → Stopped`,
/// with `Error` reachable from any state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum EngineState {
    WslMissing,
    NotInstalled,
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

impl EngineState {
    /// True while a start or stop is in flight and further commands must wait.
    pub fn is_transitioning(&self) -> bool {
        matches!(self, Self::Starting | Self::Stopping)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_starting_and_stopping_are_transitions() {
        assert!(EngineState::Starting.is_transitioning());
        assert!(EngineState::Stopping.is_transitioning());
        assert!(!EngineState::Running.is_transitioning());
        assert!(!EngineState::Error("boom".into()).is_transitioning());
    }
}
