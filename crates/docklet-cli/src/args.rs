//! Command line parsing, by hand to keep the binary small.
//!
//! Parsing touches nothing: it turns the arguments into a [`Command`] or a
//! [`Failure`], so every accepted and rejected form is unit-tested.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use docklet_core::settings::{self, IdleStop, MemorySize, Watchers, WslconfigMode};

use crate::failure::{ErrorCode, Failure};

pub const DEFAULT_WAIT: Duration = Duration::from_secs(120);

/// The flag of `install` and `up` that leaves `.wslconfig` alone.
pub const NO_WSLCONFIG: &str = "--no-wslconfig";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Bases,
    /// The installed base and the ones available.
    Base,
    /// Reinstall on another base. Without `confirmed` (`--yes`) it only
    /// says what would be deleted, and fails with `confirmation_required`.
    BaseSet {
        name: String,
        confirmed: bool,
    },
    Status,
    Install {
        source: InstallSource,
        /// `--no-wslconfig`: leave `.wslconfig` alone this time.
        no_wslconfig: bool,
    },
    Start,
    Stop,
    /// Only parsed with `--yes`.
    Uninstall,
    Up {
        base: Option<String>,
        no_wslconfig: bool,
    },
    Wslconfig(WslconfigAction),
    Wait {
        timeout: Duration,
    },
    Doctor,
    /// The arguments for `docker.exe`, untouched.
    Docker(Vec<OsString>),
    CliSetup,
    Skills(SkillsAction),
    /// One setting, or all of them when no key is given.
    ConfigGet(Option<ConfigKey>),
    ConfigSet(ConfigChange),
    /// Hidden: `start` and `stop` are the user's commands.
    ProxyStop,
    /// Hidden: runs the pipe proxy in the foreground.
    ProxyServe {
        trace: bool,
        /// Diagnostic only: without message mode clients cannot half-close.
        byte_mode: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    DefaultBase,
    Base(String),
    /// `--rootfs` with `--sha256`: a local file.
    File {
        path: PathBuf,
        sha256: String,
    },
    /// `--rootfs https://...` with `--sha256`.
    Url {
        url: String,
        sha256: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsAction {
    Status,
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WslconfigAction {
    Status,
    Apply,
    Revert,
}

/// A setting `docklet config` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    IdleStop,
    Watchers,
    Wslconfig,
    VmMemory,
}

impl ConfigKey {
    pub const ALL: [Self; 4] = [
        Self::IdleStop,
        Self::Watchers,
        Self::Wslconfig,
        Self::VmMemory,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::IdleStop => settings::IDLE_STOP_KEY,
            Self::Watchers => settings::WATCHERS_KEY,
            Self::Wslconfig => settings::WSLCONFIG_KEY,
            Self::VmMemory => settings::VM_MEMORY_KEY,
        }
    }

    fn parse(name: &str) -> Result<Self, Failure> {
        Self::ALL
            .into_iter()
            .find(|key| key.name() == name)
            .ok_or_else(|| {
                let known: Vec<&str> = Self::ALL.iter().map(|key| key.name()).collect();
                Failure::usage(format!(
                    "unknown setting `{name}`; the settings are: {}",
                    known.join(", ")
                ))
            })
    }
}

/// A setting together with the value it is to get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChange {
    IdleStop(IdleStop),
    Watchers(Watchers),
    Wslconfig(WslconfigMode),
    /// `None` is `off`.
    VmMemory(Option<MemorySize>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// `--json` was given.
    pub json: bool,
    /// Name of the command for the JSON output; `None` when the command
    /// line names no command Docklet knows.
    pub name: Option<&'static str>,
    pub command: Result<Command, Failure>,
}

/// Parses everything after the program name.
///
/// `--json` is accepted anywhere, with one exception: what follows the word
/// `docker` belongs to Docker, which has `--json` flags of its own.
pub fn parse(args: Vec<OsString>) -> Invocation {
    let mut json = false;
    let mut words: Vec<String> = Vec::new();
    let mut docker_args: Vec<OsString> = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--json" {
            json = true;
            continue;
        }
        let passthrough = words.is_empty() && arg == "docker";
        words.push(arg.to_string_lossy().into_owned());
        if passthrough {
            docker_args = args.collect();
            break;
        }
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();

    let (name, command) = match words.as_slice() {
        [] | ["help" | "-h" | "--help"] => ("help", Ok(Command::Help)),
        ["version" | "-V" | "--version"] => ("version", Ok(Command::Version)),
        ["bases"] => ("bases", Ok(Command::Bases)),
        ["base"] => ("base", Ok(Command::Base)),
        ["base", "set", options @ ..] => ("base set", base_set(options)),
        ["wslconfig", "status"] => (
            "wslconfig status",
            Ok(Command::Wslconfig(WslconfigAction::Status)),
        ),
        ["wslconfig", "apply"] => (
            "wslconfig apply",
            Ok(Command::Wslconfig(WslconfigAction::Apply)),
        ),
        ["wslconfig", "revert"] => (
            "wslconfig revert",
            Ok(Command::Wslconfig(WslconfigAction::Revert)),
        ),
        ["status"] => ("status", Ok(Command::Status)),
        ["install", options @ ..] => ("install", install(options)),
        ["start"] => ("start", Ok(Command::Start)),
        ["stop"] => ("stop", Ok(Command::Stop)),
        ["proxy", "stop"] => ("proxy stop", Ok(Command::ProxyStop)),
        ["proxy", options @ ..] => ("proxy", proxy(options)),
        ["uninstall", "--yes"] => ("uninstall", Ok(Command::Uninstall)),
        ["uninstall"] => (
            "uninstall",
            Err(Failure::new(
                ErrorCode::ConfirmationRequired,
                "`uninstall` deletes every container, image and volume in the docklet distro; \
                 pass --yes to confirm",
            )
            .hint("docklet uninstall --yes")),
        ),
        ["cli", "setup"] => ("cli setup", Ok(Command::CliSetup)),
        ["skills", "status"] => ("skills status", Ok(Command::Skills(SkillsAction::Status))),
        ["skills", "install"] => ("skills install", Ok(Command::Skills(SkillsAction::Install))),
        ["skills", "uninstall"] => (
            "skills uninstall",
            Ok(Command::Skills(SkillsAction::Uninstall)),
        ),
        ["config", "get", options @ ..] => ("config get", config_get(options)),
        ["config", "set", options @ ..] => ("config set", config_set(options)),
        ["up", options @ ..] => ("up", up(options)),
        ["wait", options @ ..] => ("wait", wait(options)),
        ["doctor"] => ("doctor", Ok(Command::Doctor)),
        ["docker"] => ("docker", Ok(Command::Docker(docker_args))),
        _ => {
            return Invocation {
                json,
                name: None,
                command: Err(Failure::usage(format!(
                    "unknown command `{}`",
                    words.join(" ")
                ))),
            };
        }
    };
    Invocation {
        json,
        name: Some(name),
        command,
    }
}

pub const INSTALL_USAGE: &str = "usage: docklet install [--base NAME | --rootfs FILE|URL --sha256 HASH] \
     [--no-wslconfig]";
const UP_USAGE: &str = "usage: docklet up [--base NAME] [--no-wslconfig]";

/// Options that take a value, and flags, in any order, each at most once.
/// `None` when anything else is there.
fn options<'a>(
    words: &[&'a str],
    with_value: &[&str],
    flags: &[&str],
) -> Option<Vec<(&'a str, Option<&'a str>)>> {
    let mut found: Vec<(&str, Option<&str>)> = Vec::new();
    let mut words = words.iter();
    while let Some(&word) = words.next() {
        if found.iter().any(|(name, _)| *name == word) {
            return None;
        }
        if with_value.contains(&word) {
            found.push((word, Some(*words.next()?)));
        } else if flags.contains(&word) {
            found.push((word, None));
        } else {
            return None;
        }
    }
    Some(found)
}

fn value<'a>(found: &[(&str, Option<&'a str>)], name: &str) -> Option<&'a str> {
    found
        .iter()
        .find(|(option, _)| *option == name)
        .and_then(|(_, value)| *value)
}

fn has(found: &[(&str, Option<&str>)], name: &str) -> bool {
    found.iter().any(|(option, _)| *option == name)
}

fn install(words: &[&str]) -> Result<Command, Failure> {
    let usage = || Failure::usage(INSTALL_USAGE);
    let found =
        options(words, &["--base", "--rootfs", "--sha256"], &[NO_WSLCONFIG]).ok_or_else(usage)?;
    let source = match (
        value(&found, "--base"),
        value(&found, "--rootfs"),
        value(&found, "--sha256"),
    ) {
        (None, None, None) => InstallSource::DefaultBase,
        (Some(base), None, None) => InstallSource::Base(base.to_string()),
        (None, Some(rootfs), Some(sha256)) => rootfs_source(rootfs, sha256)?,
        (None, Some(_), None) => {
            return Err(Failure::usage(
                "--rootfs needs --sha256 HASH, the SHA-256 the file must have",
            ));
        }
        _ => return Err(usage()),
    };
    Ok(Command::Install {
        source,
        no_wslconfig: has(&found, NO_WSLCONFIG),
    })
}

/// A rootfs of the user's: a path, or an `https://` URL, and the hash it
/// must have.
fn rootfs_source(rootfs: &str, sha256: &str) -> Result<InstallSource, Failure> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Failure::usage(format!(
            "--sha256 takes the 64 hex digits of a SHA-256; got `{sha256}`"
        )));
    }
    let sha256 = sha256.to_ascii_lowercase();
    let lower = rootfs.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(InstallSource::Url {
            url: rootfs.to_string(),
            sha256,
        });
    }
    if lower.contains("://") {
        return Err(Failure::usage(format!(
            "--rootfs takes a file or an https:// URL; got `{rootfs}`"
        )));
    }
    Ok(InstallSource::File {
        path: PathBuf::from(rootfs),
        sha256,
    })
}

fn up(words: &[&str]) -> Result<Command, Failure> {
    let found =
        options(words, &["--base"], &[NO_WSLCONFIG]).ok_or_else(|| Failure::usage(UP_USAGE))?;
    Ok(Command::Up {
        base: value(&found, "--base").map(str::to_string),
        no_wslconfig: has(&found, NO_WSLCONFIG),
    })
}

/// `base set NAME [--yes]`. Without `--yes` the command still runs, to say
/// what the reinstall would delete, and then fails.
fn base_set(words: &[&str]) -> Result<Command, Failure> {
    let usage = || Failure::usage("usage: docklet base set <name> --yes");
    let (names, flags): (Vec<&str>, Vec<&str>) =
        words.iter().partition(|word| !word.starts_with('-'));
    let [name] = names.as_slice() else {
        return Err(usage());
    };
    let confirmed = match flags.as_slice() {
        [] => false,
        ["--yes"] => true,
        _ => return Err(usage()),
    };
    Ok(Command::BaseSet {
        name: name.to_string(),
        confirmed,
    })
}

fn wait(options: &[&str]) -> Result<Command, Failure> {
    let timeout = match options {
        [] => Some(DEFAULT_WAIT),
        ["--timeout", seconds] => seconds.parse().ok().map(Duration::from_secs),
        _ => None,
    };
    timeout
        .map(|timeout| Command::Wait { timeout })
        .ok_or_else(|| Failure::usage("usage: docklet wait [--timeout SECONDS]"))
}

fn config_get(options: &[&str]) -> Result<Command, Failure> {
    match options {
        [] => Ok(Command::ConfigGet(None)),
        [key] => Ok(Command::ConfigGet(Some(ConfigKey::parse(key)?))),
        _ => Err(Failure::usage("usage: docklet config get [<key>]")),
    }
}

fn config_set(options: &[&str]) -> Result<Command, Failure> {
    let [key, value] = options else {
        return Err(Failure::usage("usage: docklet config set <key> <value>"));
    };
    let change = match ConfigKey::parse(key)? {
        ConfigKey::IdleStop => IdleStop::parse(value)
            .map(ConfigChange::IdleStop)
            .ok_or_else(|| {
                Failure::usage(format!(
                    "{} takes a number of minutes from 1 to {}, or `off`; got `{value}`",
                    settings::IDLE_STOP_KEY,
                    IdleStop::MAX_MINUTES
                ))
            })?,
        ConfigKey::Watchers => Watchers::parse(value)
            .map(ConfigChange::Watchers)
            .ok_or_else(|| {
                Failure::usage(format!(
                    "{} takes `yes` or `no`; got `{value}`",
                    settings::WATCHERS_KEY
                ))
            })?,
        ConfigKey::Wslconfig => WslconfigMode::parse(value)
            .map(ConfigChange::Wslconfig)
            .ok_or_else(|| {
                Failure::usage(format!(
                    "{} takes `light` or `keep`; got `{value}`",
                    settings::WSLCONFIG_KEY
                ))
            })?,
        ConfigKey::VmMemory => MemorySize::parse_setting(value)
            .map(ConfigChange::VmMemory)
            .ok_or_else(|| {
                Failure::usage(format!(
                    "{} takes a size from 1GB to 1024GB, such as `4GB` or `3072MB`, or `off`; \
                     got `{value}`",
                    settings::VM_MEMORY_KEY
                ))
            })?,
    };
    Ok(Command::ConfigSet(change))
}

fn proxy(options: &[&str]) -> Result<Command, Failure> {
    let mut trace = false;
    let mut byte_mode = false;
    for option in options {
        match *option {
            "--trace" => trace = true,
            "--byte" => byte_mode = true,
            other => return Err(Failure::usage(format!("unknown proxy option `{other}`"))),
        }
    }
    Ok(Command::ProxyServe { trace, byte_mode })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::COMMANDS;

    fn parse_line(line: &str) -> Invocation {
        parse(line.split_whitespace().map(OsString::from).collect())
    }

    /// The command of a line that must parse, without `--json`.
    fn command(line: &str) -> Command {
        let invocation = parse_line(line);
        assert!(!invocation.json, "{line}");
        invocation
            .command
            .unwrap_or_else(|failure| panic!("`{line}` did not parse: {failure:?}"))
    }

    /// The failure of a line that must not parse.
    fn failure(line: &str) -> Failure {
        parse_line(line)
            .command
            .expect_err(&format!("`{line}` parsed"))
    }

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_is_help() {
        let invocation = parse(Vec::new());
        assert_eq!(invocation.command, Ok(Command::Help));
        assert_eq!(invocation.name, Some("help"));
        assert!(!invocation.json);
    }

    #[test]
    fn help_and_version_have_their_short_forms() {
        for line in ["help", "-h", "--help"] {
            assert_eq!(command(line), Command::Help);
            assert_eq!(parse_line(line).name, Some("help"));
        }
        for line in ["version", "-V", "--version"] {
            assert_eq!(command(line), Command::Version);
            assert_eq!(parse_line(line).name, Some("version"));
        }
    }

    #[test]
    fn simple_commands() {
        assert_eq!(command("bases"), Command::Bases);
        assert_eq!(command("status"), Command::Status);
        assert_eq!(command("start"), Command::Start);
        assert_eq!(command("stop"), Command::Stop);
        assert_eq!(command("doctor"), Command::Doctor);
        assert_eq!(command("cli setup"), Command::CliSetup);
    }

    const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn install(source: InstallSource) -> Command {
        Command::Install {
            source,
            no_wslconfig: false,
        }
    }

    #[test]
    fn install_sources() {
        assert_eq!(command("install"), install(InstallSource::DefaultBase));
        assert_eq!(
            command("install --base ubuntu-26.04"),
            install(InstallSource::Base("ubuntu-26.04".to_string()))
        );
        assert_eq!(
            command(&format!("install --rootfs C:\\r.tar.gz --sha256 {HASH}")),
            install(InstallSource::File {
                path: PathBuf::from("C:\\r.tar.gz"),
                sha256: HASH.to_string()
            })
        );
        // In any order; the hash in any case, kept in lower case.
        assert_eq!(
            command(&format!(
                "install --sha256 {} --rootfs https://example.com/r.tar.gz",
                HASH.to_ascii_uppercase()
            )),
            install(InstallSource::Url {
                url: "https://example.com/r.tar.gz".to_string(),
                sha256: HASH.to_string()
            })
        );
    }

    #[test]
    fn install_and_up_can_leave_wslconfig_alone() {
        assert_eq!(
            command("install --no-wslconfig --base alpine"),
            Command::Install {
                source: InstallSource::Base("alpine".to_string()),
                no_wslconfig: true
            }
        );
        assert_eq!(
            command("up --no-wslconfig"),
            Command::Up {
                base: None,
                no_wslconfig: true
            }
        );
        assert_eq!(
            command("up --base debian-13 --no-wslconfig"),
            Command::Up {
                base: Some("debian-13".to_string()),
                no_wslconfig: true
            }
        );
    }

    #[test]
    fn install_rejects_anything_else_with_its_usage() {
        for line in [
            "install --base",
            "install --rootfs",
            "install debian-13",
            "install --base a --rootfs b --sha256 c",
            "install --base a b",
            "install --yes",
            "install --base a --base b",
            "install --no-wslconfig --no-wslconfig",
            "install --sha256 abc",
        ] {
            let failure = failure(line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(
                failure.message,
                "usage: docklet install [--base NAME | --rootfs FILE|URL --sha256 HASH] \
                 [--no-wslconfig]",
                "{line}"
            );
            assert_eq!(parse_line(line).name, Some("install"));
        }
    }

    #[test]
    fn a_rootfs_of_ones_own_needs_its_hash() {
        assert_eq!(
            failure("install --rootfs C:\\r.tar.gz").message,
            "--rootfs needs --sha256 HASH, the SHA-256 the file must have"
        );
        for hash in ["abc", &HASH[1..], &format!("{}g", &HASH[1..])] {
            let line = format!("install --rootfs r.tar.gz --sha256 {hash}");
            assert_eq!(failure(&line).code, ErrorCode::Usage, "{line}");
            assert!(failure(&line).message.contains("64 hex digits"), "{line}");
        }
        let plain_http = format!("install --rootfs http://example.com/r.tar.gz --sha256 {HASH}");
        assert!(failure(&plain_http).message.contains("https://"));
    }

    #[test]
    fn base_shows_and_base_set_needs_a_name() {
        assert_eq!(command("base"), Command::Base);
        assert_eq!(
            command("base set alpine --yes"),
            Command::BaseSet {
                name: "alpine".to_string(),
                confirmed: true
            }
        );
        assert_eq!(
            command("base set --yes ubuntu-26.04"),
            Command::BaseSet {
                name: "ubuntu-26.04".to_string(),
                confirmed: true
            }
        );
        // Without --yes it parses, to report what would be lost, unconfirmed.
        assert_eq!(
            command("base set debian-13"),
            Command::BaseSet {
                name: "debian-13".to_string(),
                confirmed: false
            }
        );
        assert_eq!(parse_line("base set alpine").name, Some("base set"));
        for line in [
            "base set",
            "base set a b",
            "base set a --force",
            "base set a --yes --yes",
        ] {
            let failure = failure(line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(failure.message, "usage: docklet base set <name> --yes");
        }
        assert_eq!(failure("base now").message, "unknown command `base now`");
    }

    #[test]
    fn wslconfig_actions() {
        for (line, action) in [
            ("wslconfig status", WslconfigAction::Status),
            ("wslconfig apply", WslconfigAction::Apply),
            ("wslconfig revert", WslconfigAction::Revert),
        ] {
            assert_eq!(command(line), Command::Wslconfig(action));
            assert_eq!(parse_line(line).name, Some(line));
        }
        assert_eq!(failure("wslconfig").message, "unknown command `wslconfig`");
        assert_eq!(
            failure("wslconfig edit").message,
            "unknown command `wslconfig edit`"
        );
    }

    #[test]
    fn config_set_takes_the_wslconfig_settings() {
        assert_eq!(
            command("config set wslconfig keep"),
            Command::ConfigSet(ConfigChange::Wslconfig(WslconfigMode::Keep))
        );
        assert_eq!(
            command("config set wslconfig light"),
            Command::ConfigSet(ConfigChange::Wslconfig(WslconfigMode::Light))
        );
        assert_eq!(
            command("config set vm-memory 4GB"),
            Command::ConfigSet(ConfigChange::VmMemory(MemorySize::parse("4GB")))
        );
        assert_eq!(
            command("config set vm-memory off"),
            Command::ConfigSet(ConfigChange::VmMemory(None))
        );
        assert_eq!(
            failure("config set wslconfig off").message,
            "wslconfig takes `light` or `keep`; got `off`"
        );
        for value in ["4", "512MB", "4G", "lots"] {
            let failure = failure(&format!("config set vm-memory {value}"));
            assert_eq!(failure.code, ErrorCode::Usage);
            assert!(
                failure.message.starts_with("vm-memory takes a size"),
                "{value}"
            );
        }
    }

    #[test]
    fn uninstall_needs_yes_and_says_so() {
        assert_eq!(command("uninstall --yes"), Command::Uninstall);
        let failure = failure("uninstall");
        assert_eq!(failure.code, ErrorCode::ConfirmationRequired);
        assert_eq!(failure.code.exit_code(), 2);
        assert_eq!(failure.hint, Some("docklet uninstall --yes"));
        // The text the E2E workflow has always seen.
        assert_eq!(
            failure.message,
            "`uninstall` deletes every container, image and volume in the docklet distro; \
             pass --yes to confirm"
        );
        assert_eq!(parse_line("uninstall").name, Some("uninstall"));
    }

    /// A command line for the entry with its required positionals filled
    /// in, and without `--yes`.
    fn without_yes(entry: &crate::catalog::Entry) -> String {
        let mut line = entry.name.to_string();
        for arg in entry.args.iter().filter(|arg| arg.required) {
            match arg.name {
                "--yes" => {}
                "name" => line.push_str(" debian-13"),
                "key" => line.push_str(" idle-stop"),
                "value" => line.push_str(" 5"),
                other => line.push_str(&format!(" {other}")),
            }
        }
        line
    }

    /// Deleting data needs `--yes`: `uninstall` is refused when the line is
    /// parsed, `base set` parses unconfirmed and is refused before it
    /// deletes anything (it first says what would be lost).
    #[test]
    fn nothing_that_deletes_data_parses_as_confirmed_without_yes() {
        for entry in COMMANDS.iter().filter(|entry| entry.deletes_data) {
            for line in [without_yes(entry), format!("{} --json", without_yes(entry))] {
                match parse_line(&line).command {
                    Err(failure) => {
                        assert_eq!(failure.code, ErrorCode::ConfirmationRequired, "{line}");
                        assert!(failure.message.contains("--yes"));
                    }
                    Ok(Command::BaseSet { confirmed, .. }) => assert!(!confirmed, "{line}"),
                    Ok(other) => panic!("`{line}` parsed as {other:?}"),
                }
            }
        }
    }

    #[test]
    fn skills_actions() {
        assert_eq!(
            command("skills status"),
            Command::Skills(SkillsAction::Status)
        );
        assert_eq!(
            command("skills install"),
            Command::Skills(SkillsAction::Install)
        );
        assert_eq!(
            command("skills uninstall"),
            Command::Skills(SkillsAction::Uninstall)
        );
        assert_eq!(parse_line("skills install").name, Some("skills install"));
        assert_eq!(
            failure("skills remove").message,
            "unknown command `skills remove`"
        );
        assert_eq!(failure("skills").message, "unknown command `skills`");
    }

    #[test]
    fn config_get_takes_an_optional_key() {
        assert_eq!(command("config get"), Command::ConfigGet(None));
        assert_eq!(
            command("config get idle-stop"),
            Command::ConfigGet(Some(ConfigKey::IdleStop))
        );
        assert_eq!(parse_line("config get idle-stop").name, Some("config get"));
        let unknown = failure("config get memory");
        assert_eq!(unknown.code, ErrorCode::Usage);
        assert_eq!(
            unknown.message,
            "unknown setting `memory`; the settings are: idle-stop, watchers-keep-awake, \
             wslconfig, vm-memory"
        );
        assert_eq!(
            failure("config get idle-stop now").message,
            "usage: docklet config get [<key>]"
        );
        assert_eq!(
            command("config get watchers-keep-awake"),
            Command::ConfigGet(Some(ConfigKey::Watchers))
        );
    }

    #[test]
    fn config_set_watchers_takes_yes_or_no() {
        assert_eq!(
            command("config set watchers-keep-awake yes"),
            Command::ConfigSet(ConfigChange::Watchers(Watchers::KeepAwake))
        );
        assert_eq!(
            command("config set watchers-keep-awake no"),
            Command::ConfigSet(ConfigChange::Watchers(Watchers::Ignored))
        );
        for value in ["on", "off", "true", "5"] {
            let line = format!("config set watchers-keep-awake {value}");
            let failure = failure(&line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(
                failure.message,
                format!("watchers-keep-awake takes `yes` or `no`; got `{value}`")
            );
        }
    }

    #[test]
    fn config_set_takes_minutes_or_off() {
        assert_eq!(
            command("config set idle-stop 5"),
            Command::ConfigSet(ConfigChange::IdleStop(IdleStop::After(5)))
        );
        assert_eq!(
            command("config set idle-stop off"),
            Command::ConfigSet(ConfigChange::IdleStop(IdleStop::Off))
        );
        assert_eq!(
            command("config set idle-stop 10080"),
            Command::ConfigSet(ConfigChange::IdleStop(IdleStop::After(10080)))
        );
        assert_eq!(
            parse_line("config set idle-stop 5").name,
            Some("config set")
        );
    }

    #[test]
    fn config_set_rejects_values_that_are_not_minutes() {
        // Seconds are not offered: the shortest idle time is a minute.
        for value in ["0", "10081", "-5", "1.5", "30s", "5m", "on", "never"] {
            let line = format!("config set idle-stop {value}");
            let failure = failure(&line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(
                failure.message,
                format!(
                    "idle-stop takes a number of minutes from 1 to 10080, or `off`; got `{value}`"
                )
            );
            assert_eq!(parse_line(&line).name, Some("config set"));
        }
    }

    #[test]
    fn config_set_needs_a_known_key_and_one_value() {
        for line in [
            "config set",
            "config set idle-stop",
            "config set idle-stop 5 6",
        ] {
            let failure = failure(line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(failure.message, "usage: docklet config set <key> <value>");
        }
        assert_eq!(
            failure("config set memory 4G").message,
            "unknown setting `memory`; the settings are: idle-stop, watchers-keep-awake, \
             wslconfig, vm-memory"
        );
        // `config` alone names no command.
        let bare = parse_line("config");
        assert_eq!(bare.name, None);
        assert_eq!(
            bare.command.unwrap_err().message,
            "unknown command `config`"
        );
        assert_eq!(
            failure("config unset idle-stop").message,
            "unknown command `config unset idle-stop`"
        );
    }

    #[test]
    fn every_setting_has_a_name_that_parses_back() {
        for key in ConfigKey::ALL {
            assert_eq!(ConfigKey::parse(key.name()), Ok(key));
        }
    }

    #[test]
    fn up_takes_an_optional_base() {
        assert_eq!(
            command("up"),
            Command::Up {
                base: None,
                no_wslconfig: false
            }
        );
        assert_eq!(
            command("up --base debian-13"),
            Command::Up {
                base: Some("debian-13".to_string()),
                no_wslconfig: false
            }
        );
        for line in ["up --base", "up --rootfs x", "up now", "up --base a b"] {
            let failure = failure(line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(
                failure.message,
                "usage: docklet up [--base NAME] [--no-wslconfig]"
            );
        }
    }

    #[test]
    fn wait_defaults_to_two_minutes() {
        assert_eq!(
            command("wait"),
            Command::Wait {
                timeout: Duration::from_secs(120)
            }
        );
        assert_eq!(
            command("wait --timeout 5"),
            Command::Wait {
                timeout: Duration::from_secs(5)
            }
        );
        assert_eq!(
            command("wait --timeout 0"),
            Command::Wait {
                timeout: Duration::ZERO
            }
        );
    }

    #[test]
    fn wait_rejects_a_timeout_that_is_not_whole_seconds() {
        for line in [
            "wait --timeout",
            "wait --timeout soon",
            "wait --timeout -1",
            "wait --timeout 1.5",
            "wait --timeout 5s",
            "wait --timeout 99999999999999999999999",
            "wait 5",
            "wait --timeout 5 6",
        ] {
            let failure = failure(line);
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(failure.message, "usage: docklet wait [--timeout SECONDS]");
        }
    }

    #[test]
    fn docker_takes_everything_after_it_unchanged() {
        assert_eq!(command("docker"), Command::Docker(Vec::new()));
        assert_eq!(
            command("docker compose up -d"),
            Command::Docker(os(&["compose", "up", "-d"]))
        );
        assert_eq!(
            command("docker run -i --rm alpine cat"),
            Command::Docker(os(&["run", "-i", "--rm", "alpine", "cat"]))
        );
        // Words that mean something to Docklet mean nothing here.
        assert_eq!(command("docker --help"), Command::Docker(os(&["--help"])));
        assert_eq!(
            command("docker docker uninstall --yes"),
            Command::Docker(os(&["docker", "uninstall", "--yes"]))
        );
        assert_eq!(parse_line("docker ps").name, Some("docker"));
    }

    #[test]
    fn docker_arguments_with_spaces_and_empty_ones_survive() {
        let args = vec![
            OsString::from("docker"),
            OsString::from("run"),
            OsString::from("--name"),
            OsString::from("my app"),
            OsString::from(""),
        ];
        let invocation = parse(args);
        assert_eq!(
            invocation.command,
            Ok(Command::Docker(os(&["run", "--name", "my app", ""])))
        );
    }

    #[test]
    fn json_after_docker_belongs_to_docker() {
        let after = parse_line("docker ps --json");
        assert!(!after.json);
        assert_eq!(after.command, Ok(Command::Docker(os(&["ps", "--json"]))));

        let before = parse_line("--json docker ps --json");
        assert!(before.json);
        assert_eq!(before.command, Ok(Command::Docker(os(&["ps", "--json"]))));
    }

    #[test]
    fn json_is_accepted_anywhere() {
        for line in [
            "--json status",
            "status --json",
            "--json install --base debian-13",
            "install --json --base debian-13",
            "install --base --json debian-13",
            "install --base debian-13 --json",
            "cli --json setup",
            "skills status --json",
            "--json",
            "help --json",
            "--json --help",
            "wait --timeout 3 --json",
            "--json --json status",
        ] {
            let with = parse_line(line);
            assert!(with.json, "{line}");
            let without: Vec<OsString> = line
                .split_whitespace()
                .filter(|word| *word != "--json")
                .map(OsString::from)
                .collect();
            let plain = parse(without);
            assert!(!plain.json);
            assert_eq!(with.command, plain.command, "{line}");
            assert_eq!(with.name, plain.name, "{line}");
            assert!(with.command.is_ok(), "{line}");
        }
    }

    #[test]
    fn json_is_remembered_when_the_command_line_is_wrong() {
        let invocation = parse_line("frobnicate --json");
        assert!(invocation.json);
        assert_eq!(invocation.name, None);
        let failure = invocation.command.unwrap_err();
        assert_eq!(failure.code, ErrorCode::Usage);
        // `--json` is not part of the command that was not understood.
        assert_eq!(failure.message, "unknown command `frobnicate`");
    }

    #[test]
    fn unknown_commands_are_quoted_in_full() {
        for (line, message) in [
            ("frobnicate", "unknown command `frobnicate`"),
            ("status now", "unknown command `status now`"),
            ("start --wait", "unknown command `start --wait`"),
            (
                "uninstall --yes please",
                "unknown command `uninstall --yes please`",
            ),
            ("uninstall --force", "unknown command `uninstall --force`"),
            ("cli", "unknown command `cli`"),
            ("cli install", "unknown command `cli install`"),
            ("doctor --fix", "unknown command `doctor --fix`"),
            ("Status", "unknown command `Status`"),
        ] {
            let invocation = parse_line(line);
            let failure = invocation.command.unwrap_err();
            assert_eq!(failure.code, ErrorCode::Usage, "{line}");
            assert_eq!(failure.message, message);
            assert_eq!(failure.hint, Some("docklet help"));
            assert_eq!(invocation.name, None, "{line}");
        }
    }

    #[test]
    fn the_hidden_proxy_commands_parse_as_before() {
        assert_eq!(command("proxy stop"), Command::ProxyStop);
        assert_eq!(parse_line("proxy stop").name, Some("proxy stop"));
        assert_eq!(
            command("proxy"),
            Command::ProxyServe {
                trace: false,
                byte_mode: false
            }
        );
        assert_eq!(
            command("proxy --trace"),
            Command::ProxyServe {
                trace: true,
                byte_mode: false
            }
        );
        assert_eq!(
            command("proxy --byte --trace"),
            Command::ProxyServe {
                trace: true,
                byte_mode: true
            }
        );
        let failure = failure("proxy --verbose");
        assert_eq!(failure.code, ErrorCode::Usage);
        assert_eq!(failure.message, "unknown proxy option `--verbose`");
        // `stop` is only a command in first place, as before.
        assert_eq!(
            failure_message("proxy --trace stop"),
            "unknown proxy option `stop`"
        );
    }

    fn failure_message(line: &str) -> String {
        failure(line).message
    }

    #[test]
    fn every_command_in_the_catalogue_parses() {
        for entry in COMMANDS {
            // With their required arguments, where they have some.
            let line = match entry.name {
                "uninstall" => "uninstall --yes".to_string(),
                _ => without_yes(entry).replace("base set debian-13", "base set debian-13 --yes"),
            };
            let invocation = parse_line(&line);
            assert!(invocation.command.is_ok(), "{line}");
            // The name in the JSON output is the name in the catalogue.
            assert_eq!(invocation.name, Some(entry.name));
            for alias in entry.aliases {
                assert_eq!(parse_line(alias).name, Some(entry.name));
            }
        }
    }

    #[test]
    fn every_option_in_the_catalogue_parses() {
        for entry in COMMANDS {
            let base = without_yes(entry);
            for arg in entry.args.iter().filter(|arg| arg.name.starts_with('-')) {
                let line = match (arg.name, arg.value) {
                    // The two only come together.
                    ("--rootfs" | "--sha256", _) => {
                        format!("{base} --rootfs r.tar.gz --sha256 {HASH}")
                    }
                    (_, Some("SECONDS")) => format!("{base} {} 5", arg.name),
                    (_, Some(_)) => format!("{base} {} value", arg.name),
                    (_, None) => format!("{base} {}", arg.name),
                };
                assert!(parse_line(&line).command.is_ok(), "{line}");
            }
        }
    }
}
