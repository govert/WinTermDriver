//! Integration tests for ConPTY session lifecycle.
//!
//! These tests require Windows and spawn real child processes.

#![cfg(windows)]

use std::sync::mpsc;
use std::time::Duration;

use wtd_pty::{JobObject, PtySession, PtySize};

// ── Reader thread ─────────────────────────────────────────────────────────────
// PeekNamedPipe can return 0 bytes available for ConPTY-connected pipes even
// when data is present (ConPTY uses overlapped I/O internally).  Use a blocking
// ReadFile in a background thread instead.
//
// HANDLE is *mut c_void (not Send).  We pass its numeric value as usize across
// the thread boundary and reconstruct the HANDLE inside the thread.  This is
// safe because the PtySession outlives the ReaderThread in every test.

struct ReaderThread {
    rx: mpsc::Receiver<Vec<u8>>,
}

impl ReaderThread {
    fn spawn_for(session: &PtySession) -> Self {
        // SAFETY: usize is Send; the handle remains valid while the PtySession
        // is alive, which is always the case in these tests.
        let handle_val = session.output_read_handle().0 as usize;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        std::thread::spawn(move || {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::Storage::FileSystem::ReadFile;
            let handle = HANDLE(handle_val as *mut _);
            let mut buf = [0u8; 4096];
            loop {
                let mut read = 0u32;
                let r = unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) };
                match r {
                    Ok(_) if read > 0 => {
                        if tx.send(buf[..read as usize].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Ok(_) => {
                        // read == 0 on a synchronous pipe means the write end was closed
                        break;
                    }
                    Err(e) => {
                        eprintln!("[reader-thread] ReadFile error: {e}");
                        break;
                    }
                }
            }
            eprintln!("[reader-thread] exiting");
        });

        Self { rx }
    }

    /// Accumulate output until `needle` is found or `timeout` elapses.
    /// Returns `(accumulated_output, found)`.
    fn read_until(&self, needle: &str, timeout: Duration) -> (String, bool) {
        let deadline = std::time::Instant::now() + timeout;
        let mut buf: Vec<u8> = Vec::new();

        loop {
            let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
                Some(d) if d.as_millis() > 0 => d,
                _ => break,
            };
            match self.rx.recv_timeout(remaining) {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    if buf.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                        return (String::from_utf8_lossy(&buf).into_owned(), true);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        (String::from_utf8_lossy(&buf).into_owned(), false)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Locate PowerShell: prefer `pwsh.exe` (PowerShell 7+), fall back to `powershell.exe`.
fn find_powershell() -> &'static str {
    if which_exists("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("where")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns true if a process with the given PID is still alive.
fn is_process_running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) if !h.is_invalid() => {
                let r = WaitForSingleObject(h, 0);
                let _ = CloseHandle(h);
                // WAIT_OBJECT_0 means the process has already exited
                r != WAIT_OBJECT_0
            }
            _ => false, // could not open → process already gone
        }
    }
}

// ── Test 1a: cmd.exe /C echo hello — smoke test for ConPTY output pipe ───────

#[test]
fn test_echo_hello_cmd() {
    // Use a short-lived command so the pipe closes naturally (no prompt wait needed).
    let session = PtySession::spawn(
        "cmd.exe",
        &["/C", "echo hello"],
        None,
        PtySize::new(80, 24),
        None,
    )
    .expect("PtySession::spawn failed");

    let reader = ReaderThread::spawn_for(&session);
    let (output, found) = reader.read_until("hello", Duration::from_secs(15));
    drop(session);

    assert!(
        found,
        "expected 'hello' from cmd.exe but did not find it.\nOutput was:\n{:?}",
        output
    );
}

// ── Test 1b: launch pwsh → send "echo hello\r\n" → read output ───────────────

#[test]
fn test_echo_hello() {
    let exe = find_powershell();
    let session = PtySession::spawn(
        exe,
        &["-NoLogo", "-NoExit"],
        None,
        PtySize::new(80, 24),
        None,
    )
    .expect("PtySession::spawn failed");

    let reader = ReaderThread::spawn_for(&session);

    // Wait for the initial prompt before sending input.
    let (_, _) = reader.read_until(">", Duration::from_secs(20));

    // Send the command.
    session
        .write_input(b"echo hello\r\n")
        .expect("write_input failed");

    // Read until "hello" appears in the output.
    let (output, found) = reader.read_until("hello", Duration::from_secs(10));

    // Drop session (closes PTY, terminates child) before asserting.
    drop(session);

    assert!(
        found,
        "expected 'hello' in PTY output but did not find it.\nOutput was:\n{:?}",
        output
    );
}

// ── Test 2: resize during active session ─────────────────────────────────────

#[test]
fn test_resize_no_crash() {
    let exe = find_powershell();
    let session = PtySession::spawn(
        exe,
        &["-NoLogo", "-NoExit"],
        None,
        PtySize::new(80, 24),
        None,
    )
    .expect("PtySession::spawn failed");

    let reader = ReaderThread::spawn_for(&session);
    let _ = reader.read_until(">", Duration::from_secs(20));

    // Resize to several different sizes — must not crash or return an error.
    for (cols, rows) in [(120, 30), (40, 10), (220, 50), (80, 24)] {
        session
            .resize(PtySize::new(cols, rows))
            .unwrap_or_else(|e| panic!("resize to {cols}x{rows} failed: {e}"));
    }
}

// ── Test 3: close ConPTY → child process terminates ──────────────────────────

#[test]
fn test_close_terminates_child() {
    let exe = find_powershell();
    let session = PtySession::spawn(
        exe,
        &["-NoLogo", "-NoExit"],
        None,
        PtySize::new(80, 24),
        None,
    )
    .expect("PtySession::spawn failed");

    let reader = ReaderThread::spawn_for(&session);
    let _ = reader.read_until(">", Duration::from_secs(20));

    let pid = session.pid;

    // Drop closes ConPTY (→ EOF to child) + waits + terminates if needed.
    drop(session);

    // The reader thread will exit shortly after (ReadFile fails on closed handle).
    // Give it a moment to clean up, then verify the child is gone.
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        !is_process_running(pid),
        "child process (pid {pid}) is still running after session close"
    );
}

// ── Test 4: Job Object — kill-on-close terminates child ──────────────────────

#[test]
fn test_job_object_kills_child() {
    let exe = find_powershell();
    let job = JobObject::new().expect("JobObject::new failed");

    let session = PtySession::spawn(
        exe,
        &["-NoLogo", "-NoExit"],
        None,
        PtySize::new(80, 24),
        Some(&job),
    )
    .expect("PtySession::spawn failed");

    let reader = ReaderThread::spawn_for(&session);
    let _ = reader.read_until(">", Duration::from_secs(20));

    let pid = session.pid;

    // Drop session (closes ConPTY + terminates child).
    drop(session);
    // Drop job object — JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE fires.
    drop(job);

    std::thread::sleep(Duration::from_millis(500));

    assert!(
        !is_process_running(pid),
        "child process (pid {pid}) should have been killed by job close"
    );
}
