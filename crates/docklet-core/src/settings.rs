//! The user's settings: a small text file in Docklet's data directory.
//!
//! ```text
//! # Docklet settings. Change them with `docklet config set`.
//! idle-stop = 10
//! ```
//!
//! One `key = value` per line; a line that starts with `#` is a comment. The
//! file has one setting today, so it is parsed by hand rather than with a
//! TOML or JSON dependency. Lines this version does not understand are kept
//! when the file is rewritten: a newer Docklet may have written them.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::idle::IdlePolicy;
use crate::wsl_output;

/// Name of the settings file inside the data directory
/// (`%LOCALAPPDATA%\Docklet`).
pub const FILE_NAME: &str = "config.txt";

/// Key of the idle-stop setting, in the file and on the command line.
pub const IDLE_STOP_KEY: &str = "idle-stop";

/// Environment variable that gives the pipe proxy an idle time in seconds,
/// whatever the setting says. It exists for the end-to-end tests: the setting
/// counts minutes, and a test cannot wait that long.
pub const IDLE_SECONDS_ENV: &str = "DOCKLET_IDLE_STOP_SECONDS";

const HEADER: &str = "# Docklet settings. Change them with `docklet config set`.";

/// Whether the pipe proxy stops an engine nobody uses, and after how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleStop {
    /// The engine runs until it is stopped.
    Off,
    /// Stop the engine after this many minutes without a connection or a
    /// running container; start it again when a client connects.
    After(u32),
}

impl IdleStop {
    pub const DEFAULT: Self = Self::After(10);
    /// A week. Anything longer is `off` in practice.
    pub const MAX_MINUTES: u32 = 7 * 24 * 60;

    /// Parses `off` or a whole number of minutes from 1 to `MAX_MINUTES`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("off") {
            return Some(Self::Off);
        }
        // `u32::from_str` takes a leading `+`; a setting does not.
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let minutes: u32 = value.parse().ok()?;
        (1..=Self::MAX_MINUTES)
            .contains(&minutes)
            .then_some(Self::After(minutes))
    }

    pub fn minutes(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::After(minutes) => Some(minutes),
        }
    }
}

impl Default for IdleStop {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The value as `parse` takes it: `off` or the minutes.
impl fmt::Display for IdleStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::After(minutes) => write!(f, "{minutes}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    pub idle_stop: IdleStop,
}

impl Settings {
    /// Reads the settings out of the file's text. Never fails: a setting
    /// that is missing or cannot be understood keeps its default, and each
    /// line that could not be understood is named in the second value.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut problems = Vec::new();
        let text = text.trim_start_matches('\u{feff}');
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                problems.push(format!("line {number}: `{line}` is not `key = value`"));
                continue;
            };
            // Keys of other versions are not this version's to judge.
            if !is_idle_stop(key) {
                continue;
            }
            match IdleStop::parse(value) {
                // The last one wins if the key was written twice by hand.
                Some(idle_stop) => settings.idle_stop = idle_stop,
                None => problems.push(format!(
                    "line {number}: {IDLE_STOP_KEY} is `{}`, which is neither `off` nor a \
                     number of minutes from 1 to {}",
                    value.trim(),
                    IdleStop::MAX_MINUTES
                )),
            }
        }
        (settings, problems)
    }
}

fn is_idle_stop(key: &str) -> bool {
    key.trim().eq_ignore_ascii_case(IDLE_STOP_KEY)
}

/// The file's text with the idle-stop setting changed to `value`. Comments
/// and other lines stay as they are.
pub fn with_idle_stop(text: &str, value: IdleStop) -> String {
    let setting = format!("{IDLE_STOP_KEY} = {value}");
    let mut out = String::new();
    let mut written = false;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let is_setting = !line.trim_start().starts_with('#')
            && line
                .split_once('=')
                .is_some_and(|(key, _)| is_idle_stop(key));
        if is_setting {
            // Written once: a second line for the same key is dropped.
            if !written {
                out.push_str(&setting);
                out.push('\n');
                written = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !written {
        if out.is_empty() {
            out.push_str(HEADER);
            out.push('\n');
        }
        out.push_str(&setting);
        out.push('\n');
    }
    out
}

/// Where the settings file is, given the data directory.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Loads the settings, with the lines that could not be understood. A file
/// that does not exist is not an error: every setting has a default.
pub fn load(data_dir: &Path) -> io::Result<(Settings, Vec<String>)> {
    Ok(Settings::parse(&read(data_dir)?))
}

/// Changes the idle-stop setting in the file, creating it if needed.
pub fn save_idle_stop(data_dir: &Path, value: IdleStop) -> io::Result<()> {
    // Read first, and give up if that fails: a file that cannot be read
    // must not be replaced by one that holds this setting alone.
    let text = with_idle_stop(&read(data_dir)?, value);
    fs::create_dir_all(data_dir)?;
    fs::write(path(data_dir), text)
}

fn read(data_dir: &Path) -> io::Result<String> {
    match fs::read(path(data_dir)) {
        // Editors may save the file as UTF-16 or with a BOM; `decode`
        // handles both.
        Ok(bytes) => Ok(wsl_output::decode(&bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

/// The idle policy a pipe proxy starts with, or `None` for a proxy that only
/// relays. `seconds_override` is the value of [`IDLE_SECONDS_ENV`]; a whole
/// number of seconds above zero there wins over the setting, anything else is
/// ignored.
pub fn idle_policy(setting: IdleStop, seconds_override: Option<&str>) -> Option<IdlePolicy> {
    let seconds = seconds_override
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    let idle_after = match seconds {
        Some(seconds) => Duration::from_secs(seconds),
        None => Duration::from_secs(u64::from(setting.minutes()?) * 60),
    };
    Some(IdlePolicy::new(idle_after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> IdleStop {
        let (settings, problems) = Settings::parse(text);
        assert!(problems.is_empty(), "{problems:?}");
        settings.idle_stop
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("docklet-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_default_is_ten_minutes() {
        assert_eq!(Settings::default().idle_stop, IdleStop::After(10));
        assert_eq!(parsed(""), IdleStop::After(10));
        assert_eq!(parsed("# only a comment\n\n"), IdleStop::After(10));
    }

    #[test]
    fn idle_stop_values() {
        assert_eq!(IdleStop::parse("off"), Some(IdleStop::Off));
        assert_eq!(IdleStop::parse("OFF"), Some(IdleStop::Off));
        assert_eq!(IdleStop::parse(" 5 "), Some(IdleStop::After(5)));
        assert_eq!(IdleStop::parse("1"), Some(IdleStop::After(1)));
        assert_eq!(IdleStop::parse("10080"), Some(IdleStop::After(10080)));
        for wrong in [
            "",
            "0",
            "10081",
            "-1",
            "+5",
            "1.5",
            "5m",
            "30s",
            "on",
            "never",
            "4294967296",
        ] {
            assert_eq!(IdleStop::parse(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn a_value_is_displayed_the_way_it_is_parsed() {
        for value in [IdleStop::Off, IdleStop::After(1), IdleStop::After(10080)] {
            assert_eq!(IdleStop::parse(&value.to_string()), Some(value));
        }
        assert_eq!(IdleStop::Off.to_string(), "off");
        assert_eq!(IdleStop::After(25).to_string(), "25");
        assert_eq!(IdleStop::Off.minutes(), None);
        assert_eq!(IdleStop::After(25).minutes(), Some(25));
    }

    #[test]
    fn the_file_is_key_value_lines_with_comments() {
        assert_eq!(parsed("idle-stop = 5\n"), IdleStop::After(5));
        assert_eq!(parsed("idle-stop=off"), IdleStop::Off);
        assert_eq!(parsed("  Idle-Stop  =  30  \r\n"), IdleStop::After(30));
        assert_eq!(
            parsed("# Docklet settings\r\n\r\nidle-stop = 45\r\n"),
            IdleStop::After(45)
        );
        // A commented-out setting is a comment.
        assert_eq!(parsed("# idle-stop = off\n"), IdleStop::After(10));
        // A BOM in front of the first line does not hide it.
        assert_eq!(parsed("\u{feff}idle-stop = off\n"), IdleStop::Off);
    }

    #[test]
    fn the_last_of_two_lines_for_one_key_wins() {
        assert_eq!(parsed("idle-stop = 5\nidle-stop = off\n"), IdleStop::Off);
    }

    #[test]
    fn keys_of_other_versions_are_ignored_without_complaint() {
        assert_eq!(
            parsed("kubernetes = on\nidle-stop = 7\nmemory = 4G\n"),
            IdleStop::After(7)
        );
    }

    #[test]
    fn a_value_that_cannot_be_understood_keeps_the_default_and_is_named() {
        let (settings, problems) = Settings::parse("# x\nidle-stop = 5m\n");
        assert_eq!(settings.idle_stop, IdleStop::DEFAULT);
        assert_eq!(
            problems,
            [
                "line 2: idle-stop is `5m`, which is neither `off` nor a number of minutes \
              from 1 to 10080"
            ]
        );
        // A good line after a bad one still counts.
        let (settings, problems) = Settings::parse("idle-stop = 0\nidle-stop = 3\n");
        assert_eq!(settings.idle_stop, IdleStop::After(3));
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_line_that_is_not_a_setting_is_named() {
        let (settings, problems) = Settings::parse("idle-stop 5\nidle-stop = 20\n");
        assert_eq!(settings.idle_stop, IdleStop::After(20));
        assert_eq!(problems, ["line 1: `idle-stop 5` is not `key = value`"]);
    }

    #[test]
    fn writing_into_an_empty_file_adds_the_header() {
        assert_eq!(
            with_idle_stop("", IdleStop::After(5)),
            "# Docklet settings. Change them with `docklet config set`.\nidle-stop = 5\n"
        );
    }

    #[test]
    fn writing_replaces_the_line_and_keeps_the_rest() {
        let before = "# mine\r\nkubernetes = on\r\n  idle-stop=5\r\n\r\nmemory = 4G";
        assert_eq!(
            with_idle_stop(before, IdleStop::Off),
            "# mine\nkubernetes = on\nidle-stop = off\n\nmemory = 4G\n"
        );
    }

    #[test]
    fn writing_appends_when_the_key_is_absent_and_drops_duplicates() {
        assert_eq!(
            with_idle_stop("# idle-stop = 3\nmemory = 4G\n", IdleStop::After(15)),
            "# idle-stop = 3\nmemory = 4G\nidle-stop = 15\n"
        );
        assert_eq!(
            with_idle_stop("idle-stop = 1\nx = y\nidle-stop = 2\n", IdleStop::After(9)),
            "idle-stop = 9\nx = y\n"
        );
    }

    #[test]
    fn what_is_written_is_what_is_read() {
        let texts = ["", "# c\n", "idle-stop = bogus\n", "a = b\nidle-stop = 3"];
        for text in texts {
            for value in [IdleStop::Off, IdleStop::After(1), IdleStop::After(600)] {
                let written = with_idle_stop(text, value);
                assert_eq!(parsed(&written), value, "{text:?}");
                // Writing the same value again changes nothing.
                assert_eq!(with_idle_stop(&written, value), written);
            }
        }
    }

    #[test]
    fn a_missing_file_loads_the_defaults() {
        let dir = temp_dir("missing");
        let (settings, problems) = load(&dir).unwrap();
        assert_eq!(settings, Settings::default());
        assert!(problems.is_empty());
        // Loading creates nothing.
        assert!(!dir.exists());
    }

    #[test]
    fn saving_creates_the_directory_and_loading_reads_it_back() {
        let dir = temp_dir("roundtrip").join("Docklet");
        save_idle_stop(&dir, IdleStop::Off).unwrap();
        assert_eq!(load(&dir).unwrap().0.idle_stop, IdleStop::Off);
        save_idle_stop(&dir, IdleStop::After(25)).unwrap();
        assert_eq!(load(&dir).unwrap().0.idle_stop, IdleStop::After(25));
        assert_eq!(
            fs::read_to_string(path(&dir)).unwrap(),
            "# Docklet settings. Change them with `docklet config set`.\nidle-stop = 25\n"
        );
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_file_saved_as_utf16_is_read() {
        let dir = temp_dir("utf16");
        fs::create_dir_all(&dir).unwrap();
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(
            "idle-stop = off\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        fs::write(path(&dir), bytes).unwrap();
        assert_eq!(load(&dir).unwrap().0.idle_stop, IdleStop::Off);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_policy_follows_the_setting() {
        assert_eq!(idle_policy(IdleStop::Off, None), None);
        let policy = idle_policy(IdleStop::After(10), None).unwrap();
        assert_eq!(policy.idle_after(), Duration::from_secs(600));
        assert_eq!(policy.check_every(), Duration::from_secs(60));
        let week = idle_policy(IdleStop::After(IdleStop::MAX_MINUTES), None).unwrap();
        assert_eq!(week.idle_after(), Duration::from_secs(7 * 24 * 60 * 60));
    }

    #[test]
    fn seconds_from_the_environment_win_over_the_setting() {
        for setting in [IdleStop::Off, IdleStop::After(10)] {
            let policy = idle_policy(setting, Some("20")).unwrap();
            assert_eq!(policy.idle_after(), Duration::from_secs(20));
            assert_eq!(policy.check_every(), Duration::from_secs(20));
        }
        // Anything that is not a number of seconds above zero is ignored.
        for ignored in ["", "0", "-5", "ten", "1.5", "5s"] {
            assert_eq!(
                idle_policy(IdleStop::Off, Some(ignored)),
                None,
                "{ignored:?}"
            );
            assert_eq!(
                idle_policy(IdleStop::After(10), Some(ignored)),
                idle_policy(IdleStop::After(10), None),
                "{ignored:?}"
            );
        }
    }
}
