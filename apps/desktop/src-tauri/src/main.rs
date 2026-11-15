// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(result) = docklet_lib::run_proxy_mode() {
        if let Err(err) = result {
            eprintln!("docklet proxy: {err}");
            std::process::exit(1);
        }
        return;
    }
    docklet_lib::run()
}
