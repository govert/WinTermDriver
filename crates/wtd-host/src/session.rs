//! Session lifecycle management (spec &sect;17).
//!
//! A `Session` wraps a ConPTY child process with a screen buffer, restart
//! policy, and exponential backoff.  The state machine is:
//!
//!   Creating &rarr; Running &rarr; Exited/Failed &rarr; Restarting &rarr; Running &rarr; &hellip;

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wtd_core::ids::SessionId;
use wtd_core::workspace::RestartPolicy;
use wtd_pty::{PtyError, PtySession, PtySize, ScreenBuffer};

use crate::backoff::BackoffState;

// ── Session state machine ────────────────────────────────────────────────────

/// Session state per spec &sect;17.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// Being set up (transient).
    Creating,
    /// Child process is running.
    Running,
    /// Child exited with the given code.
    Exited { exit_code: u32 },
    /// `CreateProcess` failed.
    Failed { error: String },
    /// Waiting to restart (backoff in progress).
    Restarting { attempt: u32 },
}

// ── Session configuration ────────────────────────────────────────────────────

/// Everything needed to (re)create a session.
pub struct SessionConfig {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub restart_policy: RestartPolicy,
    pub startup_command: Option<String>,
    pub size: PtySize,
    pub name: String,
    pub max_scrollback: usize,
}

// ── Session error ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("PTY error: {0}")]
    Pty(#[from] PtyError),
    #[error("session not running")]
    NotRunning,
}

// ── Session ──────────────────────────────────────────────────────────────────

/// A managed terminal session: ConPTY child + screen buffer + restart policy.
pub struct Session {
    id: SessionId,
    config: SessionConfig,
    state: SessionState,
    pty: Option<PtySession>,
    screen: ScreenBuffer,
    backoff: BackoffState,
    // Reader thread plumbing
    output_rx: Option<mpsc::Receiver<Vec<u8>>>,
    reader_handle: Option<JoinHandle<()>>,
}

impl Session {
    /// Create a session in `Creating` state. Call [`start`](Self::start) to spawn the child.
    pub fn new(id: SessionId, config: SessionConfig) -> Self {
        let screen = ScreenBuffer::new(config.size.cols, config.size.rows, config.max_scrollback);
        Self {
            id,
            config,
            state: SessionState::Creating,
            pty: None,
            screen,
            backoff: BackoffState::new(),
            output_rx: None,
            reader_handle: None,
        }
    }

    /// Spawn the ConPTY child process and begin reading output.
    ///
    /// On success the state moves to `Running`.
    /// On spawn failure the state moves to `Failed`.
    pub fn start(&mut self) -> Result<(), SessionError> {
        self.state = SessionState::Creating;

        let args: Vec<&str> = self.config.args.iter().map(|s| s.as_str()).collect();

        let pty = match PtySession::spawn(
            &self.config.executable,
            &args,
            self.config.cwd.as_deref(),
            self.config.size,
            None,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.state = SessionState::Failed {
                    error: e.to_string(),
                };
                return Err(SessionError::Pty(e));
            }
        };

        // Start a background reader thread.
        // Pass the raw HANDLE as usize to cross the thread boundary
        // (HANDLE wraps *mut c_void which is !Send).
        let (tx, rx) = mpsc::channel();
        let output_handle_raw = pty.output_read_handle().0 as usize;
        let reader = thread::spawn(move || reader_thread_fn(output_handle_raw, tx));

        // Schedule startup command delivery after 100ms (&sect;17.4).
        if let Some(ref cmd) = self.config.startup_command {
            let input_handle_raw = pty.input_write_handle().0 as usize;
            let payload = format!("{}\r\n", cmd).into_bytes();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                write_raw(input_handle_raw, &payload);
            });
        }

        self.pty = Some(pty);
        self.output_rx = Some(rx);
        self.reader_handle = Some(reader);
        self.state = SessionState::Running;
        self.backoff.record_start();

        Ok(())
    }

    // ── Accessors ────────────────────────────────────────────────────────

    pub fn id(&self) -> SessionId {
        self.id.clone()
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn screen(&self) -> &ScreenBuffer {
        &self.screen
    }

    pub fn restart_policy(&self) -> &RestartPolicy {
        &self.config.restart_policy
    }

    pub fn backoff(&self) -> &BackoffState {
        &self.backoff
    }

    // ── I/O ──────────────────────────────────────────────────────────────

    /// Write bytes to the child's stdin.
    pub fn write_input(&self, data: &[u8]) -> Result<(), SessionError> {
        self.pty
            .as_ref()
            .ok_or(SessionError::NotRunning)?
            .write_input(data)?;
        Ok(())
    }

    /// Drain pending output from the reader thread into the screen buffer.
    pub fn process_pending_output(&mut self) {
        if let Some(ref rx) = self.output_rx {
            while let Ok(chunk) = rx.try_recv() {
                self.screen.advance(&chunk);
            }
        }
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Poll whether the child has exited. If so, drains remaining output,
    /// updates state to `Exited`, and returns the exit code.
    pub fn check_exit(&mut self) -> Option<u32> {
        if self.state != SessionState::Running {
            return None;
        }
        let exited = self
            .pty
            .as_ref()
            .map(|p| p.wait_for_exit(0))
            .unwrap_or(false);
        if !exited {
            return None;
        }

        let exit_code = self.get_exit_code().unwrap_or(1);

        // Drain remaining output from the pipe (&sect;17.6 step 3).
        self.drain_remaining_output();

        self.state = SessionState::Exited { exit_code };
        Some(exit_code)
    }

    /// Whether the session should restart given its current state and policy.
    pub fn should_restart(&self) -> bool {
        match &self.state {
            SessionState::Exited { exit_code } => match self.config.restart_policy {
                RestartPolicy::Always => true,
                RestartPolicy::OnFailure => *exit_code != 0,
                RestartPolicy::Never => false,
            },
            _ => false,
        }
    }

    /// Compute the next restart delay and advance the backoff counter.
    pub fn next_restart_delay(&mut self) -> Duration {
        self.backoff.next_delay()
    }

    /// Restart the session: tear down the old child, clear the screen buffer,
    /// and spawn a fresh child process.
    pub fn restart(&mut self) -> Result<(), SessionError> {
        let attempt = self.backoff.restart_count();
        self.state = SessionState::Restarting { attempt };

        self.stop_pty();

        // Clear screen buffer (&sect;17.7 step 2).
        self.screen = ScreenBuffer::new(
            self.config.size.cols,
            self.config.size.rows,
            self.config.max_scrollback,
        );

        self.start()
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    fn stop_pty(&mut self) {
        // Dropping PtySession closes ConPTY, waits for child exit, closes handles.
        // The reader thread's ReadFile will then fail and the thread exits.
        self.pty.take();
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
        self.output_rx.take();
    }

    fn drain_remaining_output(&mut self) {
        if let Some(ref rx) = self.output_rx {
            // Give the reader thread a moment to push final chunks.
            thread::sleep(Duration::from_millis(100));
            while let Ok(chunk) = rx.try_recv() {
                self.screen.advance(&chunk);
            }
        }
    }

    #[cfg(windows)]
    fn get_exit_code(&self) -> Option<u32> {
        use windows::Win32::System::Threading::GetExitCodeProcess;

        let pty = self.pty.as_ref()?;
        let mut code = 0u32;
        unsafe {
            GetExitCodeProcess(pty.process_handle(), &mut code).ok()?;
        }
        Some(code)
    }

    #[cfg(not(windows))]
    fn get_exit_code(&self) -> Option<u32> {
        None
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_pty();
    }
}

// ── Thread helpers (raw Win32 I/O) ───────────────────────────────────────────

#[cfg(windows)]
fn reader_thread_fn(output_handle_raw: usize, tx: mpsc::Sender<Vec<u8>>) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::ReadFile;

    let handle = HANDLE(output_handle_raw as *mut std::ffi::c_void);
    let mut buf = [0u8; 4096];
    loop {
        let mut bytes_read = 0u32;
        let ok = unsafe { ReadFile(handle, Some(&mut buf), Some(&mut bytes_read), None) };
        match ok {
            Ok(()) if bytes_read > 0 => {
                if tx.send(buf[..bytes_read as usize].to_vec()).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

#[cfg(not(windows))]
fn reader_thread_fn(_handle_raw: usize, _tx: mpsc::Sender<Vec<u8>>) {}

#[cfg(windows)]
fn write_raw(handle_raw: usize, data: &[u8]) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::WriteFile;

    let handle = HANDLE(handle_raw as *mut std::ffi::c_void);
    let mut written = 0u32;
    unsafe {
        let _ = WriteFile(handle, Some(data), Some(&mut written), None);
    }
}

#[cfg(not(windows))]
fn write_raw(_handle_raw: usize, _data: &[u8]) {}
