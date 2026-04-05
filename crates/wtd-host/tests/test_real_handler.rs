//! Integration test: HostRequestHandler dispatches IPC messages to real
//! workspace instances (§8.1, §13.9–13.13).
//!
//! Verifies the full pipeline: OpenWorkspace + Send + Capture via IPC
//! with the real HostRequestHandler (not the test-only GateHandler).

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer};
use wtd_host::output_broadcaster::BroadcastEvent;
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use serde_json::json;
use tokio::net::windows::named_pipe::ClientOptions;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(9000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-real-handler-{}-{}", std::process::id(), n)
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

/// Full round-trip: IPC → HostRequestHandler → OpenWorkspace + Send + Capture.
///
/// Creates a temporary workspace YAML file in a temp `.wtd/` directory and
/// sets CWD so `find_workspace` discovers it.
#[tokio::test]
async fn real_handler_open_send_capture() {
    // 1. Create a temporary workspace file so find_workspace can discover it.
    let tmp_dir = std::env::temp_dir().join(format!("wtd-test-handler-{}", std::process::id()));
    let wtd_dir = tmp_dir.join(".wtd");
    std::fs::create_dir_all(&wtd_dir).unwrap();

    let yaml = r#"
version: 1
name: handler-test
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo HANDLER_READY"
"#;
    std::fs::write(wtd_dir.join("handler-test.yaml"), yaml).unwrap();

    // Set CWD to the temp dir so find_workspace looks in .wtd/
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&tmp_dir).unwrap();

    // 2. Start IPC server with real HostRequestHandler.
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));
    let server = Arc::new(IpcServer::with_arc_handler(pipe_name.clone(), handler.clone()).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // 3. Open workspace via IPC
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "handler-test".to_string(),
                file: None,
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

    // 4. Wait for startup command output.
    let startup_found = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("HANDLER_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        startup_found.contains("HANDLER_READY"),
        "startup output should contain HANDLER_READY, got:\n{}",
        startup_found
    );

    // 5. Send a unique command via IPC
    let marker = "REAL_HANDLER_TEST_7X2K";
    write_frame(
        &mut client,
        &Envelope::new(
            "send-1",
            &message::Send {
                target: "shell".to_string(),
                text: format!("echo {}", marker),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let send_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        send_resp.msg_type,
        OkResponse::TYPE_NAME,
        "Send should return Ok, got: {} — {:?}",
        send_resp.msg_type,
        send_resp.payload
    );

    // 6. Poll Capture until marker appears at least twice (echo + output).
    let final_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = final_text.matches(marker).count();
    assert!(
        count >= 2,
        "marker should appear at least twice, found {} times in:\n{}",
        count,
        final_text
    );

    // 7. Test ListInstances — workspace should be listed.
    write_frame(&mut client, &Envelope::new("list-1", &ListInstances {}))
        .await
        .unwrap();

    let list_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(list_resp.msg_type, ListInstancesResult::TYPE_NAME);
    let list: ListInstancesResult = list_resp.extract_payload().unwrap();
    assert_eq!(list.instances.len(), 1);
    assert_eq!(list.instances[0].name, "handler-test");

    // 8. Test ListPanes
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-1",
            &ListPanes {
                workspace: "handler-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(panes_resp.msg_type, ListPanesResult::TYPE_NAME);
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 1);
    assert_eq!(panes.panes[0].name, "shell");
    assert_eq!(panes.panes[0].tab, "main");

    // 9. Test Capture with canonical workspace/tab/pane addressing.
    write_frame(
        &mut client,
        &Envelope::new(
            "cap-canonical-1",
            &Capture {
                target: "handler-test/main/shell".to_string(),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();

    let canonical_capture_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(canonical_capture_resp.msg_type, CaptureResult::TYPE_NAME);
    let canonical_capture: CaptureResult = canonical_capture_resp.extract_payload().unwrap();
    assert!(
        canonical_capture.text.contains(marker),
        "canonical capture should resolve the pane path, got:\n{}",
        canonical_capture.text
    );

    // 10. Test Inspect
    write_frame(
        &mut client,
        &Envelope::new(
            "insp-1",
            &Inspect {
                target: "shell".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let inspect_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(inspect_resp.msg_type, InspectResult::TYPE_NAME);
    let inspect: InspectResult = inspect_resp.extract_payload().unwrap();
    assert_eq!(inspect.data["paneName"], "shell");
    assert_eq!(inspect.data["workspace"], "handler-test");
    assert_eq!(inspect.data["cols"], 80);
    assert_eq!(inspect.data["rows"], 24);
    assert_eq!(inspect.data["onAlternate"], false);
    assert_eq!(inspect.data["mouseMode"], "none");
    assert_eq!(inspect.data["cursorShape"], "block");

    write_frame(
        &mut client,
        &Envelope::new(
            "insp-canonical-1",
            &Inspect {
                target: "handler-test/main/shell".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let inspect_canonical_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(inspect_canonical_resp.msg_type, InspectResult::TYPE_NAME);
    let inspect_canonical: InspectResult = inspect_canonical_resp.extract_payload().unwrap();
    assert_eq!(inspect_canonical.data["paneName"], "shell");
    assert_eq!(inspect_canonical.data["workspace"], "handler-test");

    // 11. Test CloseWorkspace
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "handler-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();

    let close_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    let mut prev_titles = HashMap::new();
    let mut prev_progress = HashMap::new();
    let close_events = handler.drain_session_events(&mut prev_titles, &mut prev_progress);
    assert!(
        close_events.iter().any(|event| {
            matches!(
                event,
                BroadcastEvent::WorkspaceState { workspace, new_state }
                    if workspace == "handler-test" && new_state == "closing"
            )
        }),
        "closing a workspace should emit a WorkspaceStateChanged broadcast event"
    );

    // Verify workspace is gone
    write_frame(&mut client, &Envelope::new("list-2", &ListInstances {}))
        .await
        .unwrap();

    let list_resp2 = read_frame(&mut client).await.unwrap();
    let list2: ListInstancesResult = list_resp2.extract_payload().unwrap();
    assert_eq!(list2.instances.len(), 0);

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;

    // Restore CWD and clean up
    std::env::set_current_dir(&original_cwd).unwrap();
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test]
async fn real_handler_keys_and_raw_input() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-test-handler-keys-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let wtd_dir = tmp_dir.join(".wtd");
    std::fs::create_dir_all(&wtd_dir).unwrap();

    let yaml = r#"
version: 1
name: handler-keys
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo KEYS_READY"
"#;
    std::fs::write(wtd_dir.join("handler-keys.yaml"), yaml).unwrap();

    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&tmp_dir).unwrap();

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "open-keys",
            &OpenWorkspace {
                name: "handler-keys".to_string(),
                file: None,
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();
    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    let startup_found = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("KEYS_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(startup_found.contains("KEYS_READY"));

    write_frame(
        &mut client,
        &Envelope::new(
            "stage-send",
            &message::Send {
                target: "shell".to_string(),
                text: "echo KEYS_EXECUTED".to_string(),
                newline: false,
            },
        ),
    )
    .await
    .unwrap();
    let send_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    write_frame(
        &mut client,
        &Envelope::new(
            "keys-enter",
            &message::Keys {
                target: "shell".to_string(),
                keys: vec!["Enter".to_string()],
            },
        ),
    )
    .await
    .unwrap();
    let keys_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(keys_resp.msg_type, OkResponse::TYPE_NAME);

    let keyed_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("KEYS_EXECUTED"),
        Duration::from_secs(10),
    )
    .await;
    assert!(keyed_text.contains("KEYS_EXECUTED"));

    write_frame(
        &mut client,
        &Envelope::new(
            "pane-input",
            &PaneInput {
                target: "shell".to_string(),
                data: "ZWNobyBSQVdfSU5QVVRfT0sNCg==".to_string(),
            },
        ),
    )
    .await
    .unwrap();
    let input_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(input_resp.msg_type, OkResponse::TYPE_NAME);

    let raw_text = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("RAW_INPUT_OK"),
        Duration::from_secs(10),
    )
    .await;
    assert!(raw_text.contains("RAW_INPUT_OK"));

    let _ = shutdown_tx.send(true);
    server_task.await.unwrap().unwrap();
    std::env::set_current_dir(original_cwd).unwrap();
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Verify error handling for non-existent workspace.
#[tokio::test]
async fn real_handler_open_nonexistent_workspace() {
    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Try to open a workspace that doesn't exist
    write_frame(
        &mut client,
        &Envelope::new(
            "open-bad",
            &OpenWorkspace {
                name: "nonexistent-workspace-abc123".to_string(),
                file: None,
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        resp.msg_type,
        ErrorResponse::TYPE_NAME,
        "expected Error, got: {} — {:?}",
        resp.msg_type,
        resp.payload
    );
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Verify Send to non-existent pane returns TargetNotFound.
#[tokio::test]
async fn real_handler_send_to_nonexistent_pane() {
    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "send-bad",
            &message::Send {
                target: "no-such-pane".to_string(),
                text: "hello".to_string(),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Regression: workspace discovery should use the client cwd from the request,
/// not the host process cwd.
#[tokio::test]
async fn real_handler_uses_request_cwd_for_open_and_list() {
    let repo_cwd = std::env::current_dir().unwrap();
    let tmp_dir = std::env::temp_dir().join(format!("wtd-test-handler-cwd-{}", std::process::id()));
    let wtd_dir = tmp_dir.join(".wtd");
    let other_dir = tmp_dir.join("host-cwd");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&wtd_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();

    let yaml = r#"
version: 1
name: handler-cwd-test
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo CWD_READY"
"#;
    std::fs::write(wtd_dir.join("handler-cwd-test.yaml"), yaml).unwrap();

    // Deliberately move the host process away from the directory that contains .wtd/.
    std::env::set_current_dir(&other_dir).unwrap();

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope {
            id: "open-cwd".to_string(),
            msg_type: OpenWorkspace::TYPE_NAME.to_string(),
            payload: json!({
                "name": "handler-cwd-test",
                "recreate": false,
                "cwd": tmp_dir.to_string_lossy().to_string(),
            }),
        },
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    let startup_found = poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("CWD_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(startup_found.contains("CWD_READY"));

    write_frame(
        &mut client,
        &Envelope {
            id: "list-cwd".to_string(),
            msg_type: ListWorkspaces::TYPE_NAME.to_string(),
            payload: json!({
                "cwd": tmp_dir.to_string_lossy().to_string(),
            }),
        },
    )
    .await
    .unwrap();

    let list_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(list_resp.msg_type, ListWorkspacesResult::TYPE_NAME);
    let list: ListWorkspacesResult = list_resp.extract_payload().unwrap();
    assert!(
        list.workspaces
            .iter()
            .any(|w| w.name == "handler-cwd-test" && w.source == "local"),
        "workspace discovered from request cwd should be listed, got: {:?}",
        list.workspaces
    );

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;

    std::env::set_current_dir(&repo_cwd).unwrap();
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
