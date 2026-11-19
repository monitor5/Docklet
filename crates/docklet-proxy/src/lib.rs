//! Named pipe server that relays each Docker API connection to a child
//! process (in production: `wsl.exe ... docklet-relay`).
//!
//! Optionally the proxy also owns the engine's lifetime: it stops the distro
//! when nothing uses it and starts it again when a client connects (see
//! `IdleStop`). The engine calls come from the caller as `EngineHooks`;
//! `docklet_wsl::Engine` implements them. `serve_configured` is what the
//! front ends call: it reads the user's idle-stop setting and serves
//! accordingly.

// The pipe name lives in `docklet-core`, the one crate every client shares.
pub use docklet_core::engine::{ENGINE_DOCKER_HOST, ENGINE_PIPE};
pub use docklet_core::idle::{EngineHooks, IdlePolicy};

#[cfg(windows)]
pub mod control;
#[cfg(windows)]
mod lifecycle;
mod ping;
#[cfg(windows)]
mod security;
#[cfg(windows)]
mod server;

#[cfg(windows)]
pub use lifecycle::IdleStop;
pub use ping::{PING_TIMEOUT, ping};
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

/// Serves the way the user's settings say: with idle stop after the time set
/// in `data_dir`'s settings file, or as a plain relay when that is `off`.
/// `hooks` are only used with idle stop.
///
/// The settings are read once, here. A proxy that is already running keeps
/// what it started with.
#[cfg(windows)]
pub fn serve_configured(
    config: PipeProxyConfig,
    data_dir: &std::path::Path,
    hooks: impl EngineHooks + 'static,
) -> std::io::Result<()> {
    use docklet_core::settings;

    // A settings file that cannot be read must not keep Docker from working;
    // the defaults are what a missing file means as well.
    let settings = settings::load(data_dir)
        .map(|(settings, _)| settings)
        .unwrap_or_default();
    let seconds = std::env::var(settings::IDLE_SECONDS_ENV).ok();
    match settings.idle_policy(seconds.as_deref()) {
        Some(policy) => {
            if config.trace {
                eprintln!(
                    "[engine] idle stop after {:?}; {} = {}",
                    policy.idle_after(),
                    settings::WATCHERS_KEY,
                    policy.watchers()
                );
            }
            let idle =
                IdleStop::new(policy, hooks).with_report(docklet_core::idle::report_path(data_dir));
            serve_with_idle_stop(config, idle)
        }
        None => serve(config),
    }
}

#[cfg(windows)]
fn serve_with(config: PipeProxyConfig, idle: Option<IdleStop>) -> std::io::Result<()> {
    // Created before the stop event and held until the process exits, so a
    // proxy that counts as running is never taken for a plain one.
    let _idle_stop = match &idle {
        Some(_) => Some(control::IdleStopMarker::create()?),
        None => None,
    };
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
