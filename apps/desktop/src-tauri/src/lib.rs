//! Docklet desktop shell: tray icon, on-demand main window, Tauri commands.
//!
//! The process stays alive in the tray after the window is closed. Closing the
//! window destroys the WebView, so only the Rust core keeps running.

mod commands;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

const MAIN_WINDOW: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_START: &str = "engine-start";
const MENU_STOP: &str = "engine-stop";
const MENU_QUIT: &str = "quit";

/// Shows the main window, creating it (and its WebView) if it does not exist.
fn show_main_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        window.unminimize()?;
        window.show()?;
        window.set_focus()?;
        return Ok(());
    }
    WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::default())
        .title("Docklet")
        .inner_size(1000.0, 680.0)
        .min_inner_size(720.0, 480.0)
        .build()?;
    Ok(())
}

/// Creating a window inside a synchronous event handler deadlocks on Windows,
/// so tray and menu handlers hand the work to another thread.
fn show_main_window_detached(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(err) = show_main_window(&app) {
            eprintln!("failed to show main window: {err}");
        }
    });
}

/// Starts or stops the engine from the tray, then tells an open window to
/// refresh. Errors surface in the window's status area on its next poll.
fn engine_from_tray(app: &AppHandle, start: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        let result = if start {
            commands::start_engine_blocking().map(|()| commands::watch_events(&app))
        } else {
            commands::stop_engine_blocking()
        };
        let message = result.err().map(|err| err.to_string());
        let _ = app.emit(commands::ENGINE_EVENT, message);
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open Docklet", true, None::<&str>)?;
    let start = MenuItem::with_id(app, MENU_START, "Start engine", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, MENU_STOP, "Stop engine", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &separator, &start, &stop, &separator, &quit])?;

    let mut tray = TrayIconBuilder::with_id("docklet")
        .tooltip("Docklet")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_main_window_detached(app),
            MENU_START => engine_from_tray(app, true),
            MENU_STOP => engine_from_tray(app, false),
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window_detached(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// `docklet-desktop.exe --proxy`: serve the Docker pipe and nothing else.
/// Returns `None` when the process was not started in proxy mode.
pub fn run_proxy_mode() -> Option<std::io::Result<()>> {
    if !std::env::args().any(|arg| arg == "--proxy") {
        return None;
    }
    Some(docklet_proxy::serve(docklet_proxy::PipeProxyConfig {
        pipe_name: docklet_proxy::ENGINE_PIPE.to_string(),
        message_mode: true,
        relay: docklet_proxy::RelayCommand::wsl_relay(docklet_wsl::DISTRO),
        trace: false,
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::engine_status,
            commands::engine_install,
            commands::engine_start,
            commands::engine_stop,
            commands::events_watch,
            commands::containers,
            commands::container_action,
            commands::images,
            commands::image_remove,
            commands::image_pull,
            commands::volumes,
            commands::volume_remove,
            commands::networks,
            commands::network_remove,
            commands::logs_start,
            commands::logs_stop,
        ])
        .setup(|app| {
            build_tray(app.handle())?;
            show_main_window(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Docklet application")
        .run(|_app, event| {
            // Closing the last window requests an exit with no exit code. Keep
            // the core alive in the tray; `AppHandle::exit` (Quit) passes a code.
            if let RunEvent::ExitRequested { api, code, .. } = event
                && code.is_none()
            {
                api.prevent_exit();
            }
        });
}
