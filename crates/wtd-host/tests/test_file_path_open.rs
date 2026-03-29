//! Integration test: OpenWorkspace with explicit file path (§8.1, §10.3).
//!
//! Verifies that providing a YAML file path via the `file` field in
//! OpenWorkspace reads the file, parses it, creates a WorkspaceInstance
//! with live ConPTY sessions, and registers it in the host's workspace
//! registry. Also tests error cases (file not found, parse failure) and
//! SaveWorkspace file writing.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer};
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use tokio::net::windows::named_pipe::ClientOptions;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(20000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-file-open-{}-{}", std::process::id(), n)
}

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

/// Helper: start a server and return (server, shutdown_tx).
async fn start_server(
    pipe_name: &str,
) -> (Arc<IpcServer>, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.to_string(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let task = tokio::spawn(async move {
        let _ = s.run(shutdown_rx).await;
    });

    (server, shutdown_tx, task)
}

/// Helper: tear down server and client.
async fn teardown(
    shutdown_tx: watch::Sender<bool>,
    client: tokio::net::windows::named_pipe::NamedPipeClient,
    task: tokio::task::JoinHandle<()>,
) {
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

// ── Test: OpenWorkspace with explicit file path ─────────────────────────

/// Open a workspace by providing an explicit YAML file path.
/// Verify the workspace is created and sessions are running with live
/// ConPTY output.
#[tokio::test]
async fn open_workspace_with_file_path() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-file-open-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml_path = tmp_dir.join("my-workspace.yaml");
    let yaml = r#"
version: 1
name: file-open-test
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo FILE_PATH_READY"
"#;
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace with explicit file path
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "file-open-test".to_string(),
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
        open_resp.payload,
    );

    let result: OpenWorkspaceResult = open_resp.extract_payload().unwrap();
    assert!(!result.instance_id.is_empty());

    // Verify the session is running by polling for startup output.
    let text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("FILE_PATH_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.contains("FILE_PATH_READY"),
        "startup output should contain FILE_PATH_READY, got:\n{}",
        text,
    );

    // Verify workspace is listed.
    write_frame(
        &mut client,
        &Envelope::new("list-1", &ListInstances {}),
    )
    .await
    .unwrap();

    let list_resp = read_frame(&mut client).await.unwrap();
    let list: ListInstancesResult = list_resp.extract_payload().unwrap();
    assert_eq!(list.instances.len(), 1);
    assert_eq!(list.instances[0].name, "file-open-test");

    // Verify pane is listed with correct state.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-1",
            &ListPanes {
                workspace: "file-open-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp = read_frame(&mut client).await.unwrap();
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 1);
    assert_eq!(panes.panes[0].name, "shell");

    // Send a unique marker and verify round-trip.
    let marker = "FILE_OPEN_MARKER_Q9Z2";
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
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    let final_text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.matches(marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        final_text.matches(marker).count() >= 2,
        "marker should appear at least twice, got:\n{}",
        final_text,
    );

    // Close workspace.
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "file-open-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();

    let close_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    teardown(shutdown_tx, client, task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ── Test: OpenWorkspace with nonexistent file path ──────────────────────

/// OpenWorkspace with a file path that doesn't exist should return a
/// WorkspaceNotFound error.
#[tokio::test]
async fn open_workspace_file_not_found() {
    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "open-bad",
            &OpenWorkspace {
                name: "ghost-workspace".to_string(),
                file: Some(r"C:\nonexistent\path\ghost.yaml".to_string()),
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
        resp.payload,
    );
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);
    assert!(
        err.message.contains("not found"),
        "error message should mention 'not found': {}",
        err.message,
    );

    teardown(shutdown_tx, client, task).await;
}

// ── Test: OpenWorkspace with invalid YAML ───────────────────────────────

/// OpenWorkspace with a file containing invalid YAML should return a
/// DefinitionError.
#[tokio::test]
async fn open_workspace_invalid_yaml() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-bad-yaml-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml_path = tmp_dir.join("bad.yaml");
    std::fs::write(&yaml_path, "this is not valid yaml: [[[").unwrap();

    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "open-bad-yaml",
            &OpenWorkspace {
                name: "bad-yaml-test".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
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
        resp.payload,
    );
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::DefinitionError);
    assert!(
        err.message.contains("parse"),
        "error message should mention parsing: {}",
        err.message,
    );

    teardown(shutdown_tx, client, task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ── Test: OpenWorkspace with missing version field ──────────────────────

/// OpenWorkspace with a YAML file that fails validation (unsupported
/// version) should return a DefinitionError.
#[tokio::test]
async fn open_workspace_validation_failure() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-bad-def-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml_path = tmp_dir.join("bad-version.yaml");
    // Valid YAML but invalid workspace definition (unsupported version).
    let yaml = r#"
version: 999
name: bad-version
tabs:
  - name: main
    layout:
      type: pane
      name: shell
"#;
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "open-incomplete",
            &OpenWorkspace {
                name: "bad-version".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
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
        resp.payload,
    );
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::DefinitionError);

    teardown(shutdown_tx, client, task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ── Test: SaveWorkspace writes file ─────────────────────────────────────

/// Open a workspace via file path, then save it to a different file and
/// verify the output file exists with valid YAML content.
#[tokio::test]
async fn save_workspace_writes_file() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-save-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml_path = tmp_dir.join("save-test.yaml");
    let yaml = r#"
version: 1
name: save-test
tabs:
  - name: main
    layout:
      type: pane
      name: editor
      session:
        profile: cmd
"#;
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "save-test".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Save workspace to a new file
    let save_path = tmp_dir.join("saved-output.yaml");
    write_frame(
        &mut client,
        &Envelope::new(
            "save-1",
            &SaveWorkspace {
                workspace: "save-test".to_string(),
                file: Some(save_path.to_string_lossy().to_string()),
            },
        ),
    )
    .await
    .unwrap();

    let save_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        save_resp.msg_type,
        OkResponse::TYPE_NAME,
        "expected Ok, got: {} — {:?}",
        save_resp.msg_type,
        save_resp.payload,
    );

    // Verify the file was written and contains valid YAML.
    assert!(save_path.exists(), "saved file should exist");
    let content = std::fs::read_to_string(&save_path).unwrap();
    assert!(
        content.contains("save-test"),
        "saved YAML should contain workspace name, got:\n{}",
        content,
    );
    assert!(
        content.contains("editor"),
        "saved YAML should contain pane name 'editor', got:\n{}",
        content,
    );

    // Close workspace
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "save-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();

    let close_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    teardown(shutdown_tx, client, task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ── Test: Split workspace with file path ────────────────────────────────

/// Open a multi-pane split workspace via explicit file path and verify
/// both sessions are running.
#[tokio::test]
async fn open_split_workspace_with_file_path() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-split-file-{}-{}",
        std::process::id(),
        PIPE_COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml_path = tmp_dir.join("split.yaml");
    let yaml = r#"
version: 1
name: split-file-test
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
            startupCommand: "echo LEFT_PANE_OK"
        - type: pane
          name: right
          session:
            profile: cmd
            startupCommand: "echo RIGHT_PANE_OK"
"#;
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let (_server, shutdown_tx, task) = start_server(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "split-file-test".to_string(),
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
        open_resp.payload,
    );

    // Verify both panes have running sessions.
    let left_text = poll_capture_until(
        &mut client,
        "left",
        |t| t.contains("LEFT_PANE_OK"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        left_text.contains("LEFT_PANE_OK"),
        "left pane should contain LEFT_PANE_OK, got:\n{}",
        left_text,
    );

    let right_text = poll_capture_until(
        &mut client,
        "right",
        |t| t.contains("RIGHT_PANE_OK"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        right_text.contains("RIGHT_PANE_OK"),
        "right pane should contain RIGHT_PANE_OK, got:\n{}",
        right_text,
    );

    // Verify pane listing shows both panes.
    write_frame(
        &mut client,
        &Envelope::new(
            "lp-1",
            &ListPanes {
                workspace: "split-file-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let panes_resp = read_frame(&mut client).await.unwrap();
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 2);

    let pane_names: Vec<&str> = panes.panes.iter().map(|p| p.name.as_str()).collect();
    assert!(pane_names.contains(&"left"));
    assert!(pane_names.contains(&"right"));

    // Close workspace.
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "split-file-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();

    let close_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    teardown(shutdown_tx, client, task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
