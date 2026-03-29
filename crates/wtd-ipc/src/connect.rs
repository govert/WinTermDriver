//! Host connection and auto-start helpers (§16.1, §16.2).
//!
//! Used by CLI and UI processes to discover the host pipe,
//! check availability, and auto-start the host if not running.

use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// Errors from host connection and auto-start operations.
#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not determine pipe name: {0}")]
    PipeName(String),

    #[error("wtd-host executable not found")]
    HostNotFound,

    #[error("host did not become available within timeout")]
    StartupTimeout,
}

/// Check whether the host named pipe exists and has an available instance.
///
/// Uses `WaitNamedPipeW` with a 1ms timeout to probe without consuming
/// a pipe instance.
#[cfg(windows)]
pub fn is_host_pipe_available(pipe_name: &str) -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let wide: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1).as_bool() }
}

#[cfg(not(windows))]
pub fn is_host_pipe_available(_pipe_name: &str) -> bool {
    false
}

// ── Host executable discovery ──────────────────────────────────────────

#[cfg(windows)]
const HOST_EXE_NAME: &str = "wtd-host.exe";
#[cfg(not(windows))]
const HOST_EXE_NAME: &str = "wtd-host";

/// Locate the `wtd-host` executable, searching near the calling binary.
pub fn find_host_executable() -> Result<PathBuf, ConnectError> {
    let current = std::env::current_exe()?;
    let dir = current.parent().ok_or(ConnectError::HostNotFound)?;

    let candidate = dir.join(HOST_EXE_NAME);
    if candidate.exists() {
        return Ok(candidate);
    }

    // Parent directory (cargo test: target/debug/deps/ -> target/debug/)
    if let Some(parent) = dir.parent() {
        let candidate = parent.join(HOST_EXE_NAME);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(ConnectError::HostNotFound)
}

/// Launch `wtd-host` as a detached background process (§16.1 step 3).
///
/// Uses `DETACHED_PROCESS` creation flag so the host runs without a console.
#[cfg(windows)]
pub fn start_host_detached() -> Result<(), ConnectError> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;

    let host_path = find_host_executable()?;
    std::process::Command::new(host_path)
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
pub fn start_host_detached() -> Result<(), ConnectError> {
    Err(ConnectError::PipeName(
        "named pipes not supported on this platform".into(),
    ))
}

/// Ensure the host process is running, auto-starting if necessary (§16.1).
///
/// 1. Check if the named pipe is available.
/// 2. If not, launch `wtd-host` as a detached process.
/// 3. Poll the pipe at 50ms intervals for up to 5 seconds.
pub async fn ensure_host_running(pipe_name: &str) -> Result<(), ConnectError> {
    if is_host_pipe_available(pipe_name) {
        return Ok(());
    }

    start_host_detached()?;

    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if is_host_pipe_available(pipe_name) {
            return Ok(());
        }
    }

    Err(ConnectError::StartupTimeout)
}
