/// Errors that can occur in the netanvil-rs framework.
#[derive(Debug, thiserror::Error)]
pub enum NetAnvilError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("request timeout")]
    Timeout,

    #[error("channel error: {0}")]
    Channel(String),

    #[error("{0}")]
    Other(String),
}

/// Result type alias for netanvil-rs operations.
pub type Result<T> = std::result::Result<T, NetAnvilError>;
