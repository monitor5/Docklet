//! The real `wsl.exe` runner and the engine lifecycle built on it: rootfs
//! download and verification, import, keepalive, stop and removal.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use docklet_core::engine::{self, DISTRO_NAME};
use docklet_core::wsl::{Wsl, WslOutput, WslRunner};
use docklet_core::{EngineState, wsl_output};
use sha2::{Digest, Sha256};

pub use docklet_core::engine::DISTRO_NAME as DISTRO;

const START_TIMEOUT: Duration = Duration::from_secs(120);

/// Hides the console window that would otherwise flash for each child.
fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Runs the installed `wsl.exe`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl WslRunner for SystemRunner {
    fn run(&self, args: &[&str]) -> io::Result<WslOutput> {
        let mut command = Command::new("wsl.exe");
        command
            .args(args)
            // UTF-8 output where supported; `wsl_output::decode` handles the rest.
            .env("WSL_UTF8", "1")
            .stdin(Stdio::null());
        let output = no_window(&mut command).output()?;
        Ok(WslOutput {
            success: output.status.success(),
            stdout: wsl_output::decode(&output.stdout),
            stderr: wsl_output::decode(&output.stderr),
        })
    }
}

/// Where the rootfs for a new install comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootfsSource {
    /// A published base, by name (see `docklet_core::engine::bases`).
    Base(String),
    /// A local `rootfs.tar.gz` that fulfils the rootfs contract.
    File(PathBuf),
}

/// Directory that holds everything Docklet stores: `%LOCALAPPDATA%\Docklet`.
pub fn data_dir() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|base| PathBuf::from(base).join("Docklet"))
        .ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))
}

/// Engine lifecycle. All state lives in WSL itself, so the CLI and the GUI
/// can both create an `Engine` and see the same thing.
pub struct Engine<R = SystemRunner> {
    wsl: Wsl<R>,
    data_dir: PathBuf,
}

impl Engine<SystemRunner> {
    pub fn system() -> io::Result<Self> {
        Ok(Self::new(SystemRunner, data_dir()?))
    }
}

impl<R: WslRunner> Engine<R> {
    pub fn new(runner: R, data_dir: PathBuf) -> Self {
        Self {
            wsl: Wsl::new(runner),
            data_dir,
        }
    }

    pub fn wsl(&self) -> &Wsl<R> {
        &self.wsl
    }

    pub fn status(&self) -> EngineState {
        engine::probe(&self.wsl)
    }

    /// Installs the distro. `progress` receives one line per step.
    pub fn install(&self, source: &RootfsSource, progress: &mut dyn FnMut(&str)) -> io::Result<()> {
        match self.status() {
            EngineState::NotInstalled => {}
            EngineState::WslMissing => {
                return Err(io::Error::other(
                    "WSL is not installed; run `wsl --install --no-distribution` as administrator and reboot",
                ));
            }
            EngineState::Error(reason) => return Err(io::Error::other(reason)),
            _ => return Err(io::Error::other("the docklet distro is already installed")),
        }

        let (rootfs, downloaded) = match source {
            RootfsSource::File(path) => (path.clone(), false),
            RootfsSource::Base(base) => {
                let url = engine::rootfs_url(base).ok_or_else(|| {
                    io::Error::other(format!(
                        "unknown base `{base}`; available: {}",
                        engine::bases().join(", ")
                    ))
                })?;
                let cache = self.data_dir.join("cache");
                fs::create_dir_all(&cache)?;
                let rootfs = cache.join(format!("rootfs-{base}.tar.gz"));
                let checksum = cache.join(format!("rootfs-{base}.tar.gz.sha256"));
                progress(&format!("Downloading {url}"));
                download(&url, &rootfs)?;
                download(&format!("{url}.sha256"), &checksum)?;
                progress("Verifying SHA-256");
                verify_sha256(&rootfs, &fs::read_to_string(&checksum)?)?;
                let _ = fs::remove_file(&checksum);
                (rootfs, true)
            }
        };

        let install_dir = self.data_dir.join("distro");
        fs::create_dir_all(&install_dir)?;
        progress("Importing the distro into WSL");
        let imported = self.wsl.import(
            DISTRO_NAME,
            &install_dir.to_string_lossy(),
            &rootfs.to_string_lossy(),
        );
        if downloaded {
            let _ = fs::remove_file(&rootfs);
        }
        imported
    }

    /// Starts the distro, holds it up with a keepalive process and waits for
    /// dockerd. Does nothing if the engine already runs.
    pub fn start(&self) -> io::Result<()> {
        match self.status() {
            EngineState::Running => return Ok(()),
            EngineState::Stopped | EngineState::Starting => {}
            EngineState::NotInstalled => {
                return Err(io::Error::other(
                    "the docklet distro is not installed; run `docklet install`",
                ));
            }
            EngineState::WslMissing => return Err(io::Error::other("WSL is not installed")),
            EngineState::Error(reason) => return Err(io::Error::other(reason)),
            EngineState::Stopping => return Err(io::Error::other("the engine is stopping")),
        }
        spawn_keepalive()?;
        let deadline = Instant::now() + START_TIMEOUT;
        while Instant::now() < deadline {
            if engine::docker_ready(&self.wsl) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(io::Error::other(format!(
            "dockerd did not become ready within {} s",
            START_TIMEOUT.as_secs()
        )))
    }

    /// Stops the distro. Terminating it also ends the keepalive process.
    pub fn stop(&self) -> io::Result<()> {
        match self.status() {
            EngineState::Running | EngineState::Starting => self.wsl.terminate(DISTRO_NAME),
            _ => Ok(()),
        }
    }

    /// Removes the distro and its disk, including all containers, images and
    /// volumes. Other WSL distros are not touched.
    pub fn uninstall(&self) -> io::Result<()> {
        match self.status() {
            EngineState::NotInstalled | EngineState::WslMissing => return Ok(()),
            _ => {}
        }
        let _ = self.wsl.terminate(DISTRO_NAME);
        self.wsl.unregister(DISTRO_NAME)?;
        let _ = fs::remove_dir(self.data_dir.join("distro"));
        Ok(())
    }
}

/// WSL stops a distro shortly after its last process started from Windows
/// exits, even if daemons are running. A `sleep infinity` held by `wsl.exe`
/// keeps it up; `wsl --terminate` ends it.
///
/// The keepalive must not inherit any handle from this process. It outlives
/// the CLI, and an inherited stdout pipe would keep whoever reads the CLI's
/// output (a script, an agent, CI) waiting forever.
fn spawn_keepalive() -> io::Result<()> {
    spawn_detached(&format!(
        "wsl.exe -d {DISTRO_NAME} -u root --exec /bin/sleep infinity"
    ))
}

/// Starts a background process that outlives the caller: no window, no
/// inherited handles.
#[cfg(windows)]
pub fn spawn_detached(command_line: &str) -> io::Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    use windows::core::PWSTR;

    let mut command_line: Vec<u16> = command_line
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: the command line is a writable NUL-terminated buffer, the
    // structs are valid for the call, and both returned handles are closed.
    unsafe {
        CreateProcessW(
            None,
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false, // no handle inheritance
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
            None,
            None,
            &startup,
            &mut process,
        )
        .map_err(io::Error::other)?;
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn spawn_detached(_command_line: &str) -> io::Result<()> {
    Err(io::Error::other("the docklet engine only runs on Windows"))
}
/// Downloads with the `curl.exe` that ships with Windows, which keeps an HTTP
/// and TLS stack out of the Docklet binaries.
fn download(url: &str, destination: &Path) -> io::Result<()> {
    let mut command = Command::new("curl.exe");
    command
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--retry", "3", "--output"])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null());
    let output = no_window(&mut command).output()?;
    if output.status.success() {
        Ok(())
    } else {
        let _ = fs::remove_file(destination);
        Err(io::Error::other(format!(
            "download of {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Checks `file` against the first token of a `sha256sum` line.
fn verify_sha256(file: &Path, checksum_line: &str) -> io::Result<()> {
    let expected = checksum_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut hasher = Sha256::new();
    let mut reader = fs::File::open(file)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual == expected {
        Ok(())
    } else {
        let _ = fs::remove_file(file);
        Err(io::Error::other(format!(
            "SHA-256 mismatch for {}: expected {expected}, got {actual}",
            file.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str, content: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("docklet-wsl-{tag}-{}", std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn sha256_accepts_matching_sum_line() {
        let file = temp_file("ok", b"abc");
        let line =
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD  rootfs.tar.gz\n";
        verify_sha256(&file, line).unwrap();
        fs::remove_file(file).unwrap();
    }

    #[test]
    fn sha256_mismatch_deletes_the_file() {
        let file = temp_file("bad", b"abc");
        let err = verify_sha256(&file, "00  rootfs.tar.gz").unwrap_err();
        assert!(err.to_string().contains("SHA-256 mismatch"));
        assert!(!file.exists());
    }
}
