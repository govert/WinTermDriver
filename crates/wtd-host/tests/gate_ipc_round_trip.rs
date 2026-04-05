//! Gate integration test: Full headless round-trip via IPC (§23.1).
//!
//! Connects to host via IPC named pipe, opens a workspace, sends input to
//! a session, captures the visible screen buffer, and asserts the output
//! matches. This closes Slice 1.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::watch;
use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::load_workspace_definition;
use wtd_core::GlobalSettings;
use wtd_host::ipc_server::*;
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance};
use wtd_ipc::message::{
    self, Capture, CaptureResult, ClientType, ErrorCode, ErrorResponse, Handshake, HandshakeAck,
    MessagePayload, OkResponse, OpenWorkspace, OpenWorkspaceResult, TypedMessage,
};
use wtd_ipc::Envelope;

const SIMPLE_YAML: &str = include_str!("fixtures/simple-workspace.yaml");

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(2000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-ipc-{}-{}", std::process::id(), n)
}

// ── Handler ─────────────────────────────────────────────────────────────

struct GateState {
    workspace: Option<WorkspaceInstance>,
}

/// Request handler that supports OpenWorkspace, Send, and Capture for the
/// end-to-end gate test.
struct GateHandler {
    state: Mutex<GateState>,
}

impl GateHandler {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState { workspace: None }),
        }
    }
}

fn default_host_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(val) = std::env::var("USERPROFILE") {
        env.insert("USERPROFILE".to_string(), val);
    } else {
        env.insert("USERPROFILE".to_string(), r"C:\".to_string());
    }
    env
}

fn find_exe_windows(name: &str) -> bool {
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

impl RequestHandler for GateHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(_open) => {
                let def = match load_workspace_definition("gate-test.yaml", SIMPLE_YAML) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("load failed: {}", e),
                        ));
                    }
                };

                let gs = GlobalSettings::default();
                let env = default_host_env();

                let inst = match WorkspaceInstance::open(
                    WorkspaceInstanceId(200),
                    &def,
                    &gs,
                    &env,
                    find_exe_windows,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("open failed: {}", e),
                        ));
                    }
                };

                let instance_id = format!("{}", inst.id().0);
                self.state.lock().unwrap().workspace = Some(inst);

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
                let inst = match state.workspace.as_ref() {
                    Some(i) => i,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            "no workspace open",
                        ));
                    }
                };

                let pane_id = match inst.find_pane_by_name(&send.target) {
                    Some(id) => id,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::TargetNotFound,
                            &format!("pane '{}' not found", send.target),
                        ));
                    }
                };

                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "pane not attached",
                        ));
                    }
                };

                let session = match inst.session(&session_id) {
                    Some(s) => s,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "session not found",
                        ));
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
                        &format!("write failed: {}", e),
                    )),
                }
            }

            TypedMessage::Capture(capture) => {
                let mut state = self.state.lock().unwrap();
                let inst = match state.workspace.as_mut() {
                    Some(i) => i,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            "no workspace open",
                        ));
                    }
                };

                // Drain pending output from all sessions into screen buffers.
                for session in inst.sessions_mut().values_mut() {
                    session.process_pending_output();
                }

                let pane_id = match inst.find_pane_by_name(&capture.target) {
                    Some(id) => id,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::TargetNotFound,
                            &format!("pane '{}' not found", capture.target),
                        ));
                    }
                };

                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::SessionFailed,
                            "pane not attached",
                        ));
                    }
                };

                let text = inst
                    .session(&session_id)
                    .map(|s| s.screen().visible_text())
                    .unwrap_or_default();

                Some(Envelope::new(
                    &envelope.id,
                    &CaptureResult {
                        text,
                        ..Default::default()
                    },
                ))
            }

            _ => None,
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

async fn connect_client(pipe_name: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
    for _ in 0..200 {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return client,
            Err(e) if e.raw_os_error() == Some(2) => {
                // ERROR_FILE_NOT_FOUND — server not ready
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) if e.raw_os_error() == Some(231) => {
                // ERROR_PIPE_BUSY — retry
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("unexpected pipe connect error: {:?}", e),
        }
    }
    panic!("timed out waiting for pipe server");
}

async fn do_handshake(client: &mut tokio::net::windows::named_pipe::NamedPipeClient) {
    write_frame(
        client,
        &Envelope::new(
            "hs-1",
            &Handshake {
                client_type: ClientType::Cli,
                client_version: "1.0.0".to_owned(),
                protocol_version: PROTOCOL_VERSION,
            },
        ),
    )
    .await
    .unwrap();
    let ack = read_frame(client).await.unwrap();
    assert_eq!(ack.msg_type, HandshakeAck::TYPE_NAME);
}

// ── Test ────────────────────────────────────────────────────────────────

/// Full headless round-trip via IPC: open workspace → send input → capture → verify.
#[tokio::test]
async fn ipc_open_send_capture_round_trip() {
    let pipe_name = unique_pipe_name();
    let server =
        std::sync::Arc::new(IpcServer::new(pipe_name.clone(), GateHandler::new()).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // 1. Open workspace via IPC
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "gate-test".to_string(),
                file: None,
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // 2. Wait for startup command output ("echo GATE_MARKER" from fixture).
    //    Poll Capture until the startup marker appears.
    let startup_found =
        poll_capture_for(&mut client, "shell", "GATE_MARKER", Duration::from_secs(10)).await;
    assert!(
        startup_found,
        "startup command output 'GATE_MARKER' should appear via IPC Capture"
    );

    // 3. Send a unique command via IPC
    let marker = "IPC_ROUND_TRIP_9Z4K";
    write_frame(
        &mut client,
        &Envelope::new(
            "send-1",
            &message::Send {
                target: "shell".to_string(),
                text: format!("echo {}", marker),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let send_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        send_resp.msg_type,
        OkResponse::TYPE_NAME,
        "Send should return Ok, got: {} — {:?}",
        send_resp.msg_type,
        send_resp.payload
    );

    // 4. Poll Capture until the marker appears at least twice in the visible
    //    screen buffer (once from the command echo, once from the echo output).
    let final_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = final_text.matches(marker).count();
    assert!(
        count >= 2,
        "marker should appear at least twice (command echo + output), found {} times in:\n{}",
        count,
        final_text
    );

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Poll Capture requests until the target text appears, or timeout.
async fn poll_capture_for(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    target: &str,
    needle: &str,
    timeout: Duration,
) -> bool {
    let needle = needle.to_owned();
    let result = poll_capture_until(client, target, |t| t.contains(&needle), timeout).await;
    result.contains(&needle)
}

/// Poll Capture requests until a predicate on the captured text is satisfied.
/// Returns the last captured text.
async fn poll_capture_until(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    target: &str,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let start = tokio::time::Instant::now();
    let mut last_text = String::new();
    while start.elapsed() < timeout {
        last_text = do_capture(client, target).await;
        if predicate(&last_text) {
            return last_text;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    last_text
}

/// Send a Capture request and return the visible text.
async fn do_capture(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    target: &str,
) -> String {
    write_frame(
        client,
        &Envelope::new(
            "cap",
            &Capture {
                target: target.to_string(),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(client).await.unwrap();
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();
    cap.text
}
