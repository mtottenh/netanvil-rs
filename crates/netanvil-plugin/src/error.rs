//! Plugin error types.

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("WASM error: {0}")]
    Wasm(String),

    #[error("Lua error: {0}")]
    Lua(String),

    #[error("Rhai error: {0}")]
    Rhai(String),

    #[error("V8/JS error: {0}")]
    V8(String),

    #[error("plugin returned invalid response: {0}")]
    InvalidResponse(String),

    #[error("plugin initialization failed: {0}")]
    Init(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, PluginError>;
