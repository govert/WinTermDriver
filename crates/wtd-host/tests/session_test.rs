//! Integration tests for the session manager.
//!
//! These tests spawn real ConPTY sessions with `cmd.exe` and verify the full
//! session lifecycle: start, I/O, exit detection, restart with backoff, and
//! startup command delivery.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use wtd_core::ids::SessionId;
use wtd_core::workspace::RestartPolicy;
use wtd_host::session::{Session, SessionConfig, SessionState};
use wtd_pty::PtySize;

fn default_config() -> SessionConfig {
    SessionConfig {
        executable: "cmd.exe".into(),
        args: vec![],
        cwd: None,
        env: HashMap::new(),
        restart_policy: RestartPolicy::Never,
        startup_command: None,
        size: PtySize::new(80, 24),
        name: "test".into(),
        max_scrollback: 100,
    }
}

/// Helper: wait until a predicate is true, polling every `interval`, up to `timeout`.
fn wait_until(timeout: Duration, interval: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return true;
        }
        thread::sleep(interval);
    }
    false
}

// ── Test 1: Create session, verify Running, send input, read output ──────────

#[test]
fn session_start_running_and_io() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec![],
        ..default_config()
    };
    let mut session = Session::new(SessionId(1), config);
    session.start().expect("start failed");

    assert_eq!(session.state(), &SessionState::Running);

    // Send a command and wait for output
    session
        .write_input(b"echo HELLO_WTD\r\n")
        .expect("write failed");

    let found = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.process_pending_output();
        session.screen().visible_text().contains("HELLO_WTD")
    });
    assert!(found, "expected HELLO_WTD in output, got:\n{}", session.screen().visible_text());
}

// ── Test 2: Session exit detected, exit code captured ────────────────────────

#[test]
fn session_exit_detected_with_code() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "exit".into(), "42".into()],
        ..default_config()
    };
    let mut session = Session::new(SessionId(2), config);
    session.start().expect("start failed");

    // Poll for exit
    let mut exit_code = None;
    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        exit_code = session.check_exit();
        exit_code.is_some()
    });
    assert!(exited, "process should have exited");
    assert_eq!(exit_code, Some(42));
    assert_eq!(session.state(), &SessionState::Exited { exit_code: 42 });
}

#[test]
fn session_exit_code_zero() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "echo".into(), "ok".into()],
        ..default_config()
    };
    let mut session = Session::new(SessionId(3), config);
    session.start().expect("start failed");

    let mut exit_code = None;
    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        exit_code = session.check_exit();
        exit_code.is_some()
    });
    assert!(exited, "process should have exited");
    assert_eq!(exit_code, Some(0));
}

// ── Test 3: Restart on failure with backoff delay progression ────────────────

#[test]
fn restart_on_failure_with_backoff() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "exit".into(), "1".into()],
        restart_policy: RestartPolicy::OnFailure,
        ..default_config()
    };
    let mut session = Session::new(SessionId(4), config);
    session.start().expect("start failed");

    // First exit
    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited, "process should have exited");
    assert!(session.should_restart(), "on-failure policy with exit 1 should restart");

    // Check backoff progression
    let delay1 = session.next_restart_delay();
    assert_eq!(delay1, Duration::from_millis(500));

    // Restart
    session.restart().expect("restart failed");
    assert_eq!(session.state(), &SessionState::Running);

    // Second exit
    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited);
    assert!(session.should_restart());

    let delay2 = session.next_restart_delay();
    assert_eq!(delay2, Duration::from_millis(1_000));

    // Third cycle
    session.restart().expect("restart failed");
    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited);

    let delay3 = session.next_restart_delay();
    assert_eq!(delay3, Duration::from_millis(2_000));
}

#[test]
fn no_restart_on_success_with_on_failure_policy() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "exit".into(), "0".into()],
        restart_policy: RestartPolicy::OnFailure,
        ..default_config()
    };
    let mut session = Session::new(SessionId(5), config);
    session.start().expect("start failed");

    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited);
    assert!(!session.should_restart(), "exit 0 with on-failure should not restart");
}

#[test]
fn always_restart_even_on_success() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "exit".into(), "0".into()],
        restart_policy: RestartPolicy::Always,
        ..default_config()
    };
    let mut session = Session::new(SessionId(6), config);
    session.start().expect("start failed");

    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited);
    assert!(session.should_restart(), "always policy should restart on exit 0");
}

#[test]
fn never_restart_policy() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec!["/c".into(), "exit".into(), "1".into()],
        restart_policy: RestartPolicy::Never,
        ..default_config()
    };
    let mut session = Session::new(SessionId(7), config);
    session.start().expect("start failed");

    let exited = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.check_exit().is_some()
    });
    assert!(exited);
    assert!(!session.should_restart(), "never policy should not restart");
}

// ── Test 4: Backoff reset after stable run > 60s ─────────────────────────────
// Covered by unit tests in crates/wtd-host/src/backoff.rs:
//   - backoff_resets_after_stable_run
//   - backoff_does_not_reset_before_stable_threshold

// ── Test 5: Startup command delivered and visible in output ──────────────────

#[test]
fn startup_command_delivered() {
    let config = SessionConfig {
        executable: "cmd.exe".into(),
        args: vec![],
        startup_command: Some("echo STARTUP_MARKER".into()),
        ..default_config()
    };
    let mut session = Session::new(SessionId(8), config);
    session.start().expect("start failed");

    // The startup command is sent after 100ms delay. Wait for it to appear.
    let found = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        session.process_pending_output();
        session.screen().visible_text().contains("STARTUP_MARKER")
    });
    assert!(
        found,
        "startup command output not found. Screen:\n{}",
        session.screen().visible_text()
    );
}
