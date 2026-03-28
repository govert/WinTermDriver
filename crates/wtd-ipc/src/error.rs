//! IPC error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON framing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("connection closed")]
    ConnectionClosed,
}
