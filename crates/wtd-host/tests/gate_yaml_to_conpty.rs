//! Gate integration test: YAML workspace definition → running ConPTY session.
//!
//! Verifies the full pipeline: parse YAML fixture → resolve profile → create
//! workspace instance → session reaches Running state with a live ConPTY.
//! This is the first integration point connecting workspace definitions (W1)
//! and the session/PTY subsystem (W3).

#[cfg(windows)]
mod tests {
    use std::collections::HashMap;
    use std::thread;
    use std::time::{Duration, Instant};

    use wtd_core::ids::WorkspaceInstanceId;
    use wtd_core::load_workspace_definition;
    use wtd_core::GlobalSettings;
    use wtd_host::session::SessionState;
    use wtd_host::workspace_instance::{PaneState, WorkspaceInstance, WorkspaceState};

    /// Fixture YAML is loaded at compile time from the fixtures directory.
    const SIMPLE_YAML: &str = include_str!("fixtures/simple-workspace.yaml");
    const SPLIT_YAML: &str = include_str!("fixtures/split-workspace.yaml");

    fn default_host_env() -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Ok(val) = std::env::var("USERPROFILE") {
            env.insert("USERPROFILE".to_string(), val);
        } else {
            env.insert("USERPROFILE".to_string(), r"C:\".to_string());
        }
        env
    }

    fn find_exe_windows(name: &str) -> bool {
        matches!(name, "cmd.exe" | "powershell.exe" | "pwsh.exe")
    }

    /// Poll until a predicate is true, or timeout.
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

    // ── Test 1: Single-pane YAML → parse → workspace instance → Running ────

    #[test]
    fn yaml_single_pane_session_reaches_running() {
        // Step 1: Parse YAML fixture through the workspace loader
        let def = load_workspace_definition("fixtures/simple-workspace.yaml", SIMPLE_YAML)
            .expect("fixture YAML should parse and validate");

        assert_eq!(def.name, "gate-test");
        assert!(def.tabs.is_some());
        assert_eq!(def.tabs.as_ref().unwrap().len(), 1);

        // Step 2: Create workspace instance (resolves profiles, spawns sessions)
        let gs = GlobalSettings::default();
        let env = default_host_env();

        let inst = WorkspaceInstance::open(
            WorkspaceInstanceId(100),
            &def,
            &gs,
            &env,
            find_exe_windows,
        )
        .expect("workspace instance should open");

        // Step 3: Verify workspace is Active with one session
        assert_eq!(*inst.state(), WorkspaceState::Active);
        assert_eq!(inst.session_count(), 1);
        assert_eq!(inst.failed_pane_count(), 0);
        assert_eq!(inst.tabs().len(), 1);
        assert_eq!(inst.tabs()[0].name(), "main");

        // Step 4: Verify the single pane is attached
        let pane_ids = inst.tabs()[0].layout().panes();
        assert_eq!(pane_ids.len(), 1);
        assert!(
            matches!(inst.pane_state(&pane_ids[0]), Some(PaneState::Attached { .. })),
            "pane should be attached to a session"
        );

        // Step 5: Verify session reached Running state (may already have exited
        // if cmd.exe processed the startup command quickly, so accept either)
        for (_id, session) in inst.sessions() {
            assert!(
                matches!(
                    session.state(),
                    SessionState::Running | SessionState::Exited { .. }
                ),
                "session should be Running or Exited, got {:?}",
                session.state()
            );
        }
    }

    // ── Test 2: Split-pane YAML → multiple sessions all Running ────────────

    #[test]
    fn yaml_split_pane_multiple_sessions_running() {
        let def = load_workspace_definition("fixtures/split-workspace.yaml", SPLIT_YAML)
            .expect("split fixture YAML should parse and validate");

        assert_eq!(def.name, "gate-split");

        let gs = GlobalSettings::default();
        let env = default_host_env();

        let inst = WorkspaceInstance::open(
            WorkspaceInstanceId(101),
            &def,
            &gs,
            &env,
            find_exe_windows,
        )
        .expect("split workspace should open");

        assert_eq!(*inst.state(), WorkspaceState::Active);
        assert_eq!(inst.session_count(), 2);
        assert_eq!(inst.failed_pane_count(), 0);

        // Both panes attached
        let pane_ids = inst.tabs()[0].layout().panes();
        assert_eq!(pane_ids.len(), 2);
        for pane_id in &pane_ids {
            assert!(
                matches!(inst.pane_state(pane_id), Some(PaneState::Attached { .. })),
                "pane {:?} should be attached",
                pane_id
            );
        }

        // All sessions should be Running or Exited
        for (_id, session) in inst.sessions() {
            assert!(
                matches!(
                    session.state(),
                    SessionState::Running | SessionState::Exited { .. }
                ),
                "session should be Running or Exited, got {:?}",
                session.state()
            );
        }

        // Focus should be on "terminal" (second pane in depth-first order)
        assert_eq!(inst.tabs()[0].layout().focus(), pane_ids[1]);
    }

    // ── Test 3: Session produces output from startup command ────────────────

    #[test]
    fn yaml_session_delivers_startup_command_output() {
        let def = load_workspace_definition("fixtures/simple-workspace.yaml", SIMPLE_YAML)
            .expect("fixture YAML should parse");

        let gs = GlobalSettings::default();
        let env = default_host_env();

        let mut inst = WorkspaceInstance::open(
            WorkspaceInstanceId(102),
            &def,
            &gs,
            &env,
            find_exe_windows,
        )
        .expect("workspace should open");

        assert_eq!(*inst.state(), WorkspaceState::Active);

        // The startup command is "echo GATE_MARKER". Wait for output to appear.
        let found = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            // Drain pending output for all sessions
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            // Check if any session's screen contains the marker
            inst.sessions().values().any(|s| {
                s.screen().visible_text().contains("GATE_MARKER")
            })
        });

        assert!(
            found,
            "startup command output 'GATE_MARKER' should appear in session screen"
        );
    }
}
