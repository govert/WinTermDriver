//! M2 Acceptance Gate — CLI-driven workspace (§37.5)
//!
//! This test proves the M2 milestone: every CLI command works end-to-end
//! against a real IPC server with live ConPTY sessions.
//!
//! Criteria validated (§37.5 M2):
//!   1. `wtd open dev` creates a workspace instance
//!   2. `wtd list panes dev` shows all panes
//!   3. `wtd send dev/server "echo hello"` delivers input
//!   4. `wtd capture dev/server` returns output
//!   5. `wtd inspect dev/server` shows metadata
//!   6. `wtd close dev --kill` tears down cleanly
//!   7. JSON output and exit codes are correct

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

// ── Workspace YAML fixture ─────────────────────────────────────────

/// Workspace matching the M2 acceptance scenario: named "dev" with a
/// pane named "server" (matching `wtd send dev/server ...`).
const DEV_YAML: &str = r#"
version: 1
name: dev
description: "M2 acceptance: CLI-driven workspace"
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
            startupCommand: "echo M2_READY"
        - type: pane
          name: logs
          session:
            profile: cmd
            startupCommand: "echo M2_LOGS_READY"
"#;

// ── Unique pipe naming ─────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(12000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-m2-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("m2-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── TestHost harness ───────────────────────────────────────────────

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

// ── Helpers ────────────────────────────────────────────────────────

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

/// Resolve a target string to a workspace instance and pane ID.
fn resolve_target<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    // "workspace/pane" form
    if let Some((ws_name, pane_name)) = target.split_once('/') {
        if let Some(inst) = workspaces.get(ws_name) {
            if let Some(pane_id) = inst.find_pane_by_name(pane_name) {
                return Some((inst, pane_id));
            }
        }
    }
    // bare pane name
    for inst in workspaces.values() {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((inst, pane_id));
        }
    }
    None
}

// ── M2 Handler ─────────────────────────────────────────────────────

struct M2State {
    workspaces: HashMap<String, WorkspaceInstance>,
    next_instance_id: u64,
}

struct M2Handler {
    state: Mutex<M2State>,
}

impl M2Handler {
    fn new() -> Self {
        Self {
            state: Mutex::new(M2State {
                workspaces: HashMap::new(),
                next_instance_id: 1,
            }),
        }
    }
}

impl RequestHandler for M2Handler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                let yaml = match open.name.as_str() {
                    "dev" => DEV_YAML,
                    _ => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            &format!("workspace '{}' not found", open.name),
                        ))
                    }
                };

                let def = match load_workspace_definition("test.yaml", yaml) {
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

            TypedMessage::ListPanes(lp) => {
                let state = self.state.lock().unwrap();
                let inst = match state.workspaces.get(&lp.workspace) {
                    Some(i) => i,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            &format!("workspace '{}' not found", lp.workspace),
                        ))
                    }
                };

                let mut panes = Vec::new();
                for tab in inst.tabs() {
                    for pane_id in tab.layout().panes() {
                        let name = inst.pane_name(&pane_id).unwrap_or("?").to_string();
                        let session_state = match inst.pane_state(&pane_id) {
                            Some(PaneState::Attached { session_id }) => inst
                                .session(session_id)
                                .map(|s| format!("{:?}", s.state()))
                                .unwrap_or_else(|| "unknown".to_string()),
                            Some(PaneState::Detached { error }) => {
                                format!("detached: {error}")
                            }
                            None => "none".to_string(),
                        };
                        panes.push(PaneInfo {
                            name,
                            tab: tab.name().to_string(),
                            session_state,
                        });
                    }
                }

                Some(Envelope::new(&envelope.id, &ListPanesResult { panes }))
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

                let text = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => inst
                        .session(session_id)
                        .map(|s| s.screen().visible_text())
                        .unwrap_or_default(),
                    _ => String::new(),
                };

                Some(Envelope::new(&envelope.id, &CaptureResult { text, ..Default::default() }))
            }

            TypedMessage::Inspect(inspect) => {
                let state = self.state.lock().unwrap();
                let (inst, pane_id) = match resolve_target(&state.workspaces, &inspect.target) {
                    Some(r) => r,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::TargetNotFound,
                            &format!("pane '{}' not found", inspect.target),
                        ))
                    }
                };

                let pane_name = inst.pane_name(&pane_id).unwrap_or("?");
                let session_state = match inst.pane_state(&pane_id) {
                    Some(PaneState::Attached { session_id }) => inst
                        .session(session_id)
                        .map(|s| format!("{:?}", s.state()))
                        .unwrap_or_else(|| "unknown".into()),
                    Some(PaneState::Detached { error }) => format!("detached: {error}"),
                    None => "none".into(),
                };

                let data = serde_json::json!({
                    "paneName": pane_name,
                    "paneId": format!("{}", pane_id),
                    "sessionState": session_state,
                    "workspace": inst.name(),
                });

                Some(Envelope::new(&envelope.id, &InspectResult { data }))
            }

            TypedMessage::ListInstances(_) => {
                let state = self.state.lock().unwrap();
                let instances: Vec<InstanceInfo> = state
                    .workspaces
                    .iter()
                    .map(|(name, inst)| InstanceInfo {
                        name: name.clone(),
                        instance_id: format!("{}", inst.id().0),
                    })
                    .collect();
                Some(Envelope::new(
                    &envelope.id,
                    &ListInstancesResult { instances },
                ))
            }

            _ => None,
        }
    }
}

// ── Polling helper ─────────────────────────────────────────────────

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
                    ..Default::default()
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

/// Assert JSON output is valid and return the parsed value.
fn assert_valid_json(resp: &Envelope) -> serde_json::Value {
    let fmt = output::format_response(resp, true);
    serde_json::from_str(&fmt.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON output should be valid for msg_type='{}': {e}\nOutput: {}",
            resp.msg_type, fmt.stdout
        )
    })
}

// ═══════════════════════════════════════════════════════════════════
// M2 Acceptance Gate Test (§37.5)
// ═══════════════════════════════════════════════════════════════════

/// **M2 Acceptance Gate (§37.5)**
///
/// Proves the full CLI-driven workspace pipeline:
///   1. `wtd open dev` → workspace instance created
///   2. `wtd list panes dev` → both panes shown
///   3. `wtd send dev/server "echo hello"` → input delivered
///   4. `wtd capture dev/server` → output returned
///   5. `wtd inspect dev/server` → metadata shown
///   6. `wtd close dev --kill` → clean teardown
///   7. JSON output valid and exit codes correct at every step
#[tokio::test]
async fn m2_cli_driven_workspace_acceptance() {
    let host = TestHost::start(M2Handler::new()).await;
    let mut client = host.connect().await;

    // ── Criterion 1: `wtd open dev` creates a workspace instance ──
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
        "M2 criterion 1: open must return OpenWorkspaceResult. Got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );
    let open_result: OpenWorkspaceResult = resp.extract_payload().unwrap();
    assert!(
        !open_result.instance_id.is_empty(),
        "M2 criterion 1: instance_id must be non-empty"
    );

    // Criterion 7 (open): exit code and output formatting
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: open text exit code");
    assert!(
        fmt.stdout.contains(&open_result.instance_id),
        "M2 criterion 7: open text output contains instance id"
    );

    let json = assert_valid_json(&resp);
    assert!(json["instanceId"].is_string(), "M2 criterion 7: open JSON has instanceId");
    assert!(json["state"].is_object(), "M2 criterion 7: open JSON has state");
    let json_fmt = output::format_response(&resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: open JSON exit code");

    // ── Wait for sessions to start ────────────────────────────────
    let startup_text = poll_capture_until(
        &mut client,
        "server",
        |t| t.contains("M2_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        startup_text.contains("M2_READY"),
        "M2: server pane startup command must produce output. Got:\n{}",
        startup_text
    );

    // ── Criterion 2: `wtd list panes dev` shows all panes ────────
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "dev".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        ListPanesResult::TYPE_NAME,
        "M2 criterion 2: list panes must return ListPanesResult"
    );
    let lp: ListPanesResult = resp.extract_payload().unwrap();
    assert_eq!(lp.panes.len(), 2, "M2 criterion 2: workspace has 2 panes");

    let pane_names: Vec<&str> = lp.panes.iter().map(|p| p.name.as_str()).collect();
    assert!(
        pane_names.contains(&"server"),
        "M2 criterion 2: pane 'server' must appear in list"
    );
    assert!(
        pane_names.contains(&"logs"),
        "M2 criterion 2: pane 'logs' must appear in list"
    );
    for p in &lp.panes {
        assert_eq!(p.tab, "main", "M2 criterion 2: all panes in tab 'main'");
    }

    // Criterion 7 (list panes): exit code and formatting
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: list panes text exit code");
    assert!(fmt.stdout.contains("server"), "M2 criterion 7: list panes text shows 'server'");
    assert!(fmt.stdout.contains("logs"), "M2 criterion 7: list panes text shows 'logs'");
    assert!(fmt.stdout.contains("TAB"), "M2 criterion 7: list panes has TAB header");
    assert!(fmt.stdout.contains("PANE"), "M2 criterion 7: list panes has PANE header");
    assert!(fmt.stdout.contains("STATE"), "M2 criterion 7: list panes has STATE header");

    let json = assert_valid_json(&resp);
    let panes_arr = json["panes"].as_array().unwrap();
    assert_eq!(panes_arr.len(), 2, "M2 criterion 7: list panes JSON has 2 entries");
    for p in panes_arr {
        assert!(p["name"].is_string(), "M2 criterion 7: pane JSON has name");
        assert!(p["tab"].is_string(), "M2 criterion 7: pane JSON has tab");
        assert!(p["sessionState"].is_string(), "M2 criterion 7: pane JSON has sessionState");
    }

    // ── Criterion 3: `wtd send dev/server "echo hello"` delivers input ──
    let marker = "M2_ACCEPT_HELLO_9K4R";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "dev/server".to_string(),
                text: format!("echo {marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        OkResponse::TYPE_NAME,
        "M2 criterion 3: send must return Ok. Got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );

    // Criterion 7 (send): exit code
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: send text exit code");

    // ── Criterion 4: `wtd capture dev/server` returns output ─────
    let final_text = poll_capture_until(
        &mut client,
        "dev/server",
        |t| t.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = final_text.matches(marker).count();
    assert!(
        count >= 2,
        "M2 criterion 4: capture must return the echoed marker at least twice (command echo + output). Found {} in:\n{}",
        count,
        final_text
    );

    // Final capture for formatting checks
    let capture_resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "dev/server".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        capture_resp.msg_type,
        CaptureResult::TYPE_NAME,
        "M2 criterion 4: capture must return CaptureResult"
    );

    // Criterion 7 (capture): exit code and formatting
    let fmt = output::format_response(&capture_resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: capture text exit code");
    assert!(!fmt.stdout.is_empty(), "M2 criterion 7: capture text output non-empty");
    assert!(
        fmt.stdout.contains(marker),
        "M2 criterion 7: capture text contains marker"
    );

    let json = assert_valid_json(&capture_resp);
    assert!(json["text"].is_string(), "M2 criterion 7: capture JSON has text field");
    let json_fmt = output::format_response(&capture_resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: capture JSON exit code");

    // ── Criterion 5: `wtd inspect dev/server` shows metadata ─────
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "dev/server".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        InspectResult::TYPE_NAME,
        "M2 criterion 5: inspect must return InspectResult"
    );
    let inspect: InspectResult = resp.extract_payload().unwrap();
    assert_eq!(
        inspect.data["paneName"], "server",
        "M2 criterion 5: inspect shows correct pane name"
    );
    assert_eq!(
        inspect.data["workspace"], "dev",
        "M2 criterion 5: inspect shows correct workspace name"
    );
    assert!(
        inspect.data["sessionState"].is_string(),
        "M2 criterion 5: inspect shows session state"
    );
    assert!(
        inspect.data["paneId"].is_string(),
        "M2 criterion 5: inspect shows pane ID"
    );

    // Criterion 7 (inspect): exit code and formatting
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: inspect text exit code");
    assert!(
        fmt.stdout.contains("server"),
        "M2 criterion 7: inspect text mentions pane name"
    );

    let json = assert_valid_json(&resp);
    assert_eq!(json["paneName"], "server", "M2 criterion 7: inspect JSON paneName");
    assert_eq!(json["workspace"], "dev", "M2 criterion 7: inspect JSON workspace");
    assert!(json["paneId"].is_string(), "M2 criterion 7: inspect JSON paneId");
    assert!(json["sessionState"].is_string(), "M2 criterion 7: inspect JSON sessionState");
    let json_fmt = output::format_response(&resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: inspect JSON exit code");

    // ── Criterion 6: `wtd close dev --kill` tears down cleanly ───
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
    assert_eq!(
        resp.msg_type,
        OkResponse::TYPE_NAME,
        "M2 criterion 6: close must return Ok. Got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );

    // Criterion 7 (close): exit code
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "M2 criterion 7: close text exit code");

    // Verify teardown: list instances should be empty
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    let li: ListInstancesResult = resp.extract_payload().unwrap();
    assert_eq!(
        li.instances.len(),
        0,
        "M2 criterion 6: no instances remain after close --kill"
    );

    // ── Criterion 7 supplemental: error exit codes ───────────────

    // Target not found → exit code 2
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "dev/nonexistent".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(
        fmt.exit_code,
        exit_code::TARGET_NOT_FOUND,
        "M2 criterion 7: target-not-found → exit code 2"
    );
    assert!(!fmt.stderr.is_empty(), "M2 criterion 7: error produces stderr");

    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "target-not-found", "M2 criterion 7: error JSON code");
    assert!(json["message"].is_string(), "M2 criterion 7: error JSON message");

    // Workspace not found → exit code 2
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "nonexistent".to_string(),
                kill: false,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(
        fmt.exit_code,
        exit_code::TARGET_NOT_FOUND,
        "M2 criterion 7: workspace-not-found → exit code 2"
    );
}
