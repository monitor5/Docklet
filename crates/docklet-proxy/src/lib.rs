//! Named pipe server that relays each Docker API connection to a child
//! process (in production: `wsl.exe ... docklet-relay`).

// The pipe name lives in `docklet-core`, the one crate every client shares.
pub use docklet_core::engine::{ENGINE_DOCKER_HOST, ENGINE_PIPE};

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
