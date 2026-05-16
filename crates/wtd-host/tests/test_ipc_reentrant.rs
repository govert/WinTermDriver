//! Regression coverage for nested/reentrant CLI calls into the same WTD host.
//!
//! The host request handler is synchronous. A long request such as `WaitPane`
//! must not pin the async IPC worker, because agents running inside WTD commonly
//! launch another `wtd` client process to prompt, inspect, or wait on a sibling
//! pane.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::watch;

use wtd_core::GlobalSettings;
use wtd_host::ipc_server::{read_frame, write_frame, IpcServer, RequestHandler};
use wtd_host::request_handler::HostRequestHandler;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(9000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-reentrant-test-{}-{}", std::process::id(), n)
}

fn create_temp_workspace(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-reentrant-{}-{}-{}",
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
        startupCommand: "echo REENTRANT_READY"
"#,
        name = name
    );
    let yaml_path = tmp_dir.join(format!("{name}.yaml"));
    std::fs::write(&yaml_path, yaml).unwrap();
    (tmp_dir, yaml_path)
}

async fn connect_client(pipe_name: &str) -> NamedPipeClient {
    for _ in 0..200 {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return client,
            Err(e) if e.raw_os_error() == Some(2) || e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("unexpected pipe connect error: {e:?}"),
        }
    }
    panic!("timed out waiting for pipe server");
}

async fn do_handshake(client: &mut NamedPipeClient) {
    write_frame(
        client,
        &Envelope::new(
            "hs-1",
            &Handshake {
                client_type: ClientType::Cli,
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

async fn send_request(client: &mut NamedPipeClient, envelope: &Envelope) -> Envelope {
    write_frame(client, envelope).await.unwrap();
    read_frame(client).await.unwrap()
}

async fn start_host(
    pipe_name: &str,
    handler: Arc<HostRequestHandler>,
) -> (tokio::task::JoinHandle<()>, watch::Sender<bool>) {
    let dyn_handler: Arc<dyn RequestHandler> = handler;
    let server = Arc::new(IpcServer::with_arc_handler(pipe_name.to_owned(), dyn_handler).unwrap());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server_task = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.run(shutdown_rx).await;
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    (server_task, shutdown_tx)
}

#[tokio::test(flavor = "current_thread")]
async fn long_wait_request_does_not_block_other_ipc_clients() {
    let pipe_name = unique_pipe_name();
    let handler = Arc::new(HostRequestHandler::new(GlobalSettings::default()));
    let (server_task, shutdown_tx) = start_host(&pipe_name, handler).await;
    let (tmp_dir, yaml_path) = create_temp_workspace("reentrant-wait");

    let mut opener = connect_client(&pipe_name).await;
    do_handshake(&mut opener).await;
    let open_resp = send_request(
        &mut opener,
        &Envelope::new(
            "open-1",
            &OpenWorkspace {
                name: Some("reentrant-wait".to_string()),
                file: Some(yaml_path.to_string_lossy().to_string()),
                recreate: false,
                profile: None,
            },
        ),
    )
    .await;
    assert_eq!(open_resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    let wait_pipe = pipe_name.clone();
    let wait_task = tokio::spawn(async move {
        let mut client = connect_client(&wait_pipe).await;
        do_handshake(&mut client).await;
        send_request(
            &mut client,
            &Envelope::new(
                "wait-1",
                &WaitPane {
                    target: "reentrant-wait/main/shell".to_string(),
                    condition: WaitCondition::Error,
                    timeout_ms: Some(1_500),
                    poll_ms: Some(100),
                    recent_lines: Some(5),
                },
            ),
        )
        .await
    });

    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        started.elapsed() < Duration::from_millis(700),
        "long WaitPane request blocked the async IPC runtime for {:?}",
        started.elapsed()
    );

    let mut probe = connect_client(&pipe_name).await;
    do_handshake(&mut probe).await;
    let list_resp = tokio::time::timeout(
        Duration::from_millis(400),
        send_request(&mut probe, &Envelope::new("list-1", &ListInstances {})),
    )
    .await
    .expect("backend should answer other clients while WaitPane is still in flight");
    assert_eq!(list_resp.msg_type, ListInstancesResult::TYPE_NAME);
    assert!(
        !wait_task.is_finished(),
        "probe only completed after the long wait had already finished"
    );

    let wait_resp = tokio::time::timeout(Duration::from_secs(3), wait_task)
        .await
        .expect("wait task should finish")
        .expect("wait task should not panic");
    assert_eq!(wait_resp.msg_type, WaitPaneResult::TYPE_NAME);

    let close_resp = send_request(
        &mut opener,
        &Envelope::new(
            "close-1",
            &CloseWorkspace {
                workspace: "reentrant-wait".to_string(),
                kill: true,
            },
        ),
    )
    .await;
    assert_eq!(close_resp.msg_type, OkResponse::TYPE_NAME);

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    let _ = std::fs::remove_dir_all(tmp_dir);
}
