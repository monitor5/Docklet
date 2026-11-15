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
    pub about: &'static str,
    /// Text for the row an argument after the first gets in the help.
    pub help: Option<&'static str>,
}

impl Arg {
    fn kind(&self) -> &'static str {
        match (self.name.starts_with('-'), self.value) {
            (false, _) => "positional",
            (true, Some(_)) => "option",
            (true, None) => "flag",
        }
    }

    /// As written in a usage line: `[--base NAME]`, `--yes`, `<args...>`.
    fn syntax(&self) -> String {
        let core = match (self.name.starts_with('-'), self.value) {
            (false, _) => format!("<{}...>", self.name),
            (true, Some(value)) => format!("{} {value}", self.name),
            (true, None) => self.name.to_string(),
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
const DOCKER: &str = "Docker client";
const SKILL: &str = "Agent skill";
const OTHER: &str = "Other";

const BASE: Arg = Arg {
    name: "--base",
    value: Some("NAME"),
    required: false,
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
        group: DOCKER,
        name: "docker",
        aliases: &[],
        args: &[Arg {
            name: "args",
            value: None,
            required: true,
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
        // The first argument shares the command's row; each further one
        // gets a row of its own, lined up under the first.
        let mut usage = vec![match entry.args.first() {
            Some(arg) => format!("{} {}", entry.name, arg.syntax()),
            None => std::iter::once(entry.name)
                .chain(entry.aliases.iter().copied())
                .collect::<Vec<_>>()
                .join(", "),
        }];
        let indent = " ".repeat(entry.name.len() + 1);
        for arg in entry.args.iter().skip(1) {
            usage.push(format!("{indent}{}", arg.syntax()));
        }
        let notes = entry.args.iter().filter_map(|arg| arg.help);
        let about: Vec<&str> = entry.about.iter().copied().chain(notes).collect();
        push_rows(&mut text, &usage, &about);
    }
    text.push_str("\nOptions:\n");
    for option in OPTIONS {
        push_rows(&mut text, &[option.name.to_string()], option.about);
    }
    text
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
    Object::new()
        .set("docklet_version", env!("CARGO_PKG_VERSION"))
        .set("usage", USAGE)
        .set("options", options)
        .set("commands", commands)
        .set("exit_codes", exit_codes)
        .set("error_codes", error_codes)
        .into()
}

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
        ] {
            assert!(help.contains(expected), "{expected:?} not in:\n{help}");
        }
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
            let first = match entry.args.first() {
                Some(arg) => format!("{} {}", entry.name, arg.syntax()),
                None => entry.name.to_string(),
            };
            assert!(first.len() < USAGE_WIDTH, "{first}");
            for arg in entry.args.iter().skip(1) {
                let width = entry.name.len() + 1 + arg.syntax().len();
                assert!(width < USAGE_WIDTH, "{} {}", entry.name, arg.syntax());
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
