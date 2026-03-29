//! Host process lifecycle management (§16).
//!
//! Handles PID file management, single-instance enforcement, console
//! signal handling, and the main host run loop.

use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors during host lifecycle operations.
#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("another host instance is already running")]
    AlreadyRunning,

    #[error("PID file error: {0}")]
    PidFile(std::io::Error),

    #[error("ctrl handler already installed")]
    CtrlHandlerAlreadyInstalled,

    #[cfg(windows)]
    #[error("server error: {0}")]
    Server(#[from] crate::ipc_server::ServerError),

    #[cfg(windows)]
    #[error("Windows error: {0}")]
    Windows(#[from] windows::core::Error),
}

// ── Data directory and PID file ─────────────────────────────────────

/// Default host data directory: `%APPDATA%\WinTermDriver`.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WTD_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| {
        let home =
            std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        format!(r"{}\AppData\Roaming", home)
    });
    PathBuf::from(appdata).join("WinTermDriver")
}

/// PID file path within a given directory.
pub fn pid_file_in(dir: &Path) -> PathBuf {
    dir.join("host.pid")
}

/// PID file path using the default data directory.
pub fn pid_file_path() -> PathBuf {
    pid_file_in(&data_dir())
}

/// Write the current process PID to the PID file in `dir`.
pub fn write_pid_file_in(dir: &Path) -> Result<(), LifecycleError> {
    fs::create_dir_all(dir).map_err(LifecycleError::PidFile)?;
    fs::write(pid_file_in(dir), std::process::id().to_string())
        .map_err(LifecycleError::PidFile)
}

/// Write the current process PID to the default PID file.
pub fn write_pid_file() -> Result<(), LifecycleError> {
    write_pid_file_in(&data_dir())
}

/// Read the PID from the PID file in `dir`.
///
/// Returns `None` if the file doesn't exist or contains invalid data.
pub fn read_pid_in(dir: &Path) -> Option<u32> {
    fs::read_to_string(pid_file_in(dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Read the PID from the default PID file.
pub fn read_pid_file() -> Option<u32> {
    read_pid_in(&data_dir())
}

/// Remove the PID file in `dir`. Errors are silently ignored.
pub fn remove_pid_in(dir: &Path) {
    let _ = fs::remove_file(pid_file_in(dir));
}

/// Remove the default PID file.
pub fn remove_pid_file() {
    remove_pid_in(&data_dir())
}

// ── Process checking ─────────────────────────────────────────────────

/// Check if a process with the given PID is still running.
#[cfg(windows)]
pub fn is_process_running(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::*;

    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let mut exit_code = 0u32;
                let running =
                    if GetExitCodeProcess(handle, &mut exit_code).is_ok() {
                        exit_code == 259 // STILL_ACTIVE
                    } else {
                        false
                    };
                let _ = CloseHandle(handle);
                running
            }
            Err(_) => false,
        }
    }
}

#[cfg(not(windows))]
pub fn is_process_running(_pid: u32) -> bool {
    false
}

/// Clean up a stale PID file in `dir` if the recorded process is no longer running.
///
/// Returns `true` if a stale file was found and removed (§16.4).
pub fn clean_stale_pid_in(dir: &Path) -> bool {
    if let Some(pid) = read_pid_in(dir) {
        if !is_process_running(pid) {
            remove_pid_in(dir);
            return true;
        }
    }
    false
}

/// Clean up a stale PID file using the default data directory.
pub fn clean_stale_pid_file() -> bool {
    clean_stale_pid_in(&data_dir())
}

// ── Single-instance enforcement (§16.5) ─────────────────────────────

/// Result of the single-instance check.
#[derive(Debug, PartialEq, Eq)]
pub enum SingleInstanceCheck {
    /// No other instance detected; safe to proceed.
    Available,
    /// Another host is already running on the pipe.
    AlreadyRunning,
    /// A stale PID file was cleaned up; safe to proceed.
    StalePidCleaned,
}

/// Check whether another host instance is already running.
///
/// The named pipe serves as the single-instance mutex (§16.5).
/// Uses `data_dir` for PID file stale-check.
pub fn check_single_instance_in(
    pipe_name: &str,
    dir: &Path,
) -> SingleInstanceCheck {
    if wtd_ipc::connect::is_host_pipe_available(pipe_name) {
        return SingleInstanceCheck::AlreadyRunning;
    }
    if clean_stale_pid_in(dir) {
        SingleInstanceCheck::StalePidCleaned
    } else {
        SingleInstanceCheck::Available
    }
}

/// Check single instance using the default data directory.
pub fn check_single_instance(pipe_name: &str) -> SingleInstanceCheck {
    check_single_instance_in(pipe_name, &data_dir())
}

// ── Console ctrl handler (§16.3) ────────────────────────────────────

#[cfg(windows)]
mod ctrl_handler {
    use std::sync::OnceLock;
    use tokio::sync::watch;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    static SHUTDOWN_TX: OnceLock<watch::Sender<bool>> = OnceLock::new();

    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        match ctrl_type {
            // CTRL_C_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
            0 | 2 | 5 | 6 => {
                if let Some(tx) = SHUTDOWN_TX.get() {
                    let _ = tx.send(true);
                }
                BOOL(1)
            }
            _ => BOOL(0),
        }
    }

    pub fn install(
        tx: watch::Sender<bool>,
    ) -> Result<(), super::LifecycleError> {
        SHUTDOWN_TX
            .set(tx)
            .map_err(|_| super::LifecycleError::CtrlHandlerAlreadyInstalled)?;
        unsafe {
            SetConsoleCtrlHandler(Some(handler), true)?;
        }
        Ok(())
    }
}

/// Install the Windows console ctrl handler for graceful shutdown.
///
/// Routes CTRL_C, CTRL_CLOSE, CTRL_LOGOFF, and CTRL_SHUTDOWN events
/// to the shutdown watch channel. Can only be called once per process.
#[cfg(windows)]
pub fn install_ctrl_handler(
    tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), LifecycleError> {
    ctrl_handler::install(tx)
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn install_ctrl_handler(
    _tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), LifecycleError> {
    Ok(())
}

// ── Host run loop ───────────────────────────────────────────────────

/// Run the host IPC server, managing the PID file lifecycle.
///
/// 1. Writes the PID file to `dir`.
/// 2. Creates and runs the IPC server until shutdown is signalled.
/// 3. Removes the PID file on exit.
///
/// The caller is responsible for single-instance checks and installing
/// the ctrl handler before calling this function.
#[cfg(windows)]
pub async fn run_host(
    pipe_name: &str,
    handler: impl crate::ipc_server::RequestHandler,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    dir: &Path,
) -> Result<(), LifecycleError> {
    write_pid_file_in(dir)?;

    let result = {
        let server =
            crate::ipc_server::IpcServer::new(pipe_name.to_owned(), handler)?;
        server.run(shutdown_rx).await
    };

    // §16.3 shutdown sequence:
    // Steps 1-2 (notify UI clients, close workspace instances)
    // are deferred to the workspace/host management bead.
    // Step 3: pipe closed when IpcServer is dropped.
    // Step 4: remove PID file.
    remove_pid_in(dir);

    result.map_err(Into::into)
}

// ── Tests ─────────────────────────────────────────���─────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_ends_with_wintermdriver() {
        let had_override = std::env::var("WTD_DATA_DIR").ok();
        std::env::remove_var("WTD_DATA_DIR");

        let dir = data_dir();
        assert!(
            dir.ends_with("WinTermDriver"),
            "data_dir should end with WinTermDriver, got: {:?}",
            dir
        );

        if let Some(v) = had_override {
            std::env::set_var("WTD_DATA_DIR", v);
        }
    }

    #[test]
    fn single_instance_no_pipe() {
        let tmp = std::env::temp_dir().join(format!(
            "wtd-test-nopipe-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let result = check_single_instance_in(
            r"\\.\pipe\wtd-test-nonexistent-999999",
            &tmp,
        );
        assert_ne!(result, SingleInstanceCheck::AlreadyRunning);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn is_process_running_current() {
        assert!(is_process_running(std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn is_process_running_nonexistent() {
        assert!(!is_process_running(99_999_999));
    }
}
