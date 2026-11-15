//! `docklet docker <args...>`: the Docker CLI against the Docklet engine.
//!
//! `docker.exe` gets this process's stdin, stdout and stderr and its exit
//! code becomes ours, so the command behaves like `docker` itself in a pipe,
//! in a script and at a terminal.

use std::ffi::OsString;
use std::io;
use std::process::Command;

use docklet_core::docker_client::{self, DockerCli};
use docklet_core::doctor::EngineFacts;
use docklet_core::engine;
use docklet_wsl::{DISTRO, Engine};

use crate::failure::{self, ErrorCode, Failure};
use crate::proxy;

/// Runs `docker.exe` and returns its exit code.
///
/// The engine is never started from here. A Docker command that quietly
/// boots a WSL distro would surprise whoever stopped it, and `wsl -d` on a
/// stopped distro starts it without the keepalive that holds it up.
pub fn run(args: &[OsString]) -> Result<i32, Failure> {
    let engine = Engine::system()?;
    if !answers(&engine)
        && let Some(blocker) =
            failure::blocker(&EngineFacts::gather(engine.wsl()), proxy::is_running())
    {
        return Err(blocker);
    }
    let cli = find(&docklet_wsl::data_dir()?).ok_or_else(|| {
        Failure::new(
            ErrorCode::DockerCliMissing,
            "no docker.exe on PATH or in Docklet's bin directory",
        )
        .hint("docklet cli setup")
    })?;
    passthrough(&mut command(&cli, args))
        .map_err(|err| Failure::failed(format!("cannot run {}: {err}", cli.path.display())))
}

/// The quick form of "is the engine usable", for the common case that it
/// is: the proxy check costs nothing and two `wsl.exe` queries replace the
/// four of a full probe. Only when it says no is the engine examined
/// properly, to say why.
fn answers(engine: &Engine) -> bool {
    proxy::is_running()
        // Asked first: dockerd may only be queried on a running distro.
        && engine
            .wsl()
            .running()
            .is_ok_and(|names| names.iter().any(|name| name == DISTRO))
        && engine::docker_ready(engine.wsl())
}

pub fn find(data_dir: &std::path::Path) -> Option<DockerCli> {
    docker_client::find_docker(std::env::var_os("PATH").as_deref(), data_dir)
}

/// `docker.exe` pointed at the Docklet pipe.
///
/// The host is given through the environment rather than as `--host` or
/// `--context`: the caller's arguments stay exactly as they were, plugins
/// such as Compose inherit it, and it works before the `docklet` context
/// exists. `DOCKER_HOST` outranks `DOCKER_CONTEXT` and the configured
/// current context; the caller's own `--context` or `--host` still wins.
fn command(cli: &DockerCli, args: &[OsString]) -> Command {
    let mut command = Command::new(&cli.path);
    command
        .args(args)
        .env("DOCKER_HOST", proxy::DOCKER_HOST)
        .env_remove("DOCKER_CONTEXT")
        // Meant for some other, remote engine; the pipe speaks plain HTTP.
        .env_remove("DOCKER_TLS_VERIFY")
        .env_remove("DOCKER_TLS")
        .env_remove("DOCKER_CERT_PATH");
    command
}

/// Runs the command on this process's own stdin, stdout and stderr and
/// returns its exit code.
fn passthrough(command: &mut Command) -> io::Result<i32> {
    leave_interrupts_to_the_child();
    let status = command.status()?;
    // `None` is a child killed by a signal, which Windows does not have.
    Ok(status.code().unwrap_or(1))
}

/// Ctrl+C reaches every process attached to the console. Docker handles it
/// (it forwards the signal, or stops a Compose project in order); if this
/// process died of it first, the caller would get its prompt back while
/// Docker is still working.
#[cfg(windows)]
fn leave_interrupts_to_the_child() {
    // Declared here rather than through the `windows` crate, which the CLI
    // does not otherwise need; kernel32 is linked by std anyway.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// Claims Ctrl+C (0) and Ctrl+Break (1). Closing the console, logging
    /// off and shutting down keep their default handling.
    unsafe extern "system" fn handler(event: u32) -> i32 {
        i32::from(event <= 1)
    }

    // SAFETY: `handler` is a function that lives as long as the program,
    // has the signature the API calls, and touches no state. A handler is
    // registered rather than Ctrl+C ignored (a null handler), because that
    // setting would be inherited by docker.exe.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(not(windows))]
fn leave_interrupts_to_the_child() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    fn cli() -> DockerCli {
        DockerCli {
            path: PathBuf::from("docker.exe"),
            on_path: true,
        }
    }

    fn env_of<'a>(command: &'a Command, key: &str) -> Option<Option<&'a OsStr>> {
        command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(key))
            .map(|(_, value)| value)
    }

    #[test]
    fn arguments_reach_docker_unchanged_and_in_order() {
        let args: Vec<OsString> = ["compose", "-f", "my file.yml", "up", "-d", ""]
            .into_iter()
            .map(OsString::from)
            .collect();
        let command = command(&cli(), &args);
        assert_eq!(command.get_program(), OsStr::new("docker.exe"));
        let passed: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            passed,
            args.iter().map(OsString::as_os_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn docker_is_pointed_at_the_docklet_pipe() {
        let command = command(&cli(), &[]);
        assert_eq!(
            env_of(&command, "DOCKER_HOST"),
            Some(Some(OsStr::new("npipe:////./pipe/docklet_engine")))
        );
        // Removed from the child's environment, whatever the caller has set.
        for removed in [
            "DOCKER_CONTEXT",
            "DOCKER_TLS_VERIFY",
            "DOCKER_TLS",
            "DOCKER_CERT_PATH",
        ] {
            assert_eq!(env_of(&command, removed), Some(None), "{removed}");
        }
    }

    /// A program that exits with code 7 at once.
    fn exits_with_seven() -> Command {
        if cfg!(windows) {
            let mut command = Command::new(docklet_core::system_program("cmd.exe"));
            command.args(["/d", "/c", "exit 7"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 7"]);
            command
        }
    }

    #[test]
    #[ignore = "runner only"]
    fn the_exit_code_of_the_child_is_passed_through() {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(passthrough(&mut exits_with_seven()));
        });
        // The hard limit: a child that hangs fails the test instead of
        // holding the runner.
        let code = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the child did not exit within 30 s")
            .unwrap();
        assert_eq!(code, 7);
    }
}
