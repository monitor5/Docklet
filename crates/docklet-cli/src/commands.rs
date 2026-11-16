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
use docklet_core::idle::ProxyReport;
use docklet_core::settings::{self, IdleStop, Settings, Watchers};
use docklet_core::skills::{self, InstallOutcome, SkillStatus};
use docklet_core::{ProxyMode, disk, engine};
use docklet_wsl::tools::{self, CONTEXT_NAME, CliSetup};
use docklet_wsl::{DISTRO, Engine, RootfsSource};

use crate::args::{Command, ConfigChange, ConfigKey, InstallSource, SkillsAction};
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
        Command::ConfigGet(key) => config_get(ui, key)?,
        Command::ConfigSet(change) => config_set(ui, change)?,
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
        let view = StatusView::of(&engine.status(), proxy::mode());
        println!("{}", view.line());
        warn_if_mirrored_networking();
        return Ok(Json::Null);
    }
    // The same `wsl.exe` queries as `engine.status()`, kept apart so the
    // WSL version can be reported with the state.
    let facts = EngineFacts::gather(engine.wsl());
    let proxy = proxy::mode();
    let view = StatusView::of(&facts.state(), proxy);
    let mirrored_networking = warn_if_mirrored_networking();
    Ok(status::status_data(
        &view,
        &StatusFacts {
            proxy,
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
    start_engine(ui, &engine)?;
    proxy::ensure_running().map_err(|err| engine_failure(ui, &engine, Op::Start, err))?;
    ui.say(&running_line());
    Ok(running_data().into())
}

/// Starts the engine and waits until Docker answers. Wakes it if it sleeps.
///
/// An idle-stop proxy owns the engine's lifetime, so with one running the
/// engine is asked for the way a Docker client asks: with a request through
/// the pipe. The proxy starts the distro if it has to, and its idle time
/// starts over. Started behind the proxy's back, the distro would be up with
/// the proxy still believing it stopped, and nothing would stop it again.
fn start_engine(ui: &Ui, engine: &Engine) -> Result<(), Failure> {
    let through_proxy = (proxy::mode() == ProxyMode::IdleStop).then(proxy::ping);
    if let Some(Ok(())) = through_proxy {
        return Ok(());
    }
    // No such proxy, or it could not bring the engine up. Starting it here
    // says why not, in the words this command has always used.
    engine
        .start()
        .map_err(|err| engine_failure(ui, engine, Op::Start, err))?;
    if let Some(Err(err)) = through_proxy {
        eprintln!(
            "docklet: warning: the engine is running, but a request through the pipe proxy \
             failed: {err}"
        );
    }
    Ok(())
}

fn running_line() -> String {
    usable_line(false)
}

fn running_data() -> Object {
    usable_data(false)
}

/// What `wait` and `up` say about an engine Docker commands work with:
/// running, or asleep behind a proxy that wakes it.
fn usable_line(asleep: bool) -> String {
    if asleep {
        format!(
            "Engine is asleep and starts with the next Docker command. Docker host: {}",
            proxy::DOCKER_HOST
        )
    } else {
        format!("Engine is running. Docker host: {}", proxy::DOCKER_HOST)
    }
}

fn usable_data(asleep: bool) -> Object {
    Object::new()
        .set("state", if asleep { "sleeping" } else { "running" })
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
    // What stands in the way of the engine itself; the proxy comes later.
    match failure::blocker(&facts, ProxyMode::Plain) {
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
    // An engine that sleeps behind an idle-stop proxy is usable as it is:
    // the first Docker command wakes it. `up` leaves it asleep rather than
    // start a distro that may not be needed yet.
    let asleep = facts.asleep_behind(proxy::mode());
    if !asleep && facts.dockerd != Some(true) {
        start_engine(ui, &engine)?;
        changed.push("started_engine");
    }
    if !proxy::is_running() {
        proxy::ensure_running().map_err(|err| engine_failure(ui, &engine, Op::Start, err))?;
        changed.push("started_proxy");
    }
    let setup = docker_setup(ui, &home).map_err(|err| {
        Failure::failed(format!(
            "the engine is running, but the Docker CLI could not be set up: {err}"
        ))
    })?;
    changed.extend(setup_changes(&setup));
    ui.say(&usable_line(asleep));
    ui.say(&format!("Docker CLI: {}", setup.docker.display()));
    ui.say("Run Docker with: docklet docker ps");
    Ok(setup_data(usable_data(asleep), &setup, changed).into())
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

fn config_get(ui: &Ui, key: Option<ConfigKey>) -> Result<Json, Failure> {
    let data_dir = docklet_wsl::data_dir()?;
    let (settings, problems) = settings::load(&data_dir)?;
    // A line that was not understood left its setting at the default; say
    // so rather than show the default as if it had been chosen.
    for problem in &problems {
        eprintln!(
            "docklet: warning: {}: {problem}",
            settings::path(&data_dir).display()
        );
    }
    match key {
        Some(key) => {
            ui.say(&setting_value(&settings, key));
            Ok(setting_data(&settings, key).into())
        }
        None => {
            for key in ConfigKey::ALL {
                ui.say(&format!(
                    "{} = {}",
                    key.name(),
                    setting_value(&settings, key)
                ));
            }
            let all: Vec<Json> = ConfigKey::ALL
                .into_iter()
                .map(|key| setting_data(&settings, key).into())
                .collect();
            Ok(Object::new().set("settings", all).into())
        }
    }
}

/// A setting's value as `config set` takes it.
fn setting_value(settings: &Settings, key: ConfigKey) -> String {
    match key {
        ConfigKey::IdleStop => settings.idle_stop.to_string(),
        ConfigKey::Watchers => settings.watchers.to_string(),
    }
}

fn setting_data(settings: &Settings, key: ConfigKey) -> Object {
    match key {
        ConfigKey::IdleStop => idle_stop_data(settings.idle_stop),
        ConfigKey::Watchers => watchers_data(settings.watchers),
    }
}

fn config_set(ui: &Ui, change: ConfigChange) -> Result<Json, Failure> {
    let data_dir = docklet_wsl::data_dir()?;
    let data = match change {
        ConfigChange::IdleStop(idle_stop) => {
            settings::save_idle_stop(&data_dir, idle_stop)?;
            idle_stop_data(idle_stop)
        }
        ConfigChange::Watchers(watchers) => {
            settings::save_watchers(&data_dir, watchers)?;
            watchers_data(watchers)
        }
    };
    // A proxy reads the settings once, when it starts.
    let restart_needed = proxy::is_running();
    for line in config_set_lines(change, restart_needed) {
        ui.say(&line);
    }
    Ok(data
        .set("applies_from", "next_start")
        .set("restart_needed", restart_needed)
        .into())
}

/// One setting as `--json` reports it: the value as `config set` takes it,
/// and the minutes as a number, `null` for `off`.
fn idle_stop_data(idle_stop: IdleStop) -> Object {
    Object::new()
        .set("key", settings::IDLE_STOP_KEY)
        .set("value", idle_stop.to_string())
        .set("minutes", idle_stop.minutes().map(u64::from))
}

/// The watchers setting as `--json` reports it, with the value as a boolean.
fn watchers_data(watchers: Watchers) -> Object {
    Object::new()
        .set("key", settings::WATCHERS_KEY)
        .set("value", watchers.as_str())
        .set("keep_awake", watchers == Watchers::KeepAwake)
}

/// What `config set` prints: what the value means, and when it takes
/// effect.
fn config_set_lines(change: ConfigChange, proxy_running: bool) -> Vec<String> {
    let meaning = match change {
        ConfigChange::IdleStop(IdleStop::Off) => {
            "idle-stop = off: the engine runs until `docklet stop`.".to_string()
        }
        ConfigChange::IdleStop(IdleStop::After(minutes)) => format!(
            "idle-stop = {minutes}: an engine nothing used for {minutes} {} is stopped, \
             and starts again when Docker is used.",
            if minutes == 1 { "minute" } else { "minutes" }
        ),
        ConfigChange::Watchers(Watchers::Ignored) => {
            "watchers-keep-awake = no: a connection that carries nothing, such as Docker \
             Desktop or an IDE watching the engine, does not keep it awake."
                .to_string()
        }
        ConfigChange::Watchers(Watchers::KeepAwake) => {
            "watchers-keep-awake = yes: every open connection keeps the engine awake, even \
             one that carries nothing."
                .to_string()
        }
    };
    let applies = if proxy_running {
        "The change applies from the next `docklet start`. The engine that is running now \
         keeps the old setting until `docklet stop`."
    } else {
        "The change applies from the next `docklet start`."
    };
    vec![meaning, applies.to_string()]
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
        let proxy = proxy::mode();
        let Some(blocker) = failure::blocker(&facts, proxy) else {
            // Usable is running, or asleep behind a proxy that wakes it.
            let asleep = facts.asleep_behind(proxy);
            ui.say(&usable_line(asleep));
            return Ok(usable_data(asleep)
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
        proxy: proxy::mode(),
        docker_cli: docker::find(&data_dir).map(|cli| cli.path.display().to_string()),
        compose_plugin: home
            .as_deref()
            .is_some_and(|home| docker_client::compose_plugin(home).is_file()),
        docker_context: docker_context(home.as_deref()),
        mirrored_networking: docklet_wsl::mirrored_networking(),
        data_dir: data_dir.display().to_string(),
        free_space: disk::free_space(&data_dir).map_err(|err| err.to_string()),
    };
    let mut checks = doctor::report(&facts);
    checks.push(idle_check(facts.proxy, &data_dir));
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

/// What keeps the engine awake, as an idle-stop proxy reports it. Asking
/// connects to nothing, so it never wakes the engine.
fn idle_check(proxy_mode: ProxyMode, data_dir: &Path) -> Check {
    let report = match proxy_mode {
        ProxyMode::IdleStop => proxy::report(data_dir),
        _ => None,
    };
    idle_verdict(proxy_mode, report.as_ref())
}

fn idle_verdict(proxy_mode: ProxyMode, report: Option<&ProxyReport>) -> Check {
    let (level, detail, fix) = match (proxy_mode, report) {
        (ProxyMode::IdleStop, Some(report)) => report.verdict(),
        (ProxyMode::IdleStop, None) => (
            Level::Ok,
            "on; the pipe proxy did not say what its connections are doing".to_string(),
            None,
        ),
        (ProxyMode::Plain, _) => (
            Level::Ok,
            "off: the engine runs until `docklet stop`".to_string(),
            None,
        ),
        (ProxyMode::Down, _) => (Level::Ok, "no pipe proxy runs".to_string(), None),
    };
    Check {
        id: "idle_stop",
        title: "Idle stop",
        level,
        detail,
        fix,
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
            proxy: ProxyMode::Down,
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
        assert!(idle_verdict(ProxyMode::Down, None).title.len() <= 17);
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
        assert_eq!(usable_line(false), running_line());
        let running: Json = running_data().into();
        let usable: Json = usable_data(false).into();
        assert_eq!(running, usable);
    }

    #[test]
    fn a_sleeping_engine_is_reported_as_usable_and_asleep() {
        assert_eq!(
            usable_line(true),
            "Engine is asleep and starts with the next Docker command. \
             Docker host: npipe:////./pipe/docklet_engine"
        );
        let data: Json = usable_data(true).set("waited_seconds", 0u64).into();
        assert_eq!(
            data.render(),
            concat!(
                r#"{"state":"sleeping","docker_host":"npipe:////./pipe/docklet_engine","#,
                r#""pipe":"\\\\.\\pipe\\docklet_engine","waited_seconds":0}"#
            )
        );
    }

    #[test]
    fn idle_stop_data_shape() {
        let minutes: Json = idle_stop_data(IdleStop::After(10)).into();
        assert_eq!(
            minutes.render(),
            r#"{"key":"idle-stop","value":"10","minutes":10}"#
        );
        let off: Json = idle_stop_data(IdleStop::Off).into();
        assert_eq!(
            off.render(),
            r#"{"key":"idle-stop","value":"off","minutes":null}"#
        );
    }

    #[test]
    fn setting_idle_stop_says_when_the_change_applies() {
        let idle = |idle_stop| ConfigChange::IdleStop(idle_stop);
        assert_eq!(
            config_set_lines(idle(IdleStop::After(5)), false),
            [
                "idle-stop = 5: an engine nothing used for 5 minutes is stopped, \
                 and starts again when Docker is used.",
                "The change applies from the next `docklet start`.",
            ]
        );
        let one = config_set_lines(idle(IdleStop::After(1)), false);
        assert!(one[0].contains("for 1 minute is stopped"), "{}", one[0]);
        // A proxy that is running keeps what it started with.
        assert_eq!(
            config_set_lines(idle(IdleStop::Off), true),
            [
                "idle-stop = off: the engine runs until `docklet stop`.",
                "The change applies from the next `docklet start`. The engine that is \
                 running now keeps the old setting until `docklet stop`.",
            ]
        );
        let changes = [
            idle(IdleStop::Off),
            idle(IdleStop::After(30)),
            ConfigChange::Watchers(Watchers::Ignored),
            ConfigChange::Watchers(Watchers::KeepAwake),
        ];
        for change in changes {
            for proxy_running in [true, false] {
                let lines = config_set_lines(change, proxy_running);
                assert!(lines[1].contains("next `docklet start`"));
            }
        }
    }

    #[test]
    fn setting_watchers_says_what_counts_as_use() {
        let lines = config_set_lines(ConfigChange::Watchers(Watchers::Ignored), false);
        assert!(
            lines[0].starts_with("watchers-keep-awake = no: "),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("does not keep it awake"), "{}", lines[0]);
        let lines = config_set_lines(ConfigChange::Watchers(Watchers::KeepAwake), false);
        assert!(
            lines[0].starts_with("watchers-keep-awake = yes: "),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn watchers_data_shape() {
        let no: Json = watchers_data(Watchers::Ignored).into();
        assert_eq!(
            no.render(),
            r#"{"key":"watchers-keep-awake","value":"no","keep_awake":false}"#
        );
        let yes: Json = watchers_data(Watchers::KeepAwake).into();
        assert_eq!(
            yes.render(),
            r#"{"key":"watchers-keep-awake","value":"yes","keep_awake":true}"#
        );
    }

    #[test]
    fn config_get_lists_every_setting() {
        let settings = Settings {
            idle_stop: IdleStop::After(25),
            watchers: Watchers::KeepAwake,
        };
        let values: Vec<String> = ConfigKey::ALL
            .into_iter()
            .map(|key| format!("{} = {}", key.name(), setting_value(&settings, key)))
            .collect();
        assert_eq!(values, ["idle-stop = 25", "watchers-keep-awake = yes"]);
        let data: Json = setting_data(&settings, ConfigKey::Watchers).into();
        assert!(data.render().contains(r#""keep_awake":true"#));
    }

    #[test]
    fn the_idle_check_follows_the_proxy_and_its_report() {
        use docklet_core::idle::{
            ConnectionState, EnginePhase, ProxyReport, ReportedConnection, Watchers,
        };
        let plain = idle_verdict(ProxyMode::Plain, None);
        assert_eq!((plain.id, plain.title), ("idle_stop", "Idle stop"));
        assert_eq!(plain.level, Level::Ok);
        assert_eq!(plain.detail, "off: the engine runs until `docklet stop`");
        assert_eq!(
            idle_verdict(ProxyMode::Down, None).detail,
            "no pipe proxy runs"
        );
        let silent = idle_verdict(ProxyMode::IdleStop, None);
        assert_eq!(silent.level, Level::Ok);
        assert!(silent.detail.starts_with("on; "), "{}", silent.detail);
        let report = ProxyReport {
            phase: EnginePhase::Running,
            watchers: Watchers::KeepAwake,
            idle_after: Duration::from_secs(600),
            unused_for: None,
            connections: vec![ReportedConnection {
                state: ConnectionState::Open,
                pid: Some(4242),
                program: Some("com.docker.build.exe".to_string()),
                quiet: Duration::from_secs(3600),
                age: Duration::from_secs(3600),
                to_engine: 300,
                to_client: 900,
            }],
        };
        let watched = idle_verdict(ProxyMode::IdleStop, Some(&report));
        assert_eq!(watched.level, Level::Warn);
        assert!(
            watched.detail.contains("com.docker.build.exe (pid 4242)"),
            "{}",
            watched.detail
        );
        assert_eq!(
            watched.fix,
            Some("docklet config set watchers-keep-awake no")
        );
        // A warning never fails doctor.
        assert_eq!(doctor_failure(&[watched]), None);
    }
}
