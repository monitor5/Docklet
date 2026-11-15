//! `docklet-native.exe`: the native window.
//!
//! Flags: `--demo` feeds made-up data (`--logs` opens a log at once,
//! `--screen NAME` shows a message screen), `--tray` starts with only the
//! tray icon, `--light` skips dark mode.

#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() {
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

    // A second start shows the first one's window and ends here.
    let Some(_instance) = docklet_native::instance::claim() else {
        return;
    };
    let options = RunOptions {
        start_hidden: has("--tray"),
        light: has("--light"),
    };
    let handle = UiHandle::new();
    if has("--demo") {
        let demo_options = DemoOptions {
            logs: has("--logs"),
            screen: value("--screen"),
        };
        let mut demo = Demo::start(handle.clone(), &demo_options);
        docklet_native::run(handle, options, move |event| demo.on_event(event));
        return;
    }

    handle.send(UiUpdate::Page(Page::Message(Screen {
        title: "Docklet".into(),
        body: "This window is not connected to the engine yet. Start it with --demo to see it with made-up data.".into(),
        primary: Some(Button::new("quit", "Quit")),
        ..Screen::default()
    })));
    let quit = handle.clone();
    docklet_native::run(handle, options, move |event| {
        if event == (UiEvent::Button { key: "quit".into() }) {
            quit.send(UiUpdate::Quit);
        }
    });
}

#[cfg(not(windows))]
fn main() {}
