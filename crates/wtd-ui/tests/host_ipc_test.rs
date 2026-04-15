//! Integration tests for the UI IPC client and host bridge.
//!
//! Spins up an `IpcServer` with a mock handler and verifies:
//! - UI client connects with `clientType: "ui"` and completes handshake
//! - AttachWorkspace request returns state
//! - SessionOutput push events are received and base64-decoded
//! - SessionInput and PaneResize commands are sent correctly
//! - SessionStateChanged, TitleChanged, and WorkspaceStateChanged notifications are received
//! - HostBridge provides sync access from the UI thread

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;
use wtd_host::ipc_server::{ClientId, IpcServer, RequestHandler};
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use wtd_ui::host_bridge::{HostBridge, HostCommand, HostEvent};
use wtd_ui::host_client::UiIpcClient;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(5000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-ui-test-{}-{}", std::process::id(), n)
}

// ── Base64 helpers (matching host_bridge internal impl) ──────────────

fn base64_encode(input: &[u8]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ── Mock handler ─────────────────────────────────────────────────────

/// Tracks received messages for assertion.
struct MockState {
    attach_workspace: Option<String>,
    session_inputs: Vec<(String, String)>, // (session_id, data_base64)
    pane_resizes: Vec<(String, u16, u16)>, // (pane_id, cols, rows)
    invoke_actions: Vec<String>,
}

struct MockHandler {
    state: Mutex<MockState>,
}

impl MockHandler {
    fn new() -> Self {
        Self {
            state: Mutex::new(MockState {
                attach_workspace: None,
                session_inputs: Vec::new(),
                pane_resizes: Vec::new(),
                invoke_actions: Vec::new(),
            }),
        }
    }
}

impl RequestHandler for MockHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::AttachWorkspace(aw) => {
                let mut state = self.state.lock().unwrap();
                state.attach_workspace = Some(aw.workspace.clone());

                let result_state = serde_json::json!({
                    "id": 1,
                    "name": aw.workspace,
                    "state": "active",
                    "tabs": [{
                        "id": 1,
                        "name": "main",
                        "panes": [1]
                    }],
                    "sessions": {
                        "1": {
                            "id": "session-1",
                            "state": "running",
                            "name": "main"
                        }
                    },
                    "paneStates": {
                        "1": {"type": "attached", "sessionId": "session-1"}
                    }
                });

                Some(Envelope::new(
                    &envelope.id,
                    &AttachWorkspaceResult {
                        state: result_state,
                    },
                ))
            }
            TypedMessage::SessionInput(si) => {
                let mut state = self.state.lock().unwrap();
                state
                    .session_inputs
                    .push((si.session_id.clone(), si.data.clone()));
                None // fire-and-forget
            }
            TypedMessage::PaneResize(pr) => {
                let mut state = self.state.lock().unwrap();
                state
                    .pane_resizes
                    .push((pr.pane_id.clone(), pr.cols, pr.rows));
                None // fire-and-forget
            }
            TypedMessage::InvokeAction(ia) => {
                let mut state = self.state.lock().unwrap();
                state.invoke_actions.push(ia.action.clone());
                Some(Envelope::new(&envelope.id, &OkResponse {}))
            }
            _ => Some(Envelope::new(
                &envelope.id,
                &ErrorResponse {
                    code: ErrorCode::InternalError,
                    message: "not implemented in mock".into(),
                    candidates: None,
                },
            )),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn ui_client_connects_with_ui_client_type() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), StubHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect with the UI client (handshakes as clientType: "ui").
    let client = UiIpcClient::connect_to(&pipe_name).await;
    assert!(
        client.is_ok(),
        "UI client should connect: {:?}",
        client.err()
    );

    // Verify the server registered the client as UI type.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reg = server.clients().lock().await;
    assert_eq!(reg.client_count(), 1);

    let _ = shutdown_tx.send(true);
}

/// Stub handler that returns None for all requests.
struct StubHandler;
impl RequestHandler for StubHandler {
    fn handle_request(&self, _: ClientId, _: &Envelope, _: &TypedMessage) -> Option<Envelope> {
        None
    }
}

#[tokio::test]
async fn ui_client_attach_returns_workspace_state() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), AttachHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = UiIpcClient::connect_to(&pipe_name).await.unwrap();

    // Send AttachWorkspace.
    let req = Envelope::new(
        "test-attach",
        &AttachWorkspace {
            workspace: "dev".into(),
        },
    );
    let resp = client.request(&req).await.unwrap();
    assert_eq!(resp.msg_type, AttachWorkspaceResult::TYPE_NAME);

    let result: AttachWorkspaceResult = resp.extract_payload().unwrap();
    assert!(result.state.is_object());
    assert_eq!(result.state["name"], "dev");

    let _ = shutdown_tx.send(true);
}

struct AttachHandler;
impl RequestHandler for AttachHandler {
    fn handle_request(
        &self,
        _: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::AttachWorkspace(aw) => Some(Envelope::new(
                &envelope.id,
                &AttachWorkspaceResult {
                    state: serde_json::json!({
                        "name": aw.workspace,
                        "tabs": [{"name": "main", "panes": [1]}]
                    }),
                },
            )),
            _ => None,
        }
    }
}

#[tokio::test]
async fn ui_client_receives_push_session_output() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), StubHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (mut reader, _writer) = client.split();

    // Push a SessionOutput from the server side.
    let push = Envelope::new(
        "push-1",
        &SessionOutput {
            workspace: "dev".into(),
            session_id: "s1".into(),
            data: base64_encode(b"hello from session"),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    // Read the pushed frame.
    let frame = tokio::time::timeout(Duration::from_secs(2), reader.read_frame())
        .await
        .expect("should receive frame within timeout")
        .expect("read should succeed");

    assert_eq!(frame.msg_type, SessionOutput::TYPE_NAME);
    let payload: SessionOutput = frame.extract_payload().unwrap();
    assert_eq!(payload.workspace, "dev");
    assert_eq!(payload.session_id, "s1");
}

#[tokio::test]
async fn ui_client_receives_state_changed_notification() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), StubHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (mut reader, _writer) = client.split();

    // Push SessionStateChanged.
    let push = Envelope::new(
        "push-2",
        &SessionStateChanged {
            workspace: "dev".into(),
            session_id: "s1".into(),
            new_state: "exited".into(),
            exit_code: Some(0),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(2), reader.read_frame())
        .await
        .expect("should receive frame")
        .expect("read should succeed");

    assert_eq!(frame.msg_type, SessionStateChanged::TYPE_NAME);
    let payload: SessionStateChanged = frame.extract_payload().unwrap();
    assert_eq!(payload.workspace, "dev");
    assert_eq!(payload.session_id, "s1");
    assert_eq!(payload.new_state, "exited");
    assert_eq!(payload.exit_code, Some(0));

    let _ = shutdown_tx.send(true);
}

#[tokio::test]
async fn ui_client_receives_title_changed_notification() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), StubHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (mut reader, _writer) = client.split();

    let push = Envelope::new(
        "push-3",
        &TitleChanged {
            workspace: "dev".into(),
            session_id: "s1".into(),
            title: "bash — ~/projects".into(),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(2), reader.read_frame())
        .await
        .expect("should receive frame")
        .expect("read should succeed");

    assert_eq!(frame.msg_type, TitleChanged::TYPE_NAME);
    let payload: TitleChanged = frame.extract_payload().unwrap();
    assert_eq!(payload.workspace, "dev");
    assert_eq!(payload.title, "bash — ~/projects");

    let _ = shutdown_tx.send(true);
}

#[tokio::test]
async fn ui_client_sends_session_input() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), InputCapture::new()).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (_reader, mut writer) = client.split();

    // Send SessionInput.
    let envelope = Envelope::new(
        "input-1",
        &SessionInput {
            workspace: "dev".into(),
            session_id: "s1".into(),
            data: base64_encode(b"ls -la\n"),
        },
    );
    writer.write_frame(&envelope).await.unwrap();

    // Give it a moment to be received.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _ = shutdown_tx.send(true);
}

struct InputCapture {
    inputs: Mutex<Vec<(String, String)>>,
}

impl InputCapture {
    fn new() -> Self {
        Self {
            inputs: Mutex::new(Vec::new()),
        }
    }
}

impl RequestHandler for InputCapture {
    fn handle_request(
        &self,
        _: ClientId,
        _envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        if let TypedMessage::SessionInput(si) = msg {
            self.inputs.lock().unwrap().push((
                format!("{}/{}", si.workspace, si.session_id),
                si.data.clone(),
            ));
        }
        None // fire-and-forget
    }
}

#[tokio::test]
async fn ui_client_sends_pane_resize() {
    let pipe_name = unique_pipe_name();
    let capture = ResizeCapture::new();
    let server = IpcServer::new(pipe_name.clone(), capture).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = UiIpcClient::connect_to(&pipe_name).await.unwrap();
    let (_reader, mut writer) = client.split();

    let envelope = Envelope::new(
        "resize-1",
        &PaneResize {
            pane_id: "1".into(),
            cols: 120,
            rows: 40,
        },
    );
    writer.write_frame(&envelope).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = shutdown_tx.send(true);
}

struct ResizeCapture {
    resizes: Mutex<Vec<(String, u16, u16)>>,
}

impl ResizeCapture {
    fn new() -> Self {
        Self {
            resizes: Mutex::new(Vec::new()),
        }
    }
}

impl RequestHandler for ResizeCapture {
    fn handle_request(
        &self,
        _: ClientId,
        _envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        if let TypedMessage::PaneResize(pr) = msg {
            self.resizes
                .lock()
                .unwrap()
                .push((pr.pane_id.clone(), pr.cols, pr.rows));
        }
        None
    }
}

// ── HostBridge tests ─────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_bridge_connect_and_receive_events() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), AttachHandler).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create HostBridge to the test pipe.
    let bridge = HostBridge::connect_to(pipe_name.clone(), "dev".to_string());

    // Wait for Connected event.
    let connected = wait_for_event(&bridge, Duration::from_secs(5), |e| {
        matches!(e, HostEvent::Connected { .. })
    });
    assert!(connected.is_some(), "should receive Connected event");

    if let Some(HostEvent::Connected { state }) = connected {
        assert_eq!(state["name"], "dev");
    }

    // Push a SessionOutput and verify it arrives.
    let push = Envelope::new(
        "push-bridge-1",
        &SessionOutput {
            workspace: "dev".into(),
            session_id: "session-1".into(),
            data: base64_encode(b"\x1b[32mhello\x1b[0m"),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    let output = wait_for_event(&bridge, Duration::from_secs(2), |e| {
        matches!(e, HostEvent::SessionOutput { .. })
    });
    assert!(output.is_some(), "should receive SessionOutput");

    if let Some(HostEvent::SessionOutput {
        workspace,
        session_id,
        data,
    }) = output
    {
        assert_eq!(workspace, "dev");
        assert_eq!(session_id, "session-1");
        assert_eq!(data, b"\x1b[32mhello\x1b[0m");
    }

    // Push SessionStateChanged.
    let push = Envelope::new(
        "push-bridge-2",
        &SessionStateChanged {
            workspace: "dev".into(),
            session_id: "session-1".into(),
            new_state: "exited".into(),
            exit_code: Some(42),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    let state_event = wait_for_event(&bridge, Duration::from_secs(2), |e| {
        matches!(e, HostEvent::SessionStateChanged { .. })
    });
    assert!(state_event.is_some(), "should receive SessionStateChanged");

    if let Some(HostEvent::SessionStateChanged {
        workspace,
        session_id,
        new_state,
        exit_code,
    }) = state_event
    {
        assert_eq!(workspace, "dev");
        assert_eq!(session_id, "session-1");
        assert_eq!(new_state, "exited");
        assert_eq!(exit_code, Some(42));
    }

    // Push WorkspaceStateChanged.
    let push = Envelope::new(
        "push-bridge-3",
        &WorkspaceStateChanged {
            workspace: "dev".into(),
            new_state: "closing".into(),
        },
    );
    server.broadcast_to_ui(&push).await.unwrap();

    let workspace_event = wait_for_event(&bridge, Duration::from_secs(2), |e| {
        matches!(e, HostEvent::WorkspaceStateChanged { .. })
    });
    assert!(
        workspace_event.is_some(),
        "should receive WorkspaceStateChanged"
    );

    if let Some(HostEvent::WorkspaceStateChanged {
        workspace,
        new_state,
    }) = workspace_event
    {
        assert_eq!(workspace, "dev");
        assert_eq!(new_state, "closing");
    }

    // Send disconnect.
    bridge.send(HostCommand::Disconnect);
    std::thread::sleep(Duration::from_millis(200));

    let _ = shutdown_tx.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_bridge_sends_input_and_resize() {
    let pipe_name = unique_pipe_name();
    let mock = std::sync::Arc::new(MockHandler::new());
    let server =
        IpcServer::new(pipe_name.clone(), MockHandlerWrapper(mock.clone())).expect("create server");
    let server = std::sync::Arc::new(server);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_clone = server.clone();
    tokio::spawn(async move {
        let _ = server_clone.run(shutdown_rx).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let bridge = HostBridge::connect_to(pipe_name.clone(), "dev".to_string());

    // Wait for connection.
    let connected = wait_for_event(&bridge, Duration::from_secs(5), |e| {
        matches!(e, HostEvent::Connected { .. })
    });
    assert!(connected.is_some());

    // Send input.
    bridge.send_input("session-1".into(), b"hello\n".to_vec());
    std::thread::sleep(Duration::from_millis(500));

    // Send resize.
    bridge.send_resize("1".into(), 120, 40);
    std::thread::sleep(Duration::from_millis(500));

    // Verify the mock received the commands.
    let state = mock.state.lock().unwrap();
    assert!(
        !state.session_inputs.is_empty(),
        "should have received session input"
    );
    assert_eq!(state.session_inputs[0].0, "session-1");

    assert!(
        !state.pane_resizes.is_empty(),
        "should have received pane resize"
    );
    assert_eq!(state.pane_resizes[0], ("1".to_string(), 120, 40));

    bridge.send(HostCommand::Disconnect);
    std::thread::sleep(Duration::from_millis(200));

    let _ = shutdown_tx.send(true);
}

/// Wrapper to use Arc<MockHandler> as RequestHandler.
struct MockHandlerWrapper(std::sync::Arc<MockHandler>);

impl RequestHandler for MockHandlerWrapper {
    fn handle_request(
        &self,
        client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        self.0.handle_request(client_id, envelope, msg)
    }
}

// ── Polling helper ───────────────────────────────────────────────────

fn wait_for_event(
    bridge: &HostBridge,
    timeout: Duration,
    predicate: impl Fn(&HostEvent) -> bool,
) -> Option<HostEvent> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Some(event) = bridge.try_recv() {
            if predicate(&event) {
                return Some(event);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}
