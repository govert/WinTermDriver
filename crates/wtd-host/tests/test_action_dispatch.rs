//! Integration test: InvokeAction via IPC dispatches to ActionDispatcher,
//! processes ActionResult, and spawns sessions for new panes (§18.1–18.3).
//!
//! Verifies:
//! - split-right creates a new pane with a running session
//! - close-pane removes a pane and its session
//! - focus-next-pane succeeds without error

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

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(7000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-action-test-{}-{}", std::process::id(), n)
}

async fn connect_client(pipe_name: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
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

async fn do_handshake(client: &mut tokio::net::windows::named_pipe::NamedPipeClient) {
    write_frame(
        client,
        &Envelope::new(
            "hs-1",
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

/// Poll Capture until a predicate on the captured text is satisfied.
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
                "cap",
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

/// Helper: create temp workspace YAML file and return (tmp_dir, yaml_path).
fn create_workspace_file(name: &str, yaml: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-action-test-{}-{}",
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst)
    ));
    let wtd_dir = tmp_dir.join(".wtd");
    std::fs::create_dir_all(&wtd_dir).unwrap();
    let path = wtd_dir.join(format!("{}.yaml", name));
    std::fs::write(&path, yaml).unwrap();
    (tmp_dir, path)
}

/// InvokeAction split-right creates a new pane with a running session.
#[tokio::test]
async fn split_right_creates_pane_with_session() {
    let yaml = r#"
version: 1
name: action-split
tabs:
  - name: main
    layout:
      type: pane
      name: editor
      session:
        profile: cmd
        startupCommand: "echo SPLIT_READY"
"#;
    let (tmp_dir, yaml_path) = create_workspace_file("action-split", yaml);

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace via file path
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "action-split".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
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
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // Wait for initial session to be ready.
    poll_capture_until(
        &mut client,
        "editor",
        |text| text.contains("SPLIT_READY"),
        Duration::from_secs(10),
    )
    .await;

    // Verify initial pane count is 1.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-1",
            &ListPanes {
                workspace: "action-split".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(panes_resp.msg_type, ListPanesResult::TYPE_NAME);
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 1, "should start with 1 pane");
    assert_eq!(panes.panes[0].name, "editor");

    // Send InvokeAction split-right targeting the editor pane.
    write_frame(
        &mut client,
        &Envelope::new(
            "action-1",
            &InvokeAction {
                action: "split-right".to_string(),
                target_pane_id: Some("editor".to_string()),
                args: serde_json::json!({}),
            },
        ),
    )
    .await
    .unwrap();

    let action_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        action_resp.msg_type,
        InvokeActionResult::TYPE_NAME,
        "expected InvokeActionResult, got: {} — {:?}",
        action_resp.msg_type,
        action_resp.payload
    );
    let action_result: InvokeActionResult = action_resp.extract_payload().unwrap();
    assert_eq!(action_result.result, "pane-created");
    assert!(action_result.pane_id.is_some(), "should return new pane ID");

    // Verify pane count is now 2.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-2",
            &ListPanes {
                workspace: "action-split".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp2 = read_frame(&mut client).await.unwrap();
    assert_eq!(panes_resp2.msg_type, ListPanesResult::TYPE_NAME);
    let panes2: ListPanesResult = panes_resp2.extract_payload().unwrap();
    assert_eq!(
        panes2.panes.len(),
        2,
        "split-right should create a second pane"
    );

    // Verify the new pane has a session (listed in ListSessions).
    write_frame(
        &mut client,
        &Envelope::new(
            "ls-1",
            &ListSessions {
                workspace: "action-split".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let sessions_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(sessions_resp.msg_type, ListSessionsResult::TYPE_NAME);
    let sessions: ListSessionsResult = sessions_resp.extract_payload().unwrap();
    assert_eq!(
        sessions.sessions.len(),
        2,
        "should have 2 sessions after split"
    );

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// InvokeAction close-pane removes a pane and its session.
#[tokio::test]
async fn close_pane_removes_pane_and_session() {
    let yaml = r#"
version: 1
name: action-close
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
    let (tmp_dir, yaml_path) = create_workspace_file("action-close", yaml);

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "action-close".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Give sessions a moment to start.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify 2 panes initially.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-1",
            &ListPanes {
                workspace: "action-close".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp = read_frame(&mut client).await.unwrap();
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 2, "should start with 2 panes");

    // Close the right pane.
    write_frame(
        &mut client,
        &Envelope::new(
            "action-1",
            &InvokeAction {
                action: "close-pane".to_string(),
                target_pane_id: Some("right".to_string()),
                args: serde_json::json!({}),
            },
        ),
    )
    .await
    .unwrap();

    let action_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        action_resp.msg_type,
        InvokeActionResult::TYPE_NAME,
        "expected InvokeActionResult, got: {} — {:?}",
        action_resp.msg_type,
        action_resp.payload
    );
    let action_result: InvokeActionResult = action_resp.extract_payload().unwrap();
    assert_eq!(action_result.result, "pane-closed");

    // Verify 1 pane remaining.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-2",
            &ListPanes {
                workspace: "action-close".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp2 = read_frame(&mut client).await.unwrap();
    let panes2: ListPanesResult = panes_resp2.extract_payload().unwrap();
    assert_eq!(panes2.panes.len(), 1, "close-pane should leave 1 pane");
    assert_eq!(panes2.panes[0].name, "left");

    // Verify 1 session remaining.
    write_frame(
        &mut client,
        &Envelope::new(
            "ls-1",
            &ListSessions {
                workspace: "action-close".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let sessions_resp = read_frame(&mut client).await.unwrap();
    let sessions: ListSessionsResult = sessions_resp.extract_payload().unwrap();
    assert_eq!(
        sessions.sessions.len(),
        1,
        "should have 1 session after close"
    );

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// InvokeAction focus-next-pane returns success.
#[tokio::test]
async fn focus_next_pane_returns_ok() {
    let yaml = r#"
version: 1
name: action-focus
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
    let (tmp_dir, yaml_path) = create_workspace_file("action-focus", yaml);

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "action-focus".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Give sessions a moment to start.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send focus-next-pane.
    write_frame(
        &mut client,
        &Envelope::new(
            "action-1",
            &InvokeAction {
                action: "focus-next-pane".to_string(),
                target_pane_id: None,
                args: serde_json::json!({}),
            },
        ),
    )
    .await
    .unwrap();

    let action_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        action_resp.msg_type,
        InvokeActionResult::TYPE_NAME,
        "expected InvokeActionResult, got: {} — {:?}",
        action_resp.msg_type,
        action_resp.payload
    );
    let action_result: InvokeActionResult = action_resp.extract_payload().unwrap();
    assert_eq!(action_result.result, "ok");
    assert!(action_result.pane_id.is_none());

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// InvokeAction for unknown action returns InvalidAction error.
#[tokio::test]
async fn unknown_action_returns_error() {
    let yaml = r#"
version: 1
name: action-err
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
"#;
    let (tmp_dir, yaml_path) = create_workspace_file("action-err", yaml);

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "action-err".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send unknown action.
    write_frame(
        &mut client,
        &Envelope::new(
            "action-1",
            &InvokeAction {
                action: "nonexistent-action".to_string(),
                target_pane_id: None,
                args: serde_json::json!({}),
            },
        ),
    )
    .await
    .unwrap();

    let action_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(action_resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = action_resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::InvalidAction);

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
