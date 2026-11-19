//! When the app looks at the engine, and when it must not.
//!
//! All of it is pure: the services in `streams` and `app` carry it out.

use std::time::Duration;

use docklet_core::EngineState;

use crate::streams::DockerEvent;

/// What the UI last said about its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowState {
    /// No window: nothing is watched, followed or sampled.
    #[default]
    Gone,
    /// Minimised or covered. Nothing is polled or sampled. `Streams` keeps
    /// what is open, so a followed log goes on; `App` ends the event
    /// subscription as well, since it loads no list for a hidden window.
    Hidden,
    Visible,
}

pub const FAST_POLL: Duration = Duration::from_secs(4);
pub const SLOW_POLL: Duration = Duration::from_secs(15);

/// Lists reload this long after a Docker event, so a burst of events costs
/// one reload.
pub const EVENT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Time between two samples of the host's figures (VM memory, CPU, GPU,
/// disk) while the window is visible.
pub const HOST_USAGE_INTERVAL: Duration = Duration::from_secs(2);

/// Time between two rounds of container stats while the window is visible.
/// `docklet_engine::Client::follow_usage` keeps this pace.
pub const CONTAINER_USAGE_INTERVAL: Duration = Duration::from_secs(1);

/// Whether the app may talk to Docker in this state. Only a running engine:
/// a call in any other state fails, and in `Sleeping` it would wake an
/// engine nobody asked for.
pub fn uses_docker_api(state: &EngineState) -> bool {
    *state == EngineState::Running
}

/// Whether the lists reload on this message of the event stream. They do for
/// every Docker event and when the stream is open again, but not when it
/// just ended: nothing can be loaded then, and if the engine went to sleep
/// the reload would wake it. The engine state is looked at again instead,
/// and the next screen follows from that.
pub fn reloads_lists(event: &DockerEvent) -> bool {
    *event != DockerEvent::Disconnected
}

/// Whether the engine state is looked at again on this message of the event
/// stream. A lost or renewed stream may have hidden a change of the engine.
pub fn repolls_status(event: &DockerEvent) -> bool {
    matches!(event, DockerEvent::Disconnected | DockerEvent::Reconnecting)
}

/// States that only change when somebody acts, which the window hears
/// about. A sleeping engine wakes when some Docker client connects; the slow
/// poll notices, and it only asks `wsl.exe` for its lists, which wakes
/// nothing.
fn is_settled(state: &EngineState) -> bool {
    matches!(
        state,
        EngineState::Running
            | EngineState::Stopped
            | EngineState::Sleeping
            | EngineState::NotInstalled
    )
}

/// How long to wait before the next status poll, or `None` for no poll at
/// all. Nothing is polled while the window is not visible; it polls once
/// when it becomes visible again. Fast polling is for transitions and for
/// the time one of the window's own requests is in flight. `state` is `None`
/// until the first poll has answered.
pub fn next_poll_delay(state: Option<&EngineState>, visible: bool, busy: bool) -> Option<Duration> {
    if !visible {
        return None;
    }
    if busy || !state.is_some_and(is_settled) {
        Some(FAST_POLL)
    } else {
        Some(SLOW_POLL)
    }
}

/// What is sampled for the usage strip and the usage columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sampling {
    /// Memory, CPU and GPU of the WSL VM and the disk, read from Windows.
    /// Reading them starts nothing, so they are sampled whatever the engine
    /// does: a stopped engine shows as such.
    pub host: bool,
    /// The stats of each running container, which are Docker API calls.
    pub containers: bool,
}

/// What to sample: only while the window is visible, and container stats
/// only from a running engine, since asking a sleeping one would wake it.
pub fn sampling(window: WindowState, state: Option<&EngineState>) -> Sampling {
    let visible = window == WindowState::Visible;
    Sampling {
        host: visible,
        containers: visible && state.is_some_and(uses_docker_api),
    }
}

/// Why Docker's disk usage might be worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskUsageTrigger {
    /// The view that shows it was opened.
    ViewOpened,
    /// The user asked for fresh figures.
    Requested,
    /// A Docker event arrived.
    DockerEvent,
    /// Time passed.
    Timer,
}

/// Whether Docker's disk usage is worked out. The engine walks every layer
/// and volume for it, which takes seconds on a big engine, so only when the
/// view opens or the user asks; never on a timer or for an event, and never
/// from an engine that is not running.
pub fn loads_disk_usage(trigger: DiskUsageTrigger, state: Option<&EngineState>) -> bool {
    matches!(
        trigger,
        DiskUsageTrigger::ViewOpened | DiskUsageTrigger::Requested
    ) && state.is_some_and(uses_docker_api)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_states() -> Vec<EngineState> {
        vec![
            EngineState::WslMissing,
            EngineState::NotInstalled,
            EngineState::Stopped,
            EngineState::Sleeping,
            EngineState::Starting,
            EngineState::Running,
            EngineState::Stopping,
            EngineState::Error("boom".to_string()),
        ]
    }

    #[test]
    fn only_a_running_engine_is_talked_to() {
        let talked_to: Vec<_> = all_states().into_iter().filter(uses_docker_api).collect();
        assert_eq!(talked_to, [EngineState::Running]);
        // A call would wake a sleeping engine, so it in particular gets none.
        assert!(!uses_docker_api(&EngineState::Sleeping));
    }

    #[test]
    fn lists_reload_on_docker_events_and_on_reconnect_not_on_disconnect() {
        for kind in ["container", "image", "volume", "network", ""] {
            assert!(reloads_lists(&DockerEvent::Changed(kind.to_string())));
        }
        assert!(reloads_lists(&DockerEvent::Reconnecting));
        assert!(!reloads_lists(&DockerEvent::Disconnected));
    }

    #[test]
    fn the_state_is_looked_at_again_when_the_stream_breaks_or_returns() {
        assert!(repolls_status(&DockerEvent::Disconnected));
        assert!(repolls_status(&DockerEvent::Reconnecting));
        assert!(!repolls_status(&DockerEvent::Changed(
            "container".to_string()
        )));
    }

    #[test]
    fn does_not_poll_while_hidden() {
        let state = EngineState::Starting;
        assert_eq!(next_poll_delay(Some(&state), false, true), None);
    }

    #[test]
    fn polls_slowly_in_settled_states() {
        for state in [
            EngineState::Running,
            EngineState::Stopped,
            EngineState::Sleeping,
            EngineState::NotInstalled,
        ] {
            assert_eq!(next_poll_delay(Some(&state), true, false), Some(SLOW_POLL));
        }
    }

    #[test]
    fn polls_fast_in_transitions_and_unsettled_states() {
        for state in [
            EngineState::Starting,
            EngineState::Stopping,
            EngineState::Error("boom".to_string()),
            EngineState::WslMissing,
        ] {
            assert_eq!(next_poll_delay(Some(&state), true, false), Some(FAST_POLL));
        }
        assert_eq!(next_poll_delay(None, true, false), Some(FAST_POLL));
    }

    #[test]
    fn polls_fast_while_a_request_is_in_flight() {
        let state = EngineState::Running;
        assert_eq!(next_poll_delay(Some(&state), true, true), Some(FAST_POLL));
    }

    #[test]
    fn nothing_is_sampled_unless_the_window_is_visible() {
        let running = EngineState::Running;
        for window in [WindowState::Gone, WindowState::Hidden] {
            assert_eq!(sampling(window, Some(&running)), Sampling::default());
        }
        let visible = sampling(WindowState::Visible, Some(&running));
        assert!(visible.host && visible.containers);
    }

    #[test]
    fn container_stats_are_only_asked_of_a_running_engine() {
        for state in all_states() {
            let sampled = sampling(WindowState::Visible, Some(&state));
            // The host's figures cost no Docker call, so they always show.
            assert!(sampled.host, "{state:?}");
            assert_eq!(sampled.containers, state == EngineState::Running);
        }
        assert!(!sampling(WindowState::Visible, None).containers);
    }

    #[test]
    fn disk_usage_is_worked_out_only_on_demand() {
        let running = EngineState::Running;
        assert!(loads_disk_usage(
            DiskUsageTrigger::ViewOpened,
            Some(&running)
        ));
        assert!(loads_disk_usage(
            DiskUsageTrigger::Requested,
            Some(&running)
        ));
        assert!(!loads_disk_usage(DiskUsageTrigger::Timer, Some(&running)));
        assert!(!loads_disk_usage(
            DiskUsageTrigger::DockerEvent,
            Some(&running)
        ));
    }

    #[test]
    fn disk_usage_never_wakes_an_engine() {
        for state in all_states() {
            let loads = loads_disk_usage(DiskUsageTrigger::Requested, Some(&state));
            assert_eq!(loads, state == EngineState::Running, "{state:?}");
        }
        assert!(!loads_disk_usage(DiskUsageTrigger::Requested, None));
    }

    #[test]
    fn the_sampling_intervals_are_the_ones_the_owner_asked_for() {
        assert_eq!(HOST_USAGE_INTERVAL, Duration::from_secs(2));
        assert_eq!(CONTAINER_USAGE_INTERVAL, Duration::from_secs(1));
    }
}
