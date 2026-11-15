//! Docker Engine client for Docklet, built on bollard.
//!
//! Everything that talks to the Docker API lives here, free of any UI
//! framework: streams hand their output to a callback and the caller decides
//! where it goes. Lists are returned as bollard's own models, so they
//! serialize with the Docker API's JSON field names (`Id`, `Names`, ...).
//!
//! Only [`Client::connect`] needs Windows; the rest builds and tests on Linux.

mod backoff;
mod client;
mod error;
mod image_ref;
mod logs;
mod pull;

pub use backoff::Backoff;
pub use bollard::models;
pub use client::{Client, ContainerAction};
pub use error::Error;
pub use image_ref::ImageRef;
pub use logs::LineSplitter;
pub use pull::progress_line;

#[cfg(test)]
mod tests {
    use docklet_core::engine::{ENGINE_DOCKER_HOST, ENGINE_PIPE};

    /// The Docker CLI context and this client must end up on the same pipe.
    #[test]
    fn docker_host_url_names_the_engine_pipe() {
        let path = ENGINE_DOCKER_HOST
            .strip_prefix("npipe://")
            .expect("an npipe URL");
        assert_eq!(path.replace('/', "\\"), ENGINE_PIPE);
    }
}
