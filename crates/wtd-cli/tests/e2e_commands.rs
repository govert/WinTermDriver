//! End-to-end test suite for all wtd CLI subcommands (§32.2).
//!
//! Tests run each command against a real host with test workspace fixtures.
//! Uses TestHost harness with test-specific named pipe names.
//! Validates JSON output structure, exit codes per §22.9, and concurrent input.

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

// ── Test workspace YAML ───────────────────────��─────────────────────

const SIMPLE_YAML: &str = r#"
version: 1
name: e2e-test
description: "E2E test fixture: single pane with cmd.exe"
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo E2E_READY"
"#;

// ── Unique pipe naming ──────────────────��───────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(6000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-e2e-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("e2e-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── TestHost harness ─────────────��──────────────────────────────────

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
        // Small delay for server to start accepting
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

// ── Helpers ───────────────���───────────────���─────────────────────────

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

// ── E2E Request Handler ─────────���───────────────────────────────────

struct E2eState {
    workspaces: HashMap<String, WorkspaceInstance>,
    next_instance_id: u64,
}

struct E2eHandler {
    state: Mutex<E2eState>,
}

impl E2eHandler {
    fn new() -> Self {
        Self {
            state: Mutex::new(E2eState {
                workspaces: HashMap::new(),
                next_instance_id: 1,
            }),
        }
    }
}

impl RequestHandler for E2eHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                let yaml = match open.name.as_str() {
                    "e2e-test" => SIMPLE_YAML,
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

            TypedMessage::ListWorkspaces(_) => Some(Envelope::new(
                &envelope.id,
                &ListWorkspacesResult {
                    workspaces: vec![
                        WorkspaceInfo {
                            name: "e2e-test".to_string(),
                            source: "local".to_string(),
                        },
                        WorkspaceInfo {
                            name: "other-ws".to_string(),
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
                let state = self.state.lock().unwrap();

                // Special target for ambiguous test
                if send.target == "AMBIGUOUS" {
                    return Some(error_envelope_with_candidates(
                        &envelope.id,
                        ErrorCode::TargetAmbiguous,
                        "ambiguous target 'AMBIGUOUS'",
                        vec!["ws1/tab1/AMBIGUOUS".into(), "ws2/tab1/AMBIGUOUS".into()],
                    ));
                }

                let (inst, pane_id) = match find_pane(&state.workspaces, &send.target) {
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

                // Special target for ambiguous test
                if capture.target == "AMBIGUOUS" {
                    return Some(error_envelope_with_candidates(
                        &envelope.id,
                        ErrorCode::TargetAmbiguous,
                        "ambiguous target 'AMBIGUOUS'",
                        vec!["ws1/tab1/AMBIGUOUS".into(), "ws2/tab1/AMBIGUOUS".into()],
                    ));
                }

                // Drain pending output from all sessions
                for inst in state.workspaces.values_mut() {
                    for session in inst.sessions_mut().values_mut() {
                        session.process_pending_output();
                    }
                }

                let (inst, pane_id) = match find_pane(&state.workspaces, &capture.target) {
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

                // Drain pending output
                for inst in state.workspaces.values_mut() {
                    for session in inst.sessions_mut().values_mut() {
                        session.process_pending_output();
                    }
                }

                let (inst, pane_id) =
                    match find_pane(&state.workspaces, &scrollback.target) {
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
                match find_pane(&state.workspaces, &follow.target) {
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
                let (inst, pane_id) = match find_pane(&state.workspaces, &inspect.target) {
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
                match find_pane(&state.workspaces, &focus.pane_id) {
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

            TypedMessage::InvokeAction(_action) => {
                // Acknowledge action — full dispatch tested in action system
                Some(Envelope::new(&envelope.id, &OkResponse {}))
            }

            TypedMessage::Keys(keys) => {
                let state = self.state.lock().unwrap();
                match find_pane(&state.workspaces, &keys.target) {
                    Some(_) => Some(Envelope::new(&envelope.id, &OkResponse {})),
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", keys.target),
                    )),
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
                    // Simulate recreate by returning success
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

            _ => None,
        }
    }
}

/// Find a pane by name across all open workspaces.
fn find_pane<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    for inst in workspaces.values() {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((inst, pane_id));
        }
    }
    None
}

// ── Polling helper ──────────────���───────────────────────────────────

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
            .request(&Envelope::new(&next_id(), &Capture { target: target.to_string(), ..Default::default() }))
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

// ═════════��══════════════════════════════════════════════════════════
// Tests
// ═════════════���══════════════════��═══════════════════════════════════

// ── Full lifecycle: open → list → send → capture → scrollback → inspect → close ──

#[tokio::test]
async fn full_lifecycle_open_list_send_capture_inspect_close() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    // 1. Open workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);
    let open_result: OpenWorkspaceResult = resp.extract_payload().unwrap();
    assert!(!open_result.instance_id.is_empty());

    // Verify output formatting
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains(&open_result.instance_id));

    // 2. List workspaces
    let resp = client
        .request(&Envelope::new(&next_id(), &ListWorkspaces {}))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListWorkspacesResult::TYPE_NAME);
    let lw: ListWorkspacesResult = resp.extract_payload().unwrap();
    assert_eq!(lw.workspaces.len(), 2);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("e2e-test"));
    assert!(fmt.stdout.contains("NAME"));

    // 3. List instances
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListInstancesResult::TYPE_NAME);
    let li: ListInstancesResult = resp.extract_payload().unwrap();
    assert_eq!(li.instances.len(), 1);
    assert_eq!(li.instances[0].name, "e2e-test");

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("e2e-test"));

    // 4. List panes
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListPanesResult::TYPE_NAME);
    let lp: ListPanesResult = resp.extract_payload().unwrap();
    assert_eq!(lp.panes.len(), 1);
    assert_eq!(lp.panes[0].name, "shell");
    assert_eq!(lp.panes[0].tab, "main");

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("shell"));
    assert!(fmt.stdout.contains("main"));

    // 5. List sessions
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListSessionsResult::TYPE_NAME);
    let ls: ListSessionsResult = resp.extract_payload().unwrap();
    assert_eq!(ls.sessions.len(), 1);
    assert_eq!(ls.sessions[0].pane, "shell");

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("shell"));

    // 6. Wait for startup output
    let text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("E2E_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.contains("E2E_READY"),
        "startup marker should appear in capture"
    );

    // 7. Send input
    let marker = "E2E_MARKER_7X9K";
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "shell".to_string(),
                text: format!("echo {marker}"),
                newline: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // 8. Capture — poll until marker appears at least twice (echo + output)
    let text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = text.matches(marker).count();
    assert!(
        count >= 2,
        "marker should appear >= 2 times, found {count} in:\n{text}"
    );

    // Verify capture formatting
    let capture_resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "shell".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let fmt = output::format_response(&capture_resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(!fmt.stdout.is_empty());

    // 9. Scrollback — returns whatever is in scrollback (may be empty for small output)
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "shell".to_string(),
                tail: 100,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ScrollbackResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // 10. Inspect
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "shell".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, InspectResult::TYPE_NAME);
    let inspect: InspectResult = resp.extract_payload().unwrap();
    assert_eq!(inspect.data["paneName"], "shell");
    assert_eq!(inspect.data["workspace"], "e2e-test");

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("shell"));

    // 11. Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "e2e-test".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // 12. Verify instance is gone
    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();
    let li: ListInstancesResult = resp.extract_payload().unwrap();
    assert_eq!(li.instances.len(), 0);
}

// ── Follow: initial response and error ──────────────────────────────

#[tokio::test]
async fn follow_success_initial_response() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    // Open workspace first
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    // Wait for session to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Follow returns initial OkResponse
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Follow {
                target: "shell".to_string(),
                raw: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
}

#[tokio::test]
async fn follow_error_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Follow {
                target: "nonexistent".to_string(),
                raw: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

// ── Error cases: target not found (exit code 2) ─────────────────────

#[tokio::test]
async fn error_send_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &message::Send {
                target: "nonexistent".to_string(),
                text: "hello".to_string(),
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
    assert!(fmt.stderr.contains("nonexistent"));
}

#[tokio::test]
async fn error_capture_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

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

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_inspect_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_scrollback_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "nonexistent".to_string(),
                tail: 10,
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_focus_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &FocusPane {
                pane_id: "nonexistent".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_rename_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RenamePane {
                pane_id: "nonexistent".to_string(),
                new_name: "new-name".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_keys_target_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Keys {
                target: "nonexistent".to_string(),
                keys: vec!["Enter".to_string()],
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

// ── Error cases: workspace not found (exit code 2) ──────────────────

#[tokio::test]
async fn error_open_workspace_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "no-such-workspace".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
    assert!(fmt.stderr.contains("no-such-workspace"));
}

#[tokio::test]
async fn error_close_workspace_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "no-such-workspace".to_string(),
                kill: false,
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_list_panes_workspace_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "no-such-workspace".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

#[tokio::test]
async fn error_list_sessions_workspace_not_found() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "no-such-workspace".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
}

// ── Error case: ambiguous target (exit code 3) ──────────────────────

#[tokio::test]
async fn error_ambiguous_target() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

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
    assert_eq!(fmt.exit_code, exit_code::AMBIGUOUS_TARGET);
    assert!(fmt.stderr.contains("Candidates:"));
    assert!(fmt.stderr.contains("ws1/tab1/AMBIGUOUS"));
}

#[tokio::test]
async fn error_ambiguous_capture() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

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
    assert_eq!(fmt.exit_code, exit_code::AMBIGUOUS_TARGET);
}

// ── JSON output validation ──────────────────────────────────────────

#[tokio::test]
async fn json_output_open_workspace() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["instanceId"].is_string());
}

#[tokio::test]
async fn json_output_list_workspaces() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(&next_id(), &ListWorkspaces {}))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["workspaces"].is_array());
    assert_eq!(json["workspaces"].as_array().unwrap().len(), 2);
    assert!(json["workspaces"][0]["name"].is_string());
    assert!(json["workspaces"][0]["source"].is_string());
}

#[tokio::test]
async fn json_output_list_panes() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    // Open workspace first
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["panes"].is_array());
    let panes = json["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["name"], "shell");
    assert_eq!(panes[0]["tab"], "main");
    assert!(panes[0]["sessionState"].is_string());
}

#[tokio::test]
async fn json_output_list_instances() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    // Open workspace
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let resp = client
        .request(&Envelope::new(&next_id(), &ListInstances {}))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["instances"].is_array());
    let instances = json["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 1);
    assert!(instances[0]["name"].is_string());
    assert!(instances[0]["instanceId"].is_string());
}

#[tokio::test]
async fn json_output_list_sessions() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["sessions"].is_array());
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0]["sessionId"].is_string());
    assert!(sessions[0]["pane"].is_string());
    assert!(sessions[0]["state"].is_string());
}

#[tokio::test]
async fn json_output_capture() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    // Wait for startup
    tokio::time::sleep(Duration::from_millis(500)).await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Capture {
                target: "shell".to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["text"].is_string());
}

#[tokio::test]
async fn json_output_inspect() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Inspect {
                target: "shell".to_string(),
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert_eq!(json["paneName"], "shell");
    assert_eq!(json["workspace"], "e2e-test");
    assert!(json["sessionState"].is_string());
    assert!(json["paneId"].is_string());
}

#[tokio::test]
async fn json_output_error() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

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

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert_eq!(json["code"], "target-not-found");
    assert!(json["message"].is_string());
}

#[tokio::test]
async fn json_output_scrollback() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Scrollback {
                target: "shell".to_string(),
                tail: 10,
            },
        ))
        .await
        .unwrap();

    let fmt = output::format_response(&resp, true);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    let json: serde_json::Value = serde_json::from_str(&fmt.stdout).unwrap();
    assert!(json["lines"].is_array());
}

// ── Additional commands: attach, recreate, save, focus, rename, action, keys ──

#[tokio::test]
async fn attach_recreate_save_focus_rename_action_keys() {
    let host = TestHost::start(E2eHandler::new()).await;
    let mut client = host.connect().await;

    // Open workspace
    let _ = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    // Wait for session
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Attach
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &AttachWorkspace {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, AttachWorkspaceResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Recreate
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RecreateWorkspace {
                workspace: "e2e-test".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, RecreateWorkspaceResult::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Save
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &SaveWorkspace {
                workspace: "e2e-test".to_string(),
                file: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Focus
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &FocusPane {
                pane_id: "shell".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Rename
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &RenamePane {
                pane_id: "shell".to_string(),
                new_name: "renamed-shell".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Action
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &InvokeAction {
                action: "split-right".to_string(),
                target_pane_id: Some("renamed-shell".to_string()),
                args: serde_json::Value::Object(serde_json::Map::new()),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // Keys
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &Keys {
                target: "renamed-shell".to_string(),
                keys: vec!["Enter".to_string(), "Ctrl+C".to_string()],
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
}

// ── Concurrent input test ───────────────────────────────────────────

#[tokio::test]
async fn concurrent_input_no_crash_or_hang() {
    let host = TestHost::start(E2eHandler::new()).await;

    // Open workspace from first client
    let mut client1 = host.connect().await;
    let resp = client1
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "e2e-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait for session to start
    poll_capture_until(
        &mut client1,
        "shell",
        |t| t.contains("E2E_READY"),
        Duration::from_secs(10),
    )
    .await;

    // Connect second client (simulating UI)
    let mut client2 = host.connect().await;

    // Both clients send input simultaneously
    let marker1 = "CONCURRENT_A_8K3M";
    let marker2 = "CONCURRENT_B_9L4N";

    let task1 = {
        let m = marker1.to_string();
        async move {
            client1
                .request(&Envelope::new(
                    &next_id(),
                    &message::Send {
                        target: "shell".to_string(),
                        text: format!("echo {m}"),
                        newline: true,
                    },
                ))
                .await
                .unwrap()
        }
    };

    let task2 = {
        let m = marker2.to_string();
        async move {
            client2
                .request(&Envelope::new(
                    &next_id(),
                    &message::Send {
                        target: "shell".to_string(),
                        text: format!("echo {m}"),
                        newline: true,
                    },
                ))
                .await
                .unwrap()
        }
    };

    // Run both sends concurrently
    let (resp1, resp2) = tokio::join!(task1, task2);
    assert_eq!(resp1.msg_type, OkResponse::TYPE_NAME);
    assert_eq!(resp2.msg_type, OkResponse::TYPE_NAME);

    // Reconnect to capture (clients moved into tasks)
    let mut client3 = host.connect().await;

    // Poll until both markers appear
    let text = poll_capture_until(
        &mut client3,
        "shell",
        |t| t.contains(marker1) && t.contains(marker2),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.contains(marker1),
        "marker1 should appear in output:\n{text}"
    );
    assert!(
        text.contains(marker2),
        "marker2 should appear in output:\n{text}"
    );
}
