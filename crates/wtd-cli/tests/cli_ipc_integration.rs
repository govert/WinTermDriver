//! Integration tests for CLI IPC client.
//!
//! Spins up a minimal named pipe server and tests the client's
//! connect → handshake → request → response → format pipeline.
#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::windows::named_pipe::ServerOptions;
use wtd_ipc::framing::{read_frame_async, write_frame_async};
use wtd_ipc::message::{
    Capture, ErrorCode, ErrorResponse, Handshake, HandshakeAck, ListPanes, ListWorkspaces,
    ListWorkspacesResult, MessagePayload, OkResponse, WorkspaceInfo,
};
use wtd_ipc::{Envelope, PROTOCOL_VERSION};

use wtd_cli::client::IpcClient;
use wtd_cli::exit_code;
use wtd_cli::output;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(8000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-cli-test-{}-{}", std::process::id(), n)
}

// ── Minimal test server ──────────────────────────────────────────────

/// Run a single-connection test server that handles handshake then
/// dispatches requests to a callback.
async fn run_test_server(
    pipe_name: &str,
    handler: impl Fn(&Envelope) -> Envelope + Send + 'static,
) {
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)
        .unwrap();
    server.connect().await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(server);

    // Handshake
    let hs = read_frame_async(&mut reader).await.unwrap();
    assert_eq!(hs.msg_type, Handshake::TYPE_NAME);
    let ack = Envelope::new(
        &hs.id,
        &HandshakeAck {
            host_version: "test-0.1.0".to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    );
    write_frame_async(&mut writer, &ack).await.unwrap();

    // Handle requests until disconnect
    loop {
        match read_frame_async(&mut reader).await {
            Ok(req) => {
                let mut resp = handler(&req);
                resp.id = req.id.clone();
                write_frame_async(&mut writer, &resp).await.unwrap();
            }
            Err(_) => break,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

/// Connect to running host, send ListWorkspaces, format text output.
#[tokio::test]
async fn connect_send_list_workspaces_text_output() {
    let pipe_name = unique_pipe_name();
    let name = pipe_name.clone();

    tokio::spawn(async move {
        run_test_server(&name, |_req| {
            Envelope::new(
                "resp",
                &ListWorkspacesResult {
                    workspaces: vec![
                        WorkspaceInfo {
                            name: "dev".into(),
                            source: "user".into(),
                        },
                        WorkspaceInfo {
                            name: "ops".into(),
                            source: "local".into(),
                        },
                    ],
                },
            )
        })
        .await;
    });

    // Give server a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = IpcClient::connect_to(&pipe_name).await.unwrap();
    let request = Envelope::new("req-1", &ListWorkspaces {});
    let response = client.request(&request).await.unwrap();

    assert_eq!(response.msg_type, ListWorkspacesResult::TYPE_NAME);

    // Verify text formatting
    let result = output::format_response(&response, false);
    assert_eq!(result.exit_code, exit_code::SUCCESS);
    assert!(result.stdout.contains("NAME"));
    assert!(result.stdout.contains("SOURCE"));
    assert!(result.stdout.contains("dev"));
    assert!(result.stdout.contains("user"));
    assert!(result.stdout.contains("ops"));
    assert!(result.stdout.contains("local"));
}

/// --json flag produces valid JSON output.
#[tokio::test]
async fn json_flag_produces_valid_json() {
    let pipe_name = unique_pipe_name();
    let name = pipe_name.clone();

    tokio::spawn(async move {
        run_test_server(&name, |_req| {
            Envelope::new(
                "resp",
                &ListWorkspacesResult {
                    workspaces: vec![WorkspaceInfo {
                        name: "dev".into(),
                        source: "user".into(),
                    }],
                },
            )
        })
        .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = IpcClient::connect_to(&pipe_name).await.unwrap();
    let request = Envelope::new("req-1", &ListWorkspaces {});
    let response = client.request(&request).await.unwrap();

    let result = output::format_response(&response, true);
    assert_eq!(result.exit_code, exit_code::SUCCESS);

    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(parsed["workspaces"][0]["name"], "dev");
}

/// Exit codes match expected values for success, not-found, ambiguous.
#[tokio::test]
async fn exit_codes_match_error_types() {
    let pipe_name = unique_pipe_name();
    let name = pipe_name.clone();

    tokio::spawn(async move {
        run_test_server(&name, |req| {
            // Return different errors based on the request type
            match req.msg_type.as_str() {
                "Capture" => Envelope::new(
                    "resp",
                    &ErrorResponse {
                        code: ErrorCode::TargetNotFound,
                        message: "pane not found".into(),
                        candidates: None,
                    },
                ),
                "ListPanes" => Envelope::new(
                    "resp",
                    &ErrorResponse {
                        code: ErrorCode::TargetAmbiguous,
                        message: "ambiguous target".into(),
                        candidates: Some(vec!["dev/tab1/pane".into(), "dev/tab2/pane".into()]),
                    },
                ),
                _ => Envelope::new("resp", &OkResponse {}),
            }
        })
        .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = IpcClient::connect_to(&pipe_name).await.unwrap();

    // Success case
    let ok_resp = client
        .request(&Envelope::new("r1", &ListWorkspaces {}))
        .await
        .unwrap();
    let ok_result = output::format_response(&ok_resp, false);
    assert_eq!(ok_result.exit_code, exit_code::SUCCESS);

    // Not-found case
    let nf_resp = client
        .request(&Envelope::new(
            "r2",
            &Capture {
                target: "foo".into(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let nf_result = output::format_response(&nf_resp, false);
    assert_eq!(nf_result.exit_code, exit_code::TARGET_NOT_FOUND);
    assert!(nf_result.stderr.contains("pane not found"));

    // Ambiguous case
    let am_resp = client
        .request(&Envelope::new(
            "r3",
            &ListPanes {
                workspace: "dev".into(),
            },
        ))
        .await
        .unwrap();
    let am_result = output::format_response(&am_resp, false);
    assert_eq!(am_result.exit_code, exit_code::AMBIGUOUS_TARGET);
    assert!(am_result.stderr.contains("Candidates:"));
}

/// Auto-start triggers when pipe not found.
///
/// Tests that when no host pipe exists, `connect_and_handshake` calls the
/// auto-start path. Since no `wtd-host` binary is available in the test
/// environment, the auto-start fails with `HOST_START_FAILED`.
#[tokio::test]
async fn auto_start_triggers_when_pipe_not_found() {
    // Use a pipe name that definitely doesn't exist.
    let bogus_pipe = format!(
        r"\\.\pipe\wtd-auto-start-test-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst)
    );

    // ensure_host_running should fail since there's no host binary and no pipe
    let result = wtd_ipc::connect::ensure_host_running(&bogus_pipe).await;
    assert!(result.is_err(), "should fail when host not available");

    // The error should map to HOST_START_FAILED exit code
    match result.unwrap_err() {
        wtd_ipc::connect::ConnectError::HostNotFound => {
            // Auto-start was triggered, host binary not found
        }
        wtd_ipc::connect::ConnectError::StartupTimeout => {
            // Auto-start was triggered, host started but pipe never appeared
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

/// Client connects even when server starts slightly late (tests retry logic).
#[tokio::test]
async fn client_retries_until_server_available() {
    let pipe_name = unique_pipe_name();
    let name = pipe_name.clone();

    // Start server after 200ms delay
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        run_test_server(&name, |_| Envelope::new("resp", &OkResponse {})).await;
    });

    // Client should retry and connect despite the delay
    let mut client = IpcClient::connect_to(&pipe_name).await.unwrap();
    let resp = client
        .request(&Envelope::new("r1", &ListWorkspaces {}))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
}
