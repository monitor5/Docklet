//! `docklet-native.exe`: Docklet's window and tray icon.
//!
//! Flags: `--tray` starts with only the tray icon, `--light` skips dark
//! mode, `--proxy` serves the Docker pipe instead of showing anything.
//! `--demo` runs on a made-up engine (`--logs` opens a log at once,
//! `--rate N` makes that log print N lines a second instead of 10,000,
//! `--screen NAME` starts in the state of a message screen).

#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() {
    use docklet_native::app::{Start, channel, run_engine};
    use docklet_native::demo::{Demo, DemoOptions};
    use docklet_native::{Button, Page, RunOptions, Screen, UiEvent, UiHandle, UiUpdate};

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| arguments.iter().any(|argument| argument == flag);
    let value = |flag: &str| {
        arguments
            .iter()
            .position(|argument| argument == flag)
            .and_then(|index| arguments.get(index + 1))
            .cloned()
    };

    // Before the single instance is claimed: the proxy runs next to the
    // app, neither is one nor blocks one.
    if has("--proxy") {
        if let Err(err) = docklet_native::proxy::serve() {
            eprintln!("docklet proxy: {err}");
            std::process::exit(1);
        }
        return;
    }

    // A second start shows the first one's window and ends here.
    let Some(_instance) = docklet_native::instance::claim() else {
        return;
    };
    let window = RunOptions {
        start_hidden: has("--tray"),
        light: has("--light"),
    };
    let handle = UiHandle::new();
    let (sink, messages) = channel(&handle);
    if has("--demo") {
        let options = DemoOptions {
            screen: value("--screen"),
            lines_per_second: value("--rate").and_then(|rate| rate.parse().ok()),
        };
        let start = Start {
            window,
            notice: Some("Demo mode: the data is made up and nothing touches Docker.".into()),
            open_first_log: has("--logs"),
        };
        return run_engine(Demo::start(sink, &options), handle, messages, start);
    }

    match docklet_app::App::new(sink) {
        Ok(app) => {
            let start = Start {
                window,
                ..Start::default()
            };
            run_engine(app, handle, messages, start);
        }
        // Only when Windows gives the process no threads for its runtime.
        Err(err) => {
            handle.send(UiUpdate::Page(Page::message(Screen {
                title: "Docklet could not start".into(),
                detail: Some(err.to_string()),
                primary: Some(Button::new("quit", "Quit")),
                ..Screen::default()
            })));
            let quit = handle.clone();
            docklet_native::run(handle, RunOptions::default(), move |event| {
                if event == (UiEvent::Button { key: "quit".into() }) {
                    quit.send(UiUpdate::Quit);
                }
            });
        }
    }
}

#[cfg(not(windows))]
fn main() {}
