//! Integration test: AttachWorkspaceResult contains full workspace state (§13.11).
//!
//! Verifies that when a UI client attaches to a running workspace, the response
//! contains the layout tree, session IDs/states, and pane-session attachments.

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

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(11000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-attach-snap-{}-{}", std::process::id(), n)
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

/// Create a workspace via `file:` path, wait for sessions to start, then attach
/// and verify the state snapshot contains the correct structure.
#[tokio::test]
async fn attach_returns_full_workspace_state() {
    // 1. Write a multi-pane workspace YAML to a temp file.
    let tmp_dir = std::env::temp_dir().join(format!("wtd-test-attach-snap-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml = r#"
version: 1
name: attach-test
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
            startupCommand: "echo ATTACH_EDITOR"
        - type: pane
          name: terminal
          session:
            profile: cmd
            startupCommand: "echo ATTACH_TERM"
"#;
    let yaml_path = tmp_dir.join("attach-test.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    // 2. Start IPC server with real HostRequestHandler.
    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // 3. Open workspace via file: path to avoid CWD races.
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "attach-test".to_string(),
                file: Some(yaml_path.to_string_lossy().into_owned()),
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

    // 4. Wait for both panes to have output (sessions running).
    poll_capture_until(
        &mut client,
        "editor",
        |text| text.contains("ATTACH_EDITOR"),
        Duration::from_secs(10),
    )
    .await;

    poll_capture_until(
        &mut client,
        "terminal",
        |text| text.contains("ATTACH_TERM"),
        Duration::from_secs(10),
    )
    .await;

    // 5. Send AttachWorkspace and verify the state.
    write_frame(
        &mut client,
        &Envelope::new(
            "attach-1",
            &AttachWorkspace {
                workspace: "attach-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let attach_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(
        attach_resp.msg_type,
        AttachWorkspaceResult::TYPE_NAME,
        "expected AttachWorkspaceResult, got: {} — {:?}",
        attach_resp.msg_type,
        attach_resp.payload
    );

    let result: AttachWorkspaceResult = attach_resp.extract_payload().unwrap();
    let state = &result.state;

    // 5a. Verify top-level fields.
    assert_eq!(state["name"], "attach-test");
    assert_eq!(state["state"], "active");
    assert!(state["id"].is_number(), "id should be a number");

    // 5b. Verify tabs array with layout tree.
    let tabs = state["tabs"].as_array().expect("tabs should be an array");
    assert_eq!(tabs.len(), 1, "should have exactly 1 tab");

    let tab = &tabs[0];
    assert_eq!(tab["name"], "dev");
    assert!(tab["id"].is_number(), "tab id should be a number");

    // Verify panes list.
    let panes = tab["panes"].as_array().expect("panes should be an array");
    assert_eq!(panes.len(), 2, "tab should have 2 panes");

    // Verify layout tree structure (split with two pane children).
    let layout = &tab["layout"];
    assert_eq!(layout["type"], "split", "root layout should be a split");
    assert_eq!(layout["orientation"], "vertical");

    let children = layout["children"]
        .as_array()
        .expect("split should have children");
    assert_eq!(children.len(), 2, "split should have 2 children");
    assert_eq!(children[0]["type"], "pane");
    assert_eq!(children[0]["name"], "editor");
    assert_eq!(children[1]["type"], "pane");
    assert_eq!(children[1]["name"], "terminal");

    // 5c. Verify paneStates — both panes should be attached.
    let pane_states = state["paneStates"]
        .as_object()
        .expect("paneStates should be an object");
    assert_eq!(pane_states.len(), 2, "should have 2 pane state entries");
    for (_pane_id, ps) in pane_states {
        assert_eq!(
            ps["type"], "attached",
            "pane state should be attached, got: {:?}",
            ps
        );
        assert!(
            ps["sessionId"].is_number(),
            "attached pane should have sessionId"
        );
    }

    // 5d. Verify sessionStates — both sessions should be running.
    let session_states = state["sessionStates"]
        .as_object()
        .expect("sessionStates should be an object");
    assert_eq!(
        session_states.len(),
        2,
        "should have 2 session state entries"
    );
    for (_sid, ss) in session_states {
        assert_eq!(
            ss["type"], "running",
            "session state should be running, got: {:?}",
            ss
        );
    }

    // 5e. Verify sessionTitles is present (may be empty strings initially).
    let session_titles = state["sessionTitles"]
        .as_object()
        .expect("sessionTitles should be an object");
    assert_eq!(
        session_titles.len(),
        2,
        "should have 2 session title entries"
    );

    // 5f. Verify pane→session mapping is consistent: sessionIds in paneStates
    //     should appear as keys in sessionStates.
    let session_state_keys: Vec<&String> = session_states.keys().collect();
    for (_pane_id, ps) in pane_states {
        if ps["type"] == "attached" {
            let sid = ps["sessionId"].as_u64().unwrap().to_string();
            assert!(
                session_state_keys.contains(&&sid),
                "pane's sessionId {} should exist in sessionStates",
                sid
            );
        }
    }

    // Tear down.
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "attach-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();
    let _ = read_frame(&mut client).await;

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// AttachWorkspace on a non-existent workspace returns WorkspaceNotFound.
#[tokio::test]
async fn attach_nonexistent_workspace_returns_error() {
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
            "attach-bad",
            &AttachWorkspace {
                workspace: "nonexistent-xyz-789".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Verify AttachWorkspaceResult with a single-pane workspace (simplest case).
#[tokio::test]
async fn attach_single_pane_workspace() {
    let tmp_dir =
        std::env::temp_dir().join(format!("wtd-test-attach-single-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml = r#"
version: 1
name: single-test
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo SINGLE_READY"
"#;
    let yaml_path = tmp_dir.join("single-test.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace via file: path.
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "single-test".to_string(),
                file: Some(yaml_path.to_string_lossy().into_owned()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait for session output.
    poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("SINGLE_READY"),
        Duration::from_secs(10),
    )
    .await;

    // Attach.
    write_frame(
        &mut client,
        &Envelope::new(
            "attach-1",
            &AttachWorkspace {
                workspace: "single-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let attach_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(attach_resp.msg_type, AttachWorkspaceResult::TYPE_NAME);

    let result: AttachWorkspaceResult = attach_resp.extract_payload().unwrap();
    let state = &result.state;

    // Single tab, single pane, leaf layout.
    let tabs = state["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["name"], "main");
    assert_eq!(tabs[0]["panes"].as_array().unwrap().len(), 1);

    // Layout is a leaf pane, not a split.
    let layout = &tabs[0]["layout"];
    assert_eq!(layout["type"], "pane");
    assert_eq!(layout["name"], "shell");

    // One pane attached, one session running.
    let pane_states = state["paneStates"].as_object().unwrap();
    assert_eq!(pane_states.len(), 1);
    let (_, ps) = pane_states.iter().next().unwrap();
    assert_eq!(ps["type"], "attached");

    let session_states = state["sessionStates"].as_object().unwrap();
    assert_eq!(session_states.len(), 1);
    let (_, ss) = session_states.iter().next().unwrap();
    assert_eq!(ss["type"], "running");

    // Tear down.
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "single-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();
    let _ = read_frame(&mut client).await;

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ── Session-screen seeding test ────────────────────────────────────────────────

/// Decode a base64 string the same way the UI does (no external dep).
fn b64_decode(input: &str) -> Vec<u8> {
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

/// Verify that `AttachWorkspaceResult.state.sessionScreens` is populated with
/// non-empty base64-encoded VT snapshots containing the expected terminal output.
///
/// This is the regression test for the "blank pane on attach" bug: the UI must
/// receive screen content immediately on attach, without waiting for new output.
#[tokio::test]
async fn attach_includes_session_screen_snapshots() {
    let tmp_dir =
        std::env::temp_dir().join(format!("wtd-test-attach-screens-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml = r#"
version: 1
name: screen-seed-test
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo SCREEN_SEED_MARKER"
"#;
    let yaml_path = tmp_dir.join("screen-seed-test.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let pipe_name = unique_pipe_name();
    let handler = HostRequestHandler::new(GlobalSettings::default());
    let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    // Open workspace via file: path.
    write_frame(
        &mut client,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "screen-seed-test".to_string(),
                file: Some(yaml_path.to_string_lossy().into_owned()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();
    let open_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Wait until the session has produced the expected output.
    poll_capture_until(
        &mut client,
        "shell",
        |text| text.contains("SCREEN_SEED_MARKER"),
        Duration::from_secs(10),
    )
    .await;

    // Attach and get the snapshot.
    write_frame(
        &mut client,
        &Envelope::new(
            "attach-1",
            &AttachWorkspace {
                workspace: "screen-seed-test".to_string(),
            },
        ),
    )
    .await
    .unwrap();
    let attach_resp = read_frame(&mut client).await.unwrap();
    assert_eq!(attach_resp.msg_type, AttachWorkspaceResult::TYPE_NAME);

    let result: AttachWorkspaceResult = attach_resp.extract_payload().unwrap();
    let state = &result.state;

    // Verify sessionScreens is present and non-empty.
    let session_screens = state["sessionScreens"]
        .as_object()
        .expect("sessionScreens should be an object");
    assert_eq!(
        session_screens.len(),
        1,
        "should have 1 session screen entry"
    );

    // Decode the VT snapshot and verify it contains the expected text.
    let (_, b64_val) = session_screens.iter().next().unwrap();
    let b64 = b64_val
        .as_str()
        .expect("screen snapshot should be a string");
    assert!(!b64.is_empty(), "screen snapshot should be non-empty");

    let vt_bytes = b64_decode(b64);
    assert!(!vt_bytes.is_empty(), "decoded VT bytes should be non-empty");

    // Feed the VT snapshot into a fresh ScreenBuffer and verify the marker appears.
    let mut screen = wtd_pty::ScreenBuffer::new(80, 24, 1000);
    screen.advance(&vt_bytes);
    let visible = screen.visible_text();
    assert!(
        visible.contains("SCREEN_SEED_MARKER"),
        "ScreenBuffer after replaying VT snapshot should show SCREEN_SEED_MARKER; got:\n{visible}"
    );

    // Tear down.
    write_frame(
        &mut client,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "screen-seed-test".to_string(),
                kill: false,
            },
        ),
    )
    .await
    .unwrap();
    let _ = read_frame(&mut client).await;

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
