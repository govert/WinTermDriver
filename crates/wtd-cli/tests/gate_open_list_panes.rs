//! Gate: Verify `wtd open` finds definition from CWD, creates instance,
//! `wtd list panes` shows all panes with semantic names (§12, §22.3).
//!
//! Unlike e2e_commands.rs which hardcodes YAML by name, this test writes a
//! workspace file into a `.wtd/` directory and uses `find_workspace_in()` to
//! discover it — proving the full discovery → open → list pipeline.

#![cfg(windows)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use wtd_cli::client::IpcClient;
use wtd_cli::exit_code;
use wtd_cli::output;

use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::load_workspace_definition;
use wtd_core::workspace_discovery::find_workspace_in;
use wtd_core::GlobalSettings;

use wtd_host::ipc_server::{ClientId, IpcServer, RequestHandler};
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance};

use wtd_ipc::message::*;
use wtd_ipc::Envelope;

// ── Workspace YAML fixtures ─────────────────────────────────────────

/// Single-pane workspace for basic discovery test.
const SINGLE_PANE_YAML: &str = r#"
version: 1
name: gate-discover
description: "Gate test: single pane discovered from CWD"
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
        startupCommand: "echo GATE_READY"
"#;

/// Multi-pane workspace with named panes across a split layout.
const MULTI_PANE_YAML: &str = r#"
version: 1
name: gate-multi
description: "Gate test: multi-pane discovered from CWD"
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
            startupCommand: "echo EDITOR_READY"
        - type: split
          orientation: horizontal
          ratio: 0.5
          children:
            - type: pane
              name: build
              session:
                profile: cmd
                startupCommand: "echo BUILD_READY"
            - type: pane
              name: logs
              session:
                profile: cmd
                startupCommand: "echo LOGS_READY"
"#;

// ── Unique pipe naming ──────────────────────────────────────────────

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(7000);

fn unique_pipe_name() -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\wtd-gate-open-{}-{}", std::process::id(), n)
}

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("gate-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

// ── Temp directory helpers ──────────────────────────────────────────

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtd-gate-open-{}-{}",
        label,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

/// Write a workspace YAML into `cwd/.wtd/<name>.yaml`.
fn write_workspace_fixture(cwd: &Path, name: &str, yaml: &str) {
    let wtd_dir = cwd.join(".wtd");
    fs::create_dir_all(&wtd_dir).unwrap();
    fs::write(wtd_dir.join(format!("{name}.yaml")), yaml).unwrap();
}

// ── TestHost harness ────────────────────────────────────────────────

struct TestHost {
    _server: Arc<IpcServer>,
    shutdown_tx: watch::Sender<bool>,
    pipe_name: String,
}

impl TestHost {
    async fn start(handler: impl RequestHandler) -> Self {
        let pipe_name = unique_pipe_name();
        let server = Arc::new(IpcServer::new(pipe_name.clone(), handler).unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let s = server.clone();
        tokio::spawn(async move { s.run(shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        TestHost {
            _server: server,
            shutdown_tx,
            pipe_name,
        }
    }

    async fn connect(&self) -> IpcClient {
        IpcClient::connect_to(&self.pipe_name).await.unwrap()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn default_host_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(val) = std::env::var("USERPROFILE") {
        env.insert("USERPROFILE".to_string(), val);
    } else {
        env.insert("USERPROFILE".to_string(), r"C:\".to_string());
    }
    env
}

fn find_exe(name: &str) -> bool {
    matches!(name, "cmd.exe" | "powershell.exe" | "pwsh.exe")
}

fn error_envelope(id: &str, code: ErrorCode, message: &str) -> Envelope {
    Envelope::new(
        id,
        &ErrorResponse {
            code,
            message: message.to_owned(),
            candidates: None,
        },
    )
}

// ── Gate Handler — uses workspace discovery from CWD ────────────────

struct GateState {
    workspaces: HashMap<String, WorkspaceInstance>,
    next_instance_id: u64,
}

struct GateHandler {
    state: Mutex<GateState>,
    /// Simulated CWD for workspace file discovery.
    cwd: PathBuf,
    /// Empty dir so user-dir discovery never matches.
    user_dir: PathBuf,
}

impl GateHandler {
    fn new(cwd: PathBuf, user_dir: PathBuf) -> Self {
        Self {
            state: Mutex::new(GateState {
                workspaces: HashMap::new(),
                next_instance_id: 1,
            }),
            cwd,
            user_dir,
        }
    }
}

impl RequestHandler for GateHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                // §12: Discover workspace definition from CWD/.wtd/
                let discovered = match find_workspace_in(
                    &open.name,
                    open.file.as_ref().map(|f| Path::new(f.as_str())),
                    &self.cwd,
                    &self.user_dir,
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            &format!("discovery failed: {e}"),
                        ))
                    }
                };

                // Read and parse the discovered file
                let content = match fs::read_to_string(&discovered.path) {
                    Ok(c) => c,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("read failed: {e}"),
                        ))
                    }
                };

                let file_name = discovered
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let def = match load_workspace_definition(&file_name, &content) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("parse failed: {e}"),
                        ))
                    }
                };

                let mut state = self.state.lock().unwrap();
                let inst_id = state.next_instance_id;
                state.next_instance_id += 1;

                let inst = match WorkspaceInstance::open(
                    WorkspaceInstanceId(inst_id),
                    &def,
                    &GlobalSettings::default(),
                    &default_host_env(),
                    find_exe,
                ) {
                    Ok(i) => i,
                    Err(e) => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::InternalError,
                            &format!("open failed: {e}"),
                        ))
                    }
                };

                let instance_id = format!("{}", inst.id().0);
                state.workspaces.insert(open.name.clone(), inst);

                Some(Envelope::new(
                    &envelope.id,
                    &OpenWorkspaceResult {
                        instance_id,
                        state: serde_json::Value::Object(serde_json::Map::new()),
                    },
                ))
            }

            TypedMessage::ListPanes(lp) => {
                let state = self.state.lock().unwrap();
                let inst = match state.workspaces.get(&lp.workspace) {
                    Some(i) => i,
                    None => {
                        return Some(error_envelope(
                            &envelope.id,
                            ErrorCode::WorkspaceNotFound,
                            &format!("workspace '{}' not found", lp.workspace),
                        ))
                    }
                };

                let mut panes = Vec::new();
                for tab in inst.tabs() {
                    for pane_id in tab.layout().panes() {
                        let name = inst.pane_name(&pane_id).unwrap_or("?").to_string();
                        let session_state = match inst.pane_state(&pane_id) {
                            Some(PaneState::Attached { session_id }) => inst
                                .session(session_id)
                                .map(|s| format!("{:?}", s.state()))
                                .unwrap_or_else(|| "unknown".to_string()),
                            Some(PaneState::Detached { error }) => {
                                format!("detached: {error}")
                            }
                            None => "none".to_string(),
                        };
                        panes.push(PaneInfo {
                            name,
                            tab: tab.name().to_string(),
                            session_state,
                        });
                    }
                }

                Some(Envelope::new(&envelope.id, &ListPanesResult { panes }))
            }

            TypedMessage::Capture(cap) => {
                let mut state = self.state.lock().unwrap();

                // Drain output for all sessions first
                for inst in state.workspaces.values_mut() {
                    for session in inst.sessions_mut().values_mut() {
                        session.process_pending_output();
                    }
                }

                // Find pane by name
                let (text, found) = {
                    let mut text = String::new();
                    let mut found = false;
                    for inst in state.workspaces.values() {
                        if let Some(pane_id) = inst.find_pane_by_name(&cap.target) {
                            found = true;
                            if let Some(PaneState::Attached { session_id }) =
                                inst.pane_state(&pane_id)
                            {
                                if let Some(s) = inst.session(session_id) {
                                    text = s.screen().visible_text();
                                }
                            }
                            break;
                        }
                    }
                    (text, found)
                };

                if !found {
                    return Some(error_envelope(
                        &envelope.id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", cap.target),
                    ));
                }

                Some(Envelope::new(&envelope.id, &CaptureResult { text, ..Default::default() }))
            }

            TypedMessage::CloseWorkspace(close) => {
                let mut state = self.state.lock().unwrap();
                match state.workspaces.remove(&close.workspace) {
                    Some(mut inst) => {
                        inst.close();
                        Some(Envelope::new(&envelope.id, &OkResponse {}))
                    }
                    None => Some(error_envelope(
                        &envelope.id,
                        ErrorCode::WorkspaceNotFound,
                        &format!("workspace '{}' not found", close.workspace),
                    )),
                }
            }

            _ => None,
        }
    }
}

// ── Polling helper ──────────────────────────────────────────────────

async fn poll_capture_until(
    client: &mut IpcClient,
    target: &str,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let start = tokio::time::Instant::now();
    let mut last_text = String::new();
    while start.elapsed() < timeout {
        let resp = client
            .request(&Envelope::new(
                &next_id(),
                &Capture {
                    target: target.to_string(),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
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

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

/// Gate test: discover single-pane workspace from CWD/.wtd/, open it,
/// verify list panes returns the pane with its semantic name.
#[tokio::test]
async fn open_discovers_from_cwd_and_list_panes_shows_name() {
    let cwd = temp_dir("single");
    let user_dir = cwd.join("empty-user-dir");
    fs::create_dir_all(&user_dir).unwrap();

    // Write fixture into CWD/.wtd/
    write_workspace_fixture(&cwd, "gate-discover", SINGLE_PANE_YAML);

    let handler = GateHandler::new(cwd.clone(), user_dir);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // 1. Open workspace — handler discovers from CWD/.wtd/gate-discover.yaml
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "gate-discover".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);
    let open_result: OpenWorkspaceResult = resp.extract_payload().unwrap();
    assert!(!open_result.instance_id.is_empty());

    // Verify output formatting
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);

    // 2. List panes — verify semantic name "shell" from definition
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "gate-discover".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListPanesResult::TYPE_NAME);
    let lp: ListPanesResult = resp.extract_payload().unwrap();
    assert_eq!(lp.panes.len(), 1, "single-pane workspace should have 1 pane");
    assert_eq!(lp.panes[0].name, "shell", "pane name from YAML definition");
    assert_eq!(lp.panes[0].tab, "main", "tab name from YAML definition");

    // Verify text output formatting includes pane name
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("shell"), "text output should contain pane name 'shell'");
    assert!(fmt.stdout.contains("main"), "text output should contain tab name 'main'");

    // Verify JSON output includes pane name
    let fmt_json = output::format_response(&resp, true);
    assert_eq!(fmt_json.exit_code, exit_code::SUCCESS);
    assert!(fmt_json.stdout.contains("shell"));

    // 3. Verify session starts and produces output
    let text = poll_capture_until(
        &mut client,
        "shell",
        |t| t.contains("GATE_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        text.contains("GATE_READY"),
        "startup marker should appear in capture"
    );

    // 4. Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "gate-discover".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    cleanup(&cwd);
}

/// Gate test: multi-pane workspace — discover from CWD, open, verify all
/// three panes appear with their semantic names and correct tab association.
#[tokio::test]
async fn open_multi_pane_lists_all_panes_by_name() {
    let cwd = temp_dir("multi");
    let user_dir = cwd.join("empty-user-dir");
    fs::create_dir_all(&user_dir).unwrap();

    write_workspace_fixture(&cwd, "gate-multi", MULTI_PANE_YAML);

    let handler = GateHandler::new(cwd.clone(), user_dir);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    // 1. Open multi-pane workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "gate-multi".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OpenWorkspaceResult::TYPE_NAME);

    // 2. List panes — verify all three panes with semantic names
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "gate-multi".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ListPanesResult::TYPE_NAME);
    let lp: ListPanesResult = resp.extract_payload().unwrap();

    assert_eq!(lp.panes.len(), 3, "three-pane workspace should have 3 panes");

    // Collect pane names for assertion
    let pane_names: Vec<&str> = lp.panes.iter().map(|p| p.name.as_str()).collect();
    assert!(
        pane_names.contains(&"editor"),
        "pane 'editor' should be listed; got {pane_names:?}"
    );
    assert!(
        pane_names.contains(&"build"),
        "pane 'build' should be listed; got {pane_names:?}"
    );
    assert!(
        pane_names.contains(&"logs"),
        "pane 'logs' should be listed; got {pane_names:?}"
    );

    // All panes belong to the "dev" tab
    for pane in &lp.panes {
        assert_eq!(pane.tab, "dev", "all panes should be in tab 'dev'");
    }

    // Verify text output contains all pane names
    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::SUCCESS);
    assert!(fmt.stdout.contains("editor"), "text output should contain 'editor'");
    assert!(fmt.stdout.contains("build"), "text output should contain 'build'");
    assert!(fmt.stdout.contains("logs"), "text output should contain 'logs'");
    assert!(fmt.stdout.contains("dev"), "text output should contain tab 'dev'");

    // 3. Wait for all sessions to start producing output
    let text = poll_capture_until(
        &mut client,
        "editor",
        |t| t.contains("EDITOR_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("EDITOR_READY"), "editor pane should have startup output");

    let text = poll_capture_until(
        &mut client,
        "build",
        |t| t.contains("BUILD_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("BUILD_READY"), "build pane should have startup output");

    let text = poll_capture_until(
        &mut client,
        "logs",
        |t| t.contains("LOGS_READY"),
        Duration::from_secs(10),
    )
    .await;
    assert!(text.contains("LOGS_READY"), "logs pane should have startup output");

    // 4. Close workspace
    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &CloseWorkspace {
                workspace: "gate-multi".to_string(),
                kill: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, OkResponse::TYPE_NAME);

    cleanup(&cwd);
}

/// Gate test: opening a workspace that doesn't exist in CWD/.wtd/ returns
/// WorkspaceNotFound error with appropriate exit code.
#[tokio::test]
async fn open_nonexistent_workspace_returns_not_found() {
    let cwd = temp_dir("notfound");
    let user_dir = cwd.join("empty-user-dir");
    fs::create_dir_all(&user_dir).unwrap();

    // No .wtd/ directory — discovery should fail
    let handler = GateHandler::new(cwd.clone(), user_dir);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &OpenWorkspace {
                name: "nonexistent".to_string(),
                file: None,
                recreate: false,
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    let fmt = output::format_response(&resp, false);
    assert_eq!(fmt.exit_code, exit_code::TARGET_NOT_FOUND);

    cleanup(&cwd);
}

/// Gate test: listing panes for a workspace that hasn't been opened returns error.
#[tokio::test]
async fn list_panes_before_open_returns_not_found() {
    let cwd = temp_dir("list-before-open");
    let user_dir = cwd.join("empty-user-dir");
    fs::create_dir_all(&user_dir).unwrap();

    let handler = GateHandler::new(cwd.clone(), user_dir);
    let host = TestHost::start(handler).await;
    let mut client = host.connect().await;

    let resp = client
        .request(&Envelope::new(
            &next_id(),
            &ListPanes {
                workspace: "gate-discover".to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(resp.msg_type, ErrorResponse::TYPE_NAME);

    let err: ErrorResponse = resp.extract_payload().unwrap();
    assert_eq!(err.code, ErrorCode::WorkspaceNotFound);

    cleanup(&cwd);
}
