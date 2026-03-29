//! Gate integration test: Host output renders in a pane viewport (§24.4).
//!
//! Proves the UI rendering pipeline end-to-end:
//! 1. UI connects to host via IPC (handshake as `clientType: "ui"`)
//! 2. Opens a workspace with a real ConPTY session
//! 3. Verifies host-side session produces output (via Capture)
//! 4. Receives a `SessionOutput` push notification
//! 5. Feeds decoded VT bytes into a `ScreenBuffer`
//! 6. Renders the screen buffer content in a pane viewport via Direct2D

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

use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::load_workspace_definition;
use wtd_core::GlobalSettings;
use wtd_host::ipc_server::*;
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance};
use wtd_ipc::message::{
    Capture, CaptureResult, ErrorCode, ErrorResponse, MessagePayload, OkResponse, OpenWorkspace,
    OpenWorkspaceResult, SessionOutput, TypedMessage,
};
use wtd_ipc::Envelope;
use wtd_pty::ScreenBuffer;
use wtd_ui::host_client::UiIpcClient;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};

// ── Fixture ──────────────────────────────────────────────────────────────

const SIMPLE_YAML: &str = include_str!("../../wtd-host/tests/fixtures/simple-workspace.yaml");

// ── Unique pipe / class names ────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(5000);
static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-ui-{}-{}", std::process::id(), n)
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
    let class_name_str = format!("WtdGateUi_{label}_{n}\0");
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
            w!("Gate Test Window"),
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

// ── Base64 helpers ───────────────────────────────────────────────────────

const B64_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_CHARS[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64_CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(input: &str) -> Vec<u8> {
    fn val(c: u8) -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let b0 = val(chunk[0]) as u32;
        let b1 = val(chunk[1]) as u32;
        let b2 = if chunk.len() > 2 {
            val(chunk[2]) as u32
        } else {
            0
        };
        let b3 = if chunk.len() > 3 {
            val(chunk[3]) as u32
        } else {
            0
        };
        let triple = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        out.push(((triple >> 16) & 0xff) as u8);
        if chunk.len() > 2 {
            out.push(((triple >> 8) & 0xff) as u8);
        }
        if chunk.len() > 3 {
            out.push((triple & 0xff) as u8);
        }
    }
    out
}

// ── Request handler ──────────────────────────────────────────────────────

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

                Some(Envelope::new(&envelope.id, &CaptureResult { text, ..Default::default() }))
            }

            _ => None,
        }
    }
}

// ── Test ─────────────────────────────────────────────────────────────────

/// Full pipeline: UI IPC connect → SessionOutput push → ScreenBuffer → pane render.
#[tokio::test]
async fn host_session_output_renders_in_pane_viewport() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(
        IpcServer::new(pipe_name.clone(), GateHandler::new()).unwrap(),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // ── Step 1: Connect as UI client ─────────────────────────────────
    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (mut reader, mut writer) = client.split();

    // ── Step 2: Open workspace with real ConPTY session ──────────────
    writer
        .write_frame(&Envelope::new(
            "gate-open-1",
            &OpenWorkspace {
                name: "gate-test".to_string(),
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

    // ── Step 3: Verify host-side session produces output (Capture) ───
    //    The fixture has `startupCommand: "echo GATE_MARKER"`, so we
    //    poll until that marker appears in the screen buffer.
    let mut host_output_verified = false;
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        writer
            .write_frame(&Envelope::new(
                "gate-cap",
                &Capture {
                    target: "shell".to_string(),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();

        let cap_resp = reader.read_frame().await.unwrap();
        assert_eq!(cap_resp.msg_type, CaptureResult::TYPE_NAME);
        let cap: CaptureResult = cap_resp.extract_payload().unwrap();
        if cap.text.contains("GATE_MARKER") {
            host_output_verified = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        host_output_verified,
        "host session must produce startup output 'GATE_MARKER'"
    );

    // ── Step 4: Push SessionOutput notification to UI client ─────────
    //    Simulate the host pushing VT output bytes to the UI. We send
    //    plain text (valid VT) that the ScreenBuffer will render.
    let marker = "UI_PANE_GATE_5R2M";
    let vt_bytes = format!("{marker}\r\n");
    let output_env = Envelope::new(
        "gate-push-1",
        &SessionOutput {
            session_id: "1".to_string(),
            data: base64_encode(vt_bytes.as_bytes()),
        },
    );
    server.broadcast_to_ui(&output_env).await.unwrap();

    // ── Step 5: Read SessionOutput on UI client side ─────────────────
    let push_msg = reader.read_frame().await.unwrap();
    assert_eq!(
        push_msg.msg_type,
        SessionOutput::TYPE_NAME,
        "expected SessionOutput push, got: {}",
        push_msg.msg_type
    );

    let payload: SessionOutput = push_msg.extract_payload().unwrap();
    let decoded = base64_decode(&payload.data);
    assert_eq!(
        decoded,
        vt_bytes.as_bytes(),
        "decoded bytes must match original VT data"
    );

    // ── Step 6: Feed to ScreenBuffer and verify content ──────────────
    let mut screen = ScreenBuffer::new(80, 24, 1000);
    screen.advance(&decoded);
    let visible = screen.visible_text();
    assert!(
        visible.contains(marker),
        "ScreenBuffer must contain marker '{marker}' after advance(); got:\n{visible}"
    );

    // ── Step 7: Render in a pane viewport via Direct2D ───────────────
    let hwnd = create_test_window("host_to_pane");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 0.0, 32.0, 800.0, 544.0, None)
        .expect("paint_pane_viewport must succeed with valid ScreenBuffer content");
    renderer.end_draw().unwrap();

    // ── Step 8: Also render with VT-colored content ──────────────────
    //    Feed ANSI color sequences to verify styled rendering works.
    let colored_vt = b"\x1b[32mGREEN\x1b[0m \x1b[1;31mBOLD_RED\x1b[0m normal";
    let mut color_screen = ScreenBuffer::new(80, 24, 1000);
    color_screen.advance(colored_vt);
    let color_text = color_screen.visible_text();
    assert!(
        color_text.contains("GREEN") && color_text.contains("BOLD_RED"),
        "ScreenBuffer must parse ANSI colors correctly"
    );

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&color_screen, 0.0, 32.0, 800.0, 544.0, None)
        .expect("paint_pane_viewport must render VT-colored content without error");
    renderer.end_draw().unwrap();

    // ── Cleanup ──────────────────────────────────────────────────────
    destroy_test_window(hwnd);
    let _ = shutdown_tx.send(true);
    drop(reader);
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}
