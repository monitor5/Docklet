//! Engine status, derived from what `wsl.exe` reports.

use crate::state::EngineState;
use crate::wsl::{Wsl, WslRunner, WslVersion};

/// Name of the dedicated WSL2 distro that hosts the Docker Engine.
pub const DISTRO_NAME: &str = "docklet";

/// Version of the rootfs release this build installs (`distro/VERSION`).
pub const ROOTFS_VERSION: &str = include_str!("../../../distro/VERSION");

const BASES_TXT: &str = include_str!("../../../distro/bases.txt");
const RELEASES: &str = "https://github.com/monitor5/Docklet/releases/download";

/// Names of the Linux bases a rootfs is published for, default first.
///
/// The engine logic never branches on the base: every rootfs provides the
/// same contract, whatever starts dockerd in it (systemd or a boot script):
/// dockerd running after boot on `/var/run/docker.sock`,
/// `/usr/local/bin/docklet-relay`, and a `docker` CLI inside the distro.
pub fn bases() -> Vec<&'static str> {
    base_rows().filter_map(|row| row.first().copied()).collect()
}

/// The rows of `bases.txt` split into columns: name, recipe, base image, then
/// `KEY=VALUE` build arguments. Only the name matters at run time; the rest
/// is for `distro/build.sh`.
fn base_rows() -> impl Iterator<Item = Vec<&'static str>> {
    BASES_TXT
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|row| !row.is_empty())
}

/// Download URL of the published rootfs for `base`, or `None` if unknown.
pub fn rootfs_url(base: &str) -> Option<String> {
    bases().contains(&base).then(|| {
        format!(
            "{RELEASES}/distro-v{}/rootfs-{base}.tar.gz",
            ROOTFS_VERSION.trim()
        )
    })
}

/// Works out the engine state without changing anything.
///
/// The distro counts as `Running` only when dockerd answers; a distro that is
/// up while dockerd is still starting reports `Starting`.
pub fn probe<R: WslRunner>(wsl: &Wsl<R>) -> EngineState {
    let version = match wsl.version() {
        Ok(Some(version)) => version,
        Ok(None) => return EngineState::WslMissing,
        // A WSL that hangs is installed; telling the user to install it
        // would send them the wrong way.
        Err(err) => return EngineState::Error(err.to_string()),
    };
    if version < WslVersion::MINIMUM {
        return EngineState::Error(format!(
            "WSL {version} is too old; Docklet needs {} or later (run `wsl --update`)",
            WslVersion::MINIMUM
        ));
    }
    let has = |names: std::io::Result<Vec<String>>| {
        names.map(|names| names.iter().any(|name| name == DISTRO_NAME))
    };
    match has(wsl.installed()) {
        Ok(true) => {}
        Ok(false) => return EngineState::NotInstalled,
        Err(err) => return EngineState::Error(err.to_string()),
    }
    match has(wsl.running()) {
        Ok(true) => {}
        Ok(false) => return EngineState::Stopped,
        Err(err) => return EngineState::Error(err.to_string()),
    }
    if docker_ready(wsl) {
        EngineState::Running
    } else {
        EngineState::Starting
    }
}

/// True when dockerd inside the distro answers on its socket.
pub fn docker_ready<R: WslRunner>(wsl: &Wsl<R>) -> bool {
    wsl.exec(
        DISTRO_NAME,
        &["docker", "info", "--format", "{{.ServerVersion}}"],
    )
    .is_ok_and(|output| output.success)
}

/// True when the docklet distro is running. One `wsl.exe` call that, unlike
/// anything run inside the distro, never boots it.
pub fn distro_running<R: WslRunner>(wsl: &Wsl<R>) -> std::io::Result<bool> {
    Ok(wsl.running()?.iter().any(|name| name == DISTRO_NAME))
}

/// Number of running containers, from `docker ps -q` inside the distro.
///
/// Boots the distro if it is not running, so check `distro_running` first.
pub fn running_containers<R: WslRunner>(wsl: &Wsl<R>) -> std::io::Result<usize> {
    let output = wsl.exec(DISTRO_NAME, &["docker", "ps", "--quiet"])?;
    if !output.success {
        return Err(std::io::Error::other(format!(
            "docker ps failed: {}",
            output.stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

/// Named pipe that Docklet exposes to the GUI and the Windows `docker` CLI.
pub const ENGINE_PIPE: &str = r"\\.\pipe\docklet_engine";

/// The same pipe as a Docker host URL.
pub const ENGINE_DOCKER_HOST: &str = "npipe:////./pipe/docklet_engine";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsl::testing::{MockRunner, fail, ok};
    use std::io;

    /// A machine described by its WSL version, installed and running distros
    /// and whether dockerd answers.
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

    const V3: &str = "WSL version: 3.0.5.0\n";

    #[test]
    fn missing_wsl() {
        let runner = MockRunner::new(|_| Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::WslMissing);
    }

    #[test]
    fn a_wsl_that_hangs_is_an_error_not_missing() {
        let runner = MockRunner::new(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "wsl --version did not finish",
            ))
        });
        assert_eq!(
            probe(&Wsl::new(&runner)),
            EngineState::Error("wsl --version did not finish".to_string())
        );
    }

    #[test]
    fn old_wsl_is_an_error_with_advice() {
        let runner = machine("WSL version: 2.3.26.0\n", "", "", false);
        let EngineState::Error(reason) = probe(&Wsl::new(&runner)) else {
            panic!("expected an error state");
        };
        assert!(reason.contains("wsl --update"));
    }

    #[test]
    fn other_distros_do_not_count() {
        let runner = machine(V3, "Ubuntu-26.04\n", "Ubuntu-26.04\n", true);
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::NotInstalled);
    }

    #[test]
    fn installed_but_not_running() {
        let runner = machine(V3, "Ubuntu-26.04\ndocklet\n", "Ubuntu-26.04\n", true);
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::Stopped);
        let runner = machine(V3, "docklet\n", "", true);
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::Stopped);
    }

    #[test]
    fn distro_up_but_dockerd_not_ready() {
        let runner = machine(V3, "docklet\n", "docklet\n", false);
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::Starting);
    }

    #[test]
    fn running() {
        let runner = machine(V3, "docklet\n", "docklet\n", true);
        assert_eq!(probe(&Wsl::new(&runner)), EngineState::Running);
    }

    #[test]
    fn distro_running_only_counts_the_docklet_distro() {
        let runner = machine(V3, "Ubuntu-26.04\ndocklet\n", "Ubuntu-26.04\n", true);
        assert!(!distro_running(&Wsl::new(&runner)).unwrap());
        let runner = machine(V3, "docklet\n", "", true);
        assert!(!distro_running(&Wsl::new(&runner)).unwrap());
        let runner = machine(V3, "docklet\n", "docklet\n", true);
        assert!(distro_running(&Wsl::new(&runner)).unwrap());
        // Only the list of running distros is asked for.
        assert_eq!(
            runner.calls.borrow().as_slice(),
            [["--list", "--running", "--quiet"]]
        );
    }

    #[test]
    fn running_containers_counts_ids() {
        let runner = MockRunner::new(|args| match args {
            [
                "-d",
                "docklet",
                "-u",
                "root",
                "--exec",
                "docker",
                "ps",
                "--quiet",
            ] => ok("0123456789ab\r\nba9876543210\n\n"),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        assert_eq!(running_containers(&Wsl::new(&runner)).unwrap(), 2);
        let runner = MockRunner::new(|_| ok(""));
        assert_eq!(running_containers(&Wsl::new(&runner)).unwrap(), 0);
    }

    #[test]
    fn running_containers_fails_without_dockerd() {
        let runner = MockRunner::new(|_| fail("Cannot connect to the Docker daemon"));
        let err = running_containers(&Wsl::new(&runner)).unwrap_err();
        assert!(err.to_string().contains("Cannot connect"));
    }

    #[test]
    fn bases_list_default_first() {
        assert_eq!(bases(), ["debian-13", "ubuntu-26.04", "alpine"]);
    }

    #[test]
    fn rootfs_url_only_for_known_bases() {
        for base in ["ubuntu-26.04", "alpine"] {
            let url = rootfs_url(base).unwrap();
            assert!(
                url.starts_with("https://github.com/monitor5/Docklet/releases/download/distro-v")
            );
            assert!(url.ends_with(&format!("/rootfs-{base}.tar.gz")));
            assert!(!url.contains(char::is_whitespace));
        }
        assert_eq!(rootfs_url("arch"), None);
        assert_eq!(rootfs_url("apk"), None);
    }

    /// `build.sh` reads the other columns; a broken row would only show up
    /// on the distro runner, so check them here.
    #[test]
    fn every_base_row_names_a_recipe_stage_and_an_image() {
        let dockerfile = include_str!("../../../distro/Dockerfile");
        let mut names = Vec::new();
        for row in base_rows() {
            let [name, recipe, image, args @ ..] = row.as_slice() else {
                panic!("bases.txt row {row:?} needs a name, a recipe and an image");
            };
            assert!(!names.contains(name), "base {name} is listed twice");
            names.push(*name);
            assert!(
                dockerfile.contains(&format!("FROM ${{BASE_IMAGE}} AS {recipe}\n")),
                "base {name}: recipe {recipe} is not a stage in distro/Dockerfile"
            );
            assert!(image.contains(':'), "base {name}: image {image} has no tag");
            for arg in args {
                let (key, value) = arg.split_once('=').unwrap_or_default();
                // The same rule as build.sh: ^[A-Z][A-Z0-9_]*=.+$
                assert!(
                    key.starts_with(|c: char| c.is_ascii_uppercase())
                        && key
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                        && !value.is_empty(),
                    "base {name}: {arg} is not KEY=VALUE"
                );
            }
        }
    }

    #[test]
    fn rootfs_version_is_a_version() {
        assert!(crate::wsl::parse_version(ROOTFS_VERSION).is_some());
    }
}
