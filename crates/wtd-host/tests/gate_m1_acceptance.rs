//! M1 Acceptance Gate — Headless workspace round-trip (§37.5)
//!
//! This test proves the M1 milestone: every step of the headless pipeline works
//! end-to-end without any UI component.
//!
//! Criteria validated:
//!   1. Workspace YAML is parsed into a WorkspaceDefinition
//!   2. Profile is resolved to a concrete launch spec (cmd.exe)
//!   3. ConPTY session is launched and reaches Running state
//!   4. Input is sent to the session via IPC (Send message)
//!   5. Screen buffer is populated with ConPTY output
//!   6. Capture returns the expected output via IPC

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

const WORKSPACE_YAML: &str = r#"
version: 1
name: m1-acceptance
description: "M1 acceptance test: single pane, cmd.exe"
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo M1_READY"
"#;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(5000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-m1-{}-{}", std::process::id(), n)
}

// ── Handler ──────────────────────────────────────────────────────────────

struct M1State {
    workspace: Option<WorkspaceInstance>,
}

struct M1Handler {
    state: Mutex<M1State>,
}

impl M1Handler {
    fn new() -> Self {
        Self {
            state: Mutex::new(M1State { workspace: None }),
        }
    }
}

fn host_env() -> HashMap<String, String> {
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

impl RequestHandler for M1Handler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(_open) => {
                // Criterion 1: Parse workspace YAML
                let def = match load_workspace_definition("m1-acceptance.yaml", WORKSPACE_YAML) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("YAML parse failed: {e}"),
                        ));
                    }
                };

                let gs = GlobalSettings::default();
                let env = host_env();

                // Criteria 2+3: Resolve profile & launch ConPTY session
                let inst = match WorkspaceInstance::open(
                    WorkspaceInstanceId(500),
                    &def,
                    &gs,
                    &env,
                    find_exe,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("workspace open failed: {e}"),
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
                let inst = state.workspace.as_ref()?;

                let pane_id = inst.find_pane_by_name(&send.target)?;
                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => return Some(error_envelope(&envelope.id, ErrorCode::SessionFailed, "pane not attached")),
                };

                let session = inst.session(&session_id)?;
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
                let inst = state.workspace.as_mut()?;

                for session in inst.sessions_mut().values_mut() {
                    session.process_pending_output();
                }

                let pane_id = inst.find_pane_by_name(&capture.target)?;
                let session_id = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => session_id.clone(),
                    _ => return None,
                };

                let text = inst
                    .session(&session_id)
                    .map(|s| s.screen().visible_text())
                    .unwrap_or_default();

                Some(Envelope::new(&envelope.id, &CaptureResult { text }))
            }

            _ => None,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

async fn connect_client(
    pipe_name: &str,
) -> tokio::net::windows::named_pipe::NamedPipeClient {
    for _ in 0..200 {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return client,
            Err(e) if e.raw_os_error() == Some(2) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("unexpected pipe connect error: {e:?}"),
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

async fn do_capture(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    target: &str,
) -> String {
    write_frame(
        client,
        &Envelope::new("cap", &Capture { target: target.to_string() }),
    )
    .await
    .unwrap();

    let resp = read_frame(client).await.unwrap();
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();
    cap.text
}

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

// ── M1 Acceptance Test ───────────────────────────────────────────────────

/// **M1 Acceptance Gate (§37.5)**
///
/// Proves the full headless round-trip pipeline:
///   YAML → profile resolution → ConPTY → IPC → screen buffer → capture
///
/// Steps:
///   1. Start an IPC server with the M1 handler
///   2. Connect a client and complete the handshake
///   3. Send OpenWorkspace — YAML is parsed, profile resolved, ConPTY launched
///   4. Poll Capture until startup output appears — screen buffer is populated
///   5. Send input via IPC (Send message)
///   6. Poll Capture until the input's output appears — capture returns expected output
#[tokio::test]
async fn m1_headless_round_trip_acceptance() {
    let pipe_name = unique_pipe_name();
    let handler = M1Handler::new();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // ── Connect and handshake ────────────────────────────────────────
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // ── Criterion 1+2+3: Parse YAML, resolve profile, launch ConPTY ─
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "m1-acceptance".to_string(),
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
        "M1 criterion 1-3: OpenWorkspace must succeed (YAML parsed, profile resolved, ConPTY launched). Got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // ── Criterion 5: Screen buffer is populated (startup command) ────
    let startup_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("M1_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        startup_text.contains("M1_READY"),
        "M1 criterion 5: Screen buffer must contain startup command output 'M1_READY'. Got:\n{}",
        startup_text
    );

    // ── Criterion 4: Send input via IPC ──────────────────────────────
    let marker = "M1_ACCEPT_7X2Q";
    write_frame(
        &mut client,
        &Envelope::new(
            "send-1",
            &message::Send {
                target: "shell".to_string(),
                text: format!("echo {marker}"),
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
        "M1 criterion 4: Send must succeed. Got: {} — {:?}",
        send_resp.msg_type,
        send_resp.payload
    );

    // ── Criterion 6: Capture returns expected output ─────────────────
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
        "M1 criterion 6: Capture must return the echoed marker at least twice (command echo + output). Found {count} in:\n{final_text}",
    );

    // ── Teardown ─────────────────────────────────────────────────────
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}
