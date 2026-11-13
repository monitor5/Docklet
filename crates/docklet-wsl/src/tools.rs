//! Windows-side Docker client setup: the `docker` CLI, the Compose plugin and
//! a `docklet` Docker context that points at the Docklet pipe.
//!
//! Nothing the user already has is replaced: an existing `docker.exe` on PATH,
//! an existing Compose plugin and an existing `docklet` context are kept.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{download, no_window, verify_sha256};

pub const DOCKER_CLI_VERSION: &str = "29.9.3";
const DOCKER_CLI_URL: &str =
    "https://download.docker.com/win/static/stable/x86_64/docker-29.9.3.zip";
const DOCKER_CLI_SHA256: &str = "7c4ff7bbb31579046cd34dd8b91f7cbd3ab3482a1a85330170be726ef792d751";

pub const COMPOSE_VERSION: &str = "v5.7.1";
const COMPOSE_URL: &str =
    "https://github.com/docker/compose/releases/download/v5.7.1/docker-compose-windows-x86_64.exe";
const COMPOSE_SHA256: &str = "e4316597452650ae92fe9719a2a7c2fce8f56c5ceafed88d2aed6e39b2b2b2d0";

pub const CONTEXT_NAME: &str = "docklet";

/// What `setup` found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSetup {
    /// The `docker.exe` to use.
    pub docker: PathBuf,
    /// False when `docker` was installed by Docklet into a directory that is
    /// not on PATH, so it has to be called by its full path.
    pub docker_on_path: bool,
    pub installed_docker: bool,
    pub installed_compose: bool,
    pub created_context: bool,
}

/// Makes sure a Docker CLI, the Compose plugin and the `docklet` context
/// exist. `progress` receives one line per step that changes something.
pub fn setup(
    data_dir: &Path,
    home: &Path,
    docker_host: &str,
    progress: &mut dyn FnMut(&str),
) -> io::Result<CliSetup> {
    let cache = data_dir.join("cache");
    let own_docker = data_dir.join("bin").join("docker.exe");

    let (docker, docker_on_path, installed_docker) = if let Some(found) = find_on_path("docker.exe")
    {
        (found, true, false)
    } else if own_docker.is_file() {
        (own_docker, false, false)
    } else {
        progress(&format!("Downloading the Docker CLI {DOCKER_CLI_VERSION}"));
        fs::create_dir_all(&cache)?;
        fs::create_dir_all(data_dir.join("bin"))?;
        let archive = cache.join("docker-cli.zip");
        download(DOCKER_CLI_URL, &archive)?;
        verify_sha256(&archive, DOCKER_CLI_SHA256)?;
        // bsdtar ships with Windows and reads zip archives.
        let extracted = run(Command::new("tar.exe")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&cache)
            .arg("docker/docker.exe"));
        let _ = fs::remove_file(&archive);
        extracted?;
        fs::rename(cache.join("docker").join("docker.exe"), &own_docker)?;
        let _ = fs::remove_dir(cache.join("docker"));
        (own_docker, false, true)
    };

    let compose = home
        .join(".docker")
        .join("cli-plugins")
        .join("docker-compose.exe");
    let installed_compose = if compose.is_file() {
        false
    } else {
        progress(&format!("Downloading Docker Compose {COMPOSE_VERSION}"));
        fs::create_dir_all(compose.parent().expect("has a parent"))?;
        download(COMPOSE_URL, &compose)?;
        verify_sha256(&compose, COMPOSE_SHA256)?;
        true
    };

    let context_exists =
        run(Command::new(&docker).args(["context", "inspect", CONTEXT_NAME])).is_ok();
    if !context_exists {
        progress("Creating the `docklet` Docker context");
        run(Command::new(&docker).args([
            "context",
            "create",
            CONTEXT_NAME,
            "--description",
            "Docklet engine",
            "--docker",
            &format!("host={docker_host}"),
        ]))?;
    }

    Ok(CliSetup {
        docker,
        docker_on_path,
        installed_docker,
        installed_compose,
        created_context: !context_exists,
    })
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Runs a command silently; an unsuccessful exit becomes an error carrying
/// the command's stderr.
fn run(command: &mut Command) -> io::Result<()> {
    command.stdin(Stdio::null());
    let output = no_window(command).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}
