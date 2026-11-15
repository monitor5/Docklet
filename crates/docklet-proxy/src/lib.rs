//! Named pipe server that relays each Docker API connection to a child
//! process (in production: `wsl.exe ... docklet-relay`).
//!
//! Optionally the proxy also owns the engine's lifetime: it stops the distro
//! when nothing uses it and starts it again when a client connects (see
//! `IdleStop`). The engine calls come from the caller as `EngineHooks`;
//! `docklet_wsl::Engine` implements them.

// The pipe name lives in `docklet-core`, the one crate every client shares.
pub use docklet_core::engine::{ENGINE_DOCKER_HOST, ENGINE_PIPE};
pub use docklet_core::idle::{EngineHooks, IdlePolicy};

#[cfg(windows)]
pub mod control;
#[cfg(windows)]
mod lifecycle;
#[cfg(windows)]
mod security;
#[cfg(windows)]
mod server;

#[cfg(windows)]
pub use lifecycle::IdleStop;
#[cfg(windows)]
pub use server::{PipeProxy, PipeProxyConfig, RelayCommand};

/// Runs the proxy until `control::request_stop` is called or the pipe fails.
///
/// Fails immediately if another proxy already owns the pipe.
#[cfg(windows)]
pub fn serve(config: PipeProxyConfig) -> std::io::Result<()> {
    serve_with(config, None)
}

/// Like `serve`, and also stops the engine when it has been unused for the
/// policy's idle time and starts it again when a client connects.
///
/// `control::stop()` still ends the proxy, and an engine stopped after that
/// is not woken: nothing listens on the pipe any more.
#[cfg(windows)]
pub fn serve_with_idle_stop(config: PipeProxyConfig, idle: IdleStop) -> std::io::Result<()> {
    serve_with(config, Some(idle))
}

#[cfg(windows)]
fn serve_with(config: PipeProxyConfig, idle: Option<IdleStop>) -> std::io::Result<()> {
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
    runtime.block_on(async move {
        let proxy = PipeProxy::bind(config)?;
        match idle {
            Some(idle) => proxy.with_idle_stop(idle).run().await,
            None => proxy.run().await,
        }
    })
}
