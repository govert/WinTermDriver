//! Gate: Comprehensive verification of all `wtd` subcommands (§22).
//!
//! Closes Slice 2. Every CLI subcommand is tested end-to-end against a real
//! IPC server with live ConPTY sessions. Validates:
//! - Correct IPC message type in response
//! - Text-mode output formatting (human-readable)
//! - JSON-mode output is valid JSON with correct structure
//! - Exit codes match §22.9

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

// ── Workspace YAML fixtures ─────────────────────────────────────────

/// Multi-pane workspace for comprehensive testing.
const GATE_YAML: &str = r#"
version: 1
name: gate-s2
description: "S2 gate: comprehensive command verification"
tabs:
  - name: work
    layout:
      type: split
      orientation: vertical
      ratio: 0.5
      children:
        - type: pane
          name: editor
          session:
            profile: cmd
            startupCommand: "echo S2_EDITOR_READY"
        - type: pane
          name: terminal
          session:
            profile: cmd
            startupCommand: "echo S2_TERMINAL_READY"
"#;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(9000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-s2-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("gs2-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
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

fn error_envelope_with_candidates(
    id: &str,
    code: ErrorCode,
    message: &str,
    candidates: Vec<String>,
) -> Envelope {
    Envelope::new(
        id,
        &ErrorResponse {
            code,
            message: message.to_owned(),
            candidates: Some(candidates),
        },
    )
}

/// Resolve a target string to a pane. Supports:
/// - Direct pane name: "editor"
/// - Workspace/pane path: "gate-s2/editor"
fn resolve_target<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    if let Some((ws_name, pane_name)) = target.split_once('/') {
        if let Some(inst) = workspaces.get(ws_name) {
            if let Some(pane_id) = inst.find_pane_by_name(pane_name) {
                return Some((inst, pane_id));
            }
        }
    }
    for inst in workspaces.values() {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((inst, pane_id));
        }
    }
    None
}

/// Get visible text for a pane's session.
fn get_pane_screen_text(inst: &WorkspaceInstance, pane_id: &PaneId) -> String {
    match inst.pane_state(pane_id) {
        Some(PaneState::Attached { session_id }) => inst
            .session(session_id)
            .map(|s| s.screen().visible_text())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Get scrollback lines for a pane's session.
fn get_pane_scrollback(inst: &WorkspaceInstance, pane_id: &PaneId, tail: u32) -> Vec<String> {
    match inst.pane_state(pane_id) {
        Some(PaneState::Attached { session_id }) => {
            let screen = match inst.session(session_id) {
                Some(s) => s.screen(),
                None => return Vec::new(),
            };
            let total = screen.scrollback_len();
            let start = total.saturating_sub(tail as usize);
            (start..total)
                .filter_map(|idx| {
                    screen.scrollback_row(idx).map(|cells| {
                        cells
                            .iter()
                            .filter(|c| !c.wide_continuation)
                            .map(|c| c.character)
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

// ── S2 Gate Handler ─────────────────────────────────────────────────

struct GateState {
    workspaces: HashMap<String, WorkspaceInstance>,
    next_instance_id: u64,
}

struct S2Handler {
    state: Mutex<GateState>,
}

impl S2Handler {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                workspaces: HashMap::new(),
                next_instance_id: 1,
            }),
        }
    }
}

impl RequestHandler for S2Handler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                let yaml = match open.name.as_str() {
                    "gate-s2" => GATE_YAML,
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

                if open.recreate {
                    if let Some(mut old) = state.workspaces.remove(&open.name) {
                        old.close();
                    }
                }

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

            TypedMessage::AttachWorkspace(attach) => {
                let state = self.state.lock().unwrap();
                match state.workspaces.get(&attach.workspace) {
                    Some(_) => Some(Envelope::new(
                        &envelope.id,
                        &AttachWorkspaceResult {
                            state: serde_json::Value::Object(serde_json::Map::new()),
                        },
                    )),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::WorkspaceNotFound,
                        &format!("workspace '{}' not found", attach.workspace),
                    )),
                }
            }

            TypedMessage::RecreateWorkspace(recreate) => {
                let state = self.state.lock().unwrap();
                if state.workspaces.contains_key(&recreate.workspace) {
                    Some(Envelope::new(
                        &envelope.id,
                        &RecreateWorkspaceResult {
                            instance_id: "recreated".to_string(),
                            state: serde_json::Value::Object(serde_json::Map::new()),
                        },
                    ))
                } else {
                    Some(error_envelope(
                        &envelope.id,
                        ErrorCode::WorkspaceNotFound,
                        &format!("workspace '{}' not found", recreate.workspace),
                    ))
                }
            }

            TypedMessage::SaveWorkspace(save) => {
                let state = self.state.lock().unwrap();
                match state.workspaces.get(&save.workspace) {
                    Some(_) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::WorkspaceNotFound,
                        &format!("workspace '{}' not found", save.workspace),
                    )),
                }
            }

            TypedMessage::ListWorkspaces(_) => Some(Envelope::new(
                &envelope.id,
                &ListWorkspacesResult {
                    workspaces: vec![
                        WorkspaceInfo {
                            name: "gate-s2".to_string(),
                            source: "local".to_string(),
                        },
                        WorkspaceInfo {
                            name: "other-project".to_string(),
                            source: "user".to_string(),
                        },
                    ],
                },
            )),

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

            TypedMessage::ListSessions(ls) => {
                let state = self.state.lock().unwrap();
                let inst = match state.workspaces.get(&ls.workspace) {
                    Some(i) => i,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            &format!("workspace '{}' not found", ls.workspace),
                        ))
                    }
                };

                let sessions: Vec<SessionInfo> = inst
                    .sessions()
                    .iter()
                    .map(|(id, session)| SessionInfo {
                        session_id: format!("{}", id.0),
                        pane: session.name().to_string(),
                        state: format!("{:?}", session.state()),
                    })
                    .collect();

                Some(Envelope::new(
                    &envelope.id,
                    &ListSessionsResult { sessions },
                ))
            }

            TypedMessage::Send(send) => {
                // Ambiguous target simulation
                if send.target == "AMBIGUOUS" {
                    return Some(error_envelope_with_candidates(
                        &envelope.id,
                        ErrorCode::TargetAmbiguous,
                        "ambiguous target 'AMBIGUOUS'",
                        vec!["gate-s2/work/editor".into(), "gate-s2/work/terminal".into()],
                    ));
                }

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
                if capture.target == "AMBIGUOUS" {
                    return Some(error_envelope_with_candidates(
                        &envelope.id,
                        ErrorCode::TargetAmbiguous,
                        "ambiguous target 'AMBIGUOUS'",
                        vec!["gate-s2/work/editor".into(), "gate-s2/work/terminal".into()],
                    ));
                }

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

                let text = get_pane_screen_text(inst, &pane_id);
                Some(Envelope::new(&envelope.id, &CaptureResult { text, ..Default::default() }))
            }

            TypedMessage::Scrollback(scrollback) => {
                let mut state = self.state.lock().unwrap();
                for inst in state.workspaces.values_mut() {
                    for session in inst.sessions_mut().values_mut() {
                        session.process_pending_output();
                    }
                }

                let (inst, pane_id) =
                    match resolve_target(&state.workspaces, &scrollback.target) {
                        Some(r) => r,
                        None => {
                            return Some(error_envelope(
                                &envelope.id,
                                ErrorCode::TargetNotFound,
                                &format!("pane '{}' not found", scrollback.target),
                            ))
                        }
                    };

                let lines = get_pane_scrollback(inst, &pane_id, scrollback.tail);
                Some(Envelope::new(
                    &envelope.id,
                    &ScrollbackResult { lines },
                ))
            }

            TypedMessage::Follow(follow) => {
                let state = self.state.lock().unwrap();
                match resolve_target(&state.workspaces, &follow.target) {
                    Some(_) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", follow.target),
                    )),
                }
            }

            TypedMessage::Inspect(inspect) => {
                let state = self.state.lock().unwrap();
                let (inst, pane_id) = match resolve_target(&state.workspaces, &inspect.target)
                {
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

            TypedMessage::FocusPane(focus) => {
                let state = self.state.lock().unwrap();
                match resolve_target(&state.workspaces, &focus.pane_id) {
                    Some(_) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", focus.pane_id),
                    )),
                }
            }

            TypedMessage::RenamePane(rename) => {
                let mut state = self.state.lock().unwrap();
                for inst in state.workspaces.values_mut() {
                    if let Some(pane_id) = inst.find_pane_by_name(&rename.pane_id) {
                        inst.rename_pane(&pane_id, rename.new_name.clone());
                        return Some(Envelope::new(&envelope.id, &OkResponse {}));
                    }
                }
                Some(error_envelope(
                    &envelope.id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", rename.pane_id),
                ))
            }

            TypedMessage::InvokeAction(_) => {
                Some(Envelope::new(&envelope.id, &OkResponse {}))
            }

            TypedMessage::Keys(keys) => {
                let state = self.state.lock().unwrap();
                match resolve_target(&state.workspaces, &keys.target) {
                    Some(_) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", keys.target),
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

/// Helper: assert JSON output is valid and return the parsed value.
fn assert_valid_json(resp: &Envelope) -> serde_json::Value {
    let fmt = output::format_response(resp, true);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON output should be valid JSON for msg_type='{}': {e}\nOutput: {}",
            resp.msg_type, fmt.stdout
        )
    });
    json
}

// ═══════════════════════════════════════════════════════════════════
// Gate Tests — S2 Comprehensive CLI Command Verification
// ═══════════════════════════════════════════════════════════════════

// ── 1. Full command sweep: every subcommand exercised in one lifecycle ──

/// S2 gate: exercises every CLI subcommand in sequence against a real
/// workspace with live ConPTY sessions. Verifies IPC response types,
/// text and JSON output formatting, and exit codes for all commands.
#[tokio::test]
async fn s2_all_commands_full_lifecycle() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // ── open ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "gate-s2".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);
    let open_result: OpenWorkspaceResult = resp.extract_payload().unwrap();
    assert!(!open_result.instance_id.is_empty());

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "open: text exit code");
    assert!(fmt.stdout.contains(&open_result.instance_id));

    let json = assert_valid_json(&resp);
    assert!(json["instanceId"].is_string(), "open: JSON has instanceId");

    // ── list workspaces ──
    let resp = client
        .request(&Envelope::new(&next_id(), &ListWorkspaces {}))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListWorkspacesResult::TYPE_NAME);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "list workspaces: exit code");
    assert!(fmt.stdout.contains("gate-s2"), "list workspaces: text contains name");
    assert!(fmt.stdout.contains("NAME"), "list workspaces: text has header");

    let json = assert_valid_json(&resp);
    assert!(json["workspaces"].is_array());
    let ws_arr = json["workspaces"].as_array().unwrap();
    assert_eq!(ws_arr.len(), 2);
    assert!(ws_arr[0]["name"].is_string());
    assert!(ws_arr[0]["source"].is_string());

    // ── list instances ──
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListInstancesResult::TYPE_NAME);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "list instances: exit code");
    assert!(fmt.stdout.contains("gate-s2"));

    let json = assert_valid_json(&resp);
    assert!(json["instances"].is_array());
    let inst_arr = json["instances"].as_array().unwrap();
    assert_eq!(inst_arr.len(), 1);
    assert!(inst_arr[0]["name"].is_string());
    assert!(inst_arr[0]["instanceId"].is_string());

    // ── list panes ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListPanesResult::TYPE_NAME);
    let lp: ListPanesResult = resp.extract_payload().unwrap();
    assert_eq!(lp.panes.len(), 2, "split workspace has 2 panes");

    let pane_names: Vec<&str> = lp.panes.iter().map(|p| p.name.as_str()).collect();
    assert!(pane_names.contains(&"editor"));
    assert!(pane_names.contains(&"terminal"));
    for p in &lp.panes {
        assert_eq!(p.tab, "work", "all panes in tab 'work'");
    }

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "list panes: exit code");
    assert!(fmt.stdout.contains("editor"));
    assert!(fmt.stdout.contains("terminal"));

    let json = assert_valid_json(&resp);
    assert!(json["panes"].is_array());
    let panes_arr = json["panes"].as_array().unwrap();
    assert_eq!(panes_arr.len(), 2);
    assert!(panes_arr[0]["name"].is_string());
    assert!(panes_arr[0]["tab"].is_string());
    assert!(panes_arr[0]["sessionState"].is_string());

    // ── list sessions ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListSessionsResult::TYPE_NAME);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "list sessions: exit code");

    let json = assert_valid_json(&resp);
    assert!(json["sessions"].is_array());
    let sess_arr = json["sessions"].as_array().unwrap();
    assert_eq!(sess_arr.len(), 2, "2 sessions for 2 panes");
    assert!(sess_arr[0]["sessionId"].is_string());
    assert!(sess_arr[0]["pane"].is_string());
    assert!(sess_arr[0]["state"].is_string());

    // ── Wait for sessions to start ──
    let text = poll_capture_until(
        &mut client,
        "editor",
        |t| t.contains("S2_EDITOR_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("S2_EDITOR_READY"), "editor startup marker");

    let text = poll_capture_until(
        &mut client,
        "terminal",
        |t| t.contains("S2_TERMINAL_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("S2_TERMINAL_READY"), "terminal startup marker");

    // ── send ──
    let marker = "S2_GATE_CMD_4W7Z";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "editor".to_string(),
                text: format!("echo {marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "send: exit code");

    // ── capture ──
    let text = poll_capture_until(
        &mut client,
        "editor",
        |t| t.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.matches(marker).count() >= 2,
        "capture: marker appears >= 2 times (echo + output)"
    );

    let capture_resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "editor".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    assert_eq!(capture_resp.msg_type, CaptureResult::TYPE_NAME);
    let fmt = output::format_response(&capture_resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "capture: text exit code");
    assert!(!fmt.stdout.is_empty(), "capture: text output non-empty");

    let json = assert_valid_json(&capture_resp);
    assert!(json["text"].is_string(), "capture: JSON has text field");

    // ── scrollback ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "editor".to_string(),
                tail: 50,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ScrollbackResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "scrollback: exit code");

    let json = assert_valid_json(&resp);
    assert!(json["lines"].is_array(), "scrollback: JSON has lines array");

    // ── inspect ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "editor".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, InspectResult::TYPE_NAME);
    let inspect: InspectResult = resp.extract_payload().unwrap();
    assert_eq!(inspect.data["paneName"], "editor");
    assert_eq!(inspect.data["workspace"], "gate-s2");

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "inspect: exit code");
    assert!(fmt.stdout.contains("editor"), "inspect: text mentions pane");

    let json = assert_valid_json(&resp);
    assert_eq!(json["paneName"], "editor");
    assert!(json["paneId"].is_string());
    assert!(json["sessionState"].is_string());

    // ── focus ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &FocusPane {
                pane_id: "terminal".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "focus: exit code");

    // ── rename ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RenamePane {
                pane_id: "terminal".to_string(),
                new_name: "console".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "rename: exit code");

    // Verify rename took effect — focus the new name
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &FocusPane {
                pane_id: "console".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.msg_type,
        OkResponse::TYPE_NAME,
        "renamed pane 'console' should be found"
    );

    // ── action ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &InvokeAction {
                action: "split-right".to_string(),
                target_pane_id: Some("editor".to_string()),
                args: serde_json::Value::Object(serde_json::Map::new()),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "action: exit code");

    // ── keys ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Keys {
                target: "editor".to_string(),
                keys: vec!["Enter".to_string(), "Ctrl+C".to_string()],
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "keys: exit code");

    // ── follow (initial response) ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Follow {
                target: "editor".to_string(),
                raw: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "follow: exit code");

    // ── attach ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &AttachWorkspace {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, AttachWorkspaceResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "attach: exit code");

    let json = assert_valid_json(&resp);
    assert!(json["state"].is_object(), "attach: JSON has state object");

    // ── recreate ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RecreateWorkspace {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, RecreateWorkspaceResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "recreate: exit code");

    let json = assert_valid_json(&resp);
    assert!(json["instanceId"].is_string(), "recreate: JSON has instanceId");

    // ── save ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &SaveWorkspace {
                workspace: "gate-s2".to_string(),
                file: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "save: exit code");

    // ── close ──
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "gate-s2".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS, "close: exit code");

    // ── Verify closed: list instances shows empty ──
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    let li: ListInstancesResult = resp.extract_payload().unwrap();
    assert_eq!(li.instances.len(), 0, "no instances after close");
}

// ── 2. Exit code §22.9 verification ─────────────────────────────────

/// §22.9 exit code 0: Success — verified exhaustively in s2_all_commands_full_lifecycle.
/// This test verifies error exit codes specifically.

/// §22.9 exit code 2: Target not found — for all pane-targeting commands.
#[tokio::test]
async fn exit_code_2_target_not_found_all_commands() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // send
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "ghost".to_string(),
                text: "test".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "send → exit 2");
    assert!(!fmt.stderr.is_empty(), "send error: stderr non-empty");

    // capture
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "ghost".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "capture → exit 2");

    // scrollback
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "ghost".to_string(),
                tail: 10,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "scrollback → exit 2");

    // inspect
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "ghost".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "inspect → exit 2");

    // focus
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &FocusPane {
                pane_id: "ghost".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "focus → exit 2");

    // rename
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RenamePane {
                pane_id: "ghost".to_string(),
                new_name: "new".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "rename → exit 2");

    // keys
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Keys {
                target: "ghost".to_string(),
                keys: vec!["Enter".to_string()],
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "keys → exit 2");

    // follow
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Follow {
                target: "ghost".to_string(),
                raw: false,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "follow → exit 2");
}

/// §22.9 exit code 2: Workspace not found — for all workspace-targeting commands.
#[tokio::test]
async fn exit_code_2_workspace_not_found_all_commands() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // open
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "nonexistent".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "open unknown → exit 2");

    // close
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
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "close unknown → exit 2");

    // attach
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &AttachWorkspace {
                workspace: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "attach unknown → exit 2");

    // recreate
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RecreateWorkspace {
                workspace: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "recreate unknown → exit 2");

    // save
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &SaveWorkspace {
                workspace: "nonexistent".to_string(),
                file: None,
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "save unknown → exit 2");

    // list panes
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "list panes unknown → exit 2");

    // list sessions
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND, "list sessions unknown → exit 2");
}

/// §22.9 exit code 3: Ambiguous target.
#[tokio::test]
async fn exit_code_3_ambiguous_target() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // send with ambiguous target
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "AMBIGUOUS".to_string(),
                text: "test".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetAmbiguous);
    assert!(err.candidates.is_some());
    assert_eq!(err.candidates.as_ref().unwrap().len(), 2);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::AMBIGUOUS_TARGET, "ambiguous send → exit 3");
    assert!(fmt.stderr.contains("Candidates:"), "stderr lists candidates");

    // capture with ambiguous target
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "AMBIGUOUS".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::AMBIGUOUS_TARGET, "ambiguous capture → exit 3");

    // JSON mode for ambiguous error
    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "target-ambiguous", "JSON error code");
    assert!(json["message"].is_string());
    assert!(json["candidates"].is_array());
}

// ── 3. JSON output structure validation for all response types ──────

/// Validates JSON output structure for every response type that returns data.
#[tokio::test]
async fn json_output_structure_all_response_types() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // Open workspace for subsequent queries
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "gate-s2".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    // OpenWorkspaceResult JSON
    let json = assert_valid_json(&resp);
    assert!(json["instanceId"].is_string(), "OpenWorkspaceResult.instanceId");
    assert!(json["state"].is_object(), "OpenWorkspaceResult.state");
    let json_fmt = output::format_response(&resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::SUCCESS);

    // ListWorkspacesResult JSON
    let resp = client
        .request(&Envelope::new(&next_id(), &ListWorkspaces {}))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    let arr = json["workspaces"].as_array().unwrap();
    for entry in arr {
        assert!(entry["name"].is_string(), "workspace entry has name");
        assert!(entry["source"].is_string(), "workspace entry has source");
    }

    // ListInstancesResult JSON
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    let arr = json["instances"].as_array().unwrap();
    assert!(!arr.is_empty());
    assert!(arr[0]["name"].is_string());
    assert!(arr[0]["instanceId"].is_string());

    // ListPanesResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    let panes = json["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    for p in panes {
        assert!(p["name"].is_string(), "pane has name");
        assert!(p["tab"].is_string(), "pane has tab");
        assert!(p["sessionState"].is_string(), "pane has sessionState");
    }

    // ListSessionsResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    for s in sessions {
        assert!(s["sessionId"].is_string(), "session has sessionId");
        assert!(s["pane"].is_string(), "session has pane");
        assert!(s["state"].is_string(), "session has state");
    }

    // Wait for session output
    poll_capture_until(
        &mut client,
        "editor",
        |t| t.contains("S2_EDITOR_READY"),
        Duration::from_secs(10),
    )
    .await;

    // CaptureResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "editor".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert!(json["text"].is_string(), "CaptureResult.text");

    // ScrollbackResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "editor".to_string(),
                tail: 20,
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert!(json["lines"].is_array(), "ScrollbackResult.lines");

    // InspectResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "editor".to_string(),
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert_eq!(json["paneName"], "editor");
    assert_eq!(json["workspace"], "gate-s2");
    assert!(json["paneId"].is_string());
    assert!(json["sessionState"].is_string());

    // AttachWorkspaceResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &AttachWorkspace {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert!(json["state"].is_object(), "AttachWorkspaceResult.state");

    // RecreateWorkspaceResult JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RecreateWorkspace {
                workspace: "gate-s2".to_string(),
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert!(json["instanceId"].is_string(), "RecreateWorkspaceResult.instanceId");

    // ErrorResponse JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "nonexistent".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "target-not-found", "ErrorResponse.code");
    assert!(json["message"].is_string(), "ErrorResponse.message");
    let json_fmt = output::format_response(&resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::TARGET_NOT_FOUND);

    // OkResponse JSON (send returns Ok)
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "editor".to_string(),
                text: "echo ok".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    let json_fmt = output::format_response(&resp, true);
    assert_eq!(json_fmt.exit_code, exit_code::SUCCESS);
    // OkResponse may produce empty JSON or minimal JSON — just ensure it's valid
    if !json_fmt.stdout.trim().is_empty() {
        let _: serde_json::Value = serde_json::from_str(&json_fmt.stdout)
            .unwrap_or_else(|e| panic!("OkResponse JSON invalid: {e}"));
    }

    // Cleanup
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "gate-s2".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
}

// ── 4. Cross-pane isolation ─────────────────────────────────────────

/// Verify that send/capture target the correct pane in a split layout.
#[tokio::test]
async fn cross_pane_isolation_send_capture() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // Open workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "gate-s2".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait for both panes
    poll_capture_until(
        &mut client,
        "editor",
        |t| t.contains("S2_EDITOR_READY"),
        Duration::from_secs(10),
    )
    .await;
    poll_capture_until(
        &mut client,
        "terminal",
        |t| t.contains("S2_TERMINAL_READY"),
        Duration::from_secs(10),
    )
    .await;

    // Send unique marker to editor only
    let editor_marker = "EDITMRK_S2_8J3K";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "editor".to_string(),
                text: format!("echo {editor_marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    // Poll editor for marker
    let editor_text = poll_capture_until(
        &mut client,
        "editor",
        |t| t.matches(editor_marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert!(editor_text.contains(editor_marker));

    // Terminal should NOT have editor's marker
    let terminal_text = poll_capture_until(
        &mut client,
        "terminal",
        |_| true,
        Duration::from_secs(1),
    )
    .await;
    assert!(
        !terminal_text.contains(editor_marker),
        "terminal should not contain editor's marker"
    );

    // Cleanup
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "gate-s2".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
}

// ── 5. Error JSON output is valid for all error types ────────────────

/// All error responses produce valid JSON with code and message fields.
#[tokio::test]
async fn error_json_output_valid_for_all_error_types() {
    let host = TestHost::start(S2Handler::new()).await;
    let mut client = host.connect().await;

    // TargetNotFound error JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "ghost".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "target-not-found");
    assert!(json["message"].is_string());
    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);

    // WorkspaceNotFound error JSON
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "absent".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "workspace-not-found");
    assert!(json["message"].is_string());
    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);

    // TargetAmbiguous error JSON (with candidates)
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "AMBIGUOUS".to_string(),
                text: "test".to_string(),
                newline: true,
            },
        ))
        .await
        .unwrap();
    let json = assert_valid_json(&resp);
    assert_eq!(json["code"], "target-ambiguous");
    assert!(json["message"].is_string());
    assert!(json["candidates"].is_array());
    let candidates = json["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::AMBIGUOUS_TARGET);
}
