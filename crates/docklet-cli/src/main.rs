//! `docklet` command line interface.
//!
//! The CLI and the tray GUI are two front ends over the same crates. Argument
//! parsing is done by hand to keep the binary small.

use std::path::PathBuf;
use std::process::ExitCode;

use docklet_core::skills::{self, InstallOutcome, SkillStatus};

const HELP: &str = "\
Docklet - Docker Engine on WSL2 without Docker Desktop

Usage: docklet <command>

Commands:
  version             Print the Docklet version
  skills status       Show whether the Docklet agent skill is registered
  skills install      Register or update the skill for AI agents (Claude Code, Codex)
  skills uninstall    Remove the skill
  help                Show this help

Options:
  -h, --help          Show this help
  -V, --version       Print the Docklet version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        ["version" | "-V" | "--version"] => {
            println!("docklet {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["skills", action] => run_skills(action),
        _ => {
            eprintln!("docklet: unknown command `{}`\n", args.join(" "));
            eprint!("{HELP}");
            ExitCode::from(2)
        }
    }
}

fn run_skills(action: &str) -> ExitCode {
    let Some(home) = home_dir() else {
        eprintln!("docklet: cannot find the user's home directory");
        return ExitCode::FAILURE;
    };
    let targets = skills::default_targets(&home);
    let mut failed = false;
    for target in &targets {
        let path = target.skill_path();
        let line = match action {
            "status" => match target.status() {
                SkillStatus::AgentNotFound => "agent not found".to_string(),
                SkillStatus::NotInstalled => "not registered".to_string(),
                SkillStatus::Outdated => format!("outdated ({})", path.display()),
                SkillStatus::Current => format!("registered ({})", path.display()),
            },
            "install" => match target.install() {
                Ok(InstallOutcome::AgentNotFound) => "agent not found, skipped".to_string(),
                Ok(InstallOutcome::Installed) => format!("registered ({})", path.display()),
                Ok(InstallOutcome::Updated) => format!("updated ({})", path.display()),
                Ok(InstallOutcome::AlreadyCurrent) => "already up to date".to_string(),
                Err(err) => {
                    failed = true;
                    format!("failed: {err}")
                }
            },
            "uninstall" => match target.uninstall() {
                Ok(true) => "removed".to_string(),
                Ok(false) => "not registered".to_string(),
                Err(err) => {
                    failed = true;
                    format!("failed: {err}")
                }
            },
            _ => {
                eprintln!("docklet: unknown command `skills {action}`\n");
                eprint!("{HELP}");
                return ExitCode::from(2);
            }
        };
        println!("{}: {line}", target.agent);
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn home_dir() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .find_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
