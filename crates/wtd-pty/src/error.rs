//! PTY error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ConPTY creation failed (HRESULT {0:#010x})")]
    CreateFailed(u32),

    #[error("ConPTY resize failed (HRESULT {0:#010x})")]
    ResizeFailed(u32),

    #[error("child process spawn failed: {0}")]
    SpawnFailed(String),

    #[error("Win32 error: {0}")]
    Win32(#[from] windows::core::Error),

    #[error("job object error: {0}")]
    JobObject(String),
}
