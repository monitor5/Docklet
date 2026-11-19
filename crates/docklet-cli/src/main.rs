//! `docklet` command line interface.
//!
//! The CLI and the tray GUI are two front ends over the same crates. Its
//! callers are people and, more and more, programs: every command takes
//! `--json`, and with it stdout carries exactly one JSON object.
//!
//! - `args`: the command line into a `Command`, touching nothing.
//! - `catalog`: the command table behind `help` and `help --json`.
//! - `commands`: what each command does.
//! - `failure`: error codes and the JSON envelope.
//! - `json`: the JSON writer.
//! - `wslconfig`: what the commands do to `.wslconfig`, and how they say so.

mod args;
mod catalog;
mod commands;
mod docker;
mod failure;
mod json;
mod proxy;
mod status;
mod wslconfig;

use std::process::ExitCode;

use commands::{Outcome, Ui};

fn main() -> ExitCode {
    let invocation = args::parse(std::env::args_os().skip(1).collect());
    let ui = Ui {
        json: invocation.json,
    };
    let outcome = invocation
        .command
        .and_then(|command| commands::run(&ui, command));
    match outcome {
        Ok(Outcome::Data(data)) => {
            if ui.json {
                println!(
                    "{}",
                    failure::success_envelope(invocation.name, data).render()
                );
            }
            ExitCode::SUCCESS
        }
        // Docker's exit code is passed on whole; `ExitCode` only holds a byte.
        Ok(Outcome::Exit(code)) => std::process::exit(code),
        Err(failure) => {
            if ui.json {
                println!(
                    "{}",
                    failure::failure_envelope(invocation.name, &failure).render()
                );
            } else {
                eprint!("{}", failure.text());
            }
            ExitCode::from(failure.code.exit_code())
        }
    }
}
