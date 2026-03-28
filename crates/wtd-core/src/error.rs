//! Top-level error type for wtd-core.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid workspace name: {0}")]
    InvalidName(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_yaml::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
