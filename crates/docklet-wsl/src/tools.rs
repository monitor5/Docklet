//! Windows-side Docker client setup: the `docker` CLI, the Compose plugin and
//! a `docklet` Docker context that points at the Docklet pipe.
//!
//! Nothing the user already has is replaced: an existing `docker.exe` on PATH,
//! an existing Compose plugin and an existing `docklet` context are kept.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use docklet_core::system_program;

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
        let extracted = run(Command::new(system_program("tar.exe"))
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
        // Docker loads anything in cli-plugins, so only a verified file may
        // ever appear there.
        fs::create_dir_all(&cache)?;
        let staged = cache.join("docker-compose.exe");
        download(COMPOSE_URL, &staged)?;
        verify_sha256(&staged, COMPOSE_SHA256)?;
        fs::create_dir_all(compose.parent().expect("has a parent"))?;
        let moved = move_file(&staged, &compose);
        let _ = fs::remove_file(&staged);
        moved?;
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

/// Relative PATH entries (`.`, empty) are skipped: they resolve against the
/// current directory, which may be untrusted.
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Renames, or copies when `from` and `to` are on different volumes (the
/// cache under `%LOCALAPPDATA%` and the user profile can be).
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_then_rename(from, to)
}

/// Copies `from` next to `to` under a partial name and renames it into place,
/// so `to` only ever exists complete. A copy that is cut short, even by the
/// process being killed, leaves a file that Docker does not load and that the
/// next run overwrites.
fn copy_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let partial = to.with_file_name(format!(".{name}.partial"));
    let moved = fs::copy(from, &partial).and_then(|_| fs::rename(&partial, to));
    if moved.is_err() {
        let _ = fs::remove_file(&partial);
    }
    moved
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_file_replaces_the_source() {
        let dir = std::env::temp_dir().join(format!("docklet-tools-move-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let from = dir.join("staged.exe");
        let to = dir.join("final.exe");
        fs::write(&from, b"verified").unwrap();
        move_file(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(&to).unwrap(), b"verified");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_copy_fallback_leaves_only_the_complete_file() {
        let dir = std::env::temp_dir().join(format!("docklet-tools-copy-{}", std::process::id()));
        let plugins = dir.join("cli-plugins");
        fs::create_dir_all(&plugins).unwrap();
        let from = dir.join("staged.exe");
        let to = plugins.join("docker-compose.exe");
        fs::write(&from, b"verified").unwrap();
        copy_then_rename(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"verified");
        let left: Vec<_> = fs::read_dir(&plugins)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["docker-compose.exe"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_copy_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("docklet-tools-fail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let to = dir.join("docker-compose.exe");
        assert!(copy_then_rename(&dir.join("missing.exe"), &to).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
