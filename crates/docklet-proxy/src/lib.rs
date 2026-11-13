//! Named pipe server that relays each Docker API connection to a child
//! process (in production: `wsl.exe ... docklet-relay`).

/// Named pipe that Docklet exposes to the GUI and the Windows `docker` CLI.
pub const ENGINE_PIPE: &str = r"\\.\pipe\docklet_engine";

/// The same pipe as a Docker host URL.
pub const ENGINE_DOCKER_HOST: &str = "npipe:////./pipe/docklet_engine";

#[cfg(windows)]
pub mod control;
#[cfg(windows)]
mod security;
#[cfg(windows)]
mod server;

#[cfg(windows)]
pub use server::{PipeProxy, PipeProxyConfig, RelayCommand};

/// Runs the proxy until `control::request_stop` is called or the pipe fails.
///
/// Fails immediately if another proxy already owns the pipe.
#[cfg(windows)]
pub fn serve(config: PipeProxyConfig) -> std::io::Result<()> {
    let stop = control::StopSignal::create()?;
    // Exiting the process is the whole shutdown: relays are children of
    // wsl.exe processes that end when the distro is terminated.
    std::thread::spawn(move || {
        stop.wait();
        std::process::exit(0);
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move { PipeProxy::bind(config)?.run().await })
}
