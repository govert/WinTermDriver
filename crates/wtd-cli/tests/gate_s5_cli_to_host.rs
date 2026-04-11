//! Gate test for S5: CLI-to-host round-trip with real HostRequestHandler.
//!
//! Verifies the full CLI-to-host pipeline using the real `HostRequestHandler`
//! (not a test-only stub). Exercises open/send/capture/list/inspect/close
//! in sequence against a real host with live ConPTY sessions.
//!
//! This is the gate test for Slice 5 headless integration.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use wtd_cli::client::IpcClient;
use wtd_cli::exit_code;
use wtd_cli::output;

use wtd_core::GlobalSettings;

use wtd_host::ipc_server::IpcServer;
use wtd_host::request_handler::HostRequestHandler;

use wtd_ipc::message::*;
use wtd_ipc::Envelope;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(15000);

fn unique_pipe_name(instance_num: u64) -> String {
    format!(
        r"\\.\pipe\wtd-gate-s5-{}-{}",
        std::process::id(),
        instance_num
    )
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("s5-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── Test host with real handler ─────────────────────────────────────

struct TestHost {
    _server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    pipe_name: String,
    yaml_path: std::path::PathBuf,
    _tmp_dir: TempDir,
}

/// RAII temp directory that cleans up on drop.
struct TempDir {
    path: std::path::PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl TestHost {
    /// Start a real host with `HostRequestHandler` and a workspace YAML on disk.
    ///
    /// Uses explicit `file:` path for workspace opening (no CWD dependency)
    /// so tests are safe to run in parallel with other test binaries.
    async fn start() -> Self {
        // Allocate a unique number for this instance (used for both pipe and tmp dir).
        let instance_num = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp_dir = std::env::temp_dir().join(format!(
            "wtd-gate-s5-{}-{}",
            std::process::id(),
            instance_num,
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let yaml = r#"version: 1
name: s5-test
description: "S5 gate test: single pane with cmd.exe"
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo S5_READY"
"#;
        let yaml_path = tmp_dir.join("s5-test.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();

        let pipe_name = unique_pipe_name(instance_num);
        let handler = HostRequestHandler::new(GlobalSettings::default());
        let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let s = server.clone();
        tokio::spawn(async move { s.run(shutdown_rx).await });

        // Brief wait for server to start accepting connections.
        tokio::time::sleep(Duration::from_millis(50)).await;

        TestHost {
            _server: server,
            shutdown_tx,
            pipe_name,
            yaml_path,
            _tmp_dir: TempDir { path: tmp_dir },
        }
    }

    async fn connect(&self) -> IpcClient {
        IpcClient::connect_to(&self.pipe_name).await.unwrap()
    }

    /// Open the test workspace using an explicit file path.
    fn open_request(&self) -> OpenWorkspace {
        OpenWorkspace {
            name: Some("s5-test".to_string()),
            file: Some(self.yaml_path.to_string_lossy().to_string()),
            recreate: false,
            profile: None,
        }
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Send a request via IpcClient and return the response envelope.
async fn request(client: &mut IpcClient, payload: &impl MessagePayload) -> Envelope {
    let envelope = Envelope::new(&next_id(), payload);
    client.request(&envelope).await.unwrap()
}

/// Poll Capture until a predicate is satisfied on the captured text.
async fn poll_capture_until(
    client: &mut IpcClient,
    target: &str,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let start = tokio::time::Instant::now();
    let mut last_text = String::new();
    while start.elapsed() < timeout {
        let resp = request(
            client,
            &Capture {
                target: target.to_string(),
                ..Default::default()
            },
        )
        .await;
        if resp.msg_type == CaptureResult::TYPE_NAME {
            let cap: CaptureResult = resp.extract_payload().unwrap();
            last_text = cap.text;
            if predicate(&last_text) {
                return last_text;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    last_text
}

// ── Tests ───────────────────────────────────────────────────────────

/// Full S5 gate: open → send → capture → list → inspect → close via CLI client
/// against a real HostRequestHandler with live ConPTY sessions.
#[tokio::test]
async fn s5_cli_to_host_full_round_trip() {
    let host = TestHost::start().await;
    let mut client = host.connect().await;

    // ── 1. Open workspace (explicit file path) ─────────────────────
    let open_resp = request(&mut client, &host.open_request()).await;

    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    // Verify CLI output formatting for open.
    let open_out = output::format_response(&open_resp, false);
    assert_eq!(open_out.exit_code, exit_code::SUCCESS);
    assert!(
        open_out.stdout.contains("Opened workspace"),
        "open text output: {:?}",
        open_out.stdout,
    );

    // ── 2. Wait for startup command output ──────────────────────────
    let startup_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("S5_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        startup_text.contains("S5_READY"),
        "startup output should contain S5_READY, got:\n{}",
        startup_text,
    );

    // ── 3. Send input ───────────────────────────────────────────────
    let marker = "S5_GATE_MARKER_Q7W3";
    let send_resp = request(
        &mut client,
        &wtd_ipc::message::Send {
            target: "shell".to_string(),
            text: format!("echo {}", marker),
            newline: true,
        },
    )
    .await;

    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);
    let send_out = output::format_response(&send_resp, false);
    assert_eq!(send_out.exit_code, exit_code::SUCCESS);

    // ── 4. Capture output ───────────────────────────────────────────
    let capture_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = capture_text.matches(marker).count();
    assert!(
        count >= 2,
        "marker should appear at least twice (echo + output), found {} in:\n{}",
        count,
        capture_text,
    );

    // Verify CLI capture output formatting.
    let cap_resp = request(
        &mut client,
        &Capture {
            target: "shell".to_string(),
            ..Default::default()
        },
    )
    .await;
    let cap_out = output::format_response(&cap_resp, false);
    assert_eq!(cap_out.exit_code, exit_code::SUCCESS);
    assert!(
        cap_out.stdout.contains(marker),
        "capture text output should contain marker",
    );

    // ── 5. List instances ───────────────────────────────────────────
    let list_resp = request(&mut client, &ListInstances {}).await;
    assert_eq!(list_resp.msg_type, ListInstancesResult::TYPE_NAME);

    let list: ListInstancesResult = list_resp.extract_payload().unwrap();
    assert_eq!(list.instances.len(), 1);
    assert_eq!(list.instances[0].name, "s5-test");

    // Verify text formatting.
    let list_out = output::format_response(&list_resp, false);
    assert_eq!(list_out.exit_code, exit_code::SUCCESS);
    assert!(list_out.stdout.contains("s5-test"));
    assert!(list_out.stdout.contains("NAME"));

    // Verify JSON formatting.
    let list_json_out = output::format_response(&list_resp, true);
    assert_eq!(list_json_out.exit_code, exit_code::SUCCESS);
    let parsed: serde_json::Value = serde_json::from_str(&list_json_out.stdout).unwrap();
    assert_eq!(parsed["instances"][0]["name"], "s5-test");

    // ── 6. List panes ───────────────────────────────────────────────
    let panes_resp = request(
        &mut client,
        &ListPanes {
            workspace: "s5-test".to_string(),
        },
    )
    .await;
    assert_eq!(panes_resp.msg_type, ListPanesResult::TYPE_NAME);

    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 1);
    assert_eq!(panes.panes[0].name, "shell");
    assert_eq!(panes.panes[0].tab, "main");
    assert!(
        panes.panes[0].session_state.eq_ignore_ascii_case("running"),
        "expected running state, got: {}",
        panes.panes[0].session_state,
    );

    let panes_out = output::format_response(&panes_resp, false);
    assert_eq!(panes_out.exit_code, exit_code::SUCCESS);
    assert!(panes_out.stdout.contains("shell"));
    assert!(panes_out.stdout.contains("main"));

    // ── 7. List sessions ────────────────────────────────────────────
    let sessions_resp = request(
        &mut client,
        &ListSessions {
            workspace: "s5-test".to_string(),
        },
    )
    .await;
    assert_eq!(sessions_resp.msg_type, ListSessionsResult::TYPE_NAME);

    let sessions: ListSessionsResult = sessions_resp.extract_payload().unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert_eq!(sessions.sessions[0].pane, "shell");
    assert!(
        sessions.sessions[0].state.eq_ignore_ascii_case("running"),
        "expected running state, got: {}",
        sessions.sessions[0].state,
    );

    // ── 8. Inspect ──────────────────────────────────────────────────
    let inspect_resp = request(
        &mut client,
        &Inspect {
            target: "shell".to_string(),
        },
    )
    .await;
    assert_eq!(inspect_resp.msg_type, InspectResult::TYPE_NAME);

    let inspect: InspectResult = inspect_resp.extract_payload().unwrap();
    assert_eq!(inspect.data["paneName"], "shell");
    assert_eq!(inspect.data["workspace"], "s5-test");

    let inspect_out = output::format_response(&inspect_resp, false);
    assert_eq!(inspect_out.exit_code, exit_code::SUCCESS);
    assert!(inspect_out.stdout.contains("paneName"));
    assert!(inspect_out.stdout.contains("shell"));

    // ── 9. Close workspace ──────────────────────────────────────────
    let close_resp = request(
        &mut client,
        &CloseWorkspace {
            workspace: "s5-test".to_string(),
            kill: false,
        },
    )
    .await;
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    let close_out = output::format_response(&close_resp, false);
    assert_eq!(close_out.exit_code, exit_code::SUCCESS);

    // ── 10. Verify workspace is gone ────────────────────────────────
    let list_after = request(&mut client, &ListInstances {}).await;
    let list2: ListInstancesResult = list_after.extract_payload().unwrap();
    assert_eq!(list2.instances.len(), 0, "workspace should be closed");
}

/// Verify error handling: opening a nonexistent workspace returns the correct
/// error code and CLI exit code.
#[tokio::test]
async fn s5_open_nonexistent_workspace_error() {
    let host = TestHost::start().await;
    let mut client = host.connect().await;

    let resp = request(
        &mut client,
        &OpenWorkspace {
            name: Some("does-not-exist-xyz".to_string()),
            file: Some("/nonexistent/path/to/workspace.yaml".to_string()),
            recreate: false,
            profile: None,
        },
    )
    .await;

    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    // CLI output layer should map to TARGET_NOT_FOUND exit code.
    let out = output::format_response(&resp, false);
    assert_eq!(out.exit_code, exit_code::TARGET_NOT_FOUND);
    assert!(!out.stderr.is_empty());
}

/// Verify error handling: sending to a nonexistent pane returns TargetNotFound.
#[tokio::test]
async fn s5_send_to_nonexistent_pane_error() {
    let host = TestHost::start().await;
    let mut client = host.connect().await;

    let resp = request(
        &mut client,
        &wtd_ipc::message::Send {
            target: "nonexistent-pane".to_string(),
            text: "hello".to_string(),
            newline: true,
        },
    )
    .await;

    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    let out = output::format_response(&resp, false);
    assert_eq!(out.exit_code, exit_code::TARGET_NOT_FOUND);
}

/// Verify that opening a workspace via explicit file path works through the
/// CLI client layer with the real handler.
#[tokio::test]
async fn s5_open_via_file_path() {
    let host = TestHost::start().await;
    let mut client = host.connect().await;

    // Write a second workspace YAML to a separate temp file.
    let file_path = host._tmp_dir.path.join("file-test.yaml");
    let yaml = r#"version: 1
name: file-test
tabs:
  - name: main
    layout:
      type: pane
      name: editor
      session:
        profile: cmd
        startupCommand: "echo FILE_OPEN_OK"
"#;
    std::fs::write(&file_path, yaml).unwrap();

    let resp = request(
        &mut client,
        &OpenWorkspace {
            name: Some("file-test".to_string()),
            file: Some(file_path.to_string_lossy().to_string()),
            recreate: false,
            profile: None,
        },
    )
    .await;

    assert_eq!(
        resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "file-path open failed: {} — {:?}",
        resp.msg_type,
        resp.payload,
    );

    // Verify the session is live.
    let text = poll_capture_until(
        &mut client,
        "editor",
        |t| t.contains("FILE_OPEN_OK"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("FILE_OPEN_OK"));

    // Clean up.
    let _ = request(
        &mut client,
        &CloseWorkspace {
            workspace: "file-test".to_string(),
            kill: false,
        },
    )
    .await;
}

/// Verify JSON output mode works for all response types through the full pipeline.
#[tokio::test]
async fn s5_json_output_mode() {
    let host = TestHost::start().await;
    let mut client = host.connect().await;

    // Open workspace via explicit file path.
    let open_resp = request(&mut client, &host.open_request()).await;
    let open_json = output::format_response(&open_resp, true);
    assert_eq!(open_json.exit_code, exit_code::SUCCESS);
    let open_parsed: serde_json::Value = serde_json::from_str(&open_json.stdout).unwrap();
    assert!(open_parsed["instanceId"].is_string());

    // Wait for session to be ready.
    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("S5_READY"),
        Duration::from_secs(10),
    )
    .await;

    // List panes in JSON.
    let panes_resp = request(
        &mut client,
        &ListPanes {
            workspace: "s5-test".to_string(),
        },
    )
    .await;
    let panes_json = output::format_response(&panes_resp, true);
    assert_eq!(panes_json.exit_code, exit_code::SUCCESS);
    let panes_parsed: serde_json::Value = serde_json::from_str(&panes_json.stdout).unwrap();
    assert_eq!(panes_parsed["panes"][0]["name"], "shell");

    // Inspect in JSON.
    let inspect_resp = request(
        &mut client,
        &Inspect {
            target: "shell".to_string(),
        },
    )
    .await;
    let inspect_json = output::format_response(&inspect_resp, true);
    assert_eq!(inspect_json.exit_code, exit_code::SUCCESS);
    let inspect_parsed: serde_json::Value = serde_json::from_str(&inspect_json.stdout).unwrap();
    // InspectResult uses #[serde(flatten)] — data fields are at the top level.
    assert_eq!(inspect_parsed["paneName"], "shell");

    // Error in JSON.
    let err_resp = request(
        &mut client,
        &wtd_ipc::message::Send {
            target: "no-such-pane".to_string(),
            text: "hello".to_string(),
            newline: true,
        },
    )
    .await;
    let err_json = output::format_response(&err_resp, true);
    assert_eq!(err_json.exit_code, exit_code::TARGET_NOT_FOUND);
    let err_parsed: serde_json::Value = serde_json::from_str(&err_json.stdout).unwrap();
    assert_eq!(err_parsed["code"], "target-not-found");

    // Clean up.
    let _ = request(
        &mut client,
        &CloseWorkspace {
            workspace: "s5-test".to_string(),
            kill: false,
        },
    )
    .await;
}
