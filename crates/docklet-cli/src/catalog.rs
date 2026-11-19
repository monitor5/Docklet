//! The command table.
//!
//! `docklet help` and `docklet help --json` are both rendered from
//! [`COMMANDS`], so the text people read and the catalogue programs read
//! cannot drift apart. The hidden `proxy` commands are deliberately absent.

use crate::failure::ErrorCode;
use crate::json::{Json, Object};

pub struct Arg {
    /// `--base`, `--yes`, or a bare word for a positional argument.
    pub name: &'static str,
    /// Placeholder of the value an option takes; `None` for a flag.
    pub value: Option<&'static str>,
    pub required: bool,
    /// A positional argument that takes every word that follows.
    pub variadic: bool,
    pub about: &'static str,
    /// Text for the row an argument after the first gets in the help.
    pub help: Option<&'static str>,
}

impl Arg {
    fn is_positional(&self) -> bool {
        !self.name.starts_with('-')
    }

    fn kind(&self) -> &'static str {
        match (self.is_positional(), self.value) {
            (true, _) => "positional",
            (false, Some(_)) => "option",
            (false, None) => "flag",
        }
    }

    /// As written in a usage line: `[--base NAME]`, `--yes`, `<key>`,
    /// `<args...>`.
    fn syntax(&self) -> String {
        let core = match (self.is_positional(), self.value) {
            (true, _) if self.variadic => format!("<{}...>", self.name),
            (true, _) => format!("<{}>", self.name),
            (false, Some(value)) => format!("{} {value}", self.name),
            (false, None) => self.name.to_string(),
        };
        if self.required {
            core
        } else {
            format!("[{core}]")
        }
    }
}

pub struct Entry {
    pub group: &'static str,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static [Arg],
    /// The description, in the lines it is wrapped to in the help text. The
    /// catalogue joins them into one.
    pub about: &'static [&'static str],
    pub deletes_data: bool,
    /// May run for more than a few seconds; callers should allow minutes.
    pub long_running: bool,
}

const ENGINE: &str = "Engine";
const SETTINGS: &str = "Settings";
const DOCKER: &str = "Docker client";
const SKILL: &str = "Agent skill";
const OTHER: &str = "Other";

const BASE: Arg = Arg {
    name: "--base",
    value: Some("NAME"),
    required: false,
    variadic: false,
    about: "Linux base of the distro, one of `docklet bases`; the first is the default",
    help: None,
};

pub const COMMANDS: &[Entry] = &[
    Entry {
        group: ENGINE,
        name: "up",
        aliases: &[],
        args: &[BASE],
        about: &[
            "Install if needed, start, and set up the Docker CLI:",
            "one command to a usable engine, safe to repeat",
        ],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: ENGINE,
        name: "status",
        aliases: &[],
        args: &[],
        about: &["Show the engine state"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: ENGINE,
        name: "install",
        aliases: &[],
        args: &[
            BASE,
            Arg {
                name: "--rootfs",
                value: Some("FILE"),
                required: false,
                variadic: false,
                about: "Install from a local rootfs.tar.gz instead of downloading a base",
                help: Some("...or install it from a local rootfs.tar.gz"),
            },
        ],
        about: &["Download and install the docklet distro"],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: ENGINE,
        name: "bases",
        aliases: &[],
        args: &[],
        about: &["List the Linux bases available for `install --base`"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: ENGINE,
        name: "start",
        aliases: &[],
        args: &[],
        about: &["Start the engine and the Docker pipe, wait until ready"],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: ENGINE,
        name: "stop",
        aliases: &[],
        args: &[],
        about: &["Stop the engine and the Docker pipe"],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: ENGINE,
        name: "wait",
        aliases: &[],
        args: &[Arg {
            name: "--timeout",
            value: Some("SECONDS"),
            required: false,
            variadic: false,
            about: "How long to wait; 120 when not given",
            help: None,
        }],
        about: &["Wait until the engine is usable (default: 120 s)"],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: ENGINE,
        name: "doctor",
        aliases: &[],
        args: &[],
        about: &["Check the setup and name the fix for each problem"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: ENGINE,
        name: "uninstall",
        aliases: &[],
        args: &[Arg {
            name: "--yes",
            value: None,
            required: true,
            variadic: false,
            about: "Confirms the deletion; without it nothing is deleted",
            help: None,
        }],
        about: &[
            "Delete the docklet distro with all containers,",
            "images and volumes (other distros are untouched)",
        ],
        deletes_data: true,
        long_running: true,
    },
    Entry {
        group: SETTINGS,
        name: "config get",
        aliases: &[],
        args: &[Arg {
            name: "key",
            value: None,
            required: false,
            variadic: false,
            about: "The setting to show, one of `settings` in this catalogue; all when not given",
            help: None,
        }],
        about: &["Show the settings, or the one named"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: SETTINGS,
        name: "config set",
        aliases: &[],
        args: &[
            Arg {
                name: "key",
                value: None,
                required: true,
                variadic: false,
                about: "The setting to change, one of `settings` in this catalogue",
                help: None,
            },
            Arg {
                name: "value",
                value: None,
                required: true,
                variadic: false,
                about: "Its new value; `settings` in this catalogue says what each one takes",
                help: None,
            },
        ],
        about: &[
            "Change a setting. idle-stop <minutes|off>: stop the",
            "engine after that long without use (default: 10);",
            "it starts again when Docker is used.",
            "watchers-keep-awake <yes|no>: whether a connection",
            "that carries nothing counts as use (default: no)",
        ],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: DOCKER,
        name: "docker",
        aliases: &[],
        args: &[Arg {
            name: "args",
            value: None,
            required: true,
            variadic: true,
            about: "Passed to docker.exe unchanged, with its stdin, stdout, stderr and exit code",
            help: None,
        }],
        about: &[
            "Run docker against the Docklet engine, for example",
            "`docklet docker compose up -d`",
        ],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: DOCKER,
        name: "cli setup",
        aliases: &[],
        args: &[],
        about: &[
            "Install the Windows docker CLI and Compose if missing",
            "and create the `docklet` Docker context",
        ],
        deletes_data: false,
        long_running: true,
    },
    Entry {
        group: SKILL,
        name: "skills status",
        aliases: &[],
        args: &[],
        about: &["Show whether the Docklet agent skill is registered"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: SKILL,
        name: "skills install",
        aliases: &[],
        args: &[],
        about: &["Register or update the skill (Claude Code, Codex)"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: SKILL,
        name: "skills uninstall",
        aliases: &[],
        args: &[],
        about: &["Remove the skill"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: OTHER,
        name: "version",
        aliases: &["-V", "--version"],
        args: &[],
        about: &["Print the Docklet version"],
        deletes_data: false,
        long_running: false,
    },
    Entry {
        group: OTHER,
        name: "help",
        aliases: &["-h", "--help"],
        args: &[],
        about: &["Show this help"],
        deletes_data: false,
        long_running: false,
    },
];

/// Options every command accepts.
pub struct GlobalOption {
    pub name: &'static str,
    pub about: &'static [&'static str],
}

pub const OPTIONS: &[GlobalOption] = &[GlobalOption {
    name: "--json",
    about: &[
        "Print one JSON object on stdout instead of text;",
        "accepted anywhere, by every command",
    ],
}];

const USAGE: &str = "docklet <command> [--json]";

/// Width of the left column of the help text.
const USAGE_WIDTH: usize = 26;

/// `docklet help`.
pub fn help_text() -> String {
    let mut text =
        format!("Docklet - Docker Engine on WSL2 without Docker Desktop\n\nUsage: {USAGE}\n");
    let mut group = "";
    for entry in COMMANDS {
        if entry.group != group {
            group = entry.group;
            text.push_str(&format!("\n{group}:\n"));
        }
        let notes = entry.args.iter().filter_map(|arg| arg.help);
        let about: Vec<&str> = entry.about.iter().copied().chain(notes).collect();
        push_rows(&mut text, &usage_rows(entry), &about);
    }
    text.push_str("\nOptions:\n");
    for option in OPTIONS {
        push_rows(&mut text, &[option.name.to_string()], option.about);
    }
    text
}

/// The left column of a command's help: its name and arguments.
///
/// The first argument shares the command's row, and so do the positional
/// ones right after it, which read as one phrase (`config set <key>
/// <value>`). Each further option gets a row of its own, lined up under the
/// first.
fn usage_rows(entry: &Entry) -> Vec<String> {
    let Some(first) = entry.args.first() else {
        let names = std::iter::once(entry.name).chain(entry.aliases.iter().copied());
        return vec![names.collect::<Vec<_>>().join(", ")];
    };
    let mut rows = vec![format!("{} {}", entry.name, first.syntax())];
    let indent = " ".repeat(entry.name.len() + 1);
    for arg in entry.args.iter().skip(1) {
        if arg.is_positional() && rows.len() == 1 {
            rows[0].push(' ');
            rows[0].push_str(&arg.syntax());
        } else {
            rows.push(format!("{indent}{}", arg.syntax()));
        }
    }
    rows
}

/// Writes two columns side by side, as many rows as the longer one has.
fn push_rows(text: &mut String, left: &[String], right: &[&str]) {
    for row in 0..left.len().max(right.len()) {
        let usage = left.get(row).map_or("", String::as_str);
        let about = right.get(row).copied().unwrap_or("");
        let line = format!("  {usage:<USAGE_WIDTH$}{about}");
        text.push_str(line.trim_end());
        text.push('\n');
    }
}

/// `docklet help --json`.
pub fn catalogue() -> Json {
    let commands: Vec<Json> = COMMANDS.iter().map(command).collect();
    let options: Vec<Json> = OPTIONS
        .iter()
        .map(|option| {
            Object::new()
                .set("name", option.name)
                .set("description", option.about.join(" "))
                .into()
        })
        .collect();
    let exit_codes: Vec<Json> = [
        (0u64, "success"),
        (1, "the command failed"),
        (2, "the command line is wrong; nothing was done"),
    ]
    .into_iter()
    .map(|(code, meaning)| {
        Object::new()
            .set("code", code)
            .set("meaning", meaning)
            .into()
    })
    .collect();
    let error_codes: Vec<Json> = ErrorCode::ALL
        .into_iter()
        .map(|code| {
            Object::new()
                .set("code", code.as_str())
                .set("exit_code", u64::from(code.exit_code()))
                .set("description", code.meaning())
                .into()
        })
        .collect();
    let settings: Vec<Json> = SETTINGS_LIST
        .iter()
        .map(|setting| {
            Object::new()
                .set("key", setting.key)
                .set("values", setting.values)
                .set("default", setting.default)
                .set("description", setting.about)
                .into()
        })
        .collect();
    Object::new()
        .set("docklet_version", env!("CARGO_PKG_VERSION"))
        .set("usage", USAGE)
        .set("options", options)
        .set("commands", commands)
        .set("settings", settings)
        .set("exit_codes", exit_codes)
        .set("error_codes", error_codes)
        .into()
}

/// A setting of `docklet config`, as the catalogue describes it.
pub struct Setting {
    pub key: &'static str,
    /// What `config set` takes for it.
    pub values: &'static str,
    pub default: &'static str,
    pub about: &'static str,
}

pub const SETTINGS_LIST: &[Setting] = &[
    Setting {
        key: "idle-stop",
        values: "<minutes>|off",
        default: "10",
        about: "Minutes without Docker traffic or a running container after which the \
                engine is stopped to free its memory; the next Docker command starts it \
                again. `off` keeps it running until `docklet stop`. A change applies from \
                the next `docklet start`",
    },
    Setting {
        key: "watchers-keep-awake",
        values: "yes|no",
        default: "no",
        about: "Whether a connection that is open but carries nothing, such as Docker \
                Desktop, an IDE or `docker events` watching the engine, keeps it from \
                going to sleep. With `no` such a connection is closed when the engine \
                goes to sleep, and when its program reconnects only to watch, it waits \
                without waking the engine. A change applies from the next `docklet start`",
    },
];

fn command(entry: &Entry) -> Json {
    let usage = std::iter::once(format!("docklet {}", entry.name))
        .chain(entry.args.iter().map(Arg::syntax))
        .collect::<Vec<_>>()
        .join(" ");
    let arguments: Vec<Json> = entry
        .args
        .iter()
        .map(|arg| {
            Object::new()
                .set("name", arg.name)
                .set("kind", arg.kind())
                .set_some("value", arg.value)
                .set("required", arg.required)
                .set("description", arg.about)
                .into()
        })
        .collect();
    Object::new()
        .set("name", entry.name)
        .set("usage", usage)
        .set("aliases", entry.aliases.to_vec())
        .set("arguments", arguments)
        .set("description", entry.about.join(" "))
        .set("group", entry.group)
        .set("deletes_data", entry.deletes_data)
        .set("long_running", entry.long_running)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use docklet_core::skills::SKILL_MD;

    /// The help text as it was before the table existed.
    const OLD_HELP: &str = "\
Docklet - Docker Engine on WSL2 without Docker Desktop

Usage: docklet <command>

Engine:
  status                    Show the engine state
  install [--base NAME]     Download and install the docklet distro
          [--rootfs FILE]   ...or install it from a local rootfs.tar.gz
  bases                     List the Linux bases available for `install --base`
  start                     Start the engine and the Docker pipe, wait until ready
  stop                      Stop the engine and the Docker pipe
  uninstall --yes           Delete the docklet distro with all containers,
                            images and volumes (other distros are untouched)

Docker client:
  cli setup                 Install the Windows docker CLI and Compose if missing
                            and create the `docklet` Docker context

Agent skill:
  skills status             Show whether the Docklet agent skill is registered
  skills install            Register or update the skill (Claude Code, Codex)
  skills uninstall          Remove the skill

Other:
  version, -V, --version    Print the Docklet version
  help, -h, --help          Show this help
";

    fn field<'a>(json: &'a Json, key: &str) -> &'a Json {
        let Json::Object(fields) = json else {
            panic!("not an object: {json:?}");
        };
        &fields
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("no field `{key}`"))
            .1
    }

    fn items(json: &Json) -> &[Json] {
        let Json::List(items) = json else {
            panic!("not a list: {json:?}");
        };
        items
    }

    fn text(json: &Json) -> &str {
        let Json::Text(text) = json else {
            panic!("not text: {json:?}");
        };
        text
    }

    #[test]
    fn every_line_of_the_old_help_is_still_there() {
        let help = help_text();
        let lines: Vec<&str> = help.lines().collect();
        for old in OLD_HELP.lines() {
            // The usage line is the one that had to change: it gained --json.
            if old.starts_with("Usage:") {
                continue;
            }
            assert!(lines.contains(&old), "missing from the help: {old:?}");
        }
        assert!(lines.contains(&"Usage: docklet <command> [--json]"));
    }

    #[test]
    fn old_lines_keep_their_order() {
        let help = help_text();
        let mut from = 0;
        for old in OLD_HELP.lines().filter(|line| !line.is_empty()) {
            if old.starts_with("Usage:") {
                continue;
            }
            let at = help[from..]
                .find(old)
                .unwrap_or_else(|| panic!("out of order: {old:?}"));
            from += at + old.len();
        }
    }

    #[test]
    fn the_new_commands_are_in_the_help() {
        let help = help_text();
        for expected in [
            "  up [--base NAME]          Install if needed",
            "  wait [--timeout SECONDS]  Wait until the engine is usable",
            "  doctor                    Check the setup",
            "  docker <args...>          Run docker against the Docklet engine",
            "  --json                    Print one JSON object on stdout",
            "\nSettings:\n  config get [<key>]        Show the settings, or the one named\n",
            "  config set <key> <value>  Change a setting. idle-stop <minutes|off>: stop the\n",
        ] {
            assert!(help.contains(expected), "{expected:?} not in:\n{help}");
        }
    }

    #[test]
    fn positionals_share_the_first_row_and_options_get_their_own() {
        let rows = |name: &str| {
            let entry = COMMANDS.iter().find(|entry| entry.name == name).unwrap();
            usage_rows(entry)
        };
        assert_eq!(rows("config set"), ["config set <key> <value>"]);
        assert_eq!(rows("config get"), ["config get [<key>]"]);
        assert_eq!(
            rows("install"),
            ["install [--base NAME]", "        [--rootfs FILE]"]
        );
        assert_eq!(rows("version"), ["version, -V, --version"]);
        assert_eq!(rows("status"), ["status"]);
    }

    #[test]
    fn the_catalogue_describes_every_setting() {
        use crate::args::ConfigKey;
        use docklet_core::settings::IdleStop;

        let keys: Vec<&str> = SETTINGS_LIST.iter().map(|setting| setting.key).collect();
        let known: Vec<&str> = ConfigKey::ALL.iter().map(|key| key.name()).collect();
        assert_eq!(keys, known);
        let idle_stop = &SETTINGS_LIST[0];
        assert_eq!(idle_stop.default, IdleStop::DEFAULT.to_string());
        // The help names the same default.
        assert!(help_text().contains(&format!("(default: {})", idle_stop.default)));
        let catalogue = catalogue();
        let listed = items(field(&catalogue, "settings"));
        assert_eq!(listed.len(), SETTINGS_LIST.len());
        assert!(listed[0].render().starts_with(
            r#"{"key":"idle-stop","values":"<minutes>|off","default":"10","description":""#
        ));
        // What the task of this setting hinges on is said where agents read.
        assert!(idle_stop.about.contains("next `docklet start`"));
        let watchers = &SETTINGS_LIST[1];
        assert_eq!(
            watchers.default,
            docklet_core::settings::Watchers::default().to_string()
        );
        assert!(help_text().contains("(default: no)"));
        assert!(listed[1].render().starts_with(
            r#"{"key":"watchers-keep-awake","values":"yes|no","default":"no","description":""#
        ));
        assert!(watchers.about.contains("next `docklet start`"));
    }

    #[test]
    fn help_layout() {
        let help = help_text();
        assert!(help.starts_with("Docklet - "));
        assert!(help.ends_with('\n'));
        assert!(!help.ends_with("\n\n"));
        for line in help.lines() {
            assert_eq!(line, line.trim_end(), "trailing space: {line:?}");
            assert!(line.is_ascii(), "{line:?}");
        }
        // No usage may run into the description column.
        for entry in COMMANDS {
            for row in usage_rows(entry) {
                assert!(row.len() < USAGE_WIDTH, "{row}");
            }
        }
    }

    #[test]
    fn the_hidden_proxy_commands_are_in_neither_form() {
        assert!(!help_text().contains("proxy"));
        // The word itself may appear in a description; a command may not.
        assert!(!catalogue().render().contains("\"name\":\"proxy"));
        assert!(!catalogue().render().contains("docklet proxy"));
        assert!(COMMANDS.iter().all(|entry| !entry.name.contains("proxy")));
    }

    #[test]
    fn the_catalogue_has_one_entry_per_command_in_help_order() {
        let catalogue = catalogue();
        let names: Vec<&str> = items(field(&catalogue, "commands"))
            .iter()
            .map(|command| text(field(command, "name")))
            .collect();
        assert_eq!(
            names,
            [
                "up",
                "status",
                "install",
                "bases",
                "start",
                "stop",
                "wait",
                "doctor",
                "uninstall",
                "config get",
                "config set",
                "docker",
                "cli setup",
                "skills status",
                "skills install",
                "skills uninstall",
                "version",
                "help",
            ]
        );
        // The same names, in the same order, begin a row of the help text.
        let help = help_text();
        let mut from = 0;
        for name in names {
            let at = help[from..]
                .find(&format!("\n  {name}"))
                .unwrap_or_else(|| panic!("`{name}` is not in the help, or out of order"));
            from += at + 1;
        }
    }

    #[test]
    fn catalogue_entry_of_a_command_with_options() {
        let catalogue = catalogue();
        let install = items(field(&catalogue, "commands"))
            .iter()
            .find(|command| text(field(command, "name")) == "install")
            .unwrap();
        assert_eq!(
            install.render(),
            concat!(
                r#"{"name":"install","usage":"docklet install [--base NAME] [--rootfs FILE]","#,
                r#""aliases":[],"arguments":["#,
                r#"{"name":"--base","kind":"option","value":"NAME","required":false,"#,
                r#""description":"Linux base of the distro, one of `docklet bases`; the first is the default"},"#,
                r#"{"name":"--rootfs","kind":"option","value":"FILE","required":false,"#,
                r#""description":"Install from a local rootfs.tar.gz instead of downloading a base"}],"#,
                r#""description":"Download and install the docklet distro","group":"Engine","#,
                r#""deletes_data":false,"long_running":true}"#
            )
        );
    }

    #[test]
    fn wrapped_descriptions_are_one_line_in_the_catalogue() {
        let catalogue = catalogue();
        let uninstall = items(field(&catalogue, "commands"))
            .iter()
            .find(|command| text(field(command, "name")) == "uninstall")
            .unwrap();
        assert_eq!(
            text(field(uninstall, "description")),
            "Delete the docklet distro with all containers, images and volumes \
             (other distros are untouched)"
        );
        assert_eq!(text(field(uninstall, "usage")), "docklet uninstall --yes");
        let argument = &items(field(uninstall, "arguments"))[0];
        assert_eq!(text(field(argument, "kind")), "flag");
        assert_eq!(field(argument, "required"), &Json::Bool(true));
    }

    #[test]
    fn argument_syntax() {
        let docker = COMMANDS
            .iter()
            .find(|entry| entry.name == "docker")
            .unwrap();
        assert_eq!(docker.args[0].syntax(), "<args...>");
        assert_eq!(docker.args[0].kind(), "positional");
        assert_eq!(BASE.syntax(), "[--base NAME]");
        assert_eq!(BASE.kind(), "option");
    }

    #[test]
    fn only_uninstall_deletes_data() {
        let deleting: Vec<&str> = COMMANDS
            .iter()
            .filter(|entry| entry.deletes_data)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(deleting, ["uninstall"]);
        // Whatever deletes data says how it is confirmed.
        for entry in COMMANDS.iter().filter(|entry| entry.deletes_data) {
            assert!(
                entry
                    .args
                    .iter()
                    .any(|arg| arg.name == "--yes" && arg.required)
            );
        }
    }

    #[test]
    fn commands_that_wait_on_the_engine_or_the_network_are_long_running() {
        let long: Vec<&str> = COMMANDS
            .iter()
            .filter(|entry| entry.long_running)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(
            long,
            [
                "up",
                "install",
                "start",
                "stop",
                "wait",
                "uninstall",
                "docker",
                "cli setup"
            ]
        );
    }

    #[test]
    fn the_catalogue_lists_every_error_code_and_the_exit_codes() {
        let catalogue = catalogue();
        let codes: Vec<&str> = items(field(&catalogue, "error_codes"))
            .iter()
            .map(|code| text(field(code, "code")))
            .collect();
        let expected: Vec<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
        assert_eq!(codes, expected);
        assert_eq!(
            field(&catalogue, "exit_codes").render(),
            concat!(
                r#"[{"code":0,"meaning":"success"},"#,
                r#"{"code":1,"meaning":"the command failed"},"#,
                r#"{"code":2,"meaning":"the command line is wrong; nothing was done"}]"#
            )
        );
        assert_eq!(
            field(&catalogue, "options").render(),
            r#"[{"name":"--json","description":"Print one JSON object on stdout instead of text; accepted anywhere, by every command"}]"#
        );
    }

    #[test]
    fn names_and_descriptions_are_filled_in() {
        for entry in COMMANDS {
            assert!(!entry.about.is_empty(), "{}", entry.name);
            assert!(entry.about.iter().all(|line| !line.is_empty()));
            for arg in entry.args {
                assert!(!arg.about.is_empty(), "{} {}", entry.name, arg.name);
            }
        }
    }

    #[test]
    fn the_skill_mentions_every_command() {
        for entry in COMMANDS {
            assert!(
                SKILL_MD.contains(&format!("`docklet {}", entry.name)),
                "skills/docklet/SKILL.md does not mention `docklet {}`",
                entry.name
            );
        }
    }
}
