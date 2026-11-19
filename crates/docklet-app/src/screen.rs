//! Which screen shows for an engine state, and what it says.

use docklet_core::EngineState;

use crate::policy::uses_docker_api;

/// The command that installs WSL, shown for copying when WSL is missing.
pub const WSL_INSTALL_COMMAND: &str = "wsl --install --no-distribution";

/// Shown, dismissibly, when `EngineStatus::mirrored_networking` is set.
pub const MIRRORED_NETWORKING_WARNING: &str = "Your .wslconfig sets networkingMode=mirrored. \
     Published container ports rely on WSL's NAT localhost forwarding and may not be reachable \
     from Windows.";

/// The short name of a state, for the pill next to the title.
pub fn state_label(state: &EngineState) -> &'static str {
    match state {
        EngineState::WslMissing => "WSL is not installed",
        EngineState::NotInstalled => "Not installed",
        EngineState::Stopped => "Stopped",
        EngineState::Sleeping => "Sleeping",
        EngineState::Starting => "Starting",
        EngineState::Running => "Running",
        EngineState::Stopping => "Stopping",
        EngineState::Error(_) => "Error",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineScreen {
    WslMissing,
    Install,
    Stopped,
    Sleeping,
    Busy,
    Error,
    /// The resource lists, which are what talks to Docker.
    Running,
}

/// Which screen shows for an engine state. Only the `Running` screen has the
/// resource lists.
pub fn screen_for(state: &EngineState) -> EngineScreen {
    match state {
        EngineState::WslMissing => EngineScreen::WslMissing,
        EngineState::NotInstalled => EngineScreen::Install,
        EngineState::Stopped => EngineScreen::Stopped,
        EngineState::Sleeping => EngineScreen::Sleeping,
        EngineState::Starting | EngineState::Stopping => EngineScreen::Busy,
        EngineState::Error(_) => EngineScreen::Error,
        EngineState::Running => EngineScreen::Running,
    }
}

/// What a button on an engine screen asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCommand {
    /// Install the distro, from the base picked on the screen.
    Install,
    Start,
    Stop,
    /// Look at the engine state again.
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenButton {
    pub label: &'static str,
    pub command: EngineCommand,
    /// The button the screen suggests.
    pub primary: bool,
}

impl ScreenButton {
    const fn new(label: &'static str, command: EngineCommand, primary: bool) -> Self {
        Self {
            label,
            command,
            primary,
        }
    }
}

/// The text and buttons of an engine screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenText {
    pub title: &'static str,
    /// What happened or what to do, in a sentence or two.
    pub body: Option<&'static str>,
    /// The engine's own words: the reason of an error.
    pub detail: Option<String>,
    /// A command to show with a copy button.
    pub command: Option<&'static str>,
    /// Shows an indeterminate progress indicator.
    pub progress: bool,
    /// Start and Stop are disabled while one of the window's own engine
    /// requests is in flight; the engine would refuse a second one.
    pub buttons: Vec<ScreenButton>,
}

const START: ScreenButton = ScreenButton::new("Start engine", EngineCommand::Start, false);
const START_PRIMARY: ScreenButton = ScreenButton::new("Start engine", EngineCommand::Start, true);
const STOP: ScreenButton = ScreenButton::new("Stop engine", EngineCommand::Stop, false);

/// The screen for `state`. For `Running` it is only the header's button:
/// the lists take the place of the text.
pub fn screen_text(state: &EngineState) -> ScreenText {
    let blank = ScreenText {
        title: "",
        body: None,
        detail: None,
        command: None,
        progress: false,
        buttons: Vec::new(),
    };
    match state {
        EngineState::WslMissing => ScreenText {
            title: "WSL is required",
            body: Some(
                "Docklet runs Docker inside the Windows Subsystem for Linux. Run this in a \
                 terminal started as administrator, restart Windows if asked, then check again.",
            ),
            command: Some(WSL_INSTALL_COMMAND),
            buttons: vec![ScreenButton::new(
                "Check again",
                EngineCommand::Refresh,
                true,
            )],
            ..blank
        },
        EngineState::NotInstalled => ScreenText {
            title: "Set up the Docker engine",
            body: Some(
                "Docklet installs Docker Engine into its own WSL distro. Other distros are not \
                 touched.",
            ),
            buttons: vec![ScreenButton::new("Install", EngineCommand::Install, true)],
            ..blank
        },
        EngineState::Stopped => ScreenText {
            title: "The engine is stopped",
            buttons: vec![START_PRIMARY],
            ..blank
        },
        // No list is shown here on purpose: fetching one is a Docker
        // connection, and any connection wakes the engine.
        EngineState::Sleeping => ScreenText {
            title: "The engine is asleep",
            body: Some(
                "Nothing was using Docker, so Docklet stopped the engine to free its memory. It \
                 starts again when you start it here, and by itself with the next Docker command \
                 from anywhere. Stopping it keeps it off until you start it.",
            ),
            buttons: vec![START_PRIMARY, STOP],
            ..blank
        },
        // Without the buttons the window could only wait: Docker up with the
        // pipe proxy down, or a dockerd that never gets ready, both show as
        // starting for good.
        EngineState::Starting | EngineState::Stopping => ScreenText {
            title: if *state == EngineState::Starting {
                "Starting the engine…"
            } else {
                "Stopping the engine…"
            },
            body: Some("If this does not finish, start the engine again to repair it, or stop it."),
            progress: true,
            buttons: vec![START, STOP],
            ..blank
        },
        EngineState::Error(reason) => ScreenText {
            title: "The engine hit a problem",
            detail: Some(if reason.is_empty() {
                "Unknown error.".to_string()
            } else {
                reason.clone()
            }),
            buttons: vec![ScreenButton::new("Retry", EngineCommand::Refresh, true)],
            ..blank
        },
        EngineState::Running => ScreenText {
            buttons: vec![STOP],
            ..blank
        },
    }
}

/// True when the screen for `state` shows the resource lists. The same
/// states in which Docker may be called at all.
pub fn shows_lists(state: &EngineState) -> bool {
    uses_docker_api(state)
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
    fn every_engine_state_has_a_non_empty_label() {
        for state in all_states() {
            assert!(!state_label(&state).is_empty());
        }
    }

    #[test]
    fn maps_every_engine_state_to_a_screen() {
        let expected = [
            EngineScreen::WslMissing,
            EngineScreen::Install,
            EngineScreen::Stopped,
            EngineScreen::Sleeping,
            EngineScreen::Busy,
            EngineScreen::Running,
            EngineScreen::Busy,
            EngineScreen::Error,
        ];
        for (state, screen) in all_states().iter().zip(expected) {
            assert_eq!(screen_for(state), screen, "{state:?}");
        }
    }

    #[test]
    fn shows_the_lists_which_call_docker_only_when_docker_may_be_called() {
        for state in all_states() {
            let running = screen_for(&state) == EngineScreen::Running;
            assert_eq!(running, uses_docker_api(&state), "{state:?}");
            assert_eq!(running, shows_lists(&state), "{state:?}");
        }
        // A sleeping engine has a screen of its own, not the stopped one.
        assert_ne!(
            screen_for(&EngineState::Sleeping),
            screen_for(&EngineState::Stopped)
        );
    }

    fn commands(state: &EngineState) -> Vec<EngineCommand> {
        let buttons = screen_text(state).buttons;
        buttons.iter().map(|button| button.command).collect()
    }

    #[test]
    fn each_screen_offers_what_gets_the_engine_out_of_its_state() {
        use EngineCommand::{Install, Refresh, Start, Stop};
        assert_eq!(commands(&EngineState::WslMissing), [Refresh]);
        assert_eq!(commands(&EngineState::NotInstalled), [Install]);
        assert_eq!(commands(&EngineState::Stopped), [Start]);
        // A sleeping engine can be woken, or stopped for good.
        assert_eq!(commands(&EngineState::Sleeping), [Start, Stop]);
        // A transition that hangs can be repaired or given up.
        assert_eq!(commands(&EngineState::Starting), [Start, Stop]);
        assert_eq!(commands(&EngineState::Stopping), [Start, Stop]);
        assert_eq!(commands(&EngineState::Error(String::new())), [Refresh]);
        assert_eq!(commands(&EngineState::Running), [Stop]);
    }

    #[test]
    fn a_transition_shows_progress_and_says_which_one() {
        let starting = screen_text(&EngineState::Starting);
        assert_eq!(starting.title, "Starting the engine…");
        assert!(starting.progress);
        let stopping = screen_text(&EngineState::Stopping);
        assert_eq!(stopping.title, "Stopping the engine…");
        assert!(stopping.progress);
        assert!(!screen_text(&EngineState::Stopped).progress);
    }

    #[test]
    fn an_error_shows_its_reason() {
        let screen = screen_text(&EngineState::Error("dockerd exited".to_string()));
        assert_eq!(screen.detail.as_deref(), Some("dockerd exited"));
        let unknown = screen_text(&EngineState::Error(String::new()));
        assert_eq!(unknown.detail.as_deref(), Some("Unknown error."));
    }

    #[test]
    fn the_wsl_screen_gives_the_command_to_copy() {
        let screen = screen_text(&EngineState::WslMissing);
        assert_eq!(screen.command, Some(WSL_INSTALL_COMMAND));
    }

    #[test]
    fn at_most_one_button_is_the_suggested_one() {
        for state in all_states() {
            let buttons = screen_text(&state).buttons;
            assert!(buttons.iter().filter(|button| button.primary).count() <= 1);
        }
    }
}
