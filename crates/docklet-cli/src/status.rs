//! What `docklet status` reports, as text and as data.

use std::path::Path;

use docklet_core::EngineState;
use docklet_core::docker_client::DockerCli;
use docklet_core::engine::{DISTRO_NAME, ENGINE_DOCKER_HOST, ENGINE_PIPE, ROOTFS_VERSION};
use docklet_core::wsl::WslVersion;

use crate::json::{Json, Object};

/// The engine state as the user meets it: Docker counts as running only
/// once the pipe proxy is up too, because that is when clients can connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// First word of the text output: `not-installed`, `running`, ...
    pub word: &'static str,
    /// What follows the word, if anything.
    pub detail: Option<String>,
    /// The next command towards a usable engine.
    pub hint: Option<&'static str>,
}

impl StatusView {
    pub fn of(state: &EngineState, proxy_running: bool) -> Self {
        let view = |word, detail: Option<&str>, hint| Self {
            word,
            detail: detail.map(str::to_string),
            hint,
        };
        match state {
            EngineState::WslMissing => view(
                "wsl-missing",
                Some("WSL is not installed"),
                Some("wsl --install --no-distribution"),
            ),
            EngineState::NotInstalled => view(
                "not-installed",
                Some("run `docklet install` to set up the engine"),
                Some("docklet install"),
            ),
            EngineState::Stopped => view("stopped", None, Some("docklet start")),
            EngineState::Starting => view(
                "starting",
                Some("the distro is up, Docker is not ready yet"),
                Some("docklet wait"),
            ),
            EngineState::Running if proxy_running => view("running", None, None),
            EngineState::Running => view(
                "starting",
                Some("Docker is up but the pipe proxy is not; run `docklet start`"),
                Some("docklet start"),
            ),
            EngineState::Stopping => view("stopping", None, None),
            EngineState::Error(reason) => view("error", Some(reason), Some("docklet doctor")),
        }
    }

    /// The line `docklet status` prints.
    pub fn line(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{}: {detail}", self.word),
            None => self.word.to_string(),
        }
    }

    /// The word as an identifier: `not_installed` for `not-installed`.
    pub fn state(&self) -> String {
        self.word.replace('-', "_")
    }

    /// True when Docker commands will work.
    pub fn ready(&self) -> bool {
        self.word == "running"
    }
}

/// Everything `status --json` reports besides the state.
pub struct StatusFacts {
    pub proxy_running: bool,
    pub wsl_version: Option<WslVersion>,
    pub mirrored_networking: bool,
    pub context_name: &'static str,
    pub context_exists: bool,
    pub docker_cli: Option<DockerCli>,
}

pub fn status_data(view: &StatusView, facts: &StatusFacts) -> Json {
    Object::new()
        .set("state", view.state())
        .set("reason", view.detail.clone())
        .set("ready", view.ready())
        .set("hint", view.hint)
        .set("proxy_running", facts.proxy_running)
        .set("pipe", ENGINE_PIPE)
        .set("docker_host", ENGINE_DOCKER_HOST)
        .set("distro", DISTRO_NAME)
        .set("rootfs_version", ROOTFS_VERSION.trim())
        .set(
            "wsl_version",
            facts.wsl_version.map(|version| version.to_string()),
        )
        .set("docklet_version", env!("CARGO_PKG_VERSION"))
        .set("mirrored_networking", facts.mirrored_networking)
        .set(
            "docker_context",
            docker_context(facts.context_name, facts.context_exists),
        )
        .set(
            "docker_cli",
            facts
                .docker_cli
                .as_ref()
                .map(|cli| docker_cli(&cli.path, cli.on_path)),
        )
        .into()
}

pub fn docker_context(name: &str, exists: bool) -> Json {
    Object::new().set("name", name).set("exists", exists).into()
}

pub fn docker_cli(path: &Path, on_path: bool) -> Json {
    Object::new()
        .set("path", path.display().to_string())
        .set("on_path", on_path)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn line(state: EngineState, proxy_running: bool) -> String {
        StatusView::of(&state, proxy_running).line()
    }

    /// The exact lines the E2E workflow and the skill rely on.
    #[test]
    fn text_lines_are_what_they_always_were() {
        for proxy in [true, false] {
            assert_eq!(
                line(EngineState::WslMissing, proxy),
                "wsl-missing: WSL is not installed"
            );
            assert_eq!(
                line(EngineState::NotInstalled, proxy),
                "not-installed: run `docklet install` to set up the engine"
            );
            assert_eq!(line(EngineState::Stopped, proxy), "stopped");
            assert_eq!(
                line(EngineState::Starting, proxy),
                "starting: the distro is up, Docker is not ready yet"
            );
            assert_eq!(line(EngineState::Stopping, proxy), "stopping");
            assert_eq!(
                line(EngineState::Error("boom".to_string()), proxy),
                "error: boom"
            );
        }
        assert_eq!(line(EngineState::Running, true), "running");
        assert_eq!(
            line(EngineState::Running, false),
            "starting: Docker is up but the pipe proxy is not; run `docklet start`"
        );
    }

    #[test]
    fn the_json_state_is_the_first_word_of_the_text_in_snake_case() {
        let states = [
            (EngineState::WslMissing, "wsl_missing"),
            (EngineState::NotInstalled, "not_installed"),
            (EngineState::Stopped, "stopped"),
            (EngineState::Starting, "starting"),
            (EngineState::Running, "running"),
            (EngineState::Stopping, "stopping"),
            (EngineState::Error("boom".to_string()), "error"),
        ];
        for (state, expected) in states {
            let view = StatusView::of(&state, true);
            assert_eq!(view.state(), expected);
            let line = view.line();
            let first_word = line.split(':').next().unwrap();
            assert_eq!(first_word.replace('-', "_"), expected);
        }
    }

    #[test]
    fn ready_only_when_docker_and_the_proxy_are_both_up() {
        assert!(StatusView::of(&EngineState::Running, true).ready());
        let without_proxy = StatusView::of(&EngineState::Running, false);
        assert!(!without_proxy.ready());
        assert_eq!(without_proxy.state(), "starting");
        for state in [
            EngineState::WslMissing,
            EngineState::NotInstalled,
            EngineState::Stopped,
            EngineState::Starting,
            EngineState::Stopping,
            EngineState::Error("boom".to_string()),
        ] {
            assert!(!StatusView::of(&state, true).ready(), "{state:?}");
        }
    }

    #[test]
    fn hints_lead_towards_a_usable_engine() {
        let hint = |state, proxy| StatusView::of(&state, proxy).hint;
        assert_eq!(
            hint(EngineState::WslMissing, false),
            Some("wsl --install --no-distribution")
        );
        assert_eq!(
            hint(EngineState::NotInstalled, false),
            Some("docklet install")
        );
        assert_eq!(hint(EngineState::Stopped, false), Some("docklet start"));
        assert_eq!(hint(EngineState::Starting, false), Some("docklet wait"));
        assert_eq!(hint(EngineState::Running, false), Some("docklet start"));
        assert_eq!(hint(EngineState::Running, true), None);
        assert_eq!(
            hint(EngineState::Error("boom".to_string()), true),
            Some("docklet doctor")
        );
    }

    #[test]
    fn data_of_a_running_engine() {
        let view = StatusView::of(&EngineState::Running, true);
        let facts = StatusFacts {
            proxy_running: true,
            wsl_version: Some(WslVersion {
                major: 3,
                minor: 0,
                patch: 5,
            }),
            mirrored_networking: false,
            context_name: "docklet",
            context_exists: true,
            docker_cli: Some(DockerCli {
                path: PathBuf::from("docker.exe"),
                on_path: true,
            }),
        };
        let expected = format!(
            concat!(
                r#"{{"state":"running","reason":null,"ready":true,"hint":null,"#,
                r#""proxy_running":true,"pipe":"\\\\.\\pipe\\docklet_engine","#,
                r#""docker_host":"npipe:////./pipe/docklet_engine","distro":"docklet","#,
                r#""rootfs_version":"{rootfs}","wsl_version":"3.0.5","#,
                r#""docklet_version":"{docklet}","mirrored_networking":false,"#,
                r#""docker_context":{{"name":"docklet","exists":true}},"#,
                r#""docker_cli":{{"path":"docker.exe","on_path":true}}}}"#
            ),
            rootfs = ROOTFS_VERSION.trim(),
            docklet = env!("CARGO_PKG_VERSION"),
        );
        assert_eq!(status_data(&view, &facts).render(), expected);
    }

    #[test]
    fn data_of_a_machine_without_wsl_keeps_every_field() {
        let view = StatusView::of(&EngineState::WslMissing, false);
        let facts = StatusFacts {
            proxy_running: false,
            wsl_version: None,
            mirrored_networking: true,
            context_name: "docklet",
            context_exists: false,
            docker_cli: None,
        };
        let rendered = status_data(&view, &facts).render();
        assert!(rendered.starts_with(concat!(
            r#"{"state":"wsl_missing","reason":"WSL is not installed","ready":false,"#,
            r#""hint":"wsl --install --no-distribution","proxy_running":false,"#
        )));
        assert!(rendered.contains(r#""wsl_version":null"#));
        assert!(rendered.contains(r#""mirrored_networking":true"#));
        assert!(rendered.contains(r#""docker_context":{"name":"docklet","exists":false}"#));
        assert!(rendered.ends_with(r#""docker_cli":null}"#));
    }

    #[test]
    fn rootfs_version_has_no_trailing_newline() {
        let view = StatusView::of(&EngineState::Stopped, false);
        let facts = StatusFacts {
            proxy_running: false,
            wsl_version: None,
            mirrored_networking: false,
            context_name: "docklet",
            context_exists: false,
            docker_cli: None,
        };
        assert!(!status_data(&view, &facts).render().contains("\\n"));
    }
}
