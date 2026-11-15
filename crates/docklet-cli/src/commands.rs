//! What each command does.
//!
//! A command prints its text for people as it goes and returns the data for
//! `--json`. Text that predates `--json` is printed exactly as it always
//! was: scripts and the E2E workflow compare it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use docklet_core::docker_client::{self, ContextState};
use docklet_core::doctor::{self, Check, EngineFacts, Facts, Level};
use docklet_core::skills::{self, InstallOutcome, SkillStatus};
use docklet_core::{disk, engine};
use docklet_wsl::tools::{self, CONTEXT_NAME, CliSetup};
use docklet_wsl::{DISTRO, Engine, RootfsSource};

use crate::args::{Command, InstallSource, SkillsAction};
use crate::failure::{self, ErrorCode, Failure, Op};
use crate::json::{Json, Object};
use crate::status::{self, StatusFacts, StatusView};
use crate::{catalog, docker, proxy};

/// Where a command's output goes.
pub struct Ui {
    pub json: bool,
}

impl Ui {
    /// A result line for people. With `--json` the data carries the result.
    fn say(&self, line: &str) {
        if !self.json {
            println!("{line}");
        }
    }

    /// A progress line: stdout for people, stderr with `--json`, where
    /// stdout is reserved for the one JSON object.
    fn progress(&self, line: &str) {
        if self.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

pub enum Outcome {
    /// The command's data for `--json`.
    Data(Json),
    /// `docker` ran; this is its exit code.
    Exit(i32),
}

pub fn run(ui: &Ui, command: Command) -> Result<Outcome, Failure> {
    let data = match command {
        Command::Help => help(ui),
        Command::Version => version(ui),
        Command::Bases => bases(ui),
        Command::Status => status(ui)?,
        Command::Install(source) => install(ui, source)?,
        Command::Start => start(ui)?,
        Command::Stop => stop(ui)?,
        Command::Uninstall => uninstall(ui)?,
        Command::Up { base } => up(ui, base)?,
        Command::Wait { timeout } => wait(ui, timeout)?,
        Command::Doctor => run_doctor(ui)?,
        Command::Docker(args) => return docker::run(&args).map(Outcome::Exit),
        Command::CliSetup => cli_setup(ui)?,
        Command::Skills(action) => run_skills(ui, action)?,
        Command::ProxyStop => {
            if !proxy::stop() {
                return Err(Failure::failed("the pipe proxy did not exit"));
            }
            ui.say("The pipe proxy is stopped.");
            Json::from(Object::new())
        }
        Command::ProxyServe { trace, byte_mode } => {
            proxy::serve(trace, byte_mode)?;
            Json::from(Object::new())
        }
    };
    Ok(Outcome::Data(data))
}

fn help(ui: &Ui) -> Json {
    if ui.json {
        return catalog::catalogue();
    }
    print!("{}", catalog::help_text());
    Json::Null
}

fn version(ui: &Ui) -> Json {
    ui.say(&format!("docklet {}", env!("CARGO_PKG_VERSION")));
    Object::new()
        .set("version", env!("CARGO_PKG_VERSION"))
        .into()
}

fn bases(ui: &Ui) -> Json {
    let bases = engine::bases();
    for (index, base) in bases.iter().enumerate() {
        ui.say(&format!(
            "{base}{}",
            if index == 0 { " (default)" } else { "" }
        ));
    }
    Object::new()
        .set("default", bases.first().copied())
        .set("bases", bases)
        .into()
}

fn status(ui: &Ui) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    if !ui.json {
        let view = StatusView::of(&engine.status(), proxy::is_running());
        println!("{}", view.line());
        warn_if_mirrored_networking();
        return Ok(Json::Null);
    }
    // The same `wsl.exe` queries as `engine.status()`, kept apart so the
    // WSL version can be reported with the state.
    let facts = EngineFacts::gather(engine.wsl());
    let proxy_running = proxy::is_running();
    let view = StatusView::of(&facts.state(), proxy_running);
    let mirrored_networking = warn_if_mirrored_networking();
    Ok(status::status_data(
        &view,
        &StatusFacts {
            proxy_running,
            wsl_version: facts.wsl_version(),
            mirrored_networking,
            context_name: CONTEXT_NAME,
            context_exists: docker_context(home_dir().as_deref()) != ContextState::Missing,
            docker_cli: docker::find(&docklet_wsl::data_dir()?),
        },
    ))
}

/// Published ports depend on WSL's NAT localhost forwarding. Returns whether
/// the warning was given.
fn warn_if_mirrored_networking() -> bool {
    let mirrored = docklet_wsl::mirrored_networking();
    if mirrored {
        eprintln!(
            "docklet: {}",
            docklet_core::wslconfig::MIRRORED_NETWORKING_WARNING
        );
    }
    mirrored
}

fn docker_context(home: Option<&Path>) -> ContextState {
    let Some(home) = home else {
        return ContextState::Missing;
    };
    let config = docker_client::config_dir(std::env::var_os("DOCKER_CONFIG").as_deref(), home);
    docker_client::context_state(&config, CONTEXT_NAME, proxy::DOCKER_HOST)
}

/// Turns a failed engine operation into a failure.
///
/// With `--json` the engine is looked at again to give the failure its
/// code. The text output shows the message alone, so there the extra
/// `wsl.exe` calls are not made.
fn engine_failure(ui: &Ui, engine: &Engine, op: Op, err: io::Error) -> Failure {
    if !ui.json {
        return Failure::from(err);
    }
    failure::classify(op, err.to_string(), &EngineFacts::gather(engine.wsl()))
}

fn install(ui: &Ui, source: InstallSource) -> Result<Json, Failure> {
    let source = match source {
        InstallSource::DefaultBase => RootfsSource::Base(engine::bases()[0].to_string()),
        InstallSource::Base(base) => RootfsSource::Base(base),
        InstallSource::File(file) => RootfsSource::File(file),
    };
    let engine = Engine::system()?;
    install_distro(ui, &engine, &source)?;
    ui.say("Installed. Run `docklet start` to start the engine.");
    let (base, file) = match source {
        RootfsSource::Base(base) => (Some(base), None),
        RootfsSource::File(file) => (None, Some(file.display().to_string())),
    };
    Ok(Object::new()
        .set("distro", DISTRO)
        .set("base", base)
        .set("rootfs_file", file)
        .into())
}

fn install_distro(ui: &Ui, engine: &Engine, source: &RootfsSource) -> Result<(), Failure> {
    engine
        .install(source, &mut |step| ui.progress(step))
        .map_err(|err| {
            let base = match source {
                RootfsSource::Base(base) => Some(base.as_str()),
                RootfsSource::File(_) => None,
            };
            engine_failure(ui, engine, Op::Install { base }, err)
        })
}

fn start(ui: &Ui) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    warn_if_mirrored_networking();
    let failed = |err| engine_failure(ui, &engine, Op::Start, err);
    engine.start().map_err(failed)?;
    proxy::ensure_running().map_err(failed)?;
    ui.say(&running_line());
    Ok(running_data().into())
}

fn running_line() -> String {
    format!("Engine is running. Docker host: {}", proxy::DOCKER_HOST)
}

fn running_data() -> Object {
    Object::new()
        .set("state", "running")
        .set("docker_host", proxy::DOCKER_HOST)
        .set("pipe", docklet_proxy::ENGINE_PIPE)
}

fn stop(ui: &Ui) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    let proxy_stopped = proxy::stop();
    engine
        .stop()
        .map_err(|err| engine_failure(ui, &engine, Op::Other, err))?;
    ui.say("Engine is stopped.");
    Ok(Object::new().set("proxy_running", !proxy_stopped).into())
}

fn uninstall(ui: &Ui) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    proxy::stop();
    engine
        .uninstall()
        .map_err(|err| engine_failure(ui, &engine, Op::Other, err))?;
    ui.say("The docklet distro is removed.");
    Ok(Object::new().set("distro", DISTRO).into())
}

/// Installs if needed, starts, and sets up the Docker client. Every step
/// is skipped when its result is already there.
fn up(ui: &Ui, base: Option<String>) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    // Looked up before anything is changed: without it the last step fails.
    let home = home()?;
    warn_if_mirrored_networking();
    let facts = EngineFacts::gather(engine.wsl());
    let mut changed: Vec<&str> = Vec::new();
    match failure::blocker(&facts, true) {
        Some(blocker) if blocker.code == ErrorCode::NotInstalled => {
            let base = base.unwrap_or_else(|| engine::bases()[0].to_string());
            install_distro(ui, &engine, &RootfsSource::Base(base))?;
            changed.push("installed_distro");
        }
        // WSL itself is missing or broken; that is not `up`'s to repair.
        Some(blocker) if blocker.code != ErrorCode::NotRunning => return Err(blocker),
        _ => {
            if base.is_some() {
                eprintln!(
                    "docklet: warning: the docklet distro is already installed; --base is ignored"
                );
            }
        }
    }
    let failed = |err| engine_failure(ui, &engine, Op::Start, err);
    if facts.dockerd != Some(true) {
        engine.start().map_err(failed)?;
        changed.push("started_engine");
    }
    if !proxy::is_running() {
        proxy::ensure_running().map_err(failed)?;
        changed.push("started_proxy");
    }
    let setup = docker_setup(ui, &home).map_err(|err| {
        Failure::failed(format!(
            "the engine is running, but the Docker CLI could not be set up: {err}"
        ))
    })?;
    changed.extend(setup_changes(&setup));
    ui.say(&running_line());
    ui.say(&format!("Docker CLI: {}", setup.docker.display()));
    ui.say("Run Docker with: docklet docker ps");
    Ok(setup_data(running_data(), &setup, changed).into())
}

fn docker_setup(ui: &Ui, home: &Path) -> io::Result<CliSetup> {
    tools::setup(
        &docklet_wsl::data_dir()?,
        home,
        proxy::DOCKER_HOST,
        &mut |step| ui.progress(step),
    )
}

fn setup_changes(setup: &CliSetup) -> Vec<&'static str> {
    [
        (setup.installed_docker, "installed_docker_cli"),
        (setup.installed_compose, "installed_compose"),
        (setup.created_context, "created_context"),
    ]
    .into_iter()
    .filter_map(|(done, name)| done.then_some(name))
    .collect()
}

fn setup_data(object: Object, setup: &CliSetup, changed: Vec<&str>) -> Object {
    object
        .set(
            "docker_cli",
            status::docker_cli(&setup.docker, setup.docker_on_path),
        )
        .set("docker_context", status::docker_context(CONTEXT_NAME, true))
        .set("changed", changed)
}

fn cli_setup(ui: &Ui) -> Result<Json, Failure> {
    let setup = docker_setup(ui, &home()?)?;
    let docker = if setup.docker_on_path {
        "docker".to_string()
    } else {
        format!("\"{}\"", setup.docker.display())
    };
    ui.say(&format!("Docker CLI: {}", setup.docker.display()));
    ui.say(&format!("Use it with: {docker} --context docklet ps"));
    let object = Object::new().set("docker_host", proxy::DOCKER_HOST);
    Ok(setup_data(object, &setup, setup_changes(&setup)).into())
}

/// Pause between two looks at the engine while waiting for it.
const WAIT_POLL: Duration = Duration::from_secs(1);

/// Blocks until the engine is usable. It only watches: starting the engine
/// is `start`'s and `up`'s job.
fn wait(ui: &Ui, timeout: Duration) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    let started = Instant::now();
    loop {
        let facts = EngineFacts::gather(engine.wsl());
        let Some(blocker) = failure::blocker(&facts, proxy::is_running()) else {
            ui.say(&running_line());
            return Ok(running_data()
                .set("waited_seconds", started.elapsed().as_secs())
                .into());
        };
        // Installing WSL takes an administrator and a reboot; no wait
        // outlasts that.
        if blocker.code == ErrorCode::WslMissing {
            return Err(blocker);
        }
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(timed_out(timeout, blocker));
        }
        // Every round that does not return sleeps.
        std::thread::sleep(left.min(WAIT_POLL));
    }
}

/// The failure of a wait that ran out of time, with the hint of whatever
/// still blocked the engine at the end.
fn timed_out(timeout: Duration, blocker: Failure) -> Failure {
    Failure {
        code: ErrorCode::Timeout,
        message: format!(
            "the engine was not usable within {} s: {}",
            timeout.as_secs(),
            blocker.message
        ),
        hint: blocker.hint,
        data: None,
    }
}

fn run_doctor(ui: &Ui) -> Result<Json, Failure> {
    let engine = Engine::system()?;
    let data_dir = docklet_wsl::data_dir()?;
    let home = home_dir();
    let facts = Facts {
        engine: EngineFacts::gather(engine.wsl()),
        proxy_running: proxy::is_running(),
        docker_cli: docker::find(&data_dir).map(|cli| cli.path.display().to_string()),
        compose_plugin: home
            .as_deref()
            .is_some_and(|home| docker_client::compose_plugin(home).is_file()),
        docker_context: docker_context(home.as_deref()),
        mirrored_networking: docklet_wsl::mirrored_networking(),
        data_dir: data_dir.display().to_string(),
        free_space: disk::free_space(&data_dir).map_err(|err| err.to_string()),
    };
    let checks = doctor::report(&facts);
    for line in doctor_lines(&checks) {
        ui.say(&line);
    }
    let data = doctor_data(&checks);
    match doctor_failure(&checks) {
        Some(failure) => Err(failure.with_data(data)),
        None => {
            ui.say(&doctor_summary(&checks));
            Ok(data)
        }
    }
}

fn count(checks: &[Check], level: Level) -> u64 {
    checks.iter().filter(|check| check.level == level).count() as u64
}

/// One line per check, and one more for a fix.
fn doctor_lines(checks: &[Check]) -> Vec<String> {
    let mut lines = Vec::new();
    for check in checks {
        lines.push(format!(
            "{:<5} {:<17} {}",
            check.level.as_str(),
            check.title,
            check.detail
        ));
        if let Some(fix) = check.fix {
            lines.push(format!("{:<23} fix: {fix}", ""));
        }
    }
    lines
}

fn doctor_summary(checks: &[Check]) -> String {
    match count(checks, Level::Warn) {
        0 => "No problems found.".to_string(),
        1 => "No failures, 1 warning.".to_string(),
        warnings => format!("No failures, {warnings} warnings."),
    }
}

/// The failure `doctor` ends with when a check fails. Its hint is the first
/// fix there is: the checks are in the order problems have to be solved.
fn doctor_failure(checks: &[Check]) -> Option<Failure> {
    let failed = count(checks, Level::Fail);
    let message = match failed {
        0 => return None,
        1 => "1 check failed".to_string(),
        failed => format!("{failed} checks failed"),
    };
    let fix = checks
        .iter()
        .filter(|check| check.level == Level::Fail)
        .find_map(|check| check.fix);
    Some(Failure {
        code: ErrorCode::ChecksFailed,
        message,
        hint: fix,
        data: None,
    })
}

fn doctor_data(checks: &[Check]) -> Json {
    let list: Vec<Json> = checks
        .iter()
        .map(|check| {
            Object::new()
                .set("id", check.id)
                .set("title", check.title)
                .set("status", check.level.as_str())
                .set("detail", check.detail.as_str())
                .set_some("fix", check.fix)
                .into()
        })
        .collect();
    Object::new()
        .set("healthy", count(checks, Level::Fail) == 0)
        .set(
            "summary",
            Object::new()
                .set("ok", count(checks, Level::Ok))
                .set("warn", count(checks, Level::Warn))
                .set("fail", count(checks, Level::Fail)),
        )
        .set("checks", list)
        .into()
}

fn run_skills(ui: &Ui, action: SkillsAction) -> Result<Json, Failure> {
    let home = home()?;
    let mut failed = false;
    let mut agents: Vec<Json> = Vec::new();
    for target in &skills::default_targets(&home) {
        let path = target.skill_path();
        let shown = path.display();
        // The word is for programs, the line for people.
        let (word, line) = match action {
            SkillsAction::Status => match target.status() {
                SkillStatus::AgentNotFound => ("agent_not_found", "agent not found".to_string()),
                SkillStatus::NotInstalled => ("not_installed", "not registered".to_string()),
                SkillStatus::Outdated => ("outdated", format!("outdated ({shown})")),
                SkillStatus::Current => ("current", format!("registered ({shown})")),
            },
            SkillsAction::Install => match target.install() {
                Ok(InstallOutcome::AgentNotFound) => {
                    ("agent_not_found", "agent not found, skipped".to_string())
                }
                Ok(InstallOutcome::Installed) => ("installed", format!("registered ({shown})")),
                Ok(InstallOutcome::Updated) => ("updated", format!("updated ({shown})")),
                Ok(InstallOutcome::AlreadyCurrent) => {
                    ("already_current", "already up to date".to_string())
                }
                Err(err) => {
                    failed = true;
                    ("failed", format!("failed: {err}"))
                }
            },
            SkillsAction::Uninstall => match target.uninstall() {
                Ok(true) => ("removed", "removed".to_string()),
                Ok(false) => ("not_installed", "not registered".to_string()),
                Err(err) => {
                    failed = true;
                    ("failed", format!("failed: {err}"))
                }
            },
        };
        ui.say(&format!("{}: {line}", target.agent));
        agents.push(
            Object::new()
                .set("agent", target.agent)
                .set("result", word)
                .set("detail", line)
                .set("path", shown.to_string())
                .into(),
        );
    }
    let data = Object::new().set("agents", agents).into();
    if failed {
        Err(Failure::failed("some skills could not be changed").with_data(data))
    } else {
        Ok(data)
    }
}

fn home() -> Result<PathBuf, Failure> {
    home_dir().ok_or_else(|| Failure::failed("cannot find the user's home directory"))
}

fn home_dir() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .find_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, level: Level, fix: Option<&'static str>) -> Check {
        Check {
            id,
            title: "Title",
            level,
            detail: format!("detail of {id}"),
            fix,
        }
    }

    #[test]
    fn doctor_lines_are_aligned_and_name_the_fix() {
        let checks = [
            Check {
                id: "wsl",
                title: "WSL",
                level: Level::Ok,
                detail: "WSL 3.0.5".to_string(),
                fix: None,
            },
            Check {
                id: "distro_installed",
                title: "Distro installed",
                level: Level::Fail,
                detail: "the `docklet` distro is not installed".to_string(),
                fix: Some("docklet up"),
            },
            Check {
                id: "compose_plugin",
                title: "Compose plugin",
                level: Level::Warn,
                detail: "not found".to_string(),
                fix: None,
            },
        ];
        assert_eq!(
            doctor_lines(&checks),
            [
                "ok    WSL               WSL 3.0.5",
                "fail  Distro installed  the `docklet` distro is not installed",
                "                        fix: docklet up",
                "warn  Compose plugin    not found",
            ]
        );
    }

    #[test]
    fn every_real_check_title_fits_its_column() {
        let facts = Facts {
            engine: EngineFacts {
                wsl: doctor::WslFact::Missing,
                installed: None,
                running: None,
                dockerd: None,
            },
            proxy_running: false,
            docker_cli: None,
            compose_plugin: false,
            docker_context: ContextState::Missing,
            mirrored_networking: false,
            data_dir: "data".to_string(),
            free_space: Ok(0),
        };
        for check in doctor::report(&facts) {
            assert!(check.title.len() <= 17, "{}", check.title);
        }
    }

    #[test]
    fn doctor_summary_counts_the_warnings() {
        let ok = check("a", Level::Ok, None);
        let warn = check("b", Level::Warn, None);
        assert_eq!(
            doctor_summary(std::slice::from_ref(&ok)),
            "No problems found."
        );
        assert_eq!(
            doctor_summary(&[ok.clone(), warn.clone()]),
            "No failures, 1 warning."
        );
        assert_eq!(
            doctor_summary(&[warn.clone(), ok, warn]),
            "No failures, 2 warnings."
        );
    }

    #[test]
    fn doctor_fails_only_when_a_check_fails() {
        let passing = [
            check("a", Level::Ok, None),
            check("b", Level::Warn, Some("docklet cli setup")),
        ];
        assert_eq!(doctor_failure(&passing), None);
    }

    #[test]
    fn doctor_failure_counts_and_hints_at_the_first_fix() {
        let checks = [
            check("a", Level::Warn, Some("a warning's fix does not count")),
            check("b", Level::Fail, None),
            check("c", Level::Fail, Some("docklet start")),
            check("d", Level::Fail, Some("docklet cli setup")),
        ];
        let failure = doctor_failure(&checks).unwrap();
        assert_eq!(failure.code, ErrorCode::ChecksFailed);
        assert_eq!(failure.code.exit_code(), 1);
        assert_eq!(failure.message, "3 checks failed");
        assert_eq!(failure.hint, Some("docklet start"));

        let one = doctor_failure(&[check("b", Level::Fail, None)]).unwrap();
        assert_eq!(one.message, "1 check failed");
        assert_eq!(one.hint, None);
    }

    #[test]
    fn doctor_data_shape() {
        let checks = [
            check("wsl", Level::Ok, None),
            check("distro_running", Level::Fail, Some("docklet start")),
            check("compose_plugin", Level::Warn, None),
        ];
        assert_eq!(
            doctor_data(&checks).render(),
            concat!(
                r#"{"healthy":false,"summary":{"ok":1,"warn":1,"fail":1},"checks":["#,
                r#"{"id":"wsl","title":"Title","status":"ok","detail":"detail of wsl"},"#,
                r#"{"id":"distro_running","title":"Title","status":"fail","#,
                r#""detail":"detail of distro_running","fix":"docklet start"},"#,
                r#"{"id":"compose_plugin","title":"Title","status":"warn","#,
                r#""detail":"detail of compose_plugin"}]}"#
            )
        );
        let healthy = doctor_data(&[check("wsl", Level::Warn, None)]).render();
        assert!(healthy.starts_with(r#"{"healthy":true,"#));
    }

    #[test]
    fn a_wait_that_times_out_keeps_the_hint_of_what_blocked_it() {
        let blocker =
            Failure::new(ErrorCode::NotRunning, "the engine is stopped").hint("docklet start");
        let failure = timed_out(Duration::from_secs(120), blocker);
        assert_eq!(failure.code, ErrorCode::Timeout);
        assert_eq!(
            failure.message,
            "the engine was not usable within 120 s: the engine is stopped"
        );
        assert_eq!(failure.hint, Some("docklet start"));
        assert_eq!(
            failure.text(),
            "docklet: the engine was not usable within 120 s: the engine is stopped; \
             run `docklet start`\n"
        );
    }

    #[test]
    fn setup_changes_name_only_what_was_done() {
        let mut setup = CliSetup {
            docker: PathBuf::from("docker.exe"),
            docker_on_path: true,
            installed_docker: false,
            installed_compose: false,
            created_context: false,
        };
        assert!(setup_changes(&setup).is_empty());
        setup.installed_compose = true;
        setup.created_context = true;
        assert_eq!(
            setup_changes(&setup),
            ["installed_compose", "created_context"]
        );
        setup.installed_docker = true;
        assert_eq!(setup_changes(&setup).len(), 3);
    }

    #[test]
    fn up_data_shape() {
        let setup = CliSetup {
            docker: PathBuf::from("docker.exe"),
            docker_on_path: false,
            installed_docker: true,
            installed_compose: false,
            created_context: true,
        };
        let mut changed = vec!["installed_distro", "started_engine", "started_proxy"];
        changed.extend(setup_changes(&setup));
        let data: Json = setup_data(running_data(), &setup, changed).into();
        assert_eq!(
            data.render(),
            concat!(
                r#"{"state":"running","docker_host":"npipe:////./pipe/docklet_engine","#,
                r#""pipe":"\\\\.\\pipe\\docklet_engine","#,
                r#""docker_cli":{"path":"docker.exe","on_path":false},"#,
                r#""docker_context":{"name":"docklet","exists":true},"#,
                r#""changed":["installed_distro","started_engine","started_proxy","#,
                r#""installed_docker_cli","created_context"]}"#
            )
        );
    }

    #[test]
    fn an_up_that_changed_nothing_says_so_with_an_empty_list() {
        let setup = CliSetup {
            docker: PathBuf::from("docker.exe"),
            docker_on_path: true,
            installed_docker: false,
            installed_compose: false,
            created_context: false,
        };
        let data: Json = setup_data(running_data(), &setup, setup_changes(&setup)).into();
        assert!(data.render().ends_with(r#""changed":[]}"#));
    }

    #[test]
    fn the_start_line_is_what_it_always_was() {
        assert_eq!(
            running_line(),
            "Engine is running. Docker host: npipe:////./pipe/docklet_engine"
        );
    }
}
