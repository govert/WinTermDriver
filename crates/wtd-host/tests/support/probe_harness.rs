#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use wtd_core::ids::WorkspaceInstanceId;
use wtd_core::load_workspace_definition;
use wtd_core::GlobalSettings;
use wtd_host::workspace_instance::{PaneState, WorkspaceInstance, WorkspaceState};
use wtd_pty::screen::KeyboardProtocolMode;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(5000);

pub struct ProbeHarness {
    pub instance: WorkspaceInstance,
    pane_name: String,
}

impl ProbeHarness {
    pub fn open(args: &[&str]) -> Self {
        let yaml = probe_workspace_yaml(args);
        let def = load_workspace_definition("probe-harness.yaml", &yaml)
            .expect("probe harness YAML should parse and validate");
        let gs = GlobalSettings::default();
        let env = default_host_env();
        let id = WorkspaceInstanceId(WORKSPACE_COUNTER.fetch_add(1, Ordering::SeqCst));

        let instance = WorkspaceInstance::open(id, &def, &gs, &env, find_exe_probe_or_builtin)
            .expect("probe workspace should open");
        assert_eq!(*instance.state(), WorkspaceState::Active);

        Self {
            instance,
            pane_name: "probe".to_string(),
        }
    }

    pub fn wait_for_text(&mut self, needle: &str, timeout: Duration) -> bool {
        wait_until(timeout, Duration::from_millis(50), || {
            self.drain_output();
            self.capture_text().contains(needle)
        })
    }

    pub fn capture_text(&self) -> String {
        let session = self.session();
        session.screen().visible_text()
    }

    pub fn send_input(&self, bytes: &[u8]) {
        self.session()
            .write_input(bytes)
            .expect("probe input should succeed");
    }

    pub fn session_env(&self) -> &HashMap<String, String> {
        &self.session().config().env
    }

    pub fn keyboard_protocol(&mut self) -> KeyboardProtocolMode {
        self.drain_output();
        self.session().screen().keyboard_protocol()
    }

    fn drain_output(&mut self) {
        for session in self.instance.sessions_mut().values_mut() {
            session.process_pending_output();
        }
    }

    fn session(&self) -> &wtd_host::session::Session {
        let pane_id = self
            .instance
            .find_pane_by_name(&self.pane_name)
            .expect("probe pane should exist");
        let session_id = match self.instance.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => session_id,
            other => panic!("expected attached probe pane, got {other:?}"),
        };
        self.instance
            .session(session_id)
            .expect("probe session should exist")
    }
}

fn wait_until(timeout: Duration, interval: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        thread::sleep(interval);
    }
    false
}

fn default_host_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(val) = std::env::var("USERPROFILE") {
        env.insert("USERPROFILE".to_string(), val);
    } else {
        env.insert("USERPROFILE".to_string(), r"C:\".to_string());
    }
    env
}

fn find_exe_probe_or_builtin(name: &str) -> bool {
    matches!(name, "cmd.exe" | "powershell.exe" | "pwsh.exe") || Path::new(name).exists()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn probe_executable() -> PathBuf {
    let root = workspace_root();
    let probe = root.join("target").join("debug").join("wtd-probe.exe");
    if probe.exists() {
        return probe;
    }

    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "wtd-probe", "--bin", "wtd-probe"])
        .current_dir(&root)
        .status()
        .expect("cargo build for wtd-probe should start");
    assert!(status.success(), "cargo build for wtd-probe should succeed");
    assert!(
        probe.exists(),
        "wtd-probe executable not found under {} after build",
        root.display()
    );
    probe
}

fn probe_workspace_yaml(args: &[&str]) -> String {
    let exe = probe_executable()
        .display()
        .to_string()
        .replace('\\', "\\\\");
    let args_yaml = if args.is_empty() {
        String::new()
    } else {
        let items = args
            .iter()
            .map(|arg| {
                format!(
                    "      - \"{}\"",
                    arg.replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n    args:\n{items}")
    };

    format!(
        "version: 1
name: probe-harness
profiles:
  probe:
    type: custom
    executable: \"{exe}\"{args_yaml}
tabs:
  - name: main
    layout:
      type: pane
      name: probe
      session:
        profile: probe
"
    )
}
