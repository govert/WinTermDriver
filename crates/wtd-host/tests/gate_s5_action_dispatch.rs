//! Gate test for S5: Action dispatch modifies workspace via IPC.
//!
//! Verifies that actions dispatched via IPC (split-right, split-down,
//! close-pane, focus-next-pane) modify the workspace layout through
//! the real host request handler. Opens a workspace, invokes actions,
//! and asserts layout tree changes after each dispatch.
//!
//! This is the action-dispatch gate for Slice 5.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer};
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use tokio::net::windows::named_pipe::ClientOptions;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(19000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-s5act-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("s5act-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── Connection helpers ──────────────────────────────────────────────

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
            Err(e) => panic!("unexpected pipe connect error: {:?}", e),
        }
    }
    panic!("timed out waiting for pipe server");
}

async fn do_handshake(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
) {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &Handshake {
                client_type: ClientType::Cli,
                client_version: "1.0.0".to_owned(),
                protocol_version: 1,
            },
        ),
    )
    .await
    .unwrap();
    let ack = read_frame(client).await.unwrap();
    assert_eq!(ack.msg_type, HandshakeAck::TYPE_NAME);
}

// ── Fixture helpers ─────────────────────────────────────────────────

fn create_workspace_file(name: &str, yaml: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-gate-s5act-{}-{}",
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst)
    ));
    let wtd_dir = tmp_dir.join(".wtd");
    std::fs::create_dir_all(&wtd_dir).unwrap();
    let path = wtd_dir.join(format!("{}.yaml", name));
    std::fs::write(&path, yaml).unwrap();
    (tmp_dir, path)
}

// ── IPC request helpers ─────────────────────────────────────────────

async fn open_workspace(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    name: &str,
    file: &str,
) {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: name.to_string(),
                file: Some(file.to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(client).await.unwrap();
    assert_eq!(
        resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );
}

async fn list_panes(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    workspace: &str,
) -> ListPanesResult {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: workspace.to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(client).await.unwrap();
    assert_eq!(resp.msg_type, ListPanesResult::TYPE_NAME);
    resp.extract_payload().unwrap()
}

async fn list_sessions(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    workspace: &str,
) -> ListSessionsResult {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &ListSessions {
                workspace: workspace.to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(client).await.unwrap();
    assert_eq!(resp.msg_type, ListSessionsResult::TYPE_NAME);
    resp.extract_payload().unwrap()
}

async fn invoke_action(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    action: &str,
    target: Option<&str>,
) -> Envelope {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &InvokeAction {
                action: action.to_string(),
                target_pane_id: target.map(|s| s.to_string()),
                args: serde_json::json!({}),
            },
        ),
    )
    .await
    .unwrap();

    read_frame(client).await.unwrap()
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
        write_frame(
            client,
            &Envelope::new(
                &next_id(),
                &Capture {
                    target: target.to_string(),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let resp = read_frame(client).await.unwrap();
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

// ── Test harness ────────────────────────────────────────────────────

struct TestHost {
    _server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    server_task: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    pipe_name: String,
    tmp_dir: std::path::PathBuf,
}

impl TestHost {
    async fn start(workspace_name: &str, yaml: &str) -> (Self, tokio::net::windows::named_pipe::NamedPipeClient) {
        let (tmp_dir, yaml_path) = create_workspace_file(workspace_name, yaml);
        let pipe_name = unique_pipe_name();
        let handler = HostRequestHandler::new(GlobalSettings::default());
        let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let s = server.clone();
        let server_task = tokio::spawn(async move { let _ = s.run(shutdown_rx).await; });

        let mut client = connect_client(&pipe_name).await;
        do_handshake(&mut client).await;

        // Open workspace via file path
        open_workspace(&mut client, workspace_name, &yaml_path.to_string_lossy()).await;

        let host = TestHost {
            _server: server,
            shutdown_tx,
            server_task,
            pipe_name: pipe_name.clone(),
            tmp_dir,
        };

        (host, client)
    }

    async fn shutdown(self, client: tokio::net::windows::named_pipe::NamedPipeClient) {
        let _ = self.shutdown_tx.send(true);
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.server_task).await;
        let _ = std::fs::remove_dir_all(&self.tmp_dir);
    }
}

// ── Gate tests ──────────────────────────────────────────────────────

/// Full action dispatch gate: split-right adds a pane, then close-pane
/// removes it, verifying layout tree changes after each action.
#[tokio::test]
async fn gate_split_right_then_close_pane_modifies_layout() {
    let yaml = r#"
version: 1
name: gate-act1
tabs:
  - name: main
    layout:
      type: pane
      name: editor
      session:
        profile: cmd
        startupCommand: "echo GATE_ACT_READY"
"#;
    let (host, mut client) = TestHost::start("gate-act1", yaml).await;

    // Wait for session to be ready
    poll_capture_until(
        &mut client,
        "editor",
        |text| text.contains("GATE_ACT_READY"),
        Duration::from_secs(10),
    )
    .await;

    // ── Verify initial state: 1 pane, 1 session ──

    let panes = list_panes(&mut client, "gate-act1").await;
    assert_eq!(panes.panes.len(), 1, "should start with 1 pane");
    assert_eq!(panes.panes[0].name, "editor");

    let sessions = list_sessions(&mut client, "gate-act1").await;
    assert_eq!(sessions.sessions.len(), 1, "should start with 1 session");

    // ── split-right: layout grows to 2 panes ──

    let resp = invoke_action(&mut client, "split-right", Some("editor")).await;
    assert_eq!(
        resp.msg_type,
        InvokeActionResult::TYPE_NAME,
        "expected InvokeActionResult, got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );
    let result: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(result.result, "pane-created");
    let new_pane_num = result.pane_id.clone().expect("split-right should return new pane ID");
    // Split-created panes get auto-generated names like "pane-{id}"
    let new_pane_name = format!("pane-{}", new_pane_num);

    let panes = list_panes(&mut client, "gate-act1").await;
    assert_eq!(panes.panes.len(), 2, "split-right should create a second pane");

    let sessions = list_sessions(&mut client, "gate-act1").await;
    assert_eq!(sessions.sessions.len(), 2, "split-right should spawn a session for the new pane");

    // ── close-pane: layout shrinks back to 1 pane ──

    let resp = invoke_action(&mut client, "close-pane", Some(&new_pane_name)).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let result: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(result.result, "pane-closed");

    let panes = list_panes(&mut client, "gate-act1").await;
    assert_eq!(panes.panes.len(), 1, "close-pane should leave 1 pane");
    assert_eq!(panes.panes[0].name, "editor", "original pane should remain");

    let sessions = list_sessions(&mut client, "gate-act1").await;
    assert_eq!(sessions.sessions.len(), 1, "close-pane should remove the session");

    host.shutdown(client).await;
}

/// split-down adds a pane below, verifiable via layout change.
#[tokio::test]
async fn gate_split_down_modifies_layout() {
    let yaml = r#"
version: 1
name: gate-act2
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo GATE_SD_READY"
"#;
    let (host, mut client) = TestHost::start("gate-act2", yaml).await;

    poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("GATE_SD_READY"),
        Duration::from_secs(10),
    )
    .await;

    // ── split-down ──

    let resp = invoke_action(&mut client, "split-down", Some("shell")).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let result: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(result.result, "pane-created");
    assert!(result.pane_id.is_some(), "split-down should return new pane ID");

    let panes = list_panes(&mut client, "gate-act2").await;
    assert_eq!(panes.panes.len(), 2, "split-down should create a second pane");

    let sessions = list_sessions(&mut client, "gate-act2").await;
    assert_eq!(sessions.sessions.len(), 2, "split-down should spawn a session for the new pane");

    host.shutdown(client).await;
}

/// Focus actions succeed and do not alter the pane count.
#[tokio::test]
async fn gate_focus_actions_do_not_change_layout() {
    let yaml = r#"
version: 1
name: gate-act3
tabs:
  - name: main
    layout:
      type: split
      orientation: horizontal
      ratio: 0.5
      children:
        - type: pane
          name: left
          session:
            profile: cmd
        - type: pane
          name: right
          session:
            profile: cmd
"#;
    let (host, mut client) = TestHost::start("gate-act3", yaml).await;

    // Wait for sessions to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    let panes_before = list_panes(&mut client, "gate-act3").await;
    assert_eq!(panes_before.panes.len(), 2);

    // ── focus-next-pane ──

    let resp = invoke_action(&mut client, "focus-next-pane", None).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let result: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(result.result, "ok");

    // ── focus-prev-pane ──

    let resp = invoke_action(&mut client, "focus-prev-pane", None).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let result: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(result.result, "ok");

    // Pane count unchanged
    let panes_after = list_panes(&mut client, "gate-act3").await;
    assert_eq!(
        panes_after.panes.len(),
        2,
        "focus actions should not alter pane count"
    );

    host.shutdown(client).await;
}

/// Multiple splits in sequence: split-right twice creates 3 panes,
/// then closing 2 returns to 1.
#[tokio::test]
async fn gate_multiple_splits_and_closes() {
    let yaml = r#"
version: 1
name: gate-act4
tabs:
  - name: main
    layout:
      type: pane
      name: root
      session:
        profile: cmd
        startupCommand: "echo GATE_MULTI_READY"
"#;
    let (host, mut client) = TestHost::start("gate-act4", yaml).await;

    poll_capture_until(
        &mut client,
        "root",
        |text| text.contains("GATE_MULTI_READY"),
        Duration::from_secs(10),
    )
    .await;

    // ── First split-right ──

    let resp = invoke_action(&mut client, "split-right", Some("root")).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let r1: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(r1.result, "pane-created");
    let pane_a_name = format!("pane-{}", r1.pane_id.as_ref().unwrap());

    let panes = list_panes(&mut client, "gate-act4").await;
    assert_eq!(panes.panes.len(), 2);

    // ── Second split-right (on the new pane) ──

    let resp = invoke_action(&mut client, "split-right", Some(&pane_a_name)).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let r2: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(r2.result, "pane-created");
    let pane_b_name = format!("pane-{}", r2.pane_id.as_ref().unwrap());

    let panes = list_panes(&mut client, "gate-act4").await;
    assert_eq!(panes.panes.len(), 3, "two splits should yield 3 panes");

    let sessions = list_sessions(&mut client, "gate-act4").await;
    assert_eq!(sessions.sessions.len(), 3, "each pane should have a session");

    // ── Close pane_b ──

    let resp = invoke_action(&mut client, "close-pane", Some(&pane_b_name)).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);
    let cr: InvokeActionResult = resp.extract_payload().unwrap();
    assert_eq!(cr.result, "pane-closed");

    let panes = list_panes(&mut client, "gate-act4").await;
    assert_eq!(panes.panes.len(), 2);

    // ── Close pane_a ──

    let resp = invoke_action(&mut client, "close-pane", Some(&pane_a_name)).await;
    assert_eq!(resp.msg_type, InvokeActionResult::TYPE_NAME);

    let panes = list_panes(&mut client, "gate-act4").await;
    assert_eq!(panes.panes.len(), 1, "closing both splits should leave original pane");
    assert_eq!(panes.panes[0].name, "root");

    let sessions = list_sessions(&mut client, "gate-act4").await;
    assert_eq!(sessions.sessions.len(), 1, "only original session should remain");

    host.shutdown(client).await;
}

/// Unknown action returns InvalidAction error, confirming error path.
#[tokio::test]
async fn gate_unknown_action_returns_error() {
    let yaml = r#"
version: 1
name: gate-act5
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
"#;
    let (host, mut client) = TestHost::start("gate-act5", yaml).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let resp = invoke_action(&mut client, "nonexistent-action", None).await;
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::InvalidAction);

    host.shutdown(client).await;
}
