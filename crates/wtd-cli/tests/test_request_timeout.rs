//! Tests for CLI request timeout protection.
//!
//! Verifies that IpcClient.request() times out gracefully when the host
//! does not respond within the configured duration.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use wtd_cli::client::{ClientError, IpcClient};

use wtd_host::ipc_server::{ClientId, IpcServer, RequestHandler};

use wtd_ipc::message::{ListWorkspaces, TypedMessage};
use wtd_ipc::Envelope;

// ── Pipe naming ─────────────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(9000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-timeout-{}-{}", std::process::id(), n)
}

// ── Handler that never responds ─────────────────────────────────────

struct NeverRespondHandler;

impl RequestHandler for NeverRespondHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        _envelope: &Envelope,
        _msg: &TypedMessage,
    ) -> Option<Envelope> {
        // Return None — the server will not send any response frame, simulating
        // an unresponsive host.
        None
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn request_times_out_after_configured_duration() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), NeverRespondHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let s = server.clone();
    tokio::spawn(async move { s.run(shutdown_rx).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = IpcClient::connect_to(&pipe_name).await.unwrap();
    client.set_timeout(Duration::from_millis(500));

    let request = Envelope::new("t-1", &ListWorkspaces {});
    let start = Instant::now();
    let result = client.request(&request).await;
    let elapsed = start.elapsed();

    // Should have timed out.
    assert!(result.is_err(), "expected timeout error");
    let err = result.unwrap_err();
    assert!(
        matches!(err, ClientError::RequestTimeout(_)),
        "expected RequestTimeout, got: {err:?}"
    );

    // Elapsed should be close to 500ms (with margin).
    assert!(
        elapsed >= Duration::from_millis(400),
        "timed out too quickly: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2000),
        "timed out too slowly: {elapsed:?}"
    );

    let _ = shutdown_tx.send(true);
}

#[tokio::test]
async fn timeout_error_message_includes_duration() {
    let err = ClientError::RequestTimeout(30.0);
    let msg = err.to_string();
    assert!(msg.contains("30.0"), "message should include duration: {msg}");
    assert!(msg.contains("timed out"), "message should say timed out: {msg}");
}

#[tokio::test]
async fn default_timeout_is_30_seconds() {
    use wtd_cli::client::DEFAULT_TIMEOUT;
    assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(30));
}
