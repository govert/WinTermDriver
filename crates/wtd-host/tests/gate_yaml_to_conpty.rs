//! Gate integration tests: YAML workspace definition → running ConPTY session.
//!
//! Verifies the full pipeline: parse YAML fixture → resolve profile → create
//! workspace instance → session reaches Running state with a live ConPTY.
//! Also verifies the I/O round-trip: input sent to a session appears in the
//! VT screen buffer output.
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

    // ── Test 4: Input sent to session appears in VT screen buffer ────────

    #[test]
    fn input_sent_to_session_appears_in_screen_buffer() {
        let def = load_workspace_definition("fixtures/simple-workspace.yaml", SIMPLE_YAML)
            .expect("fixture YAML should parse");

        let gs = GlobalSettings::default();
        let env = default_host_env();

        let mut inst = WorkspaceInstance::open(
            WorkspaceInstanceId(103),
            &def,
            &gs,
            &env,
            find_exe_windows,
        )
        .expect("workspace should open");

        assert_eq!(*inst.state(), WorkspaceState::Active);

        // Wait for the session to be Running and the initial prompt to settle.
        let ready = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            inst.sessions().values().any(|s| {
                matches!(s.state(), &SessionState::Running)
                    && !s.screen().visible_text().is_empty()
            })
        });
        assert!(ready, "session should be Running with initial output");

        // Send a unique command. The echo output will appear in the screen buffer
        // when cmd.exe executes it and the PTY output flows back through the reader.
        let marker = "INPUT_ROUND_TRIP_7X9Q";
        let command = format!("echo {}\r\n", marker);
        for session in inst.sessions().values() {
            session
                .write_input(command.as_bytes())
                .expect("write_input should succeed on a running session");
        }

        // Poll until the marker text appears in the screen buffer.
        let found = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            inst.sessions()
                .values()
                .any(|s| s.screen().visible_text().contains(marker))
        });

        assert!(
            found,
            "text sent via write_input should appear in session screen buffer"
        );

        // Verify the screen buffer has the echoed text (not just the command line).
        // cmd.exe echoes the command AND prints the result. Both should contain the marker.
        let screen_text = inst
            .sessions()
            .values()
            .next()
            .unwrap()
            .screen()
            .visible_text();
        let marker_count = screen_text.matches(marker).count();
        assert!(
            marker_count >= 2,
            "marker should appear at least twice (command echo + output), found {} times in:\n{}",
            marker_count,
            screen_text
        );
    }

    // ── Test 5: Multiple inputs produce sequential screen buffer output ──

    #[test]
    fn multiple_inputs_appear_sequentially_in_screen_buffer() {
        let def = load_workspace_definition("fixtures/simple-workspace.yaml", SIMPLE_YAML)
            .expect("fixture YAML should parse");

        let gs = GlobalSettings::default();
        let env = default_host_env();

        let mut inst = WorkspaceInstance::open(
            WorkspaceInstanceId(104),
            &def,
            &gs,
            &env,
            find_exe_windows,
        )
        .expect("workspace should open");

        assert_eq!(*inst.state(), WorkspaceState::Active);

        // Wait for session to settle.
        let ready = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            inst.sessions().values().any(|s| {
                matches!(s.state(), &SessionState::Running)
                    && !s.screen().visible_text().is_empty()
            })
        });
        assert!(ready, "session should be Running with initial output");

        // Send two distinct commands sequentially.
        let marker_a = "MARKER_ALPHA_3K";
        let marker_b = "MARKER_BRAVO_7J";

        for session in inst.sessions().values() {
            session
                .write_input(format!("echo {}\r\n", marker_a).as_bytes())
                .expect("first write_input should succeed");
        }

        // Wait for first marker before sending second.
        let found_a = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            inst.sessions()
                .values()
                .any(|s| s.screen().visible_text().contains(marker_a))
        });
        assert!(found_a, "first marker should appear in screen buffer");

        for session in inst.sessions().values() {
            session
                .write_input(format!("echo {}\r\n", marker_b).as_bytes())
                .expect("second write_input should succeed");
        }

        // Wait for second marker.
        let found_b = wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
            inst.sessions()
                .values()
                .any(|s| s.screen().visible_text().contains(marker_b))
        });
        assert!(found_b, "second marker should appear in screen buffer");

        // Both markers should be present, with A appearing before B.
        let screen_text = inst
            .sessions()
            .values()
            .next()
            .unwrap()
            .screen()
            .visible_text();
        let pos_a = screen_text.find(marker_a).expect("marker A in screen");
        let pos_b = screen_text.find(marker_b).expect("marker B in screen");
        assert!(
            pos_a < pos_b,
            "marker A (pos {}) should appear before marker B (pos {}) in screen:\n{}",
            pos_a,
            pos_b,
            screen_text
        );
    }
}
