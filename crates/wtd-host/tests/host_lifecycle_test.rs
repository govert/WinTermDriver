//! Integration tests for host lifecycle (§16).
//!
//! Tests single-instance enforcement, PID file management, shutdown cleanup,
//! and auto-start polling.

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::watch;
use wtd_host::host_lifecycle::*;
use wtd_host::ipc_server::*;
use wtd_ipc::connect::is_host_pipe_available;
use wtd_ipc::message::TypedMessage;
use wtd_ipc::Envelope;

// ── Helpers ────────────────────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(1000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-lifecycle-{}-{}", std::process::id(), n)
}

/// Create a unique temp directory for PID file isolation.
fn temp_data_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtd-test-{}-{}-{}",
        test_name,
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct NoopHandler;

impl RequestHandler for NoopHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        _envelope: &Envelope,
        _msg: &TypedMessage,
    ) -> Option<Envelope> {
        None
    }
}

// ── Test 1: Start host, verify pipe available, connect ────────────────

#[tokio::test]
async fn start_host_verify_pipe_and_connect() {
    let pipe_name = unique_pipe_name();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let pn = pipe_name.clone();
    let server_handle = tokio::spawn(async move {
        let server = IpcServer::new(pn, NoopHandler).unwrap();
        server.run(shutdown_rx).await
    });

    // Wait for the server to be ready.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify pipe is available via WaitNamedPipeW.
    assert!(
        is_host_pipe_available(&pipe_name),
        "host pipe should be available after server start"
    );

    // Connect as a real client.
    let client = ClientOptions::new().open(&pipe_name);
    assert!(client.is_ok(), "should be able to connect to host pipe");
    drop(client);

    // Shutdown.
    shutdown_tx.send(true).unwrap();
    let _ = server_handle.await;
}

// ── Test 2: Second host launch detects existing instance ──────────────

#[tokio::test]
async fn second_host_detects_existing_instance() {
    let pipe_name = unique_pipe_name();
    let dir = temp_data_dir("single-instance");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let pn = pipe_name.clone();
    let server_handle = tokio::spawn(async move {
        let server = IpcServer::new(pn, NoopHandler).unwrap();
        server.run(shutdown_rx).await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Single-instance check should detect the running server.
    let result = check_single_instance_in(&pipe_name, &dir);
    assert_eq!(
        result,
        SingleInstanceCheck::AlreadyRunning,
        "should detect that another host is already running"
    );

    shutdown_tx.send(true).unwrap();
    let _ = server_handle.await;
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Test 3: Stop host, verify pipe gone and PID file removed ──────────

#[tokio::test]
async fn stop_host_cleans_up_pipe_and_pid_file() {
    let pipe_name = unique_pipe_name();
    let dir = temp_data_dir("stop-host");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let pn = pipe_name.clone();
    let d = dir.clone();
    let host_handle = tokio::spawn(async move {
        run_host(&pn, NoopHandler, shutdown_rx, &d).await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify host is running.
    assert!(
        is_host_pipe_available(&pipe_name),
        "pipe should be available while host is running"
    );
    assert!(
        read_pid_in(&dir).is_some(),
        "PID file should exist while host is running"
    );

    // Trigger shutdown.
    shutdown_tx.send(true).unwrap();
    let result = host_handle.await.unwrap();
    assert!(result.is_ok(), "host should shut down cleanly");

    // Brief wait for OS-level pipe cleanup.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify cleanup.
    assert!(
        !is_host_pipe_available(&pipe_name),
        "pipe should be gone after shutdown"
    );
    assert!(
        read_pid_in(&dir).is_none(),
        "PID file should be removed after shutdown"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Test 4: Auto-start from CLI client when host not running ──────────

#[tokio::test]
async fn auto_start_polling_detects_delayed_host() {
    let pipe_name = unique_pipe_name();

    // Verify host is NOT running.
    assert!(
        !is_host_pipe_available(&pipe_name),
        "pipe should not exist initially"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let pn = pipe_name.clone();

    // Simulate host auto-start: server starts after a 300ms delay
    // (mimicking the time it takes for CreateProcess + host init).
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let server = IpcServer::new(pn, NoopHandler).unwrap();
        server.run(shutdown_rx).await
    });

    // Simulate the polling loop from ensure_host_running (§16.1 step 4).
    let mut became_available = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if is_host_pipe_available(&pipe_name) {
            became_available = true;
            break;
        }
    }
    assert!(
        became_available,
        "host pipe should become available after delayed start"
    );

    shutdown_tx.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// ── Test 5: Stale PID file detected and cleaned ───────────────────────

#[tokio::test]
async fn stale_pid_file_cleaned_on_startup() {
    let dir = temp_data_dir("stale-pid");

    // Write a PID file for a non-existent process.
    std::fs::write(pid_file_in(&dir), "99999999").unwrap();

    // The PID 99999999 should not be running.
    assert!(
        !is_process_running(99_999_999),
        "fake PID should not be a running process"
    );

    // Single-instance check should clean the stale PID file.
    let pipe_name = unique_pipe_name();
    let result = check_single_instance_in(&pipe_name, &dir);
    assert_eq!(
        result,
        SingleInstanceCheck::StalePidCleaned,
        "should detect and clean stale PID file"
    );
    assert!(
        read_pid_in(&dir).is_none(),
        "stale PID file should be removed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Test 6: PID file write/read/remove cycle ──────────────────────────

#[tokio::test]
async fn pid_file_write_read_remove() {
    let dir = temp_data_dir("pid-cycle");

    // Write.
    write_pid_file_in(&dir).unwrap();

    // Read should return the current PID.
    let pid = read_pid_in(&dir);
    assert_eq!(pid, Some(std::process::id()));

    // Remove.
    remove_pid_in(&dir);
    assert!(read_pid_in(&dir).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}
