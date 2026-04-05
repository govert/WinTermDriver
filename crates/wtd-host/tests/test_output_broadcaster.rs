//! Integration test: UI client receives live session output via IPC push.
//!
//! Verifies the output broadcaster pipeline (§13.9, §13.13):
//! 1. Host opens a workspace with a ConPTY session
//! 2. UI client connects and completes handshake
//! 3. Input is sent to the session
//! 4. UI client receives SessionOutput push messages containing the echoed output

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

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(5000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-bcast-test-{}-{}", std::process::id(), n)
}

/// Create a temp YAML workspace file and return its absolute path.
fn create_temp_workspace(
    name: &str,
    startup_cmd: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-bcast-{}-{}-{}",
        name,
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let yaml = format!(
        r#"version: 1
name: {name}
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "{startup_cmd}"
"#,
        name = name,
        startup_cmd = startup_cmd
    );
    let yaml_path = tmp_dir.join(format!("{}.yaml", name));
    std::fs::write(&yaml_path, yaml).unwrap();
    (tmp_dir, yaml_path)
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

async fn do_handshake_typed(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    client_type: ClientType,
) {
    write_frame(
        client,
        &Envelope::new(
            "hs-1",
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

// ── Base64 decode (test helper) ──────────────────────────────────────

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

// ── Test harness ─────────────────────────────────────────────────────

/// Start a host with broadcaster and return (server, broadcaster, shutdown_tx).
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

    // Give server time to start listening.
    tokio::time::sleep(Duration::from_millis(100)).await;

    (server_task, broadcaster, shutdown_tx)
}

// ── Tests ────────────────────────────────────────────────────────────

/// UI client receives SessionOutput push messages containing echoed input.
#[tokio::test(flavor = "multi_thread")]
async fn ui_client_receives_session_output() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    let (tmp_dir, yaml_path) = create_temp_workspace("bcast-test", "echo BCAST_READY");

    // Connect CLI client to open workspace via explicit file path.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake_typed(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "bcast-test".to_string(),
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
        "expected OpenWorkspaceResult, got: {} — {:?}",
        open_resp.msg_type,
        open_resp.payload
    );

    // Connect UI client (will receive push messages).
    let mut ui = connect_client(&pipe_name).await;
    do_handshake_typed(&mut ui, ClientType::Ui).await;

    // Send a unique command via CLI.
    let marker = format!("BCAST_MARKER_{}", std::process::id());
    write_frame(
        &mut cli,
        &Envelope::new(
            "send-1",
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
    let mut found_output = false;
    let mut all_data = Vec::new();
    let mut session_output_count = 0u32;

    let (mut ui_read, mut _ui_write) = tokio::io::split(ui);

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ui_read)).await {
            Ok(Ok(envelope)) => {
                if envelope.msg_type == "SessionOutput" {
                    session_output_count += 1;
                    if let Ok(output) = envelope.extract_payload::<SessionOutput>() {
                        let bytes = decode_base64(&output.data);
                        all_data.extend_from_slice(&bytes);
                        let text = String::from_utf8_lossy(&all_data);
                        if text.contains(&marker) {
                            found_output = true;
                            break;
                        }
                    }
                }
                // Also accept other push message types (TitleChanged, etc.)
            }
            Ok(Err(_)) => break,
            Err(_) => continue, // timeout, try again
        }
    }

    assert!(
        found_output,
        "UI client should receive SessionOutput containing the marker '{}'. \
         Got {} SessionOutput messages, accumulated text: {}",
        marker,
        session_output_count,
        String::from_utf8_lossy(&all_data)
    );
    assert!(
        session_output_count > 0,
        "should have received at least one SessionOutput"
    );

    // Tear down.
    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// SessionOutput data field is valid base64-encoded bytes.
#[tokio::test(flavor = "multi_thread")]
async fn session_output_is_valid_base64() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));

    let (server_task, broadcaster, shutdown_tx) =
        start_host_with_broadcaster(&pipe_name, handler).await;

    let (tmp_dir, yaml_path) = create_temp_workspace("b64-test", "echo B64_READY");

    // Open workspace via CLI client with explicit file path.
    let mut cli = connect_client(&pipe_name).await;
    do_handshake_typed(&mut cli, ClientType::Cli).await;

    write_frame(
        &mut cli,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: "b64-test".to_string(),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
            },
        ),
    )
    .await
    .unwrap();
    let _ = read_frame(&mut cli).await.unwrap();

    // Connect UI client.
    let mut ui = connect_client(&pipe_name).await;
    do_handshake_typed(&mut ui, ClientType::Ui).await;

    let (mut ui_read, mut _ui_write) = tokio::io::split(ui);

    // Wait for at least one SessionOutput (startup command output).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut got_output = false;

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ui_read)).await {
            Ok(Ok(envelope)) => {
                if envelope.msg_type == "SessionOutput" {
                    let output: SessionOutput = envelope.extract_payload().unwrap();
                    // Verify base64 decodes successfully and produces non-empty bytes.
                    let decoded = decode_base64(&output.data);
                    assert!(!decoded.is_empty(), "decoded base64 should be non-empty");
                    assert!(
                        !output.session_id.is_empty(),
                        "session_id should not be empty"
                    );
                    got_output = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    assert!(got_output, "should receive at least one SessionOutput");

    let _ = shutdown_tx.send(true);
    broadcaster.abort();
    server_task.abort();

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
