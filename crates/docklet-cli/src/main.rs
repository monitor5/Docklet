//! `docklet` command line interface.
//!
//! The CLI and the tray GUI are two front ends over the same crates. Argument
//! parsing is done by hand to keep the binary small.

use std::path::PathBuf;
use std::process::ExitCode;

use docklet_core::skills::{self, InstallOutcome, SkillStatus};
use docklet_core::{EngineState, engine};
use docklet_wsl::{Engine, RootfsSource};

const HELP: &str = "\
Docklet - Docker Engine on WSL2 without Docker Desktop

Usage: docklet <command>

Engine:
  status                    Show the engine state
  install [--base NAME]     Download and install the docklet distro
          [--rootfs FILE]   ...or install it from a local rootfs.tar.gz
  bases                     List the Linux bases available for `install --base`
  start                     Start the engine and the Docker pipe, wait until ready
  stop                      Stop the engine and the Docker pipe
  uninstall --yes           Delete the docklet distro with all containers,
                            images and volumes (other distros are untouched)

Docker client:
  cli setup                 Install the Windows docker CLI and Compose if missing
                            and create the `docklet` Docker context

Agent skill:
  skills status             Show whether the Docklet agent skill is registered
  skills install            Register or update the skill (Claude Code, Codex)
  skills uninstall          Remove the skill

Other:
  version, -V, --version    Print the Docklet version
  help, -h, --help          Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => {
            print!("{HELP}");
            Ok(())
        }
        ["version" | "-V" | "--version"] => {
            println!("docklet {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["bases"] => {
            for (index, base) in engine::bases().iter().enumerate() {
                println!("{base}{}", if index == 0 { " (default)" } else { "" });
            }
            Ok(())
        }
        ["status"] => status(),
        ["install", options @ ..] => install(options),
        ["start"] => with_engine(|engine| {
            engine.start()?;
            proxy::ensure_running()?;
            println!("Engine is running. Docker host: {}", proxy::DOCKER_HOST);
            Ok(())
        }),
        ["stop"] => with_engine(|engine| {
            proxy::stop();
            engine.stop()?;
            println!("Engine is stopped.");
            Ok(())
        }),
        ["proxy", options @ ..] => proxy::serve(options),
        ["uninstall", "--yes"] => with_engine(|engine| {
            proxy::stop();
            engine.uninstall()?;
            println!("The docklet distro is removed.");
            Ok(())
        }),
        ["uninstall"] => Err(Failure::Usage(
            "`uninstall` deletes every container, image and volume in the docklet distro; \
             pass --yes to confirm"
                .to_string(),
        )),
        ["cli", "setup"] => cli_setup(),
        ["skills", action] => run_skills(action),
        _ => Err(Failure::Usage(format!(
            "unknown command `{}`",
            args.join(" ")
        ))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("docklet: {message}\n");
            eprint!("{HELP}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(message)) => {
            eprintln!("docklet: {message}");
            ExitCode::FAILURE
        }
    }
}

enum Failure {
    /// The command line was wrong; exit code 2 and the help text.
    Usage(String),
    /// The command was understood but failed; exit code 1.
    Runtime(String),
}

impl From<std::io::Error> for Failure {
    fn from(err: std::io::Error) -> Self {
        Self::Runtime(err.to_string())
    }
}

fn with_engine(action: impl FnOnce(&Engine) -> std::io::Result<()>) -> Result<(), Failure> {
    let engine = Engine::system()?;
    Ok(action(&engine)?)
}

fn status() -> Result<(), Failure> {
    let engine = Engine::system()?;
    let line = match engine.status() {
        EngineState::WslMissing => "wsl-missing: WSL is not installed".to_string(),
        EngineState::NotInstalled => {
            "not-installed: run `docklet install` to set up the engine".to_string()
        }
        EngineState::Stopped => "stopped".to_string(),
        EngineState::Starting => "starting: the distro is up, Docker is not ready yet".to_string(),
        EngineState::Running if proxy::is_running() => "running".to_string(),
        EngineState::Running => {
            "starting: Docker is up but the pipe proxy is not; run `docklet start`".to_string()
        }
        EngineState::Stopping => "stopping".to_string(),
        EngineState::Error(reason) => format!("error: {reason}"),
    };
    println!("{line}");
    Ok(())
}

fn install(options: &[&str]) -> Result<(), Failure> {
    let source = match options {
        [] => RootfsSource::Base(engine::bases()[0].to_string()),
        ["--base", base] => RootfsSource::Base(base.to_string()),
        ["--rootfs", file] => RootfsSource::File(PathBuf::from(file)),
        _ => {
            return Err(Failure::Usage(
                "usage: docklet install [--base NAME | --rootfs FILE]".to_string(),
            ));
        }
    };
    let engine = Engine::system()?;
    engine.install(&source, &mut |step| println!("{step}"))?;
    println!("Installed. Run `docklet start` to start the engine.");
    Ok(())
}

fn cli_setup() -> Result<(), Failure> {
    let home = home_dir()
        .ok_or_else(|| Failure::Runtime("cannot find the user's home directory".to_string()))?;
    let setup = docklet_wsl::tools::setup(
        &docklet_wsl::data_dir()?,
        &home,
        proxy::DOCKER_HOST,
        &mut |step| println!("{step}"),
    )?;
    let docker = if setup.docker_on_path {
        "docker".to_string()
    } else {
        format!("\"{}\"", setup.docker.display())
    };
    println!("Docker CLI: {}", setup.docker.display());
    println!("Use it with: {docker} --context docklet ps");
    Ok(())
}

fn run_skills(action: &str) -> Result<(), Failure> {
    if !matches!(action, "status" | "install" | "uninstall") {
        return Err(Failure::Usage(format!("unknown command `skills {action}`")));
    }
    let home = home_dir()
        .ok_or_else(|| Failure::Runtime("cannot find the user's home directory".to_string()))?;
    let mut failed = false;
    for target in &skills::default_targets(&home) {
        let path = target.skill_path();
        let line = match action {
            "status" => match target.status() {
                SkillStatus::AgentNotFound => "agent not found".to_string(),
                SkillStatus::NotInstalled => "not registered".to_string(),
                SkillStatus::Outdated => format!("outdated ({})", path.display()),
                SkillStatus::Current => format!("registered ({})", path.display()),
            },
            "install" => match target.install() {
                Ok(InstallOutcome::AgentNotFound) => "agent not found, skipped".to_string(),
                Ok(InstallOutcome::Installed) => format!("registered ({})", path.display()),
                Ok(InstallOutcome::Updated) => format!("updated ({})", path.display()),
                Ok(InstallOutcome::AlreadyCurrent) => "already up to date".to_string(),
                Err(err) => {
                    failed = true;
                    format!("failed: {err}")
                }
            },
            _ => match target.uninstall() {
                Ok(true) => "removed".to_string(),
                Ok(false) => "not registered".to_string(),
                Err(err) => {
                    failed = true;
                    format!("failed: {err}")
                }
            },
        };
        println!("{}: {line}", target.agent);
    }
    if failed {
        Err(Failure::Runtime(
            "some skills could not be changed".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn home_dir() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .find_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The background pipe proxy (`docklet proxy`), started by `docklet start`.
#[cfg(windows)]
mod proxy {
    use super::Failure;
    use docklet_proxy::{PipeProxyConfig, RelayCommand, control};
    use std::io;
    use std::time::{Duration, Instant};

    pub const DOCKER_HOST: &str = docklet_proxy::ENGINE_DOCKER_HOST;

    pub fn is_running() -> bool {
        control::is_running()
    }

    /// Starts the proxy as a detached process unless one is already running.
    pub fn ensure_running() -> io::Result<()> {
        if is_running() {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        docklet_wsl::spawn_detached(&format!("\"{}\" proxy", exe.display()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if is_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(io::Error::other("the pipe proxy did not start"))
    }

    pub fn stop() {
        if !control::request_stop() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `docklet proxy [--trace] [--byte]`: runs the proxy in the foreground.
    pub fn serve(options: &[&str]) -> Result<(), Failure> {
        let mut config = PipeProxyConfig {
            pipe_name: docklet_proxy::ENGINE_PIPE.to_string(),
            message_mode: true,
            relay: RelayCommand::wsl_relay(docklet_wsl::DISTRO),
            trace: false,
        };
        for option in options {
            match *option {
                "--trace" => config.trace = true,
                // Diagnostic only: without message mode clients cannot half-close.
                "--byte" => config.message_mode = false,
                other => return Err(Failure::Usage(format!("unknown proxy option `{other}`"))),
            }
        }
        if is_running() {
            return Err(Failure::Runtime(
                "a pipe proxy is already running".to_string(),
            ));
        }
        Ok(docklet_proxy::serve(config)?)
    }
}

#[cfg(not(windows))]
mod proxy {
    use super::Failure;
    use std::io;

    pub const DOCKER_HOST: &str = docklet_proxy::ENGINE_DOCKER_HOST;

    pub fn is_running() -> bool {
        false
    }

    pub fn ensure_running() -> io::Result<()> {
        Err(io::Error::other("the docklet engine only runs on Windows"))
    }

    pub fn stop() {}

    pub fn serve(_options: &[&str]) -> Result<(), Failure> {
        Err(Failure::Runtime(
            "the docklet engine only runs on Windows".to_string(),
        ))
    }
}
