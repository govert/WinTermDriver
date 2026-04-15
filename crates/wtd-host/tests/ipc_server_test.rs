//! Integration tests for the named pipe IPC server.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::watch;
use wtd_host::ipc_server::*;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

// ── Helpers ────────────────────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-test-{}-{}", std::process::id(), n)
}

/// Minimal handler that responds to `ListWorkspaces` with one entry.
struct TestHandler;

impl RequestHandler for TestHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::ListWorkspaces(_) => Some(Envelope::new(
                &envelope.id,
                &ListWorkspacesResult {
                    workspaces: vec![WorkspaceInfo {
                        name: "test".to_owned(),
                        source: "test.yaml".to_owned(),
                    }],
                },
            )),
            _ => None,
        }
    }
}

async fn connect_client(pipe_name: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
    for _ in 0..200 {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return client,
            Err(e) if e.raw_os_error() == Some(2) => {
                // ERROR_FILE_NOT_FOUND — server not ready yet
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) if e.raw_os_error() == Some(231) => {
                // ERROR_PIPE_BUSY — all instances in use, retry
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("unexpected pipe connect error: {:?}", e),
        }
    }
    panic!("timed out waiting for pipe server");
}

fn handshake_envelope(id: &str, client_type: ClientType) -> Envelope {
    Envelope::new(
        id,
        &Handshake {
            client_type,
            client_version: "1.0.0".to_owned(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
}

// ── Tests ──────────────────────────────────────────────────────────────

/// Connect, handshake, send ListWorkspaces, receive response.
#[tokio::test]
async fn connect_handshake_and_list_workspaces() {
    let pipe_name = unique_pipe_name();
    let server = IpcServer::new(pipe_name.clone(), TestHandler).unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_task = tokio::spawn({
        let server = std::sync::Arc::new(server);
        let s = server.clone();
        async move { s.run(shutdown_rx).await }
    });

    let mut client = connect_client(&pipe_name).await;

    // Handshake
    write_frame(&mut client, &handshake_envelope("hs-1", ClientType::Cli))
        .await
        .unwrap();
    let ack = read_frame(&mut client).await.unwrap();
    assert_eq!(ack.msg_type, "HandshakeAck");
    let ack_payload: HandshakeAck = ack.extract_payload().unwrap();
    assert_eq!(ack_payload.protocol_version, PROTOCOL_VERSION);

    // ListWorkspaces
    write_frame(&mut client, &Envelope::new("lw-1", &ListWorkspaces {}))
        .await
        .unwrap();
    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(resp.msg_type, "ListWorkspacesResult");
    assert_eq!(resp.id, "lw-1");
    let result: ListWorkspacesResult = resp.extract_payload().unwrap();
    assert_eq!(result.workspaces.len(), 1);
    assert_eq!(result.workspaces[0].name, "test");

    // Tear down
    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Two clients connected simultaneously.
#[tokio::test]
async fn two_concurrent_clients() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), TestHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // Client 1
    let mut c1 = connect_client(&pipe_name).await;
    write_frame(&mut c1, &handshake_envelope("c1-hs", ClientType::Cli))
        .await
        .unwrap();
    let _ = read_frame(&mut c1).await.unwrap();

    // Client 2
    let mut c2 = connect_client(&pipe_name).await;
    write_frame(&mut c2, &handshake_envelope("c2-hs", ClientType::Cli))
        .await
        .unwrap();
    let _ = read_frame(&mut c2).await.unwrap();

    // Both send ListWorkspaces
    write_frame(&mut c1, &Envelope::new("c1-lw", &ListWorkspaces {}))
        .await
        .unwrap();
    write_frame(&mut c2, &Envelope::new("c2-lw", &ListWorkspaces {}))
        .await
        .unwrap();

    let r1 = read_frame(&mut c1).await.unwrap();
    let r2 = read_frame(&mut c2).await.unwrap();

    assert_eq!(r1.id, "c1-lw");
    assert_eq!(r1.msg_type, "ListWorkspacesResult");
    assert_eq!(r2.id, "c2-lw");
    assert_eq!(r2.msg_type, "ListWorkspacesResult");

    // Registry should show 2 connected clients.
    // Give a moment for registration to propagate.
    tokio::time::sleep(Duration::from_millis(50)).await;
    {
        let reg = server.clients().lock().await;
        assert_eq!(reg.client_count(), 2);
    }

    let _ = shutdown_tx.send(true);
    drop(c1);
    drop(c2);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// SID verification: same user passes (different user rejection is hard
/// to automate without elevated privileges or impersonation).
#[tokio::test]
async fn same_user_sid_accepted() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), TestHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // Same-user client should be accepted and handshake succeeds.
    let mut client = connect_client(&pipe_name).await;
    write_frame(&mut client, &handshake_envelope("sid-hs", ClientType::Cli))
        .await
        .unwrap();
    let ack = read_frame(&mut client).await.unwrap();
    assert_eq!(ack.msg_type, "HandshakeAck");

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Server pushes SessionOutput to UI clients.
#[tokio::test]
async fn streaming_output_to_ui_client() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), TestHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    // Connect UI client
    let mut ui = connect_client(&pipe_name).await;
    write_frame(&mut ui, &handshake_envelope("ui-hs", ClientType::Ui))
        .await
        .unwrap();
    let _ = read_frame(&mut ui).await.unwrap();

    // Also connect a CLI client (should NOT receive the broadcast).
    let mut cli = connect_client(&pipe_name).await;
    write_frame(&mut cli, &handshake_envelope("cli-hs", ClientType::Cli))
        .await
        .unwrap();
    let _ = read_frame(&mut cli).await.unwrap();

    // Brief delay so both registrations are fully propagated.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Broadcast SessionOutput to UI clients.
    server
        .broadcast_to_ui(&Envelope::new(
            "evt-1",
            &SessionOutput {
                workspace: "dev".to_owned(),
                session_id: "s1".to_owned(),
                data: "aGVsbG8=".to_owned(), // base64 "hello"
            },
        ))
        .await
        .unwrap();

    // UI client receives the push.
    let output = read_frame(&mut ui).await.unwrap();
    assert_eq!(output.msg_type, "SessionOutput");
    let payload: SessionOutput = output.extract_payload().unwrap();
    assert_eq!(payload.workspace, "dev");
    assert_eq!(payload.session_id, "s1");
    assert_eq!(payload.data, "aGVsbG8=");

    // CLI client should NOT receive anything — verify with a timeout.
    let cli_read = tokio::time::timeout(Duration::from_millis(200), async {
        read_frame(&mut cli).await
    })
    .await;
    assert!(
        cli_read.is_err(),
        "CLI client should not receive UI broadcast"
    );

    let _ = shutdown_tx.send(true);
    drop(ui);
    drop(cli);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Handshake required before sending requests.
#[tokio::test]
async fn reject_request_before_handshake() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), TestHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;

    // Send ListWorkspaces WITHOUT handshake.
    write_frame(&mut client, &Envelope::new("no-hs", &ListWorkspaces {}))
        .await
        .unwrap();

    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(resp.msg_type, "Error");
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::ProtocolError);

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// Wrong protocol version is rejected.
#[tokio::test]
async fn reject_wrong_protocol_version() {
    let pipe_name = unique_pipe_name();
    let server = std::sync::Arc::new(IpcServer::new(pipe_name.clone(), TestHandler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let s = server.clone();
    let server_task = tokio::spawn(async move { s.run(shutdown_rx).await });

    let mut client = connect_client(&pipe_name).await;

    write_frame(
        &mut client,
        &Envelope::new(
            "bad-v",
            &Handshake {
                client_type: ClientType::Cli,
                client_version: "1.0.0".to_owned(),
                protocol_version: 999,
            },
        ),
    )
    .await
    .unwrap();

    let resp = read_frame(&mut client).await.unwrap();
    assert_eq!(resp.msg_type, "Error");
    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::ProtocolError);
    assert!(err.message.contains("999"));

    let _ = shutdown_tx.send(true);
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}
