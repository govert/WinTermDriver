//! IPC error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("message too large: {size} bytes exceeds {max} byte limit")]
    MessageTooLarge { size: usize, max: usize },

    #[error("frame too short: got {size} bytes, expected at least {expected}")]
    FrameTooShort { size: usize, expected: usize },

    #[error("connection closed")]
    ConnectionClosed,
}
