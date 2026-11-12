//! Typed access to `wsl.exe`, behind a trait so the logic tests on any OS.
//!
//! Only output that is not localized is parsed: version numbers and the
//! `--quiet` list forms. Headers and messages change with the Windows display
//! language and are never interpreted.

use std::io;

/// Result of one `wsl.exe` invocation, already decoded to text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `wsl.exe` with the given arguments.
///
/// An `Err` means the process could not be started (WSL is not installed).
pub trait WslRunner {
    fn run(&self, args: &[&str]) -> io::Result<WslOutput>;
}

impl<R: WslRunner + ?Sized> WslRunner for &R {
    fn run(&self, args: &[&str]) -> io::Result<WslOutput> {
        (**self).run(args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WslVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl WslVersion {
    /// Oldest WSL that Docklet supports.
    pub const MINIMUM: Self = Self {
        major: 2,
        minor: 4,
        patch: 4,
    };
}

impl std::fmt::Display for WslVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Extracts the WSL version from `wsl --version` output.
///
/// The first line is `<localized label>: 3.0.5.0`, so the first dotted number
/// in the text is taken, whatever the label says.
pub fn parse_version(text: &str) -> Option<WslVersion> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|token| {
            let mut parts = token.split('.').map(str::parse::<u32>);
            let major = parts.next()?.ok()?;
            let minor = parts.next()?.ok()?;
            let patch = parts.next()?.ok()?;
            Some(WslVersion {
                major,
                minor,
                patch,
            })
        })
}

/// Parses `wsl -l -q` / `wsl -l --running -q`: one distro name per line.
pub fn parse_name_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// `wsl.exe` commands used by Docklet.
pub struct Wsl<R> {
    runner: R,
}

impl<R: WslRunner> Wsl<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// `None` when WSL is missing or too old to answer `--version`.
    pub fn version(&self) -> Option<WslVersion> {
        let output = self.runner.run(&["--version"]).ok()?;
        if !output.success {
            return None;
        }
        parse_version(&output.stdout)
    }

    pub fn installed(&self) -> io::Result<Vec<String>> {
        self.name_list(&["--list", "--quiet"])
    }

    pub fn running(&self) -> io::Result<Vec<String>> {
        self.name_list(&["--list", "--running", "--quiet"])
    }

    fn name_list(&self, args: &[&str]) -> io::Result<Vec<String>> {
        let output = self.runner.run(args)?;
        // With nothing to list, wsl.exe fails and prints a localized message
        // instead of names. That is an empty list, not an error.
        if !output.success {
            return Ok(Vec::new());
        }
        Ok(parse_name_list(&output.stdout))
    }

    pub fn import(&self, distro: &str, install_dir: &str, rootfs: &str) -> io::Result<()> {
        self.checked(&["--import", distro, install_dir, rootfs, "--version", "2"])
    }

    pub fn terminate(&self, distro: &str) -> io::Result<()> {
        self.checked(&["--terminate", distro])
    }

    /// Deletes the distro and its disk.
    pub fn unregister(&self, distro: &str) -> io::Result<()> {
        self.checked(&["--unregister", distro])
    }

    /// Runs a program inside the distro as root, without a shell.
    pub fn exec(&self, distro: &str, command: &[&str]) -> io::Result<WslOutput> {
        let mut args = vec!["-d", distro, "-u", "root", "--exec"];
        args.extend_from_slice(command);
        self.runner.run(&args)
    }

    fn checked(&self, args: &[&str]) -> io::Result<()> {
        let output = self.runner.run(args)?;
        if output.success {
            return Ok(());
        }
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        Err(io::Error::other(format!(
            "wsl {} failed: {detail}",
            args.join(" ")
        )))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::RefCell;

    type Handler = Box<dyn Fn(&[&str]) -> io::Result<WslOutput>>;

    /// Scripted `wsl.exe` that records every invocation.
    pub struct MockRunner {
        handler: Handler,
        pub calls: RefCell<Vec<Vec<String>>>,
    }

    impl MockRunner {
        pub fn new(handler: impl Fn(&[&str]) -> io::Result<WslOutput> + 'static) -> Self {
            Self {
                handler: Box::new(handler),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl WslRunner for MockRunner {
        fn run(&self, args: &[&str]) -> io::Result<WslOutput> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            (self.handler)(args)
        }
    }

    pub fn ok(stdout: &str) -> io::Result<WslOutput> {
        Ok(WslOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    pub fn fail(stderr: &str) -> io::Result<WslOutput> {
        Ok(WslOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::wsl_output::decode;

    const VERSION_KO: &str = "WSL 버전: 3.0.5.0\r\n커널 버전: 6.18.58.1-1\r\nWSLg 버전: 1.0.79\r\nWindows 버전: 10.0.26300.9711\r\n";
    const VERSION_EN: &str = "WSL version: 2.4.4.0\r\nKernel version: 5.15.167.4-1\r\nWindows version: 10.0.22631.4460\r\n";

    fn utf16le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn parses_version_from_utf8_and_utf16_fixtures() {
        let expected = WslVersion {
            major: 3,
            minor: 0,
            patch: 5,
        };
        assert_eq!(
            parse_version(&decode(VERSION_KO.as_bytes())),
            Some(expected)
        );
        assert_eq!(parse_version(&decode(&utf16le(VERSION_KO))), Some(expected));
        assert_eq!(
            parse_version(&decode(&utf16le(VERSION_EN))),
            Some(WslVersion::MINIMUM)
        );
    }

    #[test]
    fn version_is_none_for_text_without_a_version() {
        assert_eq!(parse_version("wsl: unknown option --version"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn versions_order_numerically() {
        let old = parse_version("2.3.26.0").unwrap();
        let new = parse_version("2.10.1.0").unwrap();
        assert!(old < WslVersion::MINIMUM);
        assert!(new > WslVersion::MINIMUM);
    }

    #[test]
    fn parses_name_lists_from_both_encodings() {
        let text = "Ubuntu-26.04\r\ndocklet\r\n\r\n";
        let expected = vec!["Ubuntu-26.04".to_string(), "docklet".to_string()];
        assert_eq!(parse_name_list(&decode(text.as_bytes())), expected);
        assert_eq!(parse_name_list(&decode(&utf16le(text))), expected);
    }

    #[test]
    fn failed_list_means_no_distros() {
        let runner = MockRunner::new(|_| fail("실행 중인 배포가 없습니다."));
        assert!(Wsl::new(&runner).running().unwrap().is_empty());
    }

    #[test]
    fn import_reports_wsl_error_text() {
        let runner = MockRunner::new(|_| fail("disk full"));
        let err = Wsl::new(&runner)
            .import("docklet", "C:\\d", "C:\\r.tar.gz")
            .unwrap_err();
        assert!(err.to_string().contains("disk full"));
        assert_eq!(
            runner.calls.borrow()[0],
            [
                "--import",
                "docklet",
                "C:\\d",
                "C:\\r.tar.gz",
                "--version",
                "2"
            ]
        );
    }

    #[test]
    fn exec_runs_as_root_without_a_shell() {
        let runner = MockRunner::new(|_| ok(""));
        Wsl::new(&runner)
            .exec("docklet", &["docker", "info"])
            .unwrap();
        assert_eq!(
            runner.calls.borrow()[0],
            ["-d", "docklet", "-u", "root", "--exec", "docker", "info"]
        );
    }
}
