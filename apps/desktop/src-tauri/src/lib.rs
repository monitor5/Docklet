//! Docklet desktop shell: tray icon, on-demand main window, Tauri commands.
//!
//! The process stays alive in the tray after the window is closed. Closing the
//! window destroys the WebView, so only the Rust core keeps running.

mod commands;
mod state;
mod streams;
mod supervisor;

use docklet_app::WindowState;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Event sent to the UI when the engine was started or stopped from the tray.
/// Its payload is the error message, or null when the request succeeded.
const ENGINE_EVENT: &str = "engine-changed";

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
/// refresh. Errors surface in the window's status area, including the one for
/// a request that the supervisor refused because another was in flight.
fn engine_from_tray(app: &AppHandle, start: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        let result = if start {
            supervisor::start(&app)
        } else {
            supervisor::stop(&app)
        };
        let message = result.err().map(|err| err.to_string());
        let _ = app.emit(ENGINE_EVENT, message);
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
    Some(serve_proxy())
}

/// Serves the pipe like `docklet proxy` does: with idle stop, as the setting
/// shared with the CLI says (`docklet config set idle-stop`).
fn serve_proxy() -> std::io::Result<()> {
    let config = docklet_proxy::PipeProxyConfig {
        pipe_name: docklet_core::engine::ENGINE_PIPE.to_string(),
        message_mode: true,
        relay: docklet_proxy::RelayCommand::wsl_relay(docklet_wsl::DISTRO),
        trace: false,
    };
    let data_dir = docklet_wsl::data_dir()?;
    let engine = docklet_wsl::Engine::system()?;
    docklet_proxy::serve_configured(config, &data_dir, engine)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Registered first, as the plugin requires. A second launch lands here
        // in the running instance and brings its window up instead. Proxy mode
        // never reaches this builder, so it neither is nor blocks an instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window_detached(app);
        }))
        // A page that reloads does not get to stop the logs it followed.
        .on_page_load(|webview, payload| {
            if webview.label() == MAIN_WINDOW && payload.event() == PageLoadEvent::Started {
                let state = webview.state::<state::AppState>();
                state.backend.streams().stop_all_logs();
            }
        })
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
            commands::open_port,
            commands::logs_start,
            commands::logs_stop,
        ])
        .setup(|app| {
            // Before the tray and the window, which are what uses it. It
            // needs the handle, to send Docker's events to the window.
            app.manage(state::AppState::new(app.handle()));
            build_tray(app.handle())?;
            show_main_window(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Docklet application")
        .run(|app, event| match event {
            // Closing the last window requests an exit with no exit code. Keep
            // the core alive in the tray; `AppHandle::exit` (Quit) passes a code.
            RunEvent::ExitRequested { api, code, .. } if code.is_none() => api.prevent_exit(),
            // The window took its WebView along without running the page's
            // cleanup. Nothing stays open without a window: the next list
            // view asks for the event stream again.
            RunEvent::WindowEvent {
                label,
                event: WindowEvent::Destroyed,
                ..
            } if label == MAIN_WINDOW => {
                let state = app.state::<state::AppState>();
                state.backend.streams().set_window(WindowState::Gone);
            }
            _ => {}
        });
}
