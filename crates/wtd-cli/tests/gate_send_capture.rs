//! Gate: Verify `wtd send` and `wtd capture` work end-to-end (§22.3).
//!
//! Proves CLI-to-host-to-session round-trip: `wtd send dev/server 'echo hello'`
//! delivers input to the ConPTY session, and `wtd capture dev/server` returns
//! output containing 'hello'.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;

use wtd_cli::client::IpcClient;
use wtd_cli::exit_code;
use wtd_cli::output;

use wtd_core::ids::{PaneId, WorkspaceInstanceId};
use wtd_core::load_workspace_definition;
use wtd_core::GlobalSettings;

use wtd_host::ipc_server::{ClientId, IpcServer, RequestHandler};
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance};

use wtd_ipc::message::{self, *};
use wtd_ipc::Envelope;

use std::sync::Arc;

// ── Workspace YAML fixtures ─────────────────────────────────────────

/// Workspace named "dev" with a single pane named "server" running cmd.exe.
const DEV_WORKSPACE_YAML: &str = r#"
version: 1
name: dev
description: "Gate test: dev workspace with server pane"
tabs:
  - name: main
    layout:
      type: pane
      name: server
      session:
        profile: cmd
        startupCommand: "echo GATE_SEND_READY"
"#;

/// Multi-pane workspace named "dev" with "server" and "worker" panes.
const DEV_SPLIT_YAML: &str = r#"
version: 1
name: dev
description: "Gate test: dev workspace with split panes"
tabs:
  - name: main
    layout:
      type: split
      orientation: vertical
      ratio: 0.5
      children:
        - type: pane
          name: server
          session:
            profile: cmd
            startupCommand: "echo SERVER_READY"
        - type: pane
          name: worker
          session:
            profile: cmd
            startupCommand: "echo WORKER_READY"
"#;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(8000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-sc-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("gate-sc-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── TestHost harness ────────────────────────────────────────────────

struct TestHost {
    _server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    pipe_name: String,
}

impl TestHost {
    async fn start(handler: impl RequestHandler) -> Self {
        let pipe_name = unique_pipe_name();
        let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let s = server.clone();
        tokio::spawn(async move { s.run(shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        TestHost {
            _server: server,
            shutdown_tx,
            pipe_name,
        }
    }

    async fn connect(&self) -> IpcClient {
        IpcClient::connect_to(&self.pipe_name).await.unwrap()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn default_host_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(val) = std::env::var("USERPROFILE") {
        env.insert("USERPROFILE".to_string(), val);
    } else {
        env.insert("USERPROFILE".to_string(), r"C:\".to_string());
    }
    env
}

fn find_exe(name: &str) -> bool {
    matches!(name, "cmd.exe" | "powershell.exe" | "pwsh.exe")
}

fn error_envelope(id: &str, code: ErrorCode, message: &str) -> Envelope {
    Envelope::new(
        id,
        &ErrorResponse {
            code,
            message: message.to_owned(),
            candidates: None,
        },
    )
}

/// Resolve a target string to a pane. Supports:
/// - Direct pane name: "server"
/// - Workspace/pane path: "dev/server"
fn resolve_target<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    // Try workspace/pane format first
    if let Some((ws_name, pane_name)) = target.split_once('/') {
        if let Some(inst) = workspaces.get(ws_name) {
            if let Some(pane_id) = inst.find_pane_by_name(pane_name) {
                return Some((inst, pane_id));
            }
        }
    }
    // Fall back to searching all workspaces by pane name
    for inst in workspaces.values() {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((inst, pane_id));
        }
    }
    None
}

// ── Gate Handler ────────────────────────────────────────────────────

struct GateState {
    workspaces: HashMap<String, WorkspaceInstance>,
    next_instance_id: u64,
}

struct GateHandler {
    state: Mutex<GateState>,
    yaml: &'static str,
}

impl GateHandler {
    fn new(yaml: &'static str) -> Self {
        Self {
            state: Mutex::new(GateState {
                workspaces: HashMap::new(),
                next_instance_id: 1,
            }),
            yaml,
        }
    }
}

impl RequestHandler for GateHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                let def = match load_workspace_definition("test.yaml", self.yaml) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("load failed: {e}"),
                        ))
                    }
                };

                let mut state = self.state.lock().unwrap();
                let inst_id = state.next_instance_id;
                state.next_instance_id += 1;

                let inst = match WorkspaceInstance::open(
                    WorkspaceInstanceId(inst_id),
                    &def,
                    &GlobalSettings::default(),
                    &default_host_env(),
                    find_exe,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("open failed: {e}"),
                        ))
                    }
                };

                let instance_id = format!("{}", inst.id().0);
                state.workspaces.insert(open.name.clone(), inst);

                Some(Envelope::new(
                    &envelope.id,
                    &OpenWorkspaceResult {
                        instance_id,
                        state: serde_json::Value::Object(serde_json::Map::new()),
                    },
                ))
            }

            TypedMessage::Send(send) => {
                let state = self.state.lock().unwrap();

                let (inst, pane_id) = match resolve_target(&state.workspaces, &send.target) {
                    Some(r) => r,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::TargetNotFound,
                            &format!("pane '{}' not found", send.target),
                        ))
                    }
                };

                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "pane not attached",
                        ))
                    }
                };

                let session = match inst.session(&session_id) {
                    Some(s) => s,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "session not found",
                        ))
                    }
                };

                let mut input = send.text.clone();
                if send.newline {
                    input.push_str("\r\n");
                }

                match session.write_input(input.as_bytes()) {
                    Ok(()) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    Err(e) => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::SessionFailed,
                        &format!("write failed: {e}"),
                    )),
                }
            }

            TypedMessage::Capture(capture) => {
                let mut state = self.state.lock().unwrap();

                // Drain pending output from all sessions into screen buffers
                for inst in state.workspaces.values_mut() {
                    for session in inst.sessions_mut().values_mut() {
                        session.process_pending_output();
                    }
                }

                let (inst, pane_id) = match resolve_target(&state.workspaces, &capture.target) {
                    Some(r) => r,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::TargetNotFound,
                            &format!("pane '{}' not found", capture.target),
                        ))
                    }
                };

                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "pane not attached",
                        ))
                    }
                };

                let text = inst
                    .session(&session_id)
                    .map(|s| s.screen().visible_text())
                    .unwrap_or_default();

                Some(Envelope::new(&envelope.id, &CaptureResult { text }))
            }

            TypedMessage::CloseWorkspace(close) => {
                let mut state = self.state.lock().unwrap();
                match state.workspaces.remove(&close.workspace) {
                    Some(mut inst) => {
                        inst.close();
                        Some(Envelope::new(&envelope.id, &OkResponse {}))
                    }
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::WorkspaceNotFound,
                        &format!("workspace '{}' not found", close.workspace),
                    )),
                }
            }

            _ => None,
        }
    }
}

// ── Polling helper ──────────────────────────────────────────────────

async fn poll_capture_until(
    client: &mut IpcClient,
    target: &str,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let start = tokio::time::Instant::now();
    let mut last_text = String::new();
    while start.elapsed() < timeout {
        let resp = client
            .request(&Envelope::new(
                &next_id(),
                &Capture {
                    target: target.to_string(),
                },
            ))
            .await
            .unwrap();
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

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

/// Core gate test: send input via CLI IPC, capture output, verify round-trip.
///
/// Equivalent to: `wtd send dev/server 'echo hello'` then `wtd capture dev/server`
/// and verifying the output contains 'hello'.
#[tokio::test]
async fn send_and_capture_round_trip() {
    let handler = GateHandler::new(DEV_WORKSPACE_YAML);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // 1. Open workspace "dev"
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "dev".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "open should succeed; got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );

    // 2. Wait for startup command to appear — proves session is alive
    let text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.contains("GATE_SEND_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.contains("GATE_SEND_READY"),
        "startup marker should appear in capture"
    );

    // 3. Send input: `echo hello` — the core operation under test
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/server".to_string(),
                text: "echo hello".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        OkResponse::TYPE_NAME,
        "send should return Ok; got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );

    // Verify text-mode output formatting for send response
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // 4. Capture output and verify 'hello' appears
    //    cmd.exe echoes the command ("echo hello") and then prints the output ("hello"),
    //    so the marker appears at least twice.
    let text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.matches("hello").count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = text.matches("hello").count();
    assert!(
        count >= 2,
        "'hello' should appear at least twice (command echo + output), found {} times in:\n{}",
        count,
        text
    );

    // 5. Verify capture response formatting
    let capture_resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "dev/server".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(capture_resp.msg_type, CaptureResult::TYPE_NAME);

    // Text-mode formatting
    let fmt = output::format_response(&capture_resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(
        fmt.stdout.contains("hello"),
        "text output should contain 'hello'"
    );

    // JSON-mode formatting
    let fmt_json = output::format_response(&capture_resp, true);
    assert_eq!(fmt_json.exit_code, exit_code::SUCCESS);
    assert!(
        fmt_json.stdout.contains("hello"),
        "JSON output should contain 'hello'"
    );

    // 6. Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "dev".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
}

/// Verify send to a nonexistent target returns TargetNotFound with correct exit code.
#[tokio::test]
async fn send_to_nonexistent_target_returns_error() {
    let handler = GateHandler::new(DEV_WORKSPACE_YAML);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // Open workspace first
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "dev".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Send to nonexistent pane
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/nonexistent".to_string(),
                text: "echo oops".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

/// Verify capture of a nonexistent target returns TargetNotFound with correct exit code.
#[tokio::test]
async fn capture_nonexistent_target_returns_error() {
    let handler = GateHandler::new(DEV_WORKSPACE_YAML);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // Open workspace first
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "dev".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Capture nonexistent pane
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "dev/nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

/// Verify send+capture works with multi-pane layout — input goes to the
/// correct pane and capture returns only that pane's output.
#[tokio::test]
async fn send_and_capture_targets_correct_pane_in_split() {
    let handler = GateHandler::new(DEV_SPLIT_YAML);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // Open the split workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "dev".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait for both panes to start
    let text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.contains("SERVER_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("SERVER_READY"));

    let text = poll_capture_until(
        &mut client,
        "dev/worker",
        |t| t.contains("WORKER_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("WORKER_READY"));

    // Send a unique marker to "server" pane only
    let server_marker = "SRVMARK_7K2Q";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/server".to_string(),
                text: format!("echo {server_marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    // Wait for marker to appear in server's capture
    let server_text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.matches(server_marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        server_text.contains(server_marker),
        "server pane should contain the marker"
    );

    // Verify worker pane does NOT contain the server's marker
    let worker_text = poll_capture_until(
        &mut client,
        "dev/worker",
        |_| true, // just capture once
        Duration::from_secs(1),
    )
    .await;
    assert!(
        !worker_text.contains(server_marker),
        "worker pane should NOT contain server's marker; worker text:\n{}",
        worker_text
    );

    // Send a different marker to "worker" pane
    let worker_marker = "WRKMARK_3P8X";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/worker".to_string(),
                text: format!("echo {worker_marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    // Wait for marker in worker's capture
    let worker_text = poll_capture_until(
        &mut client,
        "dev/worker",
        |t| t.matches(worker_marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        worker_text.contains(worker_marker),
        "worker pane should contain its marker"
    );

    // Server pane should NOT contain worker's marker
    let server_text = poll_capture_until(
        &mut client,
        "dev/server",
        |_| true,
        Duration::from_secs(1),
    )
    .await;
    assert!(
        !server_text.contains(worker_marker),
        "server pane should NOT contain worker's marker; server text:\n{}",
        server_text
    );

    // Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "dev".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
}

/// Verify --no-newline flag: send without trailing newline doesn't execute the command.
#[tokio::test]
async fn send_no_newline_does_not_execute() {
    let handler = GateHandler::new(DEV_WORKSPACE_YAML);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // Open workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "dev".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait for session to start
    let _ = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.contains("GATE_SEND_READY"),
        Duration::from_secs(10),
    )
    .await;

    // Send with no newline — the command text should appear (typed into prompt)
    // but should NOT execute (no output line)
    let marker = "NONEWLINE_5X9W";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/server".to_string(),
                text: format!("echo {marker}"),
                newline: false, // key flag
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    // Give it a moment for the typed text to appear
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Capture — the typed text appears once (in the prompt) but NOT as executed output
    let text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.contains(marker),
        Duration::from_secs(5),
    )
    .await;

    let count = text.matches(marker).count();
    assert!(
        count <= 1,
        "with --no-newline, marker should appear at most once (prompt echo only), found {} in:\n{}",
        count,
        text
    );

    // Now send a newline to execute it, proving the text was pending
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/server".to_string(),
                text: String::new(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    // Now the marker should appear at least twice (command + output)
    let text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = text.matches(marker).count();
    assert!(
        count >= 2,
        "after newline, marker should appear at least twice, found {} in:\n{}",
        count,
        text
    );

    // Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "dev".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
}
