/// What a Docker client call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Docker API reported an error, or the engine pipe is not reachable.
    #[error(transparent)]
    Docker(#[from] bollard::errors::Error),

    #[error("unknown container action `{0}`")]
    UnknownAction(String),

    #[error("invalid image reference `{0}`")]
    InvalidImageRef(String),

    /// The engine is reached through a Windows named pipe.
    #[error("the Docklet engine can only be reached on Windows")]
    Unsupported,
}
