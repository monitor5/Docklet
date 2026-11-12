//! Standalone pipe proxy, used by the M0 half-close spike and the M2 E2E
//! script before the proxy is embedded in the desktop app.
//!
//! ```text
//! docklet-proxy [--pipe NAME] [--distro NAME] [--byte] [--trace] [-- PROGRAM ARGS...]
//! ```
//! Without `-- PROGRAM`, each connection is relayed through
//! `wsl.exe -d <distro> ... docklet-relay` to the Docker socket.

#[cfg(windows)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use docklet_proxy::{PipeProxy, PipeProxyConfig, RelayCommand};

    let mut pipe_name = String::from(r"\\.\pipe\docklet_engine");
    let mut distro = String::from("docklet");
    let mut message_mode = true;
    let mut trace = false;
    let mut custom: Option<RelayCommand> = None;

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--pipe") => pipe_name = required(&mut args, "--pipe"),
            Some("--distro") => distro = required(&mut args, "--distro"),
            Some("--byte") => message_mode = false,
            Some("--trace") => trace = true,
            Some("--") => {
                let program = args.next().unwrap_or_else(|| usage("missing PROGRAM"));
                custom = Some(RelayCommand {
                    program,
                    args: args.by_ref().collect(),
                });
            }
            _ => usage(&format!("unknown argument {arg:?}")),
        }
    }

    let config = PipeProxyConfig {
        pipe_name,
        message_mode,
        relay: custom.unwrap_or_else(|| RelayCommand::wsl_relay(&distro)),
        trace,
    };
    eprintln!(
        "listening on {} ({} mode)",
        config.pipe_name,
        if message_mode { "message" } else { "byte" }
    );
    PipeProxy::bind(config)?.run().await
}

#[cfg(windows)]
fn required(args: &mut impl Iterator<Item = std::ffi::OsString>, flag: &str) -> String {
    args.next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| usage(&format!("{flag} needs a value")))
}

#[cfg(windows)]
fn usage(problem: &str) -> ! {
    eprintln!("{problem}");
    eprintln!(
        "usage: docklet-proxy [--pipe NAME] [--distro NAME] [--byte] [--trace] [-- PROGRAM ARGS...]"
    );
    std::process::exit(2)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("docklet-proxy only runs on Windows");
    std::process::exit(1);
}
