//! Bridge between the async IPC connection and the synchronous Win32 UI loop.
//!
//! [`HostBridge`] spawns a background thread with a tokio runtime that
//! manages the named-pipe connection to `wtd-host`. The main UI thread
//! polls [`HostBridge::try_recv`] each frame to drain incoming events
//! and calls [`HostBridge::send`] to push fire-and-forget commands.

use std::sync::mpsc;

use serde_json::Value;
use wtd_ipc::message::{self, AttachWorkspace, AttachWorkspaceResult, ErrorResponse, MessagePayload};
use wtd_ipc::{parse_envelope, Envelope, TypedMessage};

use crate::host_client::UiIpcClient;

// ── Events pushed to the UI thread ───────────────────────────────────

/// Events delivered from the host to the UI main loop.
#[derive(Debug)]
pub enum HostEvent {
    /// Connection established and workspace state received.
    Connected {
        state: Value,
    },
    /// Raw VT output for a session (already base64-decoded to bytes).
    SessionOutput {
        session_id: String,
        data: Vec<u8>,
    },
    /// A session changed state (running → exited, etc.).
    SessionStateChanged {
        session_id: String,
        new_state: String,
        exit_code: Option<i32>,
    },
    /// A session's title changed (from VT escape sequence).
    TitleChanged {
        session_id: String,
        title: String,
    },
    /// The layout tree for a tab changed.
    LayoutChanged {
        workspace: String,
        window: String,
        tab: String,
        layout: Value,
    },
    /// Workspace instance state changed.
    WorkspaceStateChanged {
        workspace: String,
        new_state: String,
    },
    /// An error response was received.
    Error {
        message: String,
    },
    /// The IPC connection was lost.
    Disconnected {
        reason: String,
    },
}

// ── Commands sent from the UI thread ─────────────────────────────────

/// Commands sent from the UI thread to the host.
#[derive(Debug)]
pub enum HostCommand {
    /// Forward raw keyboard input bytes to a session (base64-encoded by bridge).
    SessionInput { session_id: String, data: Vec<u8> },
    /// Notify host of a pane resize.
    PaneResize { pane_id: String, cols: u16, rows: u16 },
    /// Invoke an action on the host.
    InvokeAction {
        action: String,
        target_pane_id: Option<String>,
        args: Value,
    },
    /// Disconnect from host.
    Disconnect,
}

// ── HostBridge ───────────────────────────────────────────────────────

/// Synchronous bridge to the async host IPC connection.
///
/// Create with [`HostBridge::connect`], then call [`try_recv`](Self::try_recv)
/// each frame and [`send`](Self::send) to push commands.
pub struct HostBridge {
    event_rx: mpsc::Receiver<HostEvent>,
    cmd_tx: mpsc::Sender<HostCommand>,
}

impl HostBridge {
    /// Connect to the host and attach to the given workspace.
    ///
    /// Spawns a background thread with a tokio runtime. The connection,
    /// handshake, and attach are performed asynchronously; the result
    /// arrives as a [`HostEvent::Connected`] or [`HostEvent::Error`].
    pub fn connect(workspace_name: String) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("wtd-ui-ipc".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime for IPC");
                rt.block_on(ipc_task(workspace_name, event_tx, cmd_rx));
            })
            .expect("failed to spawn IPC thread");

        Self { event_rx, cmd_tx }
    }

    /// Connect to a specific pipe name (for testing).
    pub fn connect_to(pipe_name: String, workspace_name: String) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("wtd-ui-ipc".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime for IPC");
                rt.block_on(ipc_task_to(pipe_name, workspace_name, event_tx, cmd_rx));
            })
            .expect("failed to spawn IPC thread");

        Self { event_rx, cmd_tx }
    }

    /// Poll for the next host event (non-blocking).
    pub fn try_recv(&self) -> Option<HostEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Send a command to the host.
    pub fn send(&self, cmd: HostCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Send raw input bytes to a session.
    pub fn send_input(&self, session_id: String, data: Vec<u8>) {
        self.send(HostCommand::SessionInput { session_id, data });
    }

    /// Notify the host of a pane resize.
    pub fn send_resize(&self, pane_id: String, cols: u16, rows: u16) {
        self.send(HostCommand::PaneResize {
            pane_id,
            cols,
            rows,
        });
    }

    /// Invoke an action on the host.
    pub fn send_action(
        &self,
        action: String,
        target_pane_id: Option<String>,
        args: Value,
    ) {
        self.send(HostCommand::InvokeAction {
            action,
            target_pane_id,
            args,
        });
    }
}

// ── Async IPC task (runs on background thread) ───────────────────────

async fn ipc_task(
    workspace_name: String,
    event_tx: mpsc::Sender<HostEvent>,
    cmd_rx: mpsc::Receiver<HostCommand>,
) {
    match UiIpcClient::connect_and_handshake().await {
        Ok(client) => {
            run_attached(client, workspace_name, event_tx, cmd_rx).await;
        }
        Err(e) => {
            let _ = event_tx.send(HostEvent::Error {
                message: format!("connection failed: {e}"),
            });
        }
    }
}

async fn ipc_task_to(
    pipe_name: String,
    workspace_name: String,
    event_tx: mpsc::Sender<HostEvent>,
    cmd_rx: mpsc::Receiver<HostCommand>,
) {
    match UiIpcClient::connect_to(&pipe_name).await {
        Ok(client) => {
            run_attached(client, workspace_name, event_tx, cmd_rx).await;
        }
        Err(e) => {
            let _ = event_tx.send(HostEvent::Error {
                message: format!("connection failed: {e}"),
            });
        }
    }
}

async fn run_attached(
    mut client: UiIpcClient,
    workspace_name: String,
    event_tx: mpsc::Sender<HostEvent>,
    cmd_rx: mpsc::Receiver<HostCommand>,
) {
    // Send AttachWorkspace and wait for the result.
    let attach_req = Envelope::new(
        "ui-attach-1",
        &AttachWorkspace {
            workspace: workspace_name,
        },
    );

    let attach_result = match client.request(&attach_req).await {
        Ok(resp) => resp,
        Err(e) => {
            let _ = event_tx.send(HostEvent::Disconnected {
                reason: format!("attach failed: {e}"),
            });
            return;
        }
    };

    // Check for error response.
    if attach_result.msg_type == ErrorResponse::TYPE_NAME {
        if let Ok(err) = attach_result.extract_payload::<ErrorResponse>() {
            let _ = event_tx.send(HostEvent::Error {
                message: format!("attach error: {}", err.message),
            });
        } else {
            let _ = event_tx.send(HostEvent::Error {
                message: "attach returned unknown error".into(),
            });
        }
        return;
    }

    // Extract workspace state from attach result.
    if attach_result.msg_type == AttachWorkspaceResult::TYPE_NAME {
        if let Ok(result) = attach_result.extract_payload::<AttachWorkspaceResult>() {
            let _ = event_tx.send(HostEvent::Connected {
                state: result.state,
            });
        } else {
            let _ = event_tx.send(HostEvent::Connected {
                state: Value::Null,
            });
        }
    } else {
        let _ = event_tx.send(HostEvent::Connected {
            state: Value::Null,
        });
    }

    // Split into reader/writer and run the bidirectional message loop.
    let (mut reader, mut writer) = client.split();

    // Wrap cmd_rx in a tokio-friendly channel.
    // We spawn a dedicated OS thread to relay from std::sync::mpsc to tokio::mpsc
    // because the tokio runtime may be single-threaded (current_thread).
    let (tokio_cmd_tx, mut tokio_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<HostCommand>();

    let relay_event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("wtd-ui-cmd-relay".into())
        .spawn(move || {
            loop {
                match cmd_rx.recv() {
                    Ok(HostCommand::Disconnect) => break,
                    Ok(cmd) => {
                        if tokio_cmd_tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    Err(_) => break, // channel closed
                }
            }
            let _ = relay_event_tx.send(HostEvent::Disconnected {
                reason: "UI closed".into(),
            });
        })
        .expect("failed to spawn command relay thread");

    // Main select loop: read from host OR send commands.
    let mut msg_counter: u64 = 0;
    loop {
        tokio::select! {
            result = reader.read_frame() => {
                match result {
                    Ok(envelope) => {
                        if let Some(event) = envelope_to_event(&envelope) {
                            if event_tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(HostEvent::Disconnected {
                            reason: format!("read error: {e}"),
                        });
                        break;
                    }
                }
            }
            Some(cmd) = tokio_cmd_rx.recv() => {
                msg_counter += 1;
                let envelope = command_to_envelope(cmd, msg_counter);
                if let Err(e) = writer.write_frame(&envelope).await {
                    let _ = event_tx.send(HostEvent::Disconnected {
                        reason: format!("write error: {e}"),
                    });
                    break;
                }
            }
            else => break,
        }
    }
}

/// Convert a host push envelope into a UI event.
fn envelope_to_event(envelope: &Envelope) -> Option<HostEvent> {
    let msg = match parse_envelope(envelope) {
        Ok(m) => m,
        Err(_) => return None,
    };

    match msg {
        TypedMessage::SessionOutput(so) => {
            let data = base64_decode(&so.data);
            Some(HostEvent::SessionOutput {
                session_id: so.session_id,
                data,
            })
        }
        TypedMessage::SessionStateChanged(sc) => Some(HostEvent::SessionStateChanged {
            session_id: sc.session_id,
            new_state: sc.new_state,
            exit_code: sc.exit_code,
        }),
        TypedMessage::TitleChanged(tc) => Some(HostEvent::TitleChanged {
            session_id: tc.session_id,
            title: tc.title,
        }),
        TypedMessage::LayoutChanged(lc) => Some(HostEvent::LayoutChanged {
            workspace: lc.workspace,
            window: lc.window,
            tab: lc.tab,
            layout: lc.layout,
        }),
        TypedMessage::WorkspaceStateChanged(wsc) => Some(HostEvent::WorkspaceStateChanged {
            workspace: wsc.workspace,
            new_state: wsc.new_state,
        }),
        TypedMessage::ErrorResponse(er) => Some(HostEvent::Error {
            message: er.message,
        }),
        _ => None, // Ignore unknown push messages
    }
}

/// Convert a UI command into a host envelope.
fn command_to_envelope(cmd: HostCommand, counter: u64) -> Envelope {
    let id = format!("ui-cmd-{counter}");
    match cmd {
        HostCommand::SessionInput { session_id, data } => Envelope::new(
            id,
            &message::SessionInput {
                session_id,
                data: base64_encode(&data),
            },
        ),
        HostCommand::PaneResize {
            pane_id,
            cols,
            rows,
        } => Envelope::new(id, &message::PaneResize { pane_id, cols, rows }),
        HostCommand::InvokeAction {
            action,
            target_pane_id,
            args,
        } => Envelope::new(
            id,
            &message::InvokeAction {
                action,
                target_pane_id,
                args,
            },
        ),
        HostCommand::Disconnect => {
            // Should not reach here — handled by the relay task.
            Envelope::new(id, &message::SessionInput {
                session_id: String::new(),
                data: String::new(),
            })
        }
    }
}

// ── Base64 encode/decode (simple, no external dependency) ────────────

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

    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=' && b != b'\n' && b != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let b0 = val(chunk[0]) as u32;
        let b1 = val(chunk[1]) as u32;
        let b2 = if chunk.len() > 2 { val(chunk[2]) as u32 } else { 0 };
        let b3 = if chunk.len() > 3 { val(chunk[3]) as u32 } else { 0 };
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

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_ipc::message::{InvokeAction, PaneResize, SessionInput};

    #[test]
    fn base64_roundtrip_empty() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_decode(""), Vec::<u8>::new());
    }

    #[test]
    fn base64_roundtrip_hello() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_decode("aGVsbG8="), b"hello");
    }

    #[test]
    fn base64_roundtrip_hello_world() {
        assert_eq!(base64_encode(b"Hello World"), "SGVsbG8gV29ybGQ=");
        assert_eq!(base64_decode("SGVsbG8gV29ybGQ="), b"Hello World");
    }

    #[test]
    fn base64_roundtrip_binary() {
        let data: Vec<u8> = (0..=255).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded);
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_known_vt_bytes() {
        // ESC [ 3 1 m  =  \x1b[31m  (red text)
        let vt = b"\x1b[31mhello\x1b[0m";
        let encoded = base64_encode(vt);
        let decoded = base64_decode(&encoded);
        assert_eq!(decoded, vt);
    }

    #[test]
    fn envelope_to_event_session_output() {
        let envelope = Envelope::new(
            "test-1",
            &message::SessionOutput {
                session_id: "s1".into(),
                data: base64_encode(b"hello"),
            },
        );
        let event = envelope_to_event(&envelope).unwrap();
        match event {
            HostEvent::SessionOutput { session_id, data } => {
                assert_eq!(session_id, "s1");
                assert_eq!(data, b"hello");
            }
            _ => panic!("expected SessionOutput"),
        }
    }

    #[test]
    fn envelope_to_event_session_state_changed() {
        let envelope = Envelope::new(
            "test-2",
            &message::SessionStateChanged {
                session_id: "s1".into(),
                new_state: "exited".into(),
                exit_code: Some(0),
            },
        );
        let event = envelope_to_event(&envelope).unwrap();
        match event {
            HostEvent::SessionStateChanged {
                session_id,
                new_state,
                exit_code,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(new_state, "exited");
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected SessionStateChanged"),
        }
    }

    #[test]
    fn envelope_to_event_title_changed() {
        let envelope = Envelope::new(
            "test-3",
            &message::TitleChanged {
                session_id: "s1".into(),
                title: "bash".into(),
            },
        );
        let event = envelope_to_event(&envelope).unwrap();
        match event {
            HostEvent::TitleChanged { session_id, title } => {
                assert_eq!(session_id, "s1");
                assert_eq!(title, "bash");
            }
            _ => panic!("expected TitleChanged"),
        }
    }

    #[test]
    fn envelope_to_event_layout_changed() {
        let layout_val = serde_json::json!({"type": "pane", "name": "main"});
        let envelope = Envelope::new(
            "test-4",
            &message::LayoutChanged {
                workspace: "dev".into(),
                window: "w1".into(),
                tab: "tab1".into(),
                layout: layout_val.clone(),
            },
        );
        let event = envelope_to_event(&envelope).unwrap();
        match event {
            HostEvent::LayoutChanged {
                workspace,
                tab,
                layout,
                ..
            } => {
                assert_eq!(workspace, "dev");
                assert_eq!(tab, "tab1");
                assert_eq!(layout, layout_val);
            }
            _ => panic!("expected LayoutChanged"),
        }
    }

    #[test]
    fn command_to_envelope_session_input() {
        let env = command_to_envelope(
            HostCommand::SessionInput {
                session_id: "s1".into(),
                data: b"hello".to_vec(),
            },
            1,
        );
        assert_eq!(env.msg_type, SessionInput::TYPE_NAME);
        let payload: SessionInput = env.extract_payload().unwrap();
        assert_eq!(payload.session_id, "s1");
        assert_eq!(base64_decode(&payload.data), b"hello");
    }

    #[test]
    fn command_to_envelope_pane_resize() {
        let env = command_to_envelope(
            HostCommand::PaneResize {
                pane_id: "p1".into(),
                cols: 80,
                rows: 24,
            },
            2,
        );
        assert_eq!(env.msg_type, PaneResize::TYPE_NAME);
        let payload: PaneResize = env.extract_payload().unwrap();
        assert_eq!(payload.pane_id, "p1");
        assert_eq!(payload.cols, 80);
        assert_eq!(payload.rows, 24);
    }

    #[test]
    fn command_to_envelope_invoke_action() {
        let env = command_to_envelope(
            HostCommand::InvokeAction {
                action: "split-right".into(),
                target_pane_id: Some("p1".into()),
                args: serde_json::json!({}),
            },
            3,
        );
        assert_eq!(env.msg_type, InvokeAction::TYPE_NAME);
        let payload: InvokeAction = env.extract_payload().unwrap();
        assert_eq!(payload.action, "split-right");
        assert_eq!(payload.target_pane_id, Some("p1".into()));
    }
}
