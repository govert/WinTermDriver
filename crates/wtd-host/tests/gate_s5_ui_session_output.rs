//! Gate test for S5: UI client receives live session output.
//!
//! Verifies that a UI client connected to the host receives real-time
//! `SessionOutput` broadcasts when a session produces output, and
//! `SessionStateChanged` notifications when a session exits.
//!
//! This is the push-channel verification gate for Slice 5.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer, RequestHandler};
use wtd_host::output_broadcaster;
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use tokio::net::windows::named_pipe::ClientOptions;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(16000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-s5ui-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("s5ui-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
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

// ── Test harness ────────────────────────────────────────────────────

struct TempDir {
    path: std::path::PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn create_temp_workspace(label: &str, startup_cmd: &str) -> (TempDir, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-gate-s5ui-{}-{}-{}",
        label,
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst),
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml = format!(
        r#"version: 1
name: {label}
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "{startup_cmd}"
"#,
        label = label,
        startup_cmd = startup_cmd,
    );
    let yaml_path = tmp_dir.join(format!("{}.yaml", label));
    std::fs::write(&yaml_path, &yaml).unwrap();
    (TempDir { path: tmp_dir }, yaml_path)
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

/// Start host with real handler + output broadcaster.
async fn start_host_with_broadcaster(
    pipe_name: &str,
    handler: Arc<HostRequestHandler>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    watch::Sender<bool>,
) {
    let dyn_handler: Arc<dyn RequestHandler> = handler.clone();
    let server = Arc::new(IpcServer::with_arc_handler(pipe_name.to_owned(), dyn_handler).unwrap());

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let broadcaster = {
        let h = handler;
        let s = server.clone();
        let sr = shutdown_rx.clone();
        tokio::spawn(async move {
            output_broadcaster::run(h, s, sr).await;
        })
    };

    let server_task = {
        let s = server;
        let sr = shutdown_rx;
        tokio::spawn(async move {
            let _ = s.run(sr).await;
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    (server_task, broadcaster, shutdown_tx)
}

// ── Tests ───────────────────────────────────────────────────────────

/// UI client receives SessionOutput push messages containing live session output
/// when input is sent to a session. Verifies real-time push delivery without polling.
#[tokio::test(flavor = "multi_thread")]
async fn ui_client_receives_live_session_output() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    let (_tmp_dir, yaml_path) = create_temp_workspace("s5ui-out", "echo S5UI_READY");

    // CLI client: open workspace.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "s5ui-out".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "open failed: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    // UI client: connect and start receiving push messages.
    let mut ui = connect_client(&pipe_name).await;
    do_handshake(&mut ui, ClientType::Ui).await;

    // Send a unique marker via CLI.
    let marker = format!("S5UI_MARKER_{}", std::process::id());
    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &wtd_ipc::message::Send {
                target: "shell".to_string(),
                text: format!("echo {}", marker),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let send_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    // Read push messages from UI client until we see the marker.
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
                        assert!(
                            !bytes.is_empty(),
                            "decoded SessionOutput data must not be empty",
                        );
                        accumulated.extend_from_slice(&bytes);
                        let text = String::from_utf8_lossy(&accumulated);
                        if text.contains(&marker) {
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
         Got {} SessionOutput messages, accumulated text: {}",
        marker,
        session_output_count,
        String::from_utf8_lossy(&accumulated),
    );
    assert!(
        session_output_count > 0,
        "should have received at least one SessionOutput",
    );

    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();
}

/// UI client receives SessionStateChanged notification when a session exits.
#[tokio::test(flavor = "multi_thread")]
async fn ui_client_receives_session_state_changed_on_exit() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    // Use "exit" as the startup command so cmd.exe exits quickly.
    let (_tmp_dir, yaml_path) = create_temp_workspace("s5ui-exit", "echo EXIT_READY");

    // CLI: open workspace.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "s5ui-exit".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "open failed: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    // UI client: connect.
    let mut ui = connect_client(&pipe_name).await;
    do_handshake(&mut ui, ClientType::Ui).await;

    // Send "exit" to terminate the cmd.exe session.
    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &wtd_ipc::message::Send {
                target: "shell".to_string(),
                text: "exit".to_string(),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let send_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    // Wait for SessionStateChanged push on the UI client.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut found_state_changed = false;
    let mut received_state = String::new();
    let mut received_exit_code: Option<i32> = None;

    let (mut ui_read, _ui_write) = tokio::io::split(ui);

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ui_read)).await {
            Ok(Ok(envelope)) => {
                if envelope.msg_type == "SessionStateChanged" {
                    if let Ok(changed) = envelope.extract_payload::<SessionStateChanged>() {
                        assert!(
                            !changed.session_id.is_empty(),
                            "SessionStateChanged.session_id must not be empty",
                        );
                        received_state = changed.new_state.clone();
                        received_exit_code = changed.exit_code;
                        found_state_changed = true;
                        break;
                    }
                }
                // Ignore SessionOutput and other push messages.
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    assert!(
        found_state_changed,
        "UI client should receive SessionStateChanged when session exits",
    );
    assert_eq!(
        received_state, "exited",
        "new_state should be 'exited', got: {}",
        received_state,
    );
    assert!(
        received_exit_code.is_some(),
        "exit_code should be present for exited session",
    );
    assert_eq!(
        received_exit_code.unwrap(),
        0,
        "cmd.exe 'exit' should return code 0, got: {}",
        received_exit_code.unwrap(),
    );

    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();
}

/// SessionOutput push messages arrive within expected latency after input is sent.
#[tokio::test(flavor = "multi_thread")]
async fn session_output_arrives_within_expected_latency() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    let (_tmp_dir, yaml_path) = create_temp_workspace("s5ui-lat", "echo LATENCY_READY");

    // CLI: open workspace.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "s5ui-lat".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // UI client: connect.
    let mut ui = connect_client(&pipe_name).await;
    do_handshake(&mut ui, ClientType::Ui).await;

    // Drain any initial output from startup command before measuring latency.
    let (mut ui_read, _ui_write) = tokio::io::split(ui);
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() > drain_deadline {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(500), read_frame(&mut ui_read)).await {
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }

    // Send a unique marker and measure time until UI receives it.
    let marker = format!("LATENCY_PROBE_{}", std::process::id());
    let send_time = Instant::now();

    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &wtd_ipc::message::Send {
                target: "shell".to_string(),
                text: format!("echo {}", marker),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let _ = read_frame(&mut cli).await.unwrap(); // OkResponse

    // Wait for the marker in SessionOutput.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut accumulated = Vec::new();
    let mut receive_time: Option<Instant> = None;

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ui_read)).await {
            Ok(Ok(envelope)) => {
                if envelope.msg_type == "SessionOutput" {
                    if let Ok(output) = envelope.extract_payload::<SessionOutput>() {
                        let bytes = decode_base64(&output.data);
                        accumulated.extend_from_slice(&bytes);
                        let text = String::from_utf8_lossy(&accumulated);
                        if text.contains(&marker) {
                            receive_time = Some(Instant::now());
                            break;
                        }
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    let receive_time = receive_time.unwrap_or_else(|| {
        panic!(
            "UI client did not receive marker '{}' in time. Accumulated: {}",
            marker,
            String::from_utf8_lossy(&accumulated),
        )
    });

    let latency = receive_time.duration_since(send_time);
    // Broadcaster polls every 50ms; with ConPTY echo and pipe I/O,
    // total latency should be well under 2 seconds. Use a generous
    // threshold for CI environments.
    let max_latency = Duration::from_secs(2);
    assert!(
        latency < max_latency,
        "SessionOutput push latency {:?} exceeds max {:?}",
        latency,
        max_latency,
    );

    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();
}

/// Multiple UI clients each receive the same SessionOutput broadcasts.
#[tokio::test(flavor = "multi_thread")]
async fn multiple_ui_clients_receive_broadcasts() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    let (_tmp_dir, yaml_path) = create_temp_workspace("s5ui-multi", "echo MULTI_READY");

    // CLI: open workspace.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "s5ui-multi".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();

    let open_resp = read_frame(&mut cli).await.unwrap();
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // Connect two UI clients.
    let mut ui1 = connect_client(&pipe_name).await;
    do_handshake(&mut ui1, ClientType::Ui).await;

    let mut ui2 = connect_client(&pipe_name).await;
    do_handshake(&mut ui2, ClientType::Ui).await;

    // Send a unique marker.
    let marker = format!("MULTI_MARKER_{}", std::process::id());
    write_frame(
        &mut cli,
        &Envelope::new(
            &next_id(),
            &wtd_ipc::message::Send {
                target: "shell".to_string(),
                text: format!("echo {}", marker),
                newline: true,
            },
        ),
    )
    .await
    .unwrap();

    let _ = read_frame(&mut cli).await.unwrap();

    // Helper: read SessionOutput from a UI client until marker is found.
    async fn wait_for_marker(
        client: tokio::net::windows::named_pipe::NamedPipeClient,
        marker: &str,
    ) -> bool {
        let (mut reader, _writer) = tokio::io::split(client);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut accumulated = Vec::new();

        loop {
            if tokio::time::Instant::now() > deadline {
                return false;
            }
            match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut reader)).await {
                Ok(Ok(envelope)) => {
                    if envelope.msg_type == "SessionOutput" {
                        if let Ok(output) = envelope.extract_payload::<SessionOutput>() {
                            let bytes = decode_base64(&output.data);
                            accumulated.extend_from_slice(&bytes);
                            let text = String::from_utf8_lossy(&accumulated);
                            if text.contains(marker) {
                                return true;
                            }
                        }
                    }
                }
                Ok(Err(_)) => return false,
                Err(_) => continue,
            }
        }
    }

    // Both UI clients should receive the marker concurrently.
    let (found1, found2) =
        tokio::join!(wait_for_marker(ui1, &marker), wait_for_marker(ui2, &marker),);

    assert!(
        found1,
        "UI client 1 should receive SessionOutput with marker"
    );
    assert!(
        found2,
        "UI client 2 should receive SessionOutput with marker"
    );

    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();
}
