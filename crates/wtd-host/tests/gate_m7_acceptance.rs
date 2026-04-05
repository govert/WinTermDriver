//! Gate test for M7: Full application acceptance.
//!
//! Demonstrates the complete WinTermDriver lifecycle end-to-end:
//! start wtd-host with real handler + output broadcaster, open workspace
//! from YAML file, verify sessions are running, send input and capture
//! output via CLI-style IPC, verify UI client receives live session output,
//! invoke actions that modify layout, and close workspace cleanly.
//!
//! This is the final gate proving the application works end-to-end.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer, RequestHandler};
use wtd_host::output_broadcaster;
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use tokio::net::windows::named_pipe::ClientOptions;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(21000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-m7-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("m7-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── Base64 decode (test helper) ─────────────────────────────────────

fn decode_base64(input: &str) -> Vec<u8> {
    const DECODE: [u8; 256] = {
        let mut table = [0xFFu8; 256];
        let mut i = 0u8;
        while i < 26 {
            table[(b'A' + i) as usize] = i;
            table[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        let mut d = 0u8;
        while d < 10 {
            table[(b'0' + d) as usize] = d + 52;
            d += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            buf[i] = DECODE[b as usize];
        }
        let n = chunk.len();
        if n >= 2 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
        }
        if n >= 3 {
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        if n >= 4 {
            out.push((buf[2] << 6) | buf[3]);
        }
    }
    out
}

// ── Connection helpers ──────────────────────────────────────────────

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

async fn do_handshake(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    client_type: ClientType,
) {
    write_frame(
        client,
        &Envelope::new(
            &next_id(),
            &Handshake {
                client_type,
                client_version: "test".to_owned(),
                protocol_version: 1,
            },
        ),
    )
    .await
    .unwrap();
    let ack = read_frame(client).await.unwrap();
    assert_eq!(ack.msg_type, HandshakeAck::TYPE_NAME);
}

// ── Temp directory RAII ─────────────────────────────────────────────

struct TempDir {
    path: std::path::PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── Test host with real handler + broadcaster ───────────────────────

struct TestHost {
    _server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    server_task: tokio::task::JoinHandle<()>,
    broadcaster_task: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    pipe_name: String,
    _tmp_dir: TempDir,
}

impl TestHost {
    async fn start(yaml_path: &std::path::Path, pipe_name: &str) -> Self {
        let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));
        let dyn_handler: Arc<dyn RequestHandler> = handler.clone();
        let server =
            Arc::new(IpcServer::with_arc_handler(pipe_name.to_owned(), dyn_handler).unwrap());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let broadcaster_task = {
            let h = handler;
            let s = server.clone();
            let sr = shutdown_rx.clone();
            tokio::spawn(async move {
                output_broadcaster::run(h, s, sr).await;
            })
        };

        let server_task = {
            let s = server.clone();
            let sr = shutdown_rx;
            tokio::spawn(async move {
                let _ = s.run(sr).await;
            })
        };

        tokio::time::sleep(Duration::from_millis(100)).await;

        let tmp_dir = TempDir {
            path: yaml_path.parent().unwrap().to_path_buf(),
        };

        TestHost {
            _server: server,
            shutdown_tx,
            server_task,
            broadcaster_task,
            pipe_name: pipe_name.to_owned(),
            _tmp_dir: tmp_dir,
        }
    }

    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        self.broadcaster_task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.server_task).await;
    }
}

// ── IPC request helpers ─────────────────────────────────────────────

async fn send_request(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    payload: &impl MessagePayload,
) -> Envelope {
    write_frame(client, &Envelope::new(&next_id(), payload))
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

fn create_workspace_file(label: &str, yaml: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-gate-m7-{}-{}",
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let yaml_path = tmp_dir.join(format!("{}.yaml", label));
    std::fs::write(&yaml_path, yaml).unwrap();
    (tmp_dir, yaml_path)
}

// ── M7 Gate Test ────────────────────────────────────────────────────

/// Full M7 acceptance gate: exercises the complete application lifecycle
/// end-to-end with real handler, real ConPTY sessions, and real IPC.
///
/// Steps verified:
/// 1. Start wtd-host with real HostRequestHandler + output broadcaster
/// 2. Open workspace from YAML file — sessions start in ConPTY
/// 3. Verify sessions are running via ListSessions
/// 4. Send input and capture output via CLI-style IPC
/// 5. Connect UI client and verify it receives live SessionOutput pushes
/// 6. Invoke split-right action — layout grows (2 panes, 2 sessions)
/// 7. Invoke close-pane action — layout shrinks back
/// 8. Close workspace cleanly — no running instances remain
#[tokio::test(flavor = "multi_thread")]
async fn m7_full_application_acceptance() {
    let pipe_name = unique_pipe_name();
    let yaml = r#"version: 1
name: m7-gate
description: "M7 acceptance gate workspace"
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo M7_READY"
"#;
    let (_tmp_dir, yaml_path) = create_workspace_file("m7-gate", yaml);
    let host = TestHost::start(&yaml_path, &pipe_name).await;

    // ── Step 1: Connect CLI client ──────────────────────────────────

    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    // ── Step 2: Open workspace from YAML file ───────────────────────

    let open_resp = send_request(
        &mut cli,
        &OpenWorkspace {
            name: "m7-gate".to_string(),
            file: Some(yaml_path.to_string_lossy().to_string()),
            recreate: false,
        },
    )
    .await;

    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    // Wait for session startup output
    let startup_text = poll_capture_until(
        &mut cli,
        "shell",
        |text| text.contains("M7_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        startup_text.contains("M7_READY"),
        "startup output should contain M7_READY, got:\n{}",
        startup_text,
    );

    // ── Step 3: Verify sessions are running ─────────────────────────

    let sessions_resp = send_request(
        &mut cli,
        &ListSessions {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    assert_eq!(sessions_resp.msg_type, ListSessionsResult::TYPE_NAME);
    let sessions: ListSessionsResult = sessions_resp.extract_payload().unwrap();
    assert_eq!(sessions.sessions.len(), 1, "should have 1 session");
    assert!(
        sessions.sessions[0].state.eq_ignore_ascii_case("running"),
        "session should be running, got: {}",
        sessions.sessions[0].state,
    );

    let panes_resp = send_request(
        &mut cli,
        &ListPanes {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    assert_eq!(panes_resp.msg_type, ListPanesResult::TYPE_NAME);
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 1, "should have 1 pane");
    assert_eq!(panes.panes[0].name, "shell");

    // ── Step 4: Send input and capture output ───────────────────────

    let marker = format!("M7_MARKER_{}", std::process::id());
    let send_resp = send_request(
        &mut cli,
        &wtd_ipc::message::Send {
            target: "shell".to_string(),
            text: format!("echo {}", marker),
            newline: true,
        },
    )
    .await;
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    let capture_text = poll_capture_until(
        &mut cli,
        "shell",
        |text| text.matches(&marker).count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    let count = capture_text.matches(&marker).count();
    assert!(
        count >= 2,
        "marker should appear at least twice (echo + output), found {} in:\n{}",
        count,
        capture_text,
    );

    // ── Step 5: UI client receives live session output ──────────────

    let mut ui = connect_client(&pipe_name).await;
    do_handshake(&mut ui, ClientType::Ui).await;

    // Send a second marker for the UI client to observe
    let ui_marker = format!("M7_UI_PROBE_{}", std::process::id());
    let send_resp2 = send_request(
        &mut cli,
        &wtd_ipc::message::Send {
            target: "shell".to_string(),
            text: format!("echo {}", ui_marker),
            newline: true,
        },
    )
    .await;
    assert_eq!(send_resp2.msg_type, OkResponse::TYPE_NAME);

    // Read push messages from UI client until we see the marker
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut accumulated = Vec::new();
    let mut session_output_count = 0u32;
    let mut found_marker = false;

    let (mut ui_read, _ui_write) = tokio::io::split(ui);

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ui_read)).await {
            Ok(Ok(envelope)) => {
                if envelope.msg_type == "SessionOutput" {
                    session_output_count += 1;
                    if let Ok(output) = envelope.extract_payload::<SessionOutput>() {
                        assert!(
                            !output.session_id.is_empty(),
                            "SessionOutput.session_id must not be empty",
                        );
                        let bytes = decode_base64(&output.data);
                        accumulated.extend_from_slice(&bytes);
                        let text = String::from_utf8_lossy(&accumulated);
                        if text.contains(&ui_marker) {
                            found_marker = true;
                            break;
                        }
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    assert!(
        found_marker,
        "UI client should receive SessionOutput containing marker '{}'. \
         Got {} SessionOutput messages, accumulated: {}",
        ui_marker,
        session_output_count,
        String::from_utf8_lossy(&accumulated),
    );

    // ── Step 6: Invoke action — split-right modifies layout ─────────

    let action_resp = send_request(
        &mut cli,
        &InvokeAction {
            action: "split-right".to_string(),
            target_pane_id: Some("shell".to_string()),
            args: serde_json::json!({}),
        },
    )
    .await;

    assert_eq!(
        action_resp.msg_type,
        InvokeActionResult::TYPE_NAME,
        "expected InvokeActionResult, got: {} — {:?}",
        action_resp.msg_type,
        action_resp.payload,
    );
    let action_result: InvokeActionResult = action_resp.extract_payload().unwrap();
    assert_eq!(action_result.result, "pane-created");
    let new_pane_num = action_result
        .pane_id
        .clone()
        .expect("split-right should return new pane ID");
    let new_pane_name = format!("pane-{}", new_pane_num);

    // Verify layout grew: 2 panes, 2 sessions
    let panes2 = send_request(
        &mut cli,
        &ListPanes {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    let panes2: ListPanesResult = panes2.extract_payload().unwrap();
    assert_eq!(
        panes2.panes.len(),
        2,
        "split-right should create a second pane",
    );

    let sessions2 = send_request(
        &mut cli,
        &ListSessions {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    let sessions2: ListSessionsResult = sessions2.extract_payload().unwrap();
    assert_eq!(
        sessions2.sessions.len(),
        2,
        "split-right should spawn a session for the new pane",
    );

    // ── Step 6b: Close the new pane — layout shrinks back ───────────

    let close_pane_resp = send_request(
        &mut cli,
        &InvokeAction {
            action: "close-pane".to_string(),
            target_pane_id: Some(new_pane_name.clone()),
            args: serde_json::json!({}),
        },
    )
    .await;

    assert_eq!(close_pane_resp.msg_type, InvokeActionResult::TYPE_NAME);
    let close_result: InvokeActionResult = close_pane_resp.extract_payload().unwrap();
    assert_eq!(close_result.result, "pane-closed");

    let panes3 = send_request(
        &mut cli,
        &ListPanes {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    let panes3: ListPanesResult = panes3.extract_payload().unwrap();
    assert_eq!(
        panes3.panes.len(),
        1,
        "close-pane should leave only the original pane",
    );
    assert_eq!(panes3.panes[0].name, "shell");

    let sessions3 = send_request(
        &mut cli,
        &ListSessions {
            workspace: "m7-gate".to_string(),
        },
    )
    .await;
    let sessions3: ListSessionsResult = sessions3.extract_payload().unwrap();
    assert_eq!(
        sessions3.sessions.len(),
        1,
        "only original session should remain",
    );

    // ── Step 7: Inspect pane metadata ───────────────────────────────

    let inspect_resp = send_request(
        &mut cli,
        &Inspect {
            target: "shell".to_string(),
        },
    )
    .await;
    assert_eq!(inspect_resp.msg_type, InspectResult::TYPE_NAME);
    let inspect: InspectResult = inspect_resp.extract_payload().unwrap();
    assert_eq!(inspect.data["paneName"], "shell");
    assert_eq!(inspect.data["workspace"], "m7-gate");

    // ── Step 8: Close workspace cleanly ─────────────────────────────

    let close_resp = send_request(
        &mut cli,
        &CloseWorkspace {
            workspace: "m7-gate".to_string(),
            kill: false,
        },
    )
    .await;
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    // Verify no running instances remain
    let list_resp = send_request(&mut cli, &ListInstances {}).await;
    assert_eq!(list_resp.msg_type, ListInstancesResult::TYPE_NAME);
    let list: ListInstancesResult = list_resp.extract_payload().unwrap();
    assert_eq!(
        list.instances.len(),
        0,
        "all instances should be closed after CloseWorkspace",
    );

    // ── Teardown ────────────────────────────────────────────────────

    drop(_ui_write);
    host.shutdown().await;
}

/// M7 gate: split workspace opens with multiple panes, each with live sessions.
/// Exercises the multi-pane YAML path and concurrent session I/O.
#[tokio::test(flavor = "multi_thread")]
async fn m7_split_workspace_concurrent_sessions() {
    let pipe_name = unique_pipe_name();
    let yaml = r#"version: 1
name: m7-split
tabs:
  - name: work
    layout:
      type: split
      orientation: horizontal
      ratio: 0.5
      children:
        - type: pane
          name: left
          session:
            profile: cmd
            startupCommand: "echo LEFT_READY"
        - type: pane
          name: right
          session:
            profile: cmd
            startupCommand: "echo RIGHT_READY"
"#;
    let (_tmp_dir, yaml_path) = create_workspace_file("m7-split", yaml);
    let host = TestHost::start(&yaml_path, &pipe_name).await;

    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    // Open split workspace
    let open_resp = send_request(
        &mut cli,
        &OpenWorkspace {
            name: "m7-split".to_string(),
            file: Some(yaml_path.to_string_lossy().to_string()),
            recreate: false,
        },
    )
    .await;
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "open failed: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    // Verify both panes are live
    let left_text = poll_capture_until(
        &mut cli,
        "left",
        |text| text.contains("LEFT_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(left_text.contains("LEFT_READY"));

    let right_text = poll_capture_until(
        &mut cli,
        "right",
        |text| text.contains("RIGHT_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(right_text.contains("RIGHT_READY"));

    // Verify 2 panes, 2 sessions
    let panes_resp = send_request(
        &mut cli,
        &ListPanes {
            workspace: "m7-split".to_string(),
        },
    )
    .await;
    let panes: ListPanesResult = panes_resp.extract_payload().unwrap();
    assert_eq!(panes.panes.len(), 2);

    let sessions_resp = send_request(
        &mut cli,
        &ListSessions {
            workspace: "m7-split".to_string(),
        },
    )
    .await;
    let sessions: ListSessionsResult = sessions_resp.extract_payload().unwrap();
    assert_eq!(sessions.sessions.len(), 2);

    // Send unique input to each pane and verify isolation
    let left_marker = format!("LEFT_M7_{}", std::process::id());
    let right_marker = format!("RIGHT_M7_{}", std::process::id());

    send_request(
        &mut cli,
        &wtd_ipc::message::Send {
            target: "left".to_string(),
            text: format!("echo {}", left_marker),
            newline: true,
        },
    )
    .await;

    send_request(
        &mut cli,
        &wtd_ipc::message::Send {
            target: "right".to_string(),
            text: format!("echo {}", right_marker),
            newline: true,
        },
    )
    .await;

    // Verify each pane shows its own marker
    let left_cap = poll_capture_until(
        &mut cli,
        "left",
        |text| text.contains(&left_marker),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        left_cap.contains(&left_marker),
        "left pane should contain left marker",
    );

    let right_cap = poll_capture_until(
        &mut cli,
        "right",
        |text| text.contains(&right_marker),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        right_cap.contains(&right_marker),
        "right pane should contain right marker",
    );

    // Clean close
    let close_resp = send_request(
        &mut cli,
        &CloseWorkspace {
            workspace: "m7-split".to_string(),
            kill: false,
        },
    )
    .await;
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    let list = send_request(&mut cli, &ListInstances {}).await;
    let list: ListInstancesResult = list.extract_payload().unwrap();
    assert_eq!(list.instances.len(), 0);

    host.shutdown().await;
}

/// M7 gate: error paths work correctly through the real handler.
#[tokio::test(flavor = "multi_thread")]
async fn m7_error_paths() {
    let pipe_name = unique_pipe_name();
    let yaml = r#"version: 1
name: m7-err
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
"#;
    let (_tmp_dir, yaml_path) = create_workspace_file("m7-err", yaml);
    let host = TestHost::start(&yaml_path, &pipe_name).await;

    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    // Open workspace so we have something running
    let open_resp = send_request(
        &mut cli,
        &OpenWorkspace {
            name: "m7-err".to_string(),
            file: Some(yaml_path.to_string_lossy().to_string()),
            recreate: false,
        },
    )
    .await;
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send to nonexistent pane returns TargetNotFound
    let resp = send_request(
        &mut cli,
        &wtd_ipc::message::Send {
            target: "nonexistent-pane".to_string(),
            text: "hello".to_string(),
            newline: true,
        },
    )
    .await;
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    // Open nonexistent file returns error
    let resp = send_request(
        &mut cli,
        &OpenWorkspace {
            name: "does-not-exist".to_string(),
            file: Some("/nonexistent/path.yaml".to_string()),
            recreate: false,
        },
    )
    .await;
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    // Unknown action returns InvalidAction
    let resp = send_request(
        &mut cli,
        &InvokeAction {
            action: "bogus-action".to_string(),
            target_pane_id: None,
            args: serde_json::json!({}),
        },
    )
    .await;
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::InvalidAction);

    // Capture nonexistent pane returns TargetNotFound
    let resp = send_request(
        &mut cli,
        &Capture {
            target: "no-such-pane".to_string(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::TargetNotFound);

    // Clean up
    send_request(
        &mut cli,
        &CloseWorkspace {
            workspace: "m7-err".to_string(),
            kill: false,
        },
    )
    .await;

    host.shutdown().await;
}
