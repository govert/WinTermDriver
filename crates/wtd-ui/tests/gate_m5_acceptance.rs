//! M5 Acceptance Gate — Interactive workspace milestone (§37.5)
//!
//! This test proves the M5 milestone: typing works in panes, single-stroke
//! keybindings dispatch actions, prefix chords work (Ctrl+B,% splits right),
//! mouse click changes pane focus, text selection and copy/paste work, and
//! the command palette opens, searches, and dispatches actions.
//!
//! Criteria validated (§37.5 M5):
//!   1. Typing works in panes (keystroke → terminal bytes → ConPTY → visible output)
//!   2. Single-stroke keybindings dispatch actions (Ctrl+Shift+T → new-tab)
//!   3. Prefix chords work (Ctrl+B,% → split-right)
//!   4. Mouse click changes pane focus
//!   5. Text selection and copy/paste work (extract → VT-stripped → clipboard round-trip)
//!   6. Command palette opens, searches, and dispatches actions
//!
//! Pipeline: keyboard/mouse classification → IPC → ConPTY → ScreenBuffer →
//! selection → clipboard → command palette → composited Direct2D render.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use wtd_core::global_settings::default_bindings;
use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::layout::LayoutTree;
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
use wtd_pty::ScreenBuffer;
use wtd_ui::clipboard::{
    copy_to_clipboard, extract_selection_text, prepare_paste, read_from_clipboard, strip_vt,
};
use wtd_ui::command_palette::{CommandPalette, PaletteResult};
use wtd_ui::host_client::UiIpcClient;
use wtd_ui::input::{InputClassifier, KeyEvent, KeyName, Modifiers};
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction};
use wtd_ui::prefix_state::{PrefixOutput, PrefixStateMachine};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer, TextSelection};
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::TabStrip;

// ── Workspace YAML fixture ──────────────────────────────────────────────
//
// Single tab with a vertical split: two panes with distinct markers.
// Uses a single tab to avoid the PaneId collision issue across tabs.

const M5_WORKSPACE_YAML: &str = r#"
version: 1
name: m5-gate
description: "M5 acceptance gate: interactive workspace"
tabs:
  - name: dev
    layout:
      type: split
      orientation: vertical
      ratio: 0.5
      children:
        - type: pane
          name: editor
          session:
            profile: cmd
            startupCommand: "echo M5_EDITOR_7Q2X"
        - type: pane
          name: terminal
          session:
            profile: cmd
            startupCommand: "echo M5_TERMINAL_4K8Y"
"#;

// ── Unique pipe / class names ────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(15000);
static CLASS_COUNTER: AtomicU32 = AtomicU32::new(15000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-m5-{}-{}", std::process::id(), n)
}

// ── Test window helpers ──────────────────────────────────────────────────

unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn create_test_window(label: &str) -> HWND {
    let n = CLASS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let class_name_str = format!("WtdM5Gate_{label}_{n}\0");
    let class_name_wide: Vec<u16> = class_name_str.encode_utf16().collect();

    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let hinstance: HINSTANCE = instance.into();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name_wide.as_ptr()),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name_wide.as_ptr()),
            w!("M5 Gate Test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            800,
            600,
            None,
            None,
            Some(&hinstance),
            None,
        )
        .unwrap()
    }
}

fn destroy_test_window(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

// ── Key event helpers ────────────────────────────────────────────────────

fn make_key(key: KeyName, mods: Modifiers, character: Option<char>) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: mods,
        character,
    }
}

fn char_key(ch: char) -> KeyEvent {
    let key = if ch.is_ascii_alphabetic() {
        KeyName::Char(ch.to_ascii_uppercase())
    } else if ch.is_ascii_digit() {
        KeyName::Digit(ch as u8 - b'0')
    } else if ch == ' ' {
        KeyName::Space
    } else if ch == '_' {
        KeyName::Char('_')
    } else if ch == '-' {
        KeyName::Minus
    } else {
        KeyName::Char(ch.to_ascii_uppercase())
    };
    make_key(key, Modifiers::NONE, Some(ch))
}

fn action_name(action: &ActionReference) -> &str {
    match action {
        ActionReference::Simple(s) => s.as_str(),
        ActionReference::WithArgs { action, .. } => action.as_str(),
    }
}

fn ctrl_b() -> KeyEvent {
    make_key(KeyName::Char('B'), Modifiers::CTRL, None)
}

fn percent() -> KeyEvent {
    KeyEvent {
        key: KeyName::Digit(5),
        modifiers: Modifiers::SHIFT,
        character: Some('%'),
    }
}

// ── Request handler ──────────────────────────────────────────────────────

struct M5State {
    workspace: Option<WorkspaceInstance>,
}

struct M5Handler {
    state: Mutex<M5State>,
}

impl M5Handler {
    fn new() -> Self {
        Self {
            state: Mutex::new(M5State { workspace: None }),
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

impl RequestHandler for M5Handler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(_) => {
                let def = match load_workspace_definition("m5-gate.yaml", M5_WORKSPACE_YAML) {
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
                    WorkspaceInstanceId(500),
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

                Some(Envelope::new(&envelope.id, &CaptureResult { text }))
            }

            _ => None,
        }
    }
}

// ── Polling helper ──────────────────────────────────────────────────────

async fn poll_capture_until(
    reader: &mut wtd_ui::host_client::UiIpcReader,
    writer: &mut wtd_ui::host_client::UiIpcWriter,
    target: &str,
    marker: &str,
    timeout: Duration,
) -> String {
    let start = tokio::time::Instant::now();
    let mut last_text = String::new();
    while start.elapsed() < timeout {
        writer
            .write_frame(&Envelope::new(
                "m5-poll",
                &Capture {
                    target: target.to_string(),
                },
            ))
            .await
            .unwrap();

        let resp = reader.read_frame().await.unwrap();
        assert_eq!(
            resp.msg_type,
            CaptureResult::TYPE_NAME,
            "Capture for '{}' failed: {:?}",
            target,
            resp.payload
        );
        let cap: CaptureResult = resp.extract_payload().unwrap();
        last_text = cap.text;
        if last_text.contains(marker) {
            return last_text;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    last_text
}

// ── M5 Acceptance Test ──────────────────────────────────────────────────

/// **M5 Acceptance Gate (§37.5)**
///
/// Proves the interactive workspace milestone:
///   - Typing works in panes (keystroke bytes reach ConPTY and produce output)
///   - Single-stroke keybindings dispatch actions (consumed, not forwarded)
///   - Prefix chords work (Ctrl+B,% → split-right)
///   - Mouse click changes pane focus
///   - Text selection and copy/paste work (VT-stripped, bracketed paste)
///   - Command palette opens, fuzzy-searches, and dispatches actions
///
/// The test opens a workspace with a vertical split (two ConPTY sessions)
/// via IPC, verifies live output, then exercises all interactive features
/// and renders a final composited frame with all components.
#[tokio::test]
async fn m5_interactive_workspace_acceptance() {
    // ══════════════════════════════════════════════════════════════════
    // Phase 1: Live terminal content — typing reaches ConPTY
    // ══════════════════════════════════════════════════════════════════

    // ── Start IPC server and connect as UI client ────────────────────
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(
        IpcServer::new(pipe_name.clone(), M5Handler::new())
            .expect("M5: IPC server must start"),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let client = UiIpcClient::connect_to(&pipe_name)
        .await
        .expect("M5: UI client must connect to host");
    let (mut reader, mut writer) = client.split();

    // ── Open workspace with split panes ──────────────────────────────
    writer
        .write_frame(&Envelope::new(
            "m5-open",
            &OpenWorkspace {
                name: "m5-gate".to_string(),
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
        "M5: OpenWorkspace must succeed. Got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // ── Wait for ConPTY sessions to be ready ─────────────────────────
    let timeout = Duration::from_secs(10);

    let editor_text = poll_capture_until(
        &mut reader,
        &mut writer,
        "editor",
        "M5_EDITOR_7Q2X",
        timeout,
    )
    .await;
    assert!(
        editor_text.contains("M5_EDITOR_7Q2X"),
        "M5: editor pane must have live ConPTY output. Got:\n{editor_text}"
    );

    let terminal_text = poll_capture_until(
        &mut reader,
        &mut writer,
        "terminal",
        "M5_TERMINAL_4K8Y",
        timeout,
    )
    .await;
    assert!(
        terminal_text.contains("M5_TERMINAL_4K8Y"),
        "M5: terminal pane must have live ConPTY output. Got:\n{terminal_text}"
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 1: Typing works in panes
    // ══════════════════════════════════════════════════════════════════
    //
    // Build key events for "echo M5_TYPE_3R6J" through the keyboard
    // pipeline, collect raw bytes, send via IPC, verify ConPTY output.

    let bindings = default_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    let mut psm = PrefixStateMachine::new(classifier);

    let typed_text = "echo M5_TYPE_3R6J";
    let mut raw_bytes = Vec::new();
    for ch in typed_text.chars() {
        let event = char_key(ch);
        match psm.process(&event) {
            PrefixOutput::SendToSession(bytes) => raw_bytes.extend_from_slice(&bytes),
            other => panic!(
                "M5 criterion 1: regular typing must produce SendToSession, got: {:?}",
                other
            ),
        }
    }

    // Add Enter
    let enter = make_key(KeyName::Enter, Modifiers::NONE, None);
    match psm.process(&enter) {
        PrefixOutput::SendToSession(bytes) => raw_bytes.extend_from_slice(&bytes),
        other => panic!(
            "M5 criterion 1: Enter must produce SendToSession, got: {:?}",
            other
        ),
    }

    assert!(
        !psm.is_prefix_active(),
        "M5 criterion 1: prefix must remain idle during regular typing"
    );

    // Send accumulated bytes to the editor pane via IPC
    let text = String::from_utf8_lossy(&raw_bytes).to_string();
    writer
        .write_frame(&Envelope::new(
            "m5-type",
            &wtd_ipc::message::Send {
                target: "editor".to_string(),
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
        "M5 criterion 1: Send must succeed: {:?}",
        send_resp.payload
    );

    // Poll until the typed marker appears (at least twice: echo + output)
    let mut found = false;
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        writer
            .write_frame(&Envelope::new(
                "m5-cap-type",
                &Capture {
                    target: "editor".to_string(),
                },
            ))
            .await
            .unwrap();

        let resp = reader.read_frame().await.unwrap();
        let cap: CaptureResult = resp.extract_payload().unwrap();
        if cap.text.matches("M5_TYPE_3R6J").count() >= 2 {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        found,
        "M5 criterion 1: typed command must produce visible output containing M5_TYPE_3R6J"
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 2: Single-stroke keybindings dispatch actions
    // ══════════════════════════════════════════════════════════════════

    // Ctrl+Shift+T → new-tab (single-stroke binding)
    let ctrl_shift_t = make_key(
        KeyName::Char('T'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    );
    let result = psm.process(&ctrl_shift_t);
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(
                action_name(action),
                "new-tab",
                "M5 criterion 2: Ctrl+Shift+T must dispatch 'new-tab'"
            );
        }
        other => panic!(
            "M5 criterion 2: expected DispatchAction(new-tab), got: {:?}",
            other
        ),
    }
    assert!(
        !psm.is_prefix_active(),
        "M5 criterion 2: prefix must remain idle after single-stroke binding"
    );

    // Ctrl+Shift+W → close-pane
    let result = psm.process(&make_key(
        KeyName::Char('W'),
        Modifiers::CTRL | Modifiers::SHIFT,
        None,
    ));
    assert!(
        matches!(&result, PrefixOutput::DispatchAction(a) if action_name(a) == "close-pane"),
        "M5 criterion 2: Ctrl+Shift+W must dispatch 'close-pane', got: {:?}",
        result
    );

    // F11 → toggle-fullscreen
    let result = psm.process(&make_key(KeyName::F(11), Modifiers::NONE, None));
    assert!(
        matches!(&result, PrefixOutput::DispatchAction(a) if action_name(a) == "toggle-fullscreen"),
        "M5 criterion 2: F11 must dispatch 'toggle-fullscreen', got: {:?}",
        result
    );

    // Verify that bindings are consumed (not forwarded as raw bytes)
    let regular_t = make_key(KeyName::Char('T'), Modifiers::NONE, Some('t'));
    let result = psm.process(&regular_t);
    assert!(
        matches!(result, PrefixOutput::SendToSession(_)),
        "M5 criterion 2: unbound 't' must forward as raw bytes, got: {:?}",
        result
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 3: Prefix chords work (Ctrl+B,% → split-right)
    // ══════════════════════════════════════════════════════════════════

    // Press Ctrl+B → enters prefix mode
    let result = psm.process(&ctrl_b());
    assert!(
        matches!(result, PrefixOutput::Consumed),
        "M5 criterion 3: Ctrl+B must be consumed to enter prefix mode"
    );
    assert!(
        psm.is_prefix_active(),
        "M5 criterion 3: prefix must be active after Ctrl+B"
    );
    assert_eq!(
        psm.prefix_label(),
        "Ctrl+B",
        "M5 criterion 3: prefix label must be 'Ctrl+B'"
    );

    // Press % → dispatches split-right
    let result = psm.process(&percent());
    assert!(
        !psm.is_prefix_active(),
        "M5 criterion 3: prefix must return to idle after chord dispatch"
    );
    match &result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(
                action_name(action),
                "split-right",
                "M5 criterion 3: Ctrl+B,% must dispatch 'split-right'"
            );
        }
        other => panic!(
            "M5 criterion 3: expected DispatchAction(split-right), got: {:?}",
            other
        ),
    }

    // Verify chord lifecycle: activate → chord → idle → activate → cancel
    psm.process(&ctrl_b());
    assert!(psm.is_prefix_active());
    let esc = make_key(KeyName::Escape, Modifiers::NONE, None);
    let result = psm.process(&esc);
    assert!(
        matches!(result, PrefixOutput::Consumed),
        "M5 criterion 3: Escape must cancel prefix mode"
    );
    assert!(
        !psm.is_prefix_active(),
        "M5 criterion 3: prefix must be idle after Escape cancel"
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 4: Mouse click changes pane focus
    // ══════════════════════════════════════════════════════════════════

    // Build a two-pane split layout matching the workspace
    let mut layout_tree = LayoutTree::new();
    let editor_pane = layout_tree.focus();
    let terminal_pane = layout_tree.split_right(editor_pane.clone()).unwrap();

    let cell_w = 8.0_f32;
    let cell_h = 16.0_f32;
    let tab_h = 32.0_f32;
    let status_h = 24.0_f32;
    let content_rows = ((600.0 - tab_h - status_h) / cell_h) as u16;
    let content_cols = (800.0 / cell_w) as u16;

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&layout_tree, 0.0, tab_h, content_cols, content_rows);

    let rect_editor = pane_layout
        .pane_pixel_rect(&editor_pane)
        .expect("M5: editor pane must have pixel rect");
    let rect_terminal = pane_layout
        .pane_pixel_rect(&terminal_pane)
        .expect("M5: terminal pane must have pixel rect");

    assert!(
        rect_terminal.x > rect_editor.x,
        "M5 criterion 4: terminal pane must be right of editor pane"
    );

    // Click in terminal pane (right side) → FocusPane(terminal)
    let click_x = rect_terminal.x + rect_terminal.width / 2.0;
    let click_y = rect_terminal.y + rect_terminal.height / 2.0;
    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(
                id, terminal_pane,
                "M5 criterion 4: clicking terminal pane must produce FocusPane(terminal)"
            );
        }
        other => panic!(
            "M5 criterion 4: expected FocusPane(terminal), got: {:?}",
            other
        ),
    }
    pane_layout.on_mouse_up(click_x, click_y);

    // Click in editor pane (left side) → FocusPane(editor)
    let click_x = rect_editor.x + rect_editor.width / 2.0;
    let click_y = rect_editor.y + rect_editor.height / 2.0;
    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(
                id, editor_pane,
                "M5 criterion 4: clicking editor pane must produce FocusPane(editor)"
            );
        }
        other => panic!(
            "M5 criterion 4: expected FocusPane(editor), got: {:?}",
            other
        ),
    }
    pane_layout.on_mouse_up(click_x, click_y);

    // Verify splitter exists between panes
    assert!(
        pane_layout.splitter_count() > 0,
        "M5 criterion 4: split layout must have a splitter for drag interaction"
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 5: Text selection and copy/paste work
    // ══════════════════════════════════════════════════════════════════

    // Create a ScreenBuffer with styled content
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[1;32mHello M5 Gate\x1b[0m - interactive test");

    // Extract via TextSelection — cells are VT-stripped
    let selection = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 12,
    };
    let extracted = extract_selection_text(&screen, &selection);
    assert_eq!(
        extracted, "Hello M5 Gate",
        "M5 criterion 5: selection must extract plain text without VT formatting"
    );

    // VT stripping as safety layer
    let raw_vt = "\x1b[1;31mERROR\x1b[0m: test failed";
    let stripped = strip_vt(raw_vt);
    assert_eq!(
        stripped, "ERROR: test failed",
        "M5 criterion 5: strip_vt must remove all VT sequences"
    );

    // Clipboard round-trip (copy → read)
    copy_to_clipboard(&extracted).expect("M5 criterion 5: copy must succeed");
    let from_clipboard = read_from_clipboard().expect("M5 criterion 5: read must succeed");
    assert_eq!(
        from_clipboard, extracted,
        "M5 criterion 5: clipboard round-trip must preserve text"
    );

    // Multi-row selection
    let mut multi_screen = ScreenBuffer::new(40, 5, 0);
    multi_screen.advance(b"Line one\r\nLine two");
    let multi_sel = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 1,
        end_col: 7,
    };
    let multi_text = extract_selection_text(&multi_screen, &multi_sel);
    assert!(
        multi_text.contains("Line one") && multi_text.contains("Line two"),
        "M5 criterion 5: multi-row selection must include both lines"
    );

    // Bracketed paste mode
    let mut paste_screen = ScreenBuffer::new(80, 24, 0);
    paste_screen.advance(b"\x1b[?2004h");
    assert!(
        paste_screen.bracketed_paste(),
        "M5 criterion 5: DECSET 2004 must enable bracketed paste"
    );
    let bracketed = prepare_paste("pasted text", paste_screen.bracketed_paste());
    assert_eq!(
        bracketed,
        b"\x1b[200~pasted text\x1b[201~",
        "M5 criterion 5: paste must be wrapped in bracketed paste markers"
    );

    paste_screen.advance(b"\x1b[?2004l");
    assert!(
        !paste_screen.bracketed_paste(),
        "M5 criterion 5: DECRST 2004 must disable bracketed paste"
    );
    let plain = prepare_paste("pasted text", paste_screen.bracketed_paste());
    assert_eq!(
        plain,
        b"pasted text",
        "M5 criterion 5: paste without bracketed mode must be raw bytes"
    );

    // ══════════════════════════════════════════════════════════════════
    // Criterion 6: Command palette opens, searches, and dispatches
    // ══════════════════════════════════════════════════════════════════

    let hwnd = create_test_window("m5_acceptance");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config)
        .expect("M5: TerminalRenderer must initialise D2D/DWrite resources");
    let dw = renderer.dw_factory().clone();

    let mut palette = CommandPalette::new(&dw, &bindings).unwrap();

    // Initially not visible
    assert!(
        !palette.is_visible(),
        "M5 criterion 6: palette must start hidden"
    );

    // Show palette
    palette.show();
    assert!(
        palette.is_visible(),
        "M5 criterion 6: palette must be visible after show()"
    );
    assert_eq!(
        palette.entry_count(),
        36,
        "M5 criterion 6: v1 catalog must have 36 actions"
    );
    assert_eq!(
        palette.filtered_count(),
        36,
        "M5 criterion 6: empty query must show all entries"
    );

    // Fuzzy search for "split"
    for ch in "split".chars() {
        let result = palette.on_key_event(&char_key(ch));
        assert_eq!(result, PaletteResult::Consumed);
    }
    assert_eq!(palette.query(), "split");
    let filtered = palette.filtered_count();
    assert!(
        filtered > 0 && filtered < 36,
        "M5 criterion 6: 'split' must narrow results (got {filtered})"
    );

    // Select with Enter → dispatches a split-related action
    let result = palette.on_key_event(&make_key(KeyName::Enter, Modifiers::NONE, None));
    match &result {
        PaletteResult::Action(action) => {
            let name = action_name(action);
            assert!(
                name.contains("split"),
                "M5 criterion 6: first match for 'split' must be a split action, got: {name}"
            );
        }
        other => panic!(
            "M5 criterion 6: expected Action for Enter, got: {:?}",
            other
        ),
    }
    assert!(
        !palette.is_visible(),
        "M5 criterion 6: palette must auto-hide after dispatch"
    );

    // Keyboard navigation: re-open, Down/Up, Escape dismisses
    palette.show();
    assert_eq!(palette.selected_index(), 0);
    palette.on_key_event(&make_key(KeyName::Down, Modifiers::NONE, None));
    assert_eq!(
        palette.selected_index(),
        1,
        "M5 criterion 6: Down arrow must advance selection"
    );
    palette.on_key_event(&make_key(KeyName::Up, Modifiers::NONE, None));
    assert_eq!(
        palette.selected_index(),
        0,
        "M5 criterion 6: Up arrow must move selection back"
    );
    let result = palette.on_key_event(&make_key(KeyName::Escape, Modifiers::NONE, None));
    assert_eq!(
        result,
        PaletteResult::Dismissed,
        "M5 criterion 6: Escape must dismiss palette"
    );
    assert!(!palette.is_visible());

    // ══════════════════════════════════════════════════════════════════
    // Phase 2: Full composited render proving all criteria together
    // ══════════════════════════════════════════════════════════════════

    let (cell_w, cell_h) = renderer.cell_size();

    // Tab strip
    let mut tab_strip = TabStrip::new(&dw).unwrap();
    tab_strip.add_tab("dev".to_string());
    tab_strip.layout(800.0);

    // Pane layout with renderer cell dimensions
    let tab_h = tab_strip.height();
    let status_h = 24.0_f32;
    let content_rows = ((600.0 - tab_h - status_h) / cell_h) as u16;
    let content_cols = (800.0 / cell_w) as u16;

    let mut render_tree = LayoutTree::new();
    let render_pane1 = render_tree.focus();
    let render_pane2 = render_tree.split_right(render_pane1.clone()).unwrap();

    let mut render_pane_layout = PaneLayout::new(cell_w, cell_h);
    render_pane_layout.update(&render_tree, 0.0, tab_h, content_cols, content_rows);

    let pr1 = render_pane_layout.pane_pixel_rect(&render_pane1).unwrap();
    let pr2 = render_pane_layout.pane_pixel_rect(&render_pane2).unwrap();

    // Populate screen buffers with live content markers
    let p1_cols = (pr1.width / cell_w) as u16;
    let p2_cols = (pr2.width / cell_w) as u16;

    let mut screen1 = ScreenBuffer::new(p1_cols.max(1), content_rows, 100);
    screen1.advance(b"C:\\> echo M5_TYPE_3R6J\r\nM5_TYPE_3R6J\r\n\r\nC:\\> ");

    let mut screen2 = ScreenBuffer::new(p2_cols.max(1), content_rows, 100);
    screen2.advance(b"C:\\> echo M5_TERMINAL_4K8Y\r\nM5_TERMINAL_4K8Y\r\n\r\nC:\\> ");

    // Status bar
    let mut status_bar = StatusBar::new(&dw).unwrap();
    status_bar.set_workspace_name("m5-gate".to_string());
    status_bar.set_pane_path("m5-gate/dev/editor".to_string());
    status_bar.set_session_status(SessionStatus::Running);
    status_bar.layout(800.0);

    // Text selection on pane 1
    let sel = TextSelection {
        start_row: 1,
        start_col: 0,
        end_row: 1,
        end_col: 11,
    };

    // Render composited frame with all components
    renderer.begin_draw();
    renderer.clear_background();

    tab_strip
        .paint(renderer.render_target())
        .expect("M5: tab strip must render");

    renderer
        .paint_pane_viewport(
            &screen1,
            pr1.x,
            pr1.y,
            pr1.width,
            pr1.height,
            Some(&sel),
        )
        .expect("M5: editor pane viewport with selection must render");

    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .expect("M5: terminal pane viewport must render");

    render_pane_layout
        .paint(renderer.render_target(), &render_pane1)
        .expect("M5: pane layout with focus indicator must render");

    status_bar
        .paint(renderer.render_target(), 600.0 - status_bar.height())
        .expect("M5: status bar must render");

    renderer
        .end_draw()
        .expect("M5: composited frame must complete");

    // Render a second frame with palette overlay
    palette.show();
    for ch in "zoom".chars() {
        palette.on_key_event(&char_key(ch));
    }

    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer
        .paint_pane_viewport(&screen1, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .unwrap();
    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .unwrap();
    render_pane_layout
        .paint(renderer.render_target(), &render_pane1)
        .unwrap();
    status_bar
        .paint(renderer.render_target(), 600.0 - status_bar.height())
        .unwrap();
    palette
        .paint(renderer.render_target(), 800.0, 600.0)
        .expect("M5: command palette overlay must render");
    renderer
        .end_draw()
        .expect("M5: composited frame with palette overlay must complete");

    assert!(palette.is_visible());
    assert!(
        palette.filtered_count() >= 1,
        "M5: 'zoom' must match at least one action"
    );

    // ══════════════════════════════════════════════════════════════════
    // Cleanup
    // ══════════════════════════════════════════════════════════════════

    destroy_test_window(hwnd);
    let _ = shutdown_tx.send(true);
    drop(reader);
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}
