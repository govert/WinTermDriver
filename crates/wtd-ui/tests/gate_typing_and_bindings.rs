//! Gate integration test: Typing produces session output and single-stroke
//! bindings dispatch actions (§21.1).
//!
//! Proves the keyboard pipeline end-to-end:
//! 1. Regular keystrokes classified as `SendToSession` with correct terminal bytes
//! 2. Single-stroke bindings (e.g. Ctrl+Shift+T) classified as `DispatchAction`
//! 3. Typed bytes sent through IPC to a ConPTY session produce visible output
//! 4. Binding keystrokes are consumed (not forwarded to sessions)

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;

use wtd_core::global_settings::{default_bindings, tmux_bindings};
use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::load_workspace_definition;
use wtd_core::workspace::ActionReference;
use wtd_core::GlobalSettings;
use wtd_host::ipc_server::*;
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance};
use wtd_ipc::message::{
    Capture, CaptureResult, ErrorCode, ErrorResponse, MessagePayload, OkResponse, OpenWorkspace,
    OpenWorkspaceResult, TypedMessage,
};
use wtd_ipc::Envelope;
use wtd_ui::host_client::UiIpcClient;
use wtd_ui::input::{InputClassifier, KeyEvent, KeyName, Modifiers, key_event_to_bytes};
use wtd_ui::prefix_state::{PrefixOutput, PrefixStateMachine};

// ── Fixture ──────────────────────────────────────────────────────────────

const SIMPLE_YAML: &str = include_str!("../../wtd-host/tests/fixtures/simple-workspace.yaml");

// ── Unique pipe names ────────────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(9000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-kb-{}-{}", std::process::id(), n)
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn make_key(key: KeyName, mods: Modifiers, character: Option<char>) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: mods,
        character,
    }
}

fn action_name(action: &ActionReference) -> &str {
    match action {
        ActionReference::Simple(s) => s.as_str(),
        ActionReference::WithArgs { action, .. } => action.as_str(),
        ActionReference::Removed => "",
    }
}

// ── IPC handler (reused from gate_host_to_pane pattern) ──────────────────

struct GateState {
    workspace: Option<WorkspaceInstance>,
}

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
            TypedMessage::OpenWorkspace(_) => {
                let def = match load_workspace_definition("gate-test.yaml", SIMPLE_YAML) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("load failed: {e}"),
                        ));
                    }
                };

                let gs = GlobalSettings::default();
                let env = default_host_env();

                let inst = match WorkspaceInstance::open(
                    WorkspaceInstanceId(900),
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
                            &format!("open failed: {e}"),
                        ));
                    }
                };

                let instance_id = format!("{}", inst.id().0);
                self.state.lock().unwrap().workspace = Some(inst);

                Some(Envelope::new(
                    &envelope.id,
                    &OpenWorkspaceResult {
                        instance_id,
                        state: serde_json::json!({}),
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
                        &format!("write failed: {e}"),
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

                Some(Envelope::new(&envelope.id, &CaptureResult { text, ..Default::default() }))
            }

            _ => None,
        }
    }
}

// ── Test 1: Regular keystrokes produce SendToSession with correct bytes ──

#[test]
fn regular_typing_produces_send_to_session_bytes() {
    let bindings = default_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    // Type 'h' — regular letter
    let event_h = make_key(KeyName::Char('H'), Modifiers::NONE, Some('h'));
    let result = psm.process(&event_h);
    match result {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(bytes, b"h", "typing 'h' must produce ASCII 'h'");
        }
        other => panic!("expected SendToSession for 'h', got: {:?}", other),
    }

    // Type 'i' — another letter
    let event_i = make_key(KeyName::Char('I'), Modifiers::NONE, Some('i'));
    match psm.process(&event_i) {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(bytes, b"i", "typing 'i' must produce ASCII 'i'");
        }
        other => panic!("expected SendToSession for 'i', got: {:?}", other),
    }

    // Type Enter — produces CR
    let event_enter = make_key(KeyName::Enter, Modifiers::NONE, None);
    match psm.process(&event_enter) {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(bytes, vec![0x0D], "Enter must produce CR (0x0D)");
        }
        other => panic!("expected SendToSession for Enter, got: {:?}", other),
    }

    // Type a digit '5'
    let event_5 = make_key(KeyName::Digit(5), Modifiers::NONE, Some('5'));
    match psm.process(&event_5) {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(bytes, b"5", "typing '5' must produce ASCII '5'");
        }
        other => panic!("expected SendToSession for '5', got: {:?}", other),
    }

    // Type Space
    let event_space = make_key(KeyName::Space, Modifiers::NONE, Some(' '));
    match psm.process(&event_space) {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(bytes, vec![0x20], "Space must produce 0x20");
        }
        other => panic!("expected SendToSession for Space, got: {:?}", other),
    }

    // Arrow key — should produce VT escape sequence
    let event_up = make_key(KeyName::Up, Modifiers::NONE, None);
    match psm.process(&event_up) {
        PrefixOutput::SendToSession(bytes) => {
            assert_eq!(
                bytes,
                vec![0x1B, b'[', b'A'],
                "Up arrow must produce CSI A"
            );
        }
        other => panic!("expected SendToSession for Up, got: {:?}", other),
    }

    // Prefix state should remain idle throughout
    assert!(
        !psm.is_prefix_active(),
        "prefix must remain idle after regular typing"
    );
}

// ── Test 2: Ctrl+Shift+T dispatches new-tab action ─────────────────────

#[test]
fn ctrl_shift_t_dispatches_new_tab_action() {
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    // Ctrl+Shift+T
    let event = make_key(
        KeyName::Char('T'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    );
    let result = psm.process(&event);

    match result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(
                action_name(&action),
                "new-tab",
                "Ctrl+Shift+T must dispatch 'new-tab'"
            );
        }
        other => panic!(
            "expected DispatchAction(new-tab) for Ctrl+Shift+T, got: {:?}",
            other
        ),
    }

    // Prefix state should remain idle
    assert!(!psm.is_prefix_active());
}

// ── Test 3: Multiple single-stroke bindings dispatch correct actions ─────

#[test]
fn single_stroke_bindings_dispatch_correct_actions() {
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    // Ctrl+Shift+W → close-pane
    let result = psm.process(&make_key(
        KeyName::Char('W'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    ));
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(action_name(action), "close-pane");
        }
        other => panic!("expected close-pane, got: {:?}", other),
    }

    // Alt+Shift+D → split-right
    let result = psm.process(&make_key(
        KeyName::Char('D'),
        Modifiers::ALT | Modifiers::SHIFT,
        None,
    ));
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(action_name(action), "split-right");
        }
        other => panic!("expected split-right, got: {:?}", other),
    }

    // F11 → toggle-fullscreen
    let result = psm.process(&make_key(KeyName::F(11), Modifiers::NONE, None));
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(action_name(action), "toggle-fullscreen");
        }
        other => panic!("expected toggle-fullscreen, got: {:?}", other),
    }

    // Ctrl+Tab → next-tab
    let result = psm.process(&make_key(KeyName::Tab, Modifiers::CTRL, None));
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(action_name(action), "next-tab");
        }
        other => panic!("expected next-tab, got: {:?}", other),
    }
}

// ── Test 4: Binding keys are consumed (not forwarded as raw bytes) ───────

#[test]
fn binding_keys_are_consumed_not_forwarded() {
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    // Ctrl+Shift+T must dispatch, not produce SendToSession
    let event = make_key(
        KeyName::Char('T'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    );
    let result = psm.process(&event);
    assert!(
        matches!(result, PrefixOutput::DispatchAction(_)),
        "binding Ctrl+Shift+T must not be forwarded as raw bytes"
    );

    // Regular 't' must produce raw bytes (not consumed)
    let event = make_key(KeyName::Char('T'), Modifiers::NONE, Some('t'));
    let result = psm.process(&event);
    assert!(
        matches!(result, PrefixOutput::SendToSession(_)),
        "unbound 't' must be forwarded as raw bytes"
    );
}

// ── Test 5: key_event_to_bytes produces correct terminal sequences ───────

#[test]
fn key_event_to_bytes_produces_correct_sequences() {
    // Letter
    let bytes = key_event_to_bytes(&make_key(KeyName::Char('A'), Modifiers::NONE, Some('a')));
    assert_eq!(bytes, b"a");

    // Ctrl+C → 0x03
    let bytes = key_event_to_bytes(&make_key(KeyName::Char('C'), Modifiers::CTRL, None));
    assert_eq!(bytes, vec![0x03]);

    // Escape → 0x1B
    let bytes = key_event_to_bytes(&make_key(KeyName::Escape, Modifiers::NONE, None));
    assert_eq!(bytes, vec![0x1B]);

    // Tab → 0x09
    let bytes = key_event_to_bytes(&make_key(KeyName::Tab, Modifiers::NONE, None));
    assert_eq!(bytes, vec![0x09]);

    // Backspace → 0x7F
    let bytes = key_event_to_bytes(&make_key(KeyName::Backspace, Modifiers::NONE, None));
    assert_eq!(bytes, vec![0x7F]);

    // Down arrow → CSI B
    let bytes = key_event_to_bytes(&make_key(KeyName::Down, Modifiers::NONE, None));
    assert_eq!(bytes, vec![0x1B, b'[', b'B']);

    // Shift+Tab → CSI Z (backtab)
    let bytes = key_event_to_bytes(&make_key(KeyName::Tab, Modifiers::SHIFT, None));
    assert_eq!(bytes, vec![0x1B, b'[', b'Z']);
}

// ── Test 6: Prefix key enters prefix mode, not forwarded ────────────────

#[test]
fn prefix_key_enters_prefix_mode() {
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    // Ctrl+B is the prefix key
    let event = make_key(KeyName::Char('B'), Modifiers::CTRL, None);
    let result = psm.process(&event);

    assert!(
        matches!(result, PrefixOutput::Consumed),
        "prefix key must be consumed"
    );
    assert!(
        psm.is_prefix_active(),
        "prefix state must be active after prefix key"
    );
    assert_eq!(psm.prefix_label(), "Ctrl+B");
}

// ── Test 7: Full IPC round-trip — typed bytes reach ConPTY session ───────

#[tokio::test]
async fn typed_bytes_reach_conpty_session_via_ipc() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(
        IpcServer::new(pipe_name.clone(), GateHandler::new()).unwrap(),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // Connect as UI client
    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (mut reader, mut writer) = client.split();

    // Open workspace with real ConPTY session
    writer
        .write_frame(&Envelope::new(
            "kb-open",
            &OpenWorkspace {
                name: "gate-kb-test".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();

    let open_resp = reader.read_frame().await.unwrap();
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // Wait for ConPTY to be ready (poll until startup marker appears)
    let start = tokio::time::Instant::now();
    let mut ready = false;
    while start.elapsed() < Duration::from_secs(10) {
        writer
            .write_frame(&Envelope::new(
                "kb-ready",
                &Capture {
                    target: "shell".to_string(),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();

        let resp = reader.read_frame().await.unwrap();
        let cap: CaptureResult = resp.extract_payload().unwrap();
        if cap.text.contains("GATE_MARKER") {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "ConPTY session must be ready with startup output");

    // Build key events for typing "echo KB_GATE_9PC1" using the keyboard pipeline.
    // Process through PrefixStateMachine to get the bytes that would be sent.
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    let typed_text = "echo KB_GATE_9PC1";
    let mut raw_bytes = Vec::new();
    for ch in typed_text.chars() {
        let key = if ch.is_ascii_alphabetic() {
            KeyName::Char(ch.to_ascii_uppercase())
        } else if ch.is_ascii_digit() {
            KeyName::Digit(ch as u8 - b'0')
        } else if ch == ' ' {
            KeyName::Space
        } else if ch == '_' {
            // underscore is a printable character
            KeyName::Char('_') // Won't match any named key; falls through to character
        } else {
            KeyName::Char(ch.to_ascii_uppercase())
        };

        let event = make_key(key, Modifiers::NONE, Some(ch));
        match psm.process(&event) {
            PrefixOutput::SendToSession(bytes) => raw_bytes.extend_from_slice(&bytes),
            other => panic!("regular typing must produce SendToSession, got: {:?}", other),
        }
    }

    // Add Enter to execute the command
    let enter = make_key(KeyName::Enter, Modifiers::NONE, None);
    match psm.process(&enter) {
        PrefixOutput::SendToSession(bytes) => raw_bytes.extend_from_slice(&bytes),
        other => panic!("Enter must produce SendToSession, got: {:?}", other),
    }

    // Send the accumulated raw bytes via IPC (using Send with newline=false
    // since we already included the Enter key bytes)
    let text = String::from_utf8_lossy(&raw_bytes).to_string();
    writer
        .write_frame(&Envelope::new(
            "kb-send",
            &wtd_ipc::message::Send {
                target: "shell".to_string(),
                text,
                newline: false,
            },
        ))
        .await
        .unwrap();

    let send_resp = reader.read_frame().await.unwrap();
    assert_eq!(
        send_resp.msg_type,
        OkResponse::TYPE_NAME,
        "Send must succeed: {:?}",
        send_resp.payload
    );

    // Poll Capture until our typed marker appears at least twice (echo + output)
    let mut found = false;
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        writer
            .write_frame(&Envelope::new(
                "kb-cap",
                &Capture {
                    target: "shell".to_string(),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();

        let resp = reader.read_frame().await.unwrap();
        let cap: CaptureResult = resp.extract_payload().unwrap();
        let count = cap.text.matches("KB_GATE_9PC1").count();
        if count >= 2 {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        found,
        "typed command must produce visible output (echo + result) containing KB_GATE_9PC1"
    );

    // Verify that a single-stroke binding does NOT produce session output.
    // We check that Ctrl+Shift+T is classified as DispatchAction, meaning
    // it would NOT be sent to the session.
    let ctrl_shift_t = make_key(
        KeyName::Char('T'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    );
    let result = psm.process(&ctrl_shift_t);
    assert!(
        matches!(result, PrefixOutput::DispatchAction(ref a) if action_name(a) == "new-tab"),
        "Ctrl+Shift+T must dispatch new-tab, not send to session: {:?}",
        result
    );

    // Cleanup
    let _ = shutdown_tx.send(true);
    drop(reader);
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}
