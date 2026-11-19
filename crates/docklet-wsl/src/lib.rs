//! The real `wsl.exe` runner and the engine lifecycle built on it: rootfs
//! download and verification, import, keepalive, stop and removal.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use docklet_core::engine::{self, Contents, DISTRO_NAME, Installed};
use docklet_core::idle::EngineHooks;
use docklet_core::wsl::{Wsl, WslOutput, WslRunner};
use docklet_core::wslconfig::Vm;
use docklet_core::{EngineState, system_program, wsl_output, wslconfig, wslconfig_file};
use sha2::{Digest, Sha256};

pub mod tools;
pub mod usage;

pub use docklet_core::engine::DISTRO_NAME as DISTRO;

const START_TIMEOUT: Duration = Duration::from_secs(120);

/// Hides the console window that would otherwise flash for each child.
pub(crate) fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Longest a query may take: `--version`, the `--list` forms and `docker info`
/// inside the distro. They answer in well under a second on a healthy WSL.
const QUERY_DEADLINE: Duration = Duration::from_secs(30);
/// `--terminate` and `--unregister` wait for the distro to shut down.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(3 * 60);
/// `--import` unpacks the whole rootfs: 28 s on a fast machine, minutes on a
/// slow disk.
const IMPORT_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// How long one `wsl.exe` call may run before it is given up and killed.
fn deadline_for(args: &[&str]) -> Duration {
    match args.first().copied() {
        Some("--import") => IMPORT_DEADLINE,
        Some("--terminate" | "--unregister") => TEARDOWN_DEADLINE,
        _ => QUERY_DEADLINE,
    }
}

/// Runs the installed `wsl.exe`.
///
/// Every call has a deadline (see `deadline_for`). A `wsl.exe` that hangs,
/// which happens while the WSL service is wedged or updating, is killed and
/// reported as an error, so callers that poll cannot pile up behind it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl WslRunner for SystemRunner {
    fn run(&self, args: &[&str]) -> io::Result<WslOutput> {
        let mut command = Command::new(system_program("wsl.exe"));
        command
            .args(args)
            // UTF-8 output where supported; `wsl_output::decode` handles the rest.
            .env("WSL_UTF8", "1")
            .stdin(Stdio::null());
        let limit = deadline_for(args);
        let output = output_within(no_window(&mut command), limit).map_err(|err| {
            if err.kind() != io::ErrorKind::TimedOut {
                return err;
            }
            let timed_out = format!(
                "`wsl {}` did not finish within {} s and was stopped",
                args.join(" "),
                limit.as_secs()
            );
            io::Error::new(io::ErrorKind::TimedOut, timed_out)
        })?;
        Ok(WslOutput {
            success: output.status.success(),
            stdout: wsl_output::decode(&output.stdout),
            stderr: wsl_output::decode(&output.stderr),
        })
    }
}

/// Like `Command::output`, but a child that is still running after `limit` is
/// killed and the call fails with `ErrorKind::TimedOut`.
fn output_within(command: &mut Command, limit: Duration) -> io::Result<Output> {
    /// Longest pause between two looks at the child.
    const MAX_PAUSE: Duration = Duration::from_millis(200);

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Both pipes are read while waiting: a child that fills one would
    // otherwise block on it and never exit.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + limit;
    // Short at first so a quick query is answered quickly, longer for a call
    // that takes its time. Every round sleeps; the wait never spins.
    let mut pause = Duration::from_millis(5);
    let status = loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => None,
            Ok(None) => Some(io::Error::from(io::ErrorKind::TimedOut)),
            Err(err) => Some(err),
        };
        if let Some(failure) = failure {
            let _ = child.kill();
            let _ = child.wait();
            // The readers are left to end on their own, when the pipes
            // close. Joining them here could wait on a process that the
            // child started and that still holds a pipe open.
            return Err(failure);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(MAX_PAUSE);
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Reads a child's pipe to its end on another thread.
fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Where the rootfs for a new install comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootfsSource {
    /// A published base, by name (see `docklet_core::engine::bases`).
    Base(String),
    /// A local `rootfs.tar.gz` that fulfils the rootfs contract, with the
    /// SHA-256 it must have.
    File { path: PathBuf, sha256: String },
    /// The same, downloaded from an `https://` URL.
    Url { url: String, sha256: String },
}

impl RootfsSource {
    /// What the install record says about this source.
    fn installed(&self) -> Installed {
        match self {
            Self::Base(base) => Installed::Base(base.clone()),
            Self::File { path, sha256 } => Installed::Rootfs {
                location: path.display().to_string(),
                sha256: sha256.clone(),
            },
            Self::Url { url, sha256 } => Installed::Rootfs {
                location: url.clone(),
                sha256: sha256.clone(),
            },
        }
    }
}

/// A rootfs that has been fetched and checked, ready to import.
struct Prepared {
    path: PathBuf,
    /// Docklet downloaded it into its cache, so it is deleted after use.
    downloaded: bool,
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
        let prepared = self.prepare(source, progress)?;
        self.import(source, prepared, progress)
    }

    /// Replaces the installed distro by one from `source`, deleting every
    /// container, image and volume in it. The new rootfs is downloaded and
    /// checked before anything is removed, so a failed download leaves the
    /// old engine as it was. Stopping the pipe proxy first is the caller's
    /// job.
    pub fn reinstall(
        &self,
        source: &RootfsSource,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        match self.status() {
            EngineState::NotInstalled => {
                return Err(io::Error::other(
                    "the docklet distro is not installed; run `docklet install`",
                ));
            }
            EngineState::WslMissing => return Err(io::Error::other("WSL is not installed")),
            EngineState::Error(reason) => return Err(io::Error::other(reason)),
            _ => {}
        }
        let prepared = self.prepare(source, progress)?;
        progress("Removing the old docklet distro");
        if let Err(err) = self.remove_distro() {
            if prepared.downloaded {
                let _ = fs::remove_file(&prepared.path);
            }
            return Err(err);
        }
        self.import(source, prepared, progress).map_err(|err| {
            io::Error::other(format!(
                "the old distro was removed, but the new one could not be imported: {err}"
            ))
        })
    }

    /// Fetches and checks the rootfs of `source`.
    fn prepare(
        &self,
        source: &RootfsSource,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<Prepared> {
        match source {
            RootfsSource::File { path, sha256 } => {
                progress("Verifying SHA-256");
                let actual = file_sha256(path)?;
                // The user's own file is never deleted, even when it does
                // not match.
                if actual != sha256.to_ascii_lowercase() {
                    return Err(io::Error::other(format!(
                        "SHA-256 mismatch for {}: expected {sha256}, got {actual}",
                        path.display()
                    )));
                }
                Ok(Prepared {
                    path: path.clone(),
                    downloaded: false,
                })
            }
            RootfsSource::Url { url, sha256 } => {
                let cache = self.cache()?;
                let rootfs = cache.join("rootfs-custom.tar.gz");
                progress(&format!("Downloading {url}"));
                download(url, &rootfs)?;
                progress("Verifying SHA-256");
                verify_sha256(&rootfs, sha256)?;
                Ok(Prepared {
                    path: rootfs,
                    downloaded: true,
                })
            }
            RootfsSource::Base(base) => {
                let url = engine::rootfs_url(base).ok_or_else(|| {
                    io::Error::other(format!(
                        "unknown base `{base}`; available: {}",
                        engine::bases().join(", ")
                    ))
                })?;
                let cache = self.cache()?;
                let rootfs = cache.join(format!("rootfs-{base}.tar.gz"));
                let checksum = cache.join(format!("rootfs-{base}.tar.gz.sha256"));
                progress(&format!("Downloading {url}"));
                download(&url, &rootfs)?;
                download(&format!("{url}.sha256"), &checksum)?;
                progress("Verifying SHA-256");
                verify_sha256(&rootfs, &fs::read_to_string(&checksum)?)?;
                let _ = fs::remove_file(&checksum);
                Ok(Prepared {
                    path: rootfs,
                    downloaded: true,
                })
            }
        }
    }

    fn cache(&self) -> io::Result<PathBuf> {
        let cache = self.data_dir.join("cache");
        fs::create_dir_all(&cache)?;
        Ok(cache)
    }

    /// Imports a prepared rootfs and records what it was made from.
    fn import(
        &self,
        source: &RootfsSource,
        prepared: Prepared,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<()> {
        let install_dir = self.data_dir.join("distro");
        fs::create_dir_all(&install_dir)?;
        progress("Importing the distro into WSL");
        let imported = self.wsl.import(
            DISTRO_NAME,
            &install_dir.to_string_lossy(),
            &prepared.path.to_string_lossy(),
        );
        if prepared.downloaded {
            let _ = fs::remove_file(&prepared.path);
        }
        imported?;
        // The distro works without the record; only `docklet base` misses it.
        let _ = fs::write(
            self.data_dir.join(Installed::FILE_NAME),
            source.installed().render(),
        );
        Ok(())
    }

    /// What the installed distro was made from: the record Docklet wrote,
    /// or, for an install older than the record, the base named inside the
    /// distro, read only while it runs (reading would start it).
    pub fn installed(&self) -> Option<Installed> {
        if let Some(installed) = fs::read_to_string(self.data_dir.join(Installed::FILE_NAME))
            .ok()
            .and_then(|text| Installed::parse(&text))
        {
            return Some(installed);
        }
        if !engine::distro_running(&self.wsl).unwrap_or(false) {
            return None;
        }
        let output = self
            .wsl
            .exec(
                DISTRO_NAME,
                &["cat", "/usr/local/share/docklet/versions.txt"],
            )
            .ok()?;
        output
            .success
            .then(|| engine::base_in_versions(&output.stdout))
            .flatten()
            .map(Installed::Base)
    }

    /// The containers, images and volumes in a running engine. Asks dockerd
    /// inside the distro, so call it only while the distro runs.
    pub fn contents(&self) -> io::Result<Contents> {
        let output = self
            .wsl
            .exec(DISTRO_NAME, &["sh", "-c", engine::COUNT_CONTENTS])?;
        if !output.success {
            return Err(io::Error::other(format!(
                "docker could not list the engine's contents: {}",
                output.stderr.trim()
            )));
        }
        engine::parse_contents(&output.stdout)
            .ok_or_else(|| io::Error::other("docker's lists could not be counted"))
    }

    fn remove_distro(&self) -> io::Result<()> {
        let _ = self.wsl.terminate(DISTRO_NAME);
        self.wsl.unregister(DISTRO_NAME)?;
        let _ = fs::remove_file(self.data_dir.join(Installed::FILE_NAME));
        Ok(())
    }

    /// Starts the distro, holds it up with a keepalive process and waits for
    /// dockerd. Does nothing if the engine already runs.
    pub fn start(&self) -> io::Result<()> {
        match self.status() {
            EngineState::Running => return Ok(()),
            // A probe of the engine alone never says `Sleeping`; to this
            // call it is a stopped distro like any other.
            EngineState::Stopped | EngineState::Sleeping | EngineState::Starting => {}
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
        self.remove_distro()?;
        let _ = fs::remove_dir(self.data_dir.join("distro"));
        Ok(())
    }
}

/// What the pipe proxy needs to stop an idle engine and start it again when
/// a client connects (see `docklet_core::idle`). `Engine::system()?` is all a
/// caller has to build.
impl<R: WslRunner + Send + Sync> EngineHooks for Engine<R> {
    fn start_engine(&self) -> io::Result<()> {
        self.start()
    }

    /// What `stop` does, minus the full status probe: `stop` takes a WSL that
    /// does not answer for an engine that is not running and succeeds, and
    /// the proxy would then believe a running distro stopped.
    fn stop_distro(&self) -> io::Result<()> {
        if engine::distro_running(&self.wsl)? {
            self.wsl.terminate(DISTRO_NAME)?;
        }
        Ok(())
    }

    fn distro_running(&self) -> io::Result<bool> {
        engine::distro_running(&self.wsl)
    }

    fn running_containers(&self) -> io::Result<usize> {
        engine::running_containers(&self.wsl)
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
        "\"{}\" -d {DISTRO_NAME} -u root --exec /bin/sleep infinity",
        system_program("wsl.exe").display()
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

/// True when the user's `%USERPROFILE%\.wslconfig` turns on mirrored
/// networking, under which published ports may not reach Windows (see
/// `docklet_core::wslconfig`). The file is only read, never written; a
/// missing or unreadable file counts as not mirrored.
pub fn mirrored_networking() -> bool {
    wslconfig_path()
        .and_then(fs::read)
        .ok()
        // Editors may save it as UTF-16; `decode` handles that and the BOM.
        .is_some_and(|bytes| wslconfig::mirrored_networking(&wsl_output::decode(&bytes)))
}

/// `%USERPROFILE%\.wslconfig`, which configures the VM of every WSL 2
/// distro of the user. Docklet changes it only through
/// `docklet_core::wslconfig_file`.
pub fn wslconfig_path() -> io::Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".wslconfig"))
        .ok_or_else(|| io::Error::other("USERPROFILE is not set"))
}

/// The paths `docklet_core::wslconfig_file` works on, for this user.
pub fn wslconfig_paths() -> io::Result<wslconfig_file::Paths> {
    Ok(wslconfig_file::Paths::new(wslconfig_path()?, &data_dir()?))
}

/// Whether the WSL VM runs, for which distros, and since when if the
/// docklet distro is among them: its `/proc/uptime` is the VM's, since all
/// distros share one kernel. Nothing runs in the user's other distros, and
/// a stopped docklet distro is not started to ask.
pub fn vm_state<R: WslRunner>(wsl: &Wsl<R>) -> io::Result<Vm> {
    let distros = wsl.running()?;
    if distros.is_empty() {
        return Ok(Vm::Stopped);
    }
    let booted = if distros.iter().any(|name| name == DISTRO_NAME) {
        wsl.exec(DISTRO_NAME, &["cat", "/proc/uptime"])
            .ok()
            .filter(|output| output.success)
            .and_then(|output| wslconfig::parse_uptime(&output.stdout))
            .and_then(|uptime| SystemTime::now().checked_sub(uptime))
    } else {
        None
    };
    Ok(Vm::Running { distros, booted })
}

/// Downloads with the `curl.exe` that ships with Windows, which keeps an HTTP
/// and TLS stack out of the Docklet binaries.
pub(crate) fn download(url: &str, destination: &Path) -> io::Result<()> {
    let mut command = Command::new(system_program("curl.exe"));
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

/// The SHA-256 of a file, in lower-case hex.
fn file_sha256(file: &Path) -> io::Result<String> {
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
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Checks a downloaded `file` against the first token of a `sha256sum`
/// line, and deletes it when it does not match.
pub(crate) fn verify_sha256(file: &Path, checksum_line: &str) -> io::Result<()> {
    let expected = checksum_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual = file_sha256(file)?;
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
    fn queries_get_the_short_deadline() {
        assert_eq!(deadline_for(&["--version"]), QUERY_DEADLINE);
        assert_eq!(deadline_for(&["--list", "--quiet"]), QUERY_DEADLINE);
        assert_eq!(
            deadline_for(&["--list", "--running", "--quiet"]),
            QUERY_DEADLINE
        );
        let docker_info = ["-d", "docklet", "-u", "root", "--exec", "docker", "info"];
        assert_eq!(deadline_for(&docker_info), QUERY_DEADLINE);
        assert_eq!(deadline_for(&[]), QUERY_DEADLINE);
    }

    #[test]
    fn slow_calls_get_the_time_they_need() {
        let import = ["--import", "docklet", "C:\\d", "C:\\r.tar.gz"];
        assert_eq!(deadline_for(&import), IMPORT_DEADLINE);
        assert_eq!(deadline_for(&["--terminate", "docklet"]), TEARDOWN_DEADLINE);
        assert_eq!(
            deadline_for(&["--unregister", "docklet"]),
            TEARDOWN_DEADLINE
        );
        // An import took 28 s on a fast machine; a query must never wait
        // that long, and an import must be allowed far more.
        assert!(QUERY_DEADLINE < TEARDOWN_DEADLINE);
        assert!(TEARDOWN_DEADLINE < IMPORT_DEADLINE);
        assert!(IMPORT_DEADLINE >= Duration::from_secs(10 * 60));
    }

    /// A program that runs for about 30 s and then ends on its own, which
    /// also bounds this test if the kill were to fail.
    fn slow_command() -> Command {
        if cfg!(windows) {
            let mut command = Command::new(system_program("ping.exe"));
            command.args(["-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
    }

    #[test]
    #[ignore = "runner only"]
    fn a_child_past_its_deadline_is_killed() {
        let started = Instant::now();
        let mut command = slow_command();
        command.stdin(Stdio::null());
        let err = output_within(&mut command, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    #[ignore = "runner only"]
    fn a_child_within_its_deadline_returns_its_output() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new(system_program("cmd.exe"));
            command.args(["/d", "/c", "echo", "docklet"]);
            command
        } else {
            let mut command = Command::new("echo");
            command.arg("docklet");
            command
        };
        command.stdin(Stdio::null());
        let output = output_within(&mut command, Duration::from_secs(30)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "docklet");
    }

    /// `wsl.exe` scripted by a closure; records every call.
    struct ScriptedRunner<F> {
        answer: F,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl<F: Fn(&[&str]) -> WslOutput> WslRunner for ScriptedRunner<F> {
        fn run(&self, args: &[&str]) -> io::Result<WslOutput> {
            self.calls.lock().unwrap().push(args.join(" "));
            Ok((self.answer)(args))
        }
    }

    fn scripted(
        answer: impl Fn(&[&str]) -> WslOutput,
    ) -> ScriptedRunner<impl Fn(&[&str]) -> WslOutput> {
        ScriptedRunner {
            answer,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn output(success: bool, stdout: &str) -> WslOutput {
        WslOutput {
            success,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn idle_stop_terminates_only_a_running_distro() {
        let running = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(true, "docklet\n"),
            ["--terminate", "docklet"] => output(true, ""),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        let engine = Engine::new(&running, PathBuf::new());
        engine.stop_distro().unwrap();
        assert_eq!(
            *running.calls.lock().unwrap(),
            ["--list --running --quiet", "--terminate docklet"]
        );

        let stopped = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(false, ""),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        Engine::new(&stopped, PathBuf::new()).stop_distro().unwrap();
        assert_eq!(stopped.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn idle_check_reads_the_running_list_and_docker_ps() {
        let runner = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(true, "Ubuntu\ndocklet\n"),
            [.., "docker", "ps", "--quiet"] => output(true, "abc\ndef\n"),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        let engine = Engine::new(&runner, PathBuf::new());
        assert!(engine.distro_running().unwrap());
        assert_eq!(engine.running_containers().unwrap(), 2);
    }

    /// SHA-256 of `abc`.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// A machine with WSL 3 where the docklet distro is installed or not,
    /// and stopped. Import, terminate and unregister succeed.
    fn wsl_with(installed: bool) -> ScriptedRunner<impl Fn(&[&str]) -> WslOutput> {
        scripted(move |args| match args {
            ["--version"] => output(true, "WSL version: 3.0.5.0\n"),
            ["--list", "--quiet"] => output(true, if installed { "docklet\n" } else { "" }),
            ["--list", "--running", "--quiet"] => output(false, ""),
            ["--import", "docklet", _, _, "--version", "2"] => output(true, ""),
            ["--terminate", "docklet"] | ["--unregister", "docklet"] => output(true, ""),
            other => panic!("unexpected wsl call: {other:?}"),
        })
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("docklet-wsl-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_local_rootfs_is_checked_imported_and_recorded() {
        let dir = temp_dir("install-file");
        let rootfs = dir.join("rootfs.tar.gz");
        fs::write(&rootfs, b"abc").unwrap();
        let runner = wsl_with(false);
        let engine = Engine::new(&runner, dir.join("data"));
        let source = RootfsSource::File {
            path: rootfs.clone(),
            sha256: ABC.to_ascii_uppercase(),
        };
        engine.install(&source, &mut |_| {}).unwrap();
        // The user's file stays where it was.
        assert!(rootfs.exists());
        let record = fs::read_to_string(dir.join("data").join(Installed::FILE_NAME)).unwrap();
        assert_eq!(
            Installed::parse(&record),
            Some(Installed::Rootfs {
                location: rootfs.display().to_string(),
                sha256: ABC.to_ascii_uppercase(),
            })
        );
        assert!(
            runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("--import docklet"))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_local_rootfs_with_another_hash_is_refused_and_kept() {
        let dir = temp_dir("install-mismatch");
        let rootfs = dir.join("rootfs.tar.gz");
        fs::write(&rootfs, b"abd").unwrap();
        let runner = wsl_with(false);
        let engine = Engine::new(&runner, dir.join("data"));
        let source = RootfsSource::File {
            path: rootfs.clone(),
            sha256: ABC.to_string(),
        };
        let err = engine.install(&source, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("SHA-256 mismatch"), "{err}");
        assert!(rootfs.exists());
        assert!(
            !runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("--import"))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reinstall_checks_the_new_rootfs_before_removing_the_old_distro() {
        let dir = temp_dir("reinstall");
        let rootfs = dir.join("rootfs.tar.gz");
        fs::write(&rootfs, b"abd").unwrap();
        let runner = wsl_with(true);
        let engine = Engine::new(&runner, dir.join("data"));
        let bad = RootfsSource::File {
            path: rootfs.clone(),
            sha256: ABC.to_string(),
        };
        assert!(engine.reinstall(&bad, &mut |_| {}).is_err());
        assert!(
            !runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("--unregister"))
        );

        fs::write(&rootfs, b"abc").unwrap();
        let good = RootfsSource::File {
            path: rootfs,
            sha256: ABC.to_string(),
        };
        engine.reinstall(&good, &mut |_| {}).unwrap();
        let calls = runner.calls.lock().unwrap();
        let unregister = calls
            .iter()
            .position(|call| call == "--unregister docklet")
            .unwrap();
        let import = calls
            .iter()
            .position(|call| call.starts_with("--import"))
            .unwrap();
        assert!(unregister < import);
        drop(calls);
        assert!(matches!(engine.installed(), Some(Installed::Rootfs { .. })));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reinstall_needs_an_installed_distro() {
        let runner = wsl_with(false);
        let engine = Engine::new(&runner, PathBuf::new());
        let err = engine
            .reinstall(&RootfsSource::Base("alpine".to_string()), &mut |_| {})
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }

    #[test]
    fn without_a_record_the_base_is_read_only_from_a_running_distro() {
        let stopped = wsl_with(true);
        let engine = Engine::new(&stopped, temp_dir("installed-stopped"));
        assert_eq!(engine.installed(), None);
        // A stopped distro is not started to ask.
        assert!(
            !stopped
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("-d"))
        );

        let running = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(true, "docklet\n"),
            [.., "cat", "/usr/local/share/docklet/versions.txt"] => {
                output(true, "base debian-13\ndocker 29.9.3\n")
            }
            other => panic!("unexpected wsl call: {other:?}"),
        });
        let dir = temp_dir("installed-running");
        let engine = Engine::new(&running, dir.clone());
        assert_eq!(
            engine.installed(),
            Some(Installed::Base("debian-13".to_string()))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_vm_boot_is_asked_only_of_a_running_docklet_distro() {
        let none = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(false, ""),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        assert_eq!(vm_state(&Wsl::new(&none)).unwrap(), Vm::Stopped);

        let others = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(true, "Ubuntu\n"),
            other => panic!("nothing may run in the user's distro: {other:?}"),
        });
        assert_eq!(
            vm_state(&Wsl::new(&others)).unwrap(),
            Vm::Running {
                distros: vec!["Ubuntu".to_string()],
                booted: None
            }
        );

        let docklet = scripted(|args| match args {
            ["--list", "--running", "--quiet"] => output(true, "Ubuntu\ndocklet\n"),
            [
                "-d",
                "docklet",
                "-u",
                "root",
                "--exec",
                "cat",
                "/proc/uptime",
            ] => output(true, "60.00 100.00\n"),
            other => panic!("unexpected wsl call: {other:?}"),
        });
        let Vm::Running {
            booted: Some(booted),
            ..
        } = vm_state(&Wsl::new(&docklet)).unwrap()
        else {
            panic!("expected a boot time");
        };
        let ago = SystemTime::now().duration_since(booted).unwrap();
        assert!(ago >= Duration::from_secs(60) && ago < Duration::from_secs(120));
    }

    #[test]
    fn contents_are_counted_with_one_call() {
        let runner = scripted(|args| match args {
            ["-d", "docklet", "-u", "root", "--exec", "sh", "-c", line] => {
                assert_eq!(*line, engine::COUNT_CONTENTS);
                output(true, "2\n5\n1\n")
            }
            other => panic!("unexpected wsl call: {other:?}"),
        });
        let contents = Engine::new(&runner, PathBuf::new()).contents().unwrap();
        assert_eq!(
            contents,
            Contents {
                containers: 2,
                images: 5,
                volumes: 1
            }
        );
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
