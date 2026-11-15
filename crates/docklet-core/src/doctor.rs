//! `docklet doctor`: what is checked and how each finding is judged.
//!
//! Gathering the facts needs the machine; judging them does not. The report
//! is a pure function of [`Facts`], so every verdict tests on any OS.

use crate::docker_client::ContextState;
use crate::engine::{self, DISTRO_NAME, ENGINE_PIPE};
use crate::state::EngineState;
use crate::wsl::{Wsl, WslRunner, WslVersion};

const GIB: u64 = 1024 * 1024 * 1024;
/// Below this an install, a pull or a build is likely to run out of space.
pub const DISK_FAIL_BELOW: u64 = 2 * GIB;
pub const DISK_WARN_BELOW: u64 = 10 * GIB;

/// What `wsl --version` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WslFact {
    Missing,
    /// `wsl.exe` was started and did not answer in time.
    Unresponsive(String),
    Found(WslVersion),
}

/// What `wsl.exe` reports about the engine, asked one step at a time in the
/// order of [`engine::probe`]. A step is `None` when an earlier one already
/// rules it out, and in particular dockerd is only asked while the distro
/// runs: `wsl -d` would start a stopped distro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineFacts {
    pub wsl: WslFact,
    pub installed: Option<Result<bool, String>>,
    pub running: Option<Result<bool, String>>,
    pub dockerd: Option<bool>,
}

impl EngineFacts {
    pub fn gather<R: WslRunner>(wsl: &Wsl<R>) -> Self {
        let mut facts = Self {
            wsl: match wsl.version() {
                Ok(Some(version)) => WslFact::Found(version),
                Ok(None) => WslFact::Missing,
                Err(err) => WslFact::Unresponsive(err.to_string()),
            },
            installed: None,
            running: None,
            dockerd: None,
        };
        if facts.wsl_version().is_none_or(|v| v < WslVersion::MINIMUM) {
            return facts;
        }
        let has = |names: std::io::Result<Vec<String>>| {
            names
                .map(|names| names.iter().any(|name| name == DISTRO_NAME))
                .map_err(|err| err.to_string())
        };
        facts.installed = Some(has(wsl.installed()));
        if facts.installed != Some(Ok(true)) {
            return facts;
        }
        facts.running = Some(has(wsl.running()));
        if facts.running != Some(Ok(true)) {
            return facts;
        }
        facts.dockerd = Some(engine::docker_ready(wsl));
        facts
    }

    pub fn wsl_version(&self) -> Option<WslVersion> {
        match self.wsl {
            WslFact::Found(version) => Some(version),
            _ => None,
        }
    }

    /// The engine state these facts amount to; the same answer
    /// [`engine::probe`] gives for the same machine.
    pub fn state(&self) -> EngineState {
        let version = match &self.wsl {
            WslFact::Missing => return EngineState::WslMissing,
            WslFact::Unresponsive(reason) => return EngineState::Error(reason.clone()),
            WslFact::Found(version) => *version,
        };
        if version < WslVersion::MINIMUM {
            return EngineState::Error(format!(
                "WSL {version} is too old; Docklet needs {} or later (run `wsl --update`)",
                WslVersion::MINIMUM
            ));
        }
        match &self.installed {
            Some(Ok(true)) => {}
            Some(Err(reason)) => return EngineState::Error(reason.clone()),
            _ => return EngineState::NotInstalled,
        }
        match &self.running {
            Some(Ok(true)) => {}
            Some(Err(reason)) => return EngineState::Error(reason.clone()),
            _ => return EngineState::Stopped,
        }
        if self.dockerd == Some(true) {
            EngineState::Running
        } else {
            EngineState::Starting
        }
    }
}

/// Everything the doctor looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub engine: EngineFacts,
    pub proxy_running: bool,
    /// The `docker.exe` that would be used, if there is one.
    pub docker_cli: Option<String>,
    pub compose_plugin: bool,
    pub docker_context: ContextState,
    pub mirrored_networking: bool,
    /// Where Docklet stores the distro disk and its downloads.
    pub data_dir: String,
    /// Bytes free on the drive of `data_dir`, or why that is unknown.
    pub free_space: Result<u64, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    /// Works, but something may bite later.
    Warn,
    /// Docker does not work until this is fixed.
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable identifier for programs.
    pub id: &'static str,
    pub title: &'static str,
    pub level: Level,
    pub detail: String,
    /// The command that fixes it, when there is an obvious one.
    pub fix: Option<&'static str>,
}

const NOT_CHECKED: &str = "not checked: an earlier check has to pass first";

/// The checks, always the same ones in the same order.
pub fn report(facts: &Facts) -> Vec<Check> {
    let check = |id, title, (level, detail, fix): (Level, String, Option<&'static str>)| Check {
        id,
        title,
        level,
        detail,
        fix,
    };
    vec![
        check("wsl", "WSL", wsl(&facts.engine.wsl)),
        check(
            "distro_installed",
            "Distro installed",
            listed(
                &facts.engine.installed,
                "the `docklet` distro is installed",
                "the `docklet` distro is not installed",
                "docklet up",
            ),
        ),
        check(
            "distro_running",
            "Distro running",
            listed(
                &facts.engine.running,
                "the distro is running",
                "the engine is stopped",
                "docklet start",
            ),
        ),
        check("dockerd", "Docker daemon", dockerd(facts.engine.dockerd)),
        check("pipe_proxy", "Pipe proxy", pipe_proxy(facts)),
        check(
            "docker_cli",
            "Docker CLI",
            match &facts.docker_cli {
                Some(path) => (Level::Ok, path.clone(), None),
                None => (
                    Level::Fail,
                    "no docker.exe on PATH or in Docklet's bin directory".to_string(),
                    Some("docklet cli setup"),
                ),
            },
        ),
        check(
            "compose_plugin",
            "Compose plugin",
            if facts.compose_plugin {
                (
                    Level::Ok,
                    "docker-compose.exe is installed".to_string(),
                    None,
                )
            } else {
                (
                    Level::Warn,
                    "not in the user's cli-plugins directory; `docker compose` may be missing"
                        .to_string(),
                    Some("docklet cli setup"),
                )
            },
        ),
        check(
            "docker_context",
            "Docker context",
            match facts.docker_context {
                ContextState::Present => (
                    Level::Ok,
                    "the `docklet` context points at the Docklet pipe".to_string(),
                    None,
                ),
                ContextState::Missing => (
                    Level::Warn,
                    "there is no `docklet` context; `docklet docker` works without it".to_string(),
                    Some("docklet cli setup"),
                ),
                ContextState::OtherHost => (
                    Level::Warn,
                    "a `docklet` context exists but does not point at the Docklet pipe; \
                     `docklet docker` is not affected"
                        .to_string(),
                    None,
                ),
            },
        ),
        check(
            "mirrored_networking",
            "WSL networking",
            if facts.mirrored_networking {
                (
                    Level::Warn,
                    ".wslconfig sets networkingMode=mirrored; published ports may not be \
                     reachable from Windows"
                        .to_string(),
                    None,
                )
            } else {
                (Level::Ok, "not mirrored".to_string(), None)
            },
        ),
        check("disk_space", "Disk space", disk_space(facts)),
    ]
}

type Verdict = (Level, String, Option<&'static str>);

fn wsl(fact: &WslFact) -> Verdict {
    match fact {
        WslFact::Missing => (
            Level::Fail,
            "WSL is not installed; install it as administrator, then reboot".to_string(),
            Some("wsl --install --no-distribution"),
        ),
        WslFact::Unresponsive(reason) => (Level::Fail, reason.clone(), None),
        WslFact::Found(version) if *version < WslVersion::MINIMUM => (
            Level::Fail,
            format!(
                "WSL {version} is too old; Docklet needs {} or later",
                WslVersion::MINIMUM
            ),
            Some("wsl --update"),
        ),
        WslFact::Found(version) => (Level::Ok, format!("WSL {version}"), None),
    }
}

/// A check answered by one of the `wsl --list` forms.
fn listed(fact: &Option<Result<bool, String>>, yes: &str, no: &str, fix: &'static str) -> Verdict {
    match fact {
        Some(Ok(true)) => (Level::Ok, yes.to_string(), None),
        Some(Ok(false)) => (Level::Fail, no.to_string(), Some(fix)),
        Some(Err(reason)) => (Level::Fail, reason.clone(), None),
        // Not known to be broken: the check that blocks it carries the failure.
        None => (Level::Warn, NOT_CHECKED.to_string(), None),
    }
}

fn dockerd(fact: Option<bool>) -> Verdict {
    match fact {
        Some(true) => (Level::Ok, "dockerd answers".to_string(), None),
        Some(false) => (
            Level::Fail,
            "dockerd does not answer; `docklet start` waits until it does".to_string(),
            Some("docklet start"),
        ),
        None => (Level::Warn, NOT_CHECKED.to_string(), None),
    }
}

fn pipe_proxy(facts: &Facts) -> Verdict {
    if facts.proxy_running {
        (Level::Ok, format!("serving {ENGINE_PIPE}"), None)
    } else if facts.engine.dockerd == Some(true) {
        (
            Level::Fail,
            "Docker is up but the pipe proxy is not, so Windows clients cannot reach it"
                .to_string(),
            Some("docklet start"),
        )
    } else {
        // Expected while the engine is down; it starts with the engine.
        (
            Level::Warn,
            "not running; it starts together with the engine".to_string(),
            None,
        )
    }
}

fn disk_space(facts: &Facts) -> Verdict {
    match &facts.free_space {
        Ok(bytes) => {
            let level = match *bytes {
                bytes if bytes < DISK_FAIL_BELOW => Level::Fail,
                bytes if bytes < DISK_WARN_BELOW => Level::Warn,
                _ => Level::Ok,
            };
            let detail = format!(
                "{} free on the drive of {}",
                gigabytes(*bytes),
                facts.data_dir
            );
            (level, detail, None)
        }
        Err(reason) => (
            Level::Warn,
            format!("could not be measured for {}: {reason}", facts.data_dir),
            None,
        ),
    }
}

/// `12.3 GB`, rounded down. Integer arithmetic keeps float formatting out of
/// the binaries.
pub fn gigabytes(bytes: u64) -> String {
    format!("{}.{} GB", bytes / GIB, bytes % GIB * 10 / GIB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsl::testing::{MockRunner, fail, ok};
    use std::io;

    const V3: &str = "WSL version: 3.0.5.0\n";
    const V3_PARSED: WslVersion = WslVersion {
        major: 3,
        minor: 0,
        patch: 5,
    };

    fn machine(
        version: &'static str,
        installed: &'static str,
        running: &'static str,
        docker: bool,
    ) -> MockRunner {
        MockRunner::new(move |args| match args {
            ["--version"] => ok(version),
            ["--list", "--quiet"] => ok(installed),
            ["--list", "--running", "--quiet"] => {
                if running.is_empty() {
                    fail("no running distributions")
                } else {
                    ok(running)
                }
            }
            [.., "docker", "info", "--format", _] => {
                if docker {
                    ok("29.9.3\n")
                } else {
                    fail("Cannot connect to the Docker daemon")
                }
            }
            other => panic!("unexpected wsl call: {other:?}"),
        })
    }

    fn healthy_engine() -> EngineFacts {
        EngineFacts {
            wsl: WslFact::Found(V3_PARSED),
            installed: Some(Ok(true)),
            running: Some(Ok(true)),
            dockerd: Some(true),
        }
    }

    fn healthy() -> Facts {
        Facts {
            engine: healthy_engine(),
            proxy_running: true,
            docker_cli: Some(r"C:\tools\docker.exe".to_string()),
            compose_plugin: true,
            docker_context: ContextState::Present,
            mirrored_networking: false,
            data_dir: r"C:\Users\me\AppData\Local\Docklet".to_string(),
            free_space: Ok(80 * GIB),
        }
    }

    fn level_of(checks: &[Check], id: &str) -> Level {
        checks.iter().find(|check| check.id == id).unwrap().level
    }

    fn failing(checks: &[Check]) -> Vec<&'static str> {
        checks
            .iter()
            .filter(|check| check.level == Level::Fail)
            .map(|check| check.id)
            .collect()
    }

    #[test]
    fn facts_agree_with_the_engine_probe_on_every_machine() {
        let machines = [
            ("WSL version: 2.3.26.0\n", "docklet\n", "docklet\n", true),
            (V3, "", "", false),
            (V3, "Ubuntu-26.04\n", "Ubuntu-26.04\n", true),
            (V3, "Ubuntu-26.04\ndocklet\n", "Ubuntu-26.04\n", true),
            (V3, "docklet\n", "", true),
            (V3, "docklet\n", "docklet\n", false),
            (V3, "docklet\n", "docklet\n", true),
        ];
        for (version, installed, running, docker) in machines {
            let runner = machine(version, installed, running, docker);
            let wsl = Wsl::new(&runner);
            assert_eq!(
                EngineFacts::gather(&wsl).state(),
                engine::probe(&wsl),
                "{version:?} {installed:?} {running:?} {docker}"
            );
        }
        let missing = MockRunner::new(|_| Err(io::Error::from(io::ErrorKind::NotFound)));
        let wsl = Wsl::new(&missing);
        assert_eq!(EngineFacts::gather(&wsl).state(), engine::probe(&wsl));
        let hung = MockRunner::new(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "no answer")));
        let wsl = Wsl::new(&hung);
        assert_eq!(EngineFacts::gather(&wsl).state(), engine::probe(&wsl));
        let list_fails = MockRunner::new(|args| match args {
            ["--version"] => ok(V3),
            _ => Err(io::Error::new(io::ErrorKind::TimedOut, "list hung")),
        });
        let wsl = Wsl::new(&list_fails);
        assert_eq!(EngineFacts::gather(&wsl).state(), engine::probe(&wsl));
    }

    #[test]
    fn gathering_stops_at_the_first_step_that_rules_out_the_rest() {
        let runner = machine(V3, "docklet\n", "", true);
        let facts = EngineFacts::gather(&Wsl::new(&runner));
        assert_eq!(facts.installed, Some(Ok(true)));
        assert_eq!(facts.running, Some(Ok(false)));
        assert_eq!(facts.dockerd, None);
        // dockerd is never asked on a stopped distro: `wsl -d` would start it.
        assert_eq!(runner.calls.borrow().len(), 3);

        let runner = machine("WSL version: 2.3.26.0\n", "docklet\n", "docklet\n", true);
        let facts = EngineFacts::gather(&Wsl::new(&runner));
        assert_eq!(facts.installed, None);
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[test]
    fn a_healthy_machine_passes_every_check() {
        let checks = report(&healthy());
        assert_eq!(checks.len(), 10);
        for check in &checks {
            assert_eq!(check.level, Level::Ok, "{check:?}");
            assert_eq!(check.fix, None, "{check:?}");
            assert!(!check.detail.is_empty());
        }
    }

    #[test]
    fn the_checks_are_always_the_same_ones_in_the_same_order() {
        let ids =
            |facts: &Facts| -> Vec<&str> { report(facts).iter().map(|check| check.id).collect() };
        let expected = [
            "wsl",
            "distro_installed",
            "distro_running",
            "dockerd",
            "pipe_proxy",
            "docker_cli",
            "compose_plugin",
            "docker_context",
            "mirrored_networking",
            "disk_space",
        ];
        assert_eq!(ids(&healthy()), expected);
        let mut broken = healthy();
        broken.engine = EngineFacts {
            wsl: WslFact::Missing,
            installed: None,
            running: None,
            dockerd: None,
        };
        assert_eq!(ids(&broken), expected);
    }

    #[test]
    fn missing_wsl_is_the_one_failure_and_names_the_installer() {
        let mut facts = healthy();
        facts.proxy_running = false;
        facts.engine = EngineFacts {
            wsl: WslFact::Missing,
            installed: None,
            running: None,
            dockerd: None,
        };
        let checks = report(&facts);
        assert_eq!(failing(&checks), ["wsl"]);
        assert_eq!(checks[0].fix, Some("wsl --install --no-distribution"));
        for id in ["distro_installed", "distro_running", "dockerd"] {
            assert_eq!(level_of(&checks, id), Level::Warn, "{id}");
        }
        assert_eq!(level_of(&checks, "pipe_proxy"), Level::Warn);
    }

    #[test]
    fn old_wsl_fails_with_the_update_command() {
        let mut facts = healthy();
        facts.engine = EngineFacts {
            wsl: WslFact::Found(WslVersion {
                major: 2,
                minor: 3,
                patch: 26,
            }),
            installed: None,
            running: None,
            dockerd: None,
        };
        let checks = report(&facts);
        assert_eq!(checks[0].level, Level::Fail);
        assert_eq!(checks[0].fix, Some("wsl --update"));
        assert!(checks[0].detail.contains("2.3.26"));
        assert!(checks[0].detail.contains("2.4.4"));
    }

    #[test]
    fn a_wsl_that_hangs_fails_without_a_fix() {
        let mut facts = healthy();
        facts.engine.wsl = WslFact::Unresponsive("`wsl --version` did not finish".to_string());
        let checks = report(&facts);
        assert_eq!(checks[0].level, Level::Fail);
        assert_eq!(checks[0].fix, None);
        assert!(checks[0].detail.contains("did not finish"));
    }

    #[test]
    fn a_missing_distro_points_at_up() {
        let mut facts = healthy();
        facts.proxy_running = false;
        facts.engine.installed = Some(Ok(false));
        facts.engine.running = None;
        facts.engine.dockerd = None;
        let checks = report(&facts);
        assert_eq!(failing(&checks), ["distro_installed"]);
        assert_eq!(checks[1].fix, Some("docklet up"));
    }

    #[test]
    fn a_stopped_engine_points_at_start() {
        let mut facts = healthy();
        facts.proxy_running = false;
        facts.engine.running = Some(Ok(false));
        facts.engine.dockerd = None;
        let checks = report(&facts);
        assert_eq!(failing(&checks), ["distro_running"]);
        assert_eq!(checks[2].fix, Some("docklet start"));
        assert_eq!(level_of(&checks, "dockerd"), Level::Warn);
        assert_eq!(level_of(&checks, "pipe_proxy"), Level::Warn);
    }

    #[test]
    fn a_list_that_fails_is_reported_with_its_reason() {
        let mut facts = healthy();
        facts.engine.installed = Some(Err("`wsl --list --quiet` did not finish".to_string()));
        facts.engine.running = None;
        facts.engine.dockerd = None;
        let checks = report(&facts);
        assert_eq!(checks[1].level, Level::Fail);
        assert!(checks[1].detail.contains("did not finish"));
        assert_eq!(checks[1].fix, None);
    }

    #[test]
    fn dockerd_that_does_not_answer_fails() {
        let mut facts = healthy();
        facts.engine.dockerd = Some(false);
        let checks = report(&facts);
        assert_eq!(failing(&checks), ["dockerd"]);
        assert_eq!(checks[3].fix, Some("docklet start"));
    }

    #[test]
    fn a_proxy_that_is_down_while_docker_is_up_fails() {
        let mut facts = healthy();
        facts.proxy_running = false;
        let checks = report(&facts);
        assert_eq!(failing(&checks), ["pipe_proxy"]);
        assert_eq!(checks[4].fix, Some("docklet start"));
    }

    #[test]
    fn client_pieces_are_judged_by_what_docklet_docker_needs() {
        let mut facts = healthy();
        facts.docker_cli = None;
        facts.compose_plugin = false;
        facts.docker_context = ContextState::Missing;
        let checks = report(&facts);
        // Without docker.exe nothing runs; Compose and the context are extras.
        assert_eq!(failing(&checks), ["docker_cli"]);
        assert_eq!(level_of(&checks, "compose_plugin"), Level::Warn);
        assert_eq!(level_of(&checks, "docker_context"), Level::Warn);
        for id in ["docker_cli", "compose_plugin", "docker_context"] {
            let check = checks.iter().find(|check| check.id == id).unwrap();
            assert_eq!(check.fix, Some("docklet cli setup"), "{id}");
        }

        facts.docker_context = ContextState::OtherHost;
        let checks = report(&facts);
        let context = checks.iter().find(|c| c.id == "docker_context").unwrap();
        assert_eq!(context.level, Level::Warn);
        // Replacing a context the user made is not Docklet's call.
        assert_eq!(context.fix, None);
    }

    #[test]
    fn mirrored_networking_is_a_warning_without_a_fix() {
        let mut facts = healthy();
        facts.mirrored_networking = true;
        let checks = report(&facts);
        assert!(failing(&checks).is_empty());
        let check = checks
            .iter()
            .find(|c| c.id == "mirrored_networking")
            .unwrap();
        assert_eq!(check.level, Level::Warn);
        // Docklet never edits .wslconfig and does not tell anyone to.
        assert_eq!(check.fix, None);
    }

    #[test]
    fn disk_space_thresholds() {
        let level = |free: Result<u64, String>| {
            let mut facts = healthy();
            facts.free_space = free;
            level_of(&report(&facts), "disk_space")
        };
        assert_eq!(level(Ok(0)), Level::Fail);
        assert_eq!(level(Ok(DISK_FAIL_BELOW - 1)), Level::Fail);
        assert_eq!(level(Ok(DISK_FAIL_BELOW)), Level::Warn);
        assert_eq!(level(Ok(DISK_WARN_BELOW - 1)), Level::Warn);
        assert_eq!(level(Ok(DISK_WARN_BELOW)), Level::Ok);
        // Not knowing is not a failure.
        assert_eq!(level(Err("access denied".to_string())), Level::Warn);
    }

    #[test]
    fn disk_space_detail_names_the_amount_and_the_place() {
        let mut facts = healthy();
        facts.free_space = Ok(5 * GIB + GIB / 2);
        let checks = report(&facts);
        let check = checks.iter().find(|c| c.id == "disk_space").unwrap();
        assert!(check.detail.starts_with("5.5 GB free"), "{}", check.detail);
        assert!(check.detail.contains(r"AppData\Local\Docklet"));
    }

    #[test]
    fn gigabytes_round_down_to_one_decimal() {
        assert_eq!(gigabytes(0), "0.0 GB");
        assert_eq!(gigabytes(GIB - 1), "0.9 GB");
        assert_eq!(gigabytes(GIB), "1.0 GB");
        assert_eq!(gigabytes(12 * GIB + 3 * GIB / 10 + 1), "12.3 GB");
        assert_eq!(gigabytes(u64::MAX), "17179869183.9 GB");
    }

    #[test]
    fn level_names_are_the_three_words() {
        let names = [Level::Ok, Level::Warn, Level::Fail].map(Level::as_str);
        assert_eq!(names, ["ok", "warn", "fail"]);
    }
}
