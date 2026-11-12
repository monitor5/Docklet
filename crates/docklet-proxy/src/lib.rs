//! Named pipe server that relays each Docker API connection to a child
//! process (in production: `wsl.exe ... docklet-relay`).

#[cfg(windows)]
mod security;
#[cfg(windows)]
mod server;

#[cfg(windows)]
pub use server::{PipeProxy, PipeProxyConfig, RelayCommand};
