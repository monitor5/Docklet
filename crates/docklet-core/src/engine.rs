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
/// same contract (systemd-managed dockerd and `docklet-relay`).
pub fn bases() -> Vec<&'static str> {
    BASES_TXT
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_whitespace().next())
        .collect()
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
    let Some(version) = wsl.version() else {
        return EngineState::WslMissing;
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
    fn bases_list_default_first() {
        assert_eq!(bases(), ["debian-13", "ubuntu-26.04"]);
    }

    #[test]
    fn rootfs_url_only_for_known_bases() {
        let url = rootfs_url("ubuntu-26.04").unwrap();
        assert!(url.starts_with("https://github.com/monitor5/Docklet/releases/download/distro-v"));
        assert!(url.ends_with("/rootfs-ubuntu-26.04.tar.gz"));
        assert!(!url.contains(char::is_whitespace));
        assert_eq!(rootfs_url("arch"), None);
    }
    #[test]
    fn rootfs_version_is_a_version() {
        assert!(crate::wsl::parse_version(ROOTFS_VERSION).is_some());
    }
}
