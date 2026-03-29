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

// ── Pipe name resolution ─────────────────────────────────────────────

/// Convert raw SID binary bytes to standard string form (`S-1-5-21-…`).
#[cfg(windows)]
fn sid_to_string(sid_bytes: &[u8]) -> String {
    assert!(sid_bytes.len() >= 8, "SID buffer too short");
    let revision = sid_bytes[0];
    let sub_count = sid_bytes[1] as usize;
    let authority = &sid_bytes[2..8];

    let authority_value = if authority[0] != 0 || authority[1] != 0 {
        format!(
            "0x{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            authority[0], authority[1], authority[2],
            authority[3], authority[4], authority[5],
        )
    } else {
        let val = ((authority[2] as u64) << 24)
            | ((authority[3] as u64) << 16)
            | ((authority[4] as u64) << 8)
            | (authority[5] as u64);
        val.to_string()
    };

    let mut result = format!("S-{}-{}", revision, authority_value);
    for i in 0..sub_count {
        let off = 8 + i * 4;
        let sub = u32::from_le_bytes([
            sid_bytes[off],
            sid_bytes[off + 1],
            sid_bytes[off + 2],
            sid_bytes[off + 3],
        ]);
        result.push_str(&format!("-{}", sub));
    }
    result
}

/// Build the named-pipe path for the current user: `\\.\pipe\wtd-{SID}`.
///
/// Retrieves the current process token's user SID and constructs the
/// pipe name that `wtd-host` listens on.
#[cfg(windows)]
pub fn pipe_name_for_current_user() -> Result<String, ConnectError> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::*;
    use windows::Win32::Security::*;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let sid_str = unsafe {
        let process = GetCurrentProcess();
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token)
            .map_err(|e| ConnectError::PipeName(format!("OpenProcessToken: {e}")))?;

        let mut size = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buf = vec![0u8; size as usize];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            size,
            &mut size,
        )
        .map_err(|e| {
            let _ = CloseHandle(token);
            ConnectError::PipeName(format!("GetTokenInformation: {e}"))
        })?;
        let _ = CloseHandle(token);

        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
        let psid = token_user.User.Sid;
        let sid_len = GetLengthSid(psid) as usize;
        let sid_bytes = std::slice::from_raw_parts(psid.0 as *const u8, sid_len).to_vec();
        sid_to_string(&sid_bytes)
    };

    Ok(format!(r"\\.\pipe\wtd-{}", sid_str))
}

#[cfg(not(windows))]
pub fn pipe_name_for_current_user() -> Result<String, ConnectError> {
    Err(ConnectError::PipeName(
        "named pipes not supported on this platform".into(),
    ))
}

// ── Host auto-start ──────────────────────────────────────────────────

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
