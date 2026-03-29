//! Integration tests for all capture_extended workflow patterns.
//!
//! Exercises all capture modes (default, lines=N, all, after anchor, after_regex,
//! fallback, max_lines, count, cursor field) against a real HostRequestHandler
//! with live ConPTY cmd.exe sessions.
//!
//! Completion evidence: `cargo test --test capture_extended_test` passes.

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

// ── Unique pipe and message naming ─────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(31000);
static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-capext-{}-{}", std::process::id(), n)
}

fn next_id() -> String {
    format!("ce-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── IPC connection helpers ──────────────────────────────────────────────────

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

async fn send_request(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    payload: &impl MessagePayload,
) -> Envelope {
    write_frame(client, &Envelope::new(&next_id(), payload))
        .await
        .unwrap();
    read_frame(client).await.unwrap()
}

/// Poll default Capture until a predicate on the captured text is satisfied.
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
                    target: target.to_owned(),
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

// ── TestHost (IPC server with real HostRequestHandler, no broadcaster) ──────

struct TestHost {
    #[allow(dead_code)]
    server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    server_task: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    pipe_name: String,
}

impl TestHost {
    async fn start(pipe_name: &str) -> Self {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        let server = Arc::new(IpcServer::new(pipe_name.to_owned(), handler).unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let server_task = {
            let s = server.clone();
            tokio::spawn(async move {
                let _ = s.run(shutdown_rx).await;
            })
        };

        // Brief pause so the server can bind the pipe before the client connects.
        tokio::time::sleep(Duration::from_millis(50)).await;

        TestHost {
            server,
            shutdown_tx,
            server_task,
            pipe_name: pipe_name.to_owned(),
        }
    }

    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.server_task).await;
    }
}

// ── Workspace YAML helpers ──────────────────────────────────────────────────

/// Create a temp directory with a single-pane cmd.exe workspace YAML.
/// Returns (tmp_dir, yaml_path).
fn create_workspace_yaml(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tmp_dir = std::env::temp_dir().join(format!(
        "wtd-capext-{}-{}-{}",
        std::process::id(),
        PIPE_COUNTER.load(Ordering::SeqCst),
        label,
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let yaml_path = tmp_dir.join(format!("{}.yaml", label));
    let yaml = format!(
        "version: 1\nname: {label}\ntabs:\n  - name: main\n    layout:\n      type: pane\n      name: shell\n      session:\n        profile: cmd\n        startupCommand: \"echo {label}_READY\"\n"
    );
    std::fs::write(&yaml_path, yaml).unwrap();
    (tmp_dir, yaml_path)
}

/// Open a workspace and wait until its startup marker appears.
async fn open_workspace_and_wait(
    client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
    name: &str,
    file_path: &str,
    pane: &str,
    ready_marker: &str,
) {
    let open_resp = send_request(
        client,
        &OpenWorkspace {
            name: name.to_owned(),
            file: Some(file_path.to_owned()),
            recreate: false,
        },
    )
    .await;
    assert_eq!(
        open_resp.msg_type,
        OpenWorkspaceResult::TYPE_NAME,
        "expected OpenWorkspaceResult, got {} — {:?}",
        open_resp.msg_type,
        open_resp.payload,
    );

    let text = poll_capture_until(
        client,
        pane,
        |t| t.contains(ready_marker),
        Duration::from_secs(15),
    )
    .await;
    assert!(
        text.contains(ready_marker),
        "session did not print '{}' within timeout; got:\n{}",
        ready_marker,
        text,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 1 — Default capture: visible screen only
// ═══════════════════════════════════════════════════════════════════════════

/// Default capture (no flags) returns the visible screen only.
///
/// * `lines` == screen height (24 rows)
/// * `total_lines` >= `lines`
/// * `anchor_found` is absent (None) — no anchor was requested
#[tokio::test(flavor = "multi_thread")]
async fn capture_1_default_visible_only() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap1-default");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap1-default",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap1-default_READY",
    )
    .await;

    // Default capture — no flags.
    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    // Visible screen is exactly the screen height (24 rows, no scrollback yet).
    assert_eq!(
        cap.lines, 24,
        "default capture should return screen height (24 rows); got lines={}",
        cap.lines,
    );
    assert!(
        cap.total_lines >= cap.lines,
        "total_lines ({}) must be >= lines ({})",
        cap.total_lines,
        cap.lines,
    );
    // No anchor was requested → anchor_found should be absent.
    assert!(
        cap.anchor_found.is_none(),
        "anchor_found should be None when no anchor was requested; got {:?}",
        cap.anchor_found,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2 — lines=N with scrollback
// ═══════════════════════════════════════════════════════════════════════════

/// After generating 100 lines of output (causing scrollback), `lines=50`
/// returns exactly 50 lines.
#[tokio::test(flavor = "multi_thread")]
async fn capture_2_lines_parameter_with_scrollback() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap2-lines");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap2-lines",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap2-lines_READY",
    )
    .await;

    // Generate 100 output lines to overflow the 24-row screen into scrollback.
    let send_resp = send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "for /l %i in (1,1,100) do echo LINE%i".to_owned(),
            newline: true,
        },
    )
    .await;
    assert_eq!(send_resp.msg_type, OkResponse::TYPE_NAME);

    // Wait until LINE100 appears in the visible screen.
    let text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("LINE100"),
        Duration::from_secs(20),
    )
    .await;
    assert!(
        text.contains("LINE100"),
        "LINE100 did not appear within timeout",
    );

    // Verify there is scrollback (total_lines > 24).
    let all_resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            all: Some(true),
            ..Default::default()
        },
    )
    .await;
    let all_cap: CaptureResult = all_resp.extract_payload().unwrap();
    assert!(
        all_cap.total_lines > 50,
        "expected scrollback; total_lines should be > 50, got {}",
        all_cap.total_lines,
    );

    // Request last 50 lines — must return exactly 50.
    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            lines: Some(50),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert_eq!(
        cap.lines, 50,
        "lines=50 should return exactly 50 lines (total_lines={}); got {}",
        cap.total_lines, cap.lines,
    );
    assert!(
        cap.total_lines >= cap.lines,
        "total_lines ({}) must be >= lines ({})",
        cap.total_lines,
        cap.lines,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 3 — after= exact anchor found
// ═══════════════════════════════════════════════════════════════════════════

/// Capture with `after='===MARKER==='` finds the newest occurrence and
/// starts the capture from that line.
/// `anchor_found=true`, text contains the marker, `cursor` is set.
#[tokio::test(flavor = "multi_thread")]
async fn capture_3_after_anchor_exact_match() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap3-after");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap3-after",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap3-after_READY",
    )
    .await;

    // Plant the marker and a following line.
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo AFTER_MARKER".to_owned(),
            newline: true,
        },
    )
    .await;

    // Wait until both commands have produced output.
    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("AFTER_MARKER"),
        Duration::from_secs(10),
    )
    .await;

    // Anchor capture.
    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===MARKER===".to_owned()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert_eq!(
        cap.anchor_found,
        Some(true),
        "anchor_found should be true for '===MARKER==='",
    );
    assert!(
        cap.text.contains("===MARKER==="),
        "capture text should contain the marker line; got:\n{}",
        cap.text,
    );
    assert!(cap.lines > 0, "lines should be > 0 when anchor found");
    assert!(cap.cursor.is_some(), "cursor should always be set");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4 — after_regex= anchor found
// ═══════════════════════════════════════════════════════════════════════════

/// Capture with `after_regex='===MARK.*==='` finds the same line as exact match.
/// Both exact and regex anchors should yield the same cursor position.
#[tokio::test(flavor = "multi_thread")]
async fn capture_4_after_regex_anchor() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap4-regex");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap4-regex",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap4-regex_READY",
    )
    .await;

    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo AFTER_MARKER".to_owned(),
            newline: true,
        },
    )
    .await;

    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("AFTER_MARKER"),
        Duration::from_secs(10),
    )
    .await;

    // Exact anchor for reference.
    let exact_resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===MARKER===".to_owned()),
            ..Default::default()
        },
    )
    .await;
    let exact_cap: CaptureResult = exact_resp.extract_payload().unwrap();
    assert_eq!(exact_cap.anchor_found, Some(true));

    // Regex anchor — should match the same line.
    let regex_resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after_regex: Some(r"===MARK.*===".to_owned()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(regex_resp.msg_type, CaptureResult::TYPE_NAME);
    let regex_cap: CaptureResult = regex_resp.extract_payload().unwrap();

    assert_eq!(
        regex_cap.anchor_found,
        Some(true),
        "anchor_found should be true for regex '===MARK.*==='",
    );
    assert!(
        regex_cap.text.contains("===MARKER==="),
        "regex capture text should contain marker; got:\n{}",
        regex_cap.text,
    );
    // Both anchors should resolve to the same cursor position.
    assert_eq!(
        exact_cap.cursor, regex_cap.cursor,
        "exact and regex anchors should resolve to the same cursor position",
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5 — after= not found: fallback to lines=N
// ═══════════════════════════════════════════════════════════════════════════

/// When the anchor is not found, `anchor_found=false` and the capture
/// falls back to returning the last `lines=N` lines.
#[tokio::test(flavor = "multi_thread")]
async fn capture_5_after_not_found_falls_back_to_lines() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap5-fallback");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap5-fallback",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap5-fallback_READY",
    )
    .await;

    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("TOTALLY_NONEXISTENT_ANCHOR_XYZ99".to_owned()),
            lines: Some(20),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert_eq!(
        cap.anchor_found,
        Some(false),
        "anchor_found should be false for nonexistent anchor",
    );
    assert_eq!(
        cap.lines, 20,
        "fallback should return last 20 lines (total_lines={}); got {}",
        cap.total_lines, cap.lines,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 6 — after= with max_lines cap
// ═══════════════════════════════════════════════════════════════════════════

/// Anchor is found but `max_lines=5` caps the output to at most 5 lines.
/// `anchor_found` is still `true` even when max_lines trims the window.
#[tokio::test(flavor = "multi_thread")]
async fn capture_6_after_with_max_lines() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap6-maxlines");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap6-maxlines",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap6-maxlines_READY",
    )
    .await;

    // Plant marker, then add 10 more lines after it so max_lines kicks in.
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    for i in 0..10usize {
        send_request(
            &mut client,
            &message::Send {
                target: "shell".to_owned(),
                text: format!("echo AFTER_{}", i),
                newline: true,
            },
        )
        .await;
    }

    // Wait until all AFTER lines have been rendered.
    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("AFTER_9"),
        Duration::from_secs(15),
    )
    .await;

    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===MARKER===".to_owned()),
            max_lines: Some(5),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert_eq!(
        cap.anchor_found,
        Some(true),
        "anchor_found should be true even when max_lines caps the output",
    );
    // With 10 AFTER commands (≥ 20 lines after anchor), max_lines=5 caps to exactly 5.
    assert_eq!(
        cap.lines, 5,
        "max_lines=5 should cap output to exactly 5 lines; got {}",
        cap.lines,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 7 — count mode: metadata only, no text
// ═══════════════════════════════════════════════════════════════════════════

/// `count=true` suppresses the text body but reports line count and anchor status.
#[tokio::test(flavor = "multi_thread")]
async fn capture_7_count_mode() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap7-count");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap7-count",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap7-count_READY",
    )
    .await;

    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo AFTER_MARKER".to_owned(),
            newline: true,
        },
    )
    .await;

    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("AFTER_MARKER"),
        Duration::from_secs(10),
    )
    .await;

    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===MARKER===".to_owned()),
            count: Some(true),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert!(
        cap.text.is_empty(),
        "count mode should return empty text; got {:?}",
        cap.text,
    );
    assert!(
        cap.lines > 0,
        "count mode should report lines > 0 when anchor found; got 0",
    );
    assert_eq!(
        cap.anchor_found,
        Some(true),
        "anchor_found should be true when marker exists",
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 8 — all=true: entire buffer (scrollback + visible)
// ═══════════════════════════════════════════════════════════════════════════

/// `all=true` returns the entire buffer: `lines == total_lines`.
/// With scrollback present, `total_lines > 24`.
#[tokio::test(flavor = "multi_thread")]
async fn capture_8_all_mode_with_scrollback() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap8-all");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap8-all",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap8-all_READY",
    )
    .await;

    // Generate scrollback.
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "for /l %i in (1,1,100) do echo LINE%i".to_owned(),
            newline: true,
        },
    )
    .await;
    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("LINE100"),
        Duration::from_secs(20),
    )
    .await;

    let resp = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            all: Some(true),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.msg_type, CaptureResult::TYPE_NAME);
    let cap: CaptureResult = resp.extract_payload().unwrap();

    assert_eq!(
        cap.lines, cap.total_lines,
        "all=true should return all lines: lines ({}) == total_lines ({})",
        cap.lines, cap.total_lines,
    );
    assert!(
        cap.total_lines > 24,
        "with scrollback, total_lines ({}) should exceed screen height (24)",
        cap.total_lines,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 9 — cursor field consistency
// ═══════════════════════════════════════════════════════════════════════════

/// Anchor capture returns `cursor=C`.  Requesting `lines = total_lines - C`
/// (i.e., from the same absolute position) should yield identical text.
#[tokio::test(flavor = "multi_thread")]
async fn capture_9_cursor_field_consistency() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap9-cursor");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap9-cursor",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap9-cursor_READY",
    )
    .await;

    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===CURSOR_MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo AFTER_CURSOR".to_owned(),
            newline: true,
        },
    )
    .await;

    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("AFTER_CURSOR"),
        Duration::from_secs(10),
    )
    .await;

    // Capture A: anchor-based — note cursor and total_lines.
    let resp_a = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===CURSOR_MARKER===".to_owned()),
            ..Default::default()
        },
    )
    .await;
    let cap_a: CaptureResult = resp_a.extract_payload().unwrap();
    assert_eq!(cap_a.anchor_found, Some(true));

    let cursor = cap_a.cursor.expect("cursor must be set on anchor capture");
    let total = cap_a.total_lines;
    assert!(
        total > cursor,
        "cursor ({}) must be < total_lines ({})",
        cursor, total,
    );

    // Capture B: lines = total - cursor.  This starts from the same absolute
    // position as the anchor, so the captured text must be identical.
    let lines_n = total - cursor;
    let resp_b = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            lines: Some(lines_n),
            ..Default::default()
        },
    )
    .await;
    let cap_b: CaptureResult = resp_b.extract_payload().unwrap();

    assert_eq!(
        cap_a.lines, cap_b.lines,
        "anchor capture and lines={} capture should return same line count",
        lines_n,
    );
    assert_eq!(
        cap_a.text, cap_b.text,
        "anchor capture and lines={} capture should return identical text",
        lines_n,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 10 — JSON output shape
// ═══════════════════════════════════════════════════════════════════════════

/// Verify the CaptureResult wire format:
/// * Field names are camelCase (`totalLines`, `anchorFound`)
/// * snake_case names (`total_lines`, `anchor_found`) must NOT appear
/// * `anchorFound` is absent when no anchor was requested
/// * `anchorFound` is a boolean when anchor was requested
#[tokio::test(flavor = "multi_thread")]
async fn capture_10_json_shape() {
    let pipe_name = unique_pipe_name();
    let (tmp_dir, yaml_path) = create_workspace_yaml("cap10-json");
    let host = TestHost::start(&pipe_name).await;
    let mut client = connect_client(&pipe_name).await;
    do_handshake(&mut client).await;

    open_workspace_and_wait(
        &mut client,
        "cap10-json",
        &yaml_path.to_string_lossy(),
        "shell",
        "cap10-json_READY",
    )
    .await;

    // ── Default capture: no anchor → anchorFound field absent ──────────────
    let resp1 = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        resp1.msg_type, "CaptureResult",
        "response type should be 'CaptureResult'",
    );
    let p = &resp1.payload;

    assert!(
        p.get("text").map(|v| v.is_string()).unwrap_or(false),
        "'text' must be a string field; payload: {:?}",
        p,
    );
    assert!(
        p.get("lines").map(|v| v.is_number()).unwrap_or(false),
        "'lines' must be a number field; payload: {:?}",
        p,
    );
    assert!(
        p.get("totalLines").map(|v| v.is_number()).unwrap_or(false),
        "'totalLines' (camelCase) must be a number field; payload: {:?}",
        p,
    );
    assert!(
        p.get("cursor").map(|v| v.is_number()).unwrap_or(false),
        "'cursor' must be a number field; payload: {:?}",
        p,
    );
    // anchorFound absent when no anchor was requested (skip_serializing_if = "Option::is_none").
    assert!(
        p.get("anchorFound").is_none(),
        "'anchorFound' must be absent when no anchor was requested; got {:?}",
        p.get("anchorFound"),
    );
    // snake_case names must NOT appear on the wire.
    assert!(
        p.get("total_lines").is_none(),
        "snake_case 'total_lines' must not appear on wire; payload: {:?}",
        p,
    );
    assert!(
        p.get("anchor_found").is_none(),
        "snake_case 'anchor_found' must not appear on wire; payload: {:?}",
        p,
    );

    // ── Anchor capture: anchorFound present as boolean ──────────────────────
    send_request(
        &mut client,
        &message::Send {
            target: "shell".to_owned(),
            text: "echo ===JSON_SHAPE_MARKER===".to_owned(),
            newline: true,
        },
    )
    .await;
    poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("===JSON_SHAPE_MARKER==="),
        Duration::from_secs(10),
    )
    .await;

    let resp2 = send_request(
        &mut client,
        &Capture {
            target: "shell".to_owned(),
            after: Some("===JSON_SHAPE_MARKER===".to_owned()),
            ..Default::default()
        },
    )
    .await;
    let p2 = &resp2.payload;

    assert!(
        p2.get("anchorFound").map(|v| v.is_boolean()).unwrap_or(false),
        "'anchorFound' must be a boolean when anchor was requested; payload: {:?}",
        p2,
    );
    assert_eq!(
        p2["anchorFound"].as_bool(),
        Some(true),
        "'anchorFound' should be true; payload: {:?}",
        p2,
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
