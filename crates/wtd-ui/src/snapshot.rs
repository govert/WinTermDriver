use std::collections::HashMap;

use serde_json::Value;
use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_core::workspace::PaneNode;
use wtd_ipc::message::{AttentionState, ProgressInfo};
use wtd_pty::ScreenBuffer;

/// Session mapping: pane ID -> session ID/path pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSession {
    pub host_pane_id: Option<String>,
    pub session_id: String,
    pub pane_path: String,
    pub title: Option<String>,
    pub session_size: Option<(u16, u16)>,
    pub progress: Option<ProgressInfo>,
    pub attention: AttentionState,
    pub attention_message: Option<String>,
    pub phase: Option<String>,
    pub status_text: Option<String>,
    pub queue_pending: Option<u32>,
    pub health_state: Option<String>,
    pub source: Option<String>,
    pub driver_profile: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
}

/// Snapshot of one tab after attach.
pub struct SnapshotTab {
    pub layout_tree: LayoutTree,
    pub pane_sessions: HashMap<PaneId, PaneSession>,
    pub screens: HashMap<PaneId, ScreenBuffer>,
}

/// Output of [`rebuild_from_snapshot`].
pub struct SnapshotRebuild {
    pub workspace_name: String,
    pub workspace_saved: bool,
    pub workspace_dirty: bool,
    pub workspace_save_path: Option<String>,
    pub tab_names: Vec<String>,
    pub active_tab_index: usize,
    pub tabs: Vec<SnapshotTab>,
}

/// Rebuild the UI-side layout tree(s), pane→session mappings, and seeded
/// screen buffers from an `AttachWorkspaceResult.state` JSON value.
pub fn rebuild_from_snapshot(state: &Value, cols: u16, rows: u16) -> Option<SnapshotRebuild> {
    let workspace_name = state["name"].as_str().unwrap_or("workspace").to_string();
    let save_status = state.get("saveStatus");
    let workspace_saved = save_status
        .and_then(|value| value.get("saved"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let workspace_dirty = save_status
        .and_then(|value| value.get("dirty"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let workspace_save_path = save_status
        .and_then(|value| value.get("path"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tabs = state["tabs"].as_array()?;
    let active_tab_index = state
        .get("activeTabIndex")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);

    let tab_names = tabs
        .iter()
        .map(|tab| tab["name"].as_str().unwrap_or("tab").to_string())
        .collect::<Vec<_>>();
    let pane_states = state["paneStates"].as_object()?;
    let session_screens = state["sessionScreens"].as_object();
    let session_history = state["sessionHistory"].as_object();
    let session_sizes = state["sessionSizes"].as_object();
    let session_titles = state["sessionTitles"].as_object();
    let session_progress = state["sessionProgress"].as_object();
    let session_states = state["sessionStates"].as_object();
    let pane_attention = state["paneAttention"].as_object();
    let pane_metadata = state["paneMetadata"].as_object();

    let mut rebuilt_tabs = Vec::new();
    for tab in tabs {
        let tab_name = tab["name"].as_str().unwrap_or("tab");
        let layout_node: PaneNode = serde_json::from_value(tab["layout"].clone()).ok()?;
        let (mut layout_tree, pane_mappings) = LayoutTree::from_pane_node(&layout_node);
        if let Some(focus_name) = tab.get("focus").and_then(|value| value.as_str()) {
            if let Some((_, pane_id)) = pane_mappings.iter().find(|(name, _)| name == focus_name) {
                let _ = layout_tree.set_focus(pane_id.clone());
            }
        }

        let host_panes: Vec<u64> = tab["panes"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_u64())
            .collect();

        let mut pane_sessions = HashMap::new();
        let mut screens = HashMap::new();

        for (i, (pane_name, ui_pane_id)) in pane_mappings.iter().enumerate() {
            let mut screen = ScreenBuffer::new(cols, rows, 1000);

            if let Some(&host_pane_id) = host_panes.get(i) {
                let host_pane_key = host_pane_id.to_string();
                if let Some(ps) = pane_states.get(&host_pane_key) {
                    if ps["type"] == "attached" {
                        if let Some(session_id) = session_id_string(&ps["sessionId"]) {
                            let session_size = session_sizes
                                .and_then(|sizes| sizes.get(&session_id))
                                .and_then(session_size_from_value);
                            let scrollback_rows = session_history
                                .and_then(|history| history.get(&session_id))
                                .and_then(|value| value.get("scrollbackRows"))
                                .and_then(|value| value.as_u64())
                                .and_then(|value| usize::try_from(value).ok())
                                .unwrap_or(0);
                            let session_title = session_titles
                                .and_then(|title_map| title_map.get(&session_id))
                                .and_then(|value| value.as_str())
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_owned);
                            let max_scrollback = 1000usize.max(scrollback_rows);
                            if let Some((session_cols, session_rows)) = session_size {
                                screen =
                                    ScreenBuffer::new(session_cols, session_rows, max_scrollback);
                            } else if max_scrollback > 1000 {
                                screen = ScreenBuffer::new(cols, rows, max_scrollback);
                            }

                            if let Some(b64) = session_history
                                .and_then(|history| history.get(&session_id))
                                .and_then(|value| value.get("scrollbackVt"))
                                .and_then(|v| v.as_str())
                            {
                                let data = base64_decode(b64);
                                if !data.is_empty() {
                                    screen.advance(&data);
                                    let blank_lines = "\r\n".repeat(screen.rows());
                                    screen.advance(blank_lines.as_bytes());
                                }
                            }

                            if let Some(b64) = session_screens
                                .and_then(|screens| screens.get(&session_id))
                                .and_then(|v| v.as_str())
                            {
                                let data = base64_decode(b64);
                                if !data.is_empty() {
                                    screen.advance(&data);
                                }
                            }

                            let health_state = session_states
                                .and_then(|states| states.get(&session_id))
                                .and_then(|value| value.get("type"))
                                .and_then(|value| value.as_str())
                                .map(str::to_string);

                            pane_sessions.insert(
                                ui_pane_id.clone(),
                                PaneSession {
                                    host_pane_id: Some(host_pane_key.clone()),
                                    progress: session_progress
                                        .and_then(|progress_map| progress_map.get(&session_id))
                                        .and_then(|value| {
                                            serde_json::from_value::<ProgressInfo>(value.clone())
                                                .ok()
                                        }),
                                    session_id,
                                    pane_path: format!("{workspace_name}/{tab_name}/{pane_name}",),
                                    title: session_title,
                                    session_size,
                                    attention: pane_attention
                                        .and_then(|attention_map| attention_map.get(&host_pane_key))
                                        .and_then(|value| value.get("state"))
                                        .and_then(|value| {
                                            serde_json::from_value::<AttentionState>(value.clone())
                                                .ok()
                                        })
                                        .unwrap_or(AttentionState::Active),
                                    attention_message: pane_attention
                                        .and_then(|attention_map| attention_map.get(&host_pane_key))
                                        .and_then(|value| value.get("message"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    phase: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("phase"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    status_text: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("statusText"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    queue_pending: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("queuePending"))
                                        .and_then(|value| value.as_u64())
                                        .and_then(|value| value.try_into().ok()),
                                    health_state,
                                    source: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("source"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    driver_profile: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("driverProfile"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    cwd: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("cwd"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                    branch: pane_metadata
                                        .and_then(|metadata_map| metadata_map.get(&host_pane_key))
                                        .and_then(|value| value.get("branch"))
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string),
                                },
                            );
                        }
                    }
                }
            }

            screens.insert(ui_pane_id.clone(), screen);
        }

        rebuilt_tabs.push(SnapshotTab {
            layout_tree,
            pane_sessions,
            screens,
        });
    }

    let active_tab_index = if rebuilt_tabs.is_empty() {
        0
    } else {
        active_tab_index.min(rebuilt_tabs.len() - 1)
    };

    Some(SnapshotRebuild {
        workspace_name,
        workspace_saved,
        workspace_dirty,
        workspace_save_path,
        tab_names,
        active_tab_index,
        tabs: rebuilt_tabs,
    })
}

fn session_size_from_value(v: &Value) -> Option<(u16, u16)> {
    let cols = v.get("cols")?.as_u64()?.try_into().ok()?;
    let rows = v.get("rows")?.as_u64()?.try_into().ok()?;
    Some((cols, rows))
}

fn session_id_string(v: &Value) -> Option<String> {
    if let Some(sid) = v.as_u64() {
        return Some(sid.to_string());
    }
    v.as_str().map(ToOwned::to_owned)
}

fn base64_decode(input: &str) -> Vec<u8> {
    fn val(c: u8) -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }

    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let b0 = val(chunk[0]) as u32;
        let b1 = val(chunk[1]) as u32;
        let b2 = if chunk.len() > 2 {
            val(chunk[2]) as u32
        } else {
            0
        };
        let b3 = if chunk.len() > 3 {
            val(chunk[3]) as u32
        } else {
            0
        };
        let triple = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        out.push(((triple >> 16) & 0xff) as u8);
        if chunk.len() > 2 {
            out.push(((triple >> 8) & 0xff) as u8);
        }
        if chunk.len() > 3 {
            out.push((triple & 0xff) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base64_encode(input: &[u8]) -> String {
        const B64_CHARS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(B64_CHARS[((triple >> 18) & 0x3f) as usize] as char);
            out.push(B64_CHARS[((triple >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(B64_CHARS[((triple >> 6) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(B64_CHARS[(triple & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn rebuild_snapshot_maps_focused_pane_to_session_and_path() {
        let state = json!({
            "name": "dev",
            "tabs": [{
                "name": "main",
                "layout": {
                    "type": "pane",
                    "name": "shell"
                },
                "panes": [11]
            }],
            "paneStates": {
                "11": {
                    "type": "attached",
                    "sessionId": 21
                }
            },
            "sessionStates": {
                "21": {
                    "type": "running"
                }
            },
            "paneAttention": {
                "11": {
                    "state": "needs_attention",
                    "message": "review requested",
                    "source": "pi"
                }
            },
            "paneMetadata": {
                "11": {
                    "phase": "working",
                    "statusText": "running tests",
                    "queuePending": 1,
                    "source": "codex",
                    "driverProfile": "codex",
                    "cwd": "C:/Work/WinTermDriver",
                    "branch": "main"
                }
            }
        });

        let rebuilt = rebuild_from_snapshot(&state, 80, 24).expect("snapshot must rebuild");
        let tab = &rebuilt.tabs[0];
        let focused = tab.layout_tree.focus();
        let ps = tab
            .pane_sessions
            .get(&focused)
            .expect("focused pane must map to a session");

        assert_eq!(rebuilt.workspace_name, "dev");
        assert_eq!(rebuilt.tab_names, vec!["main"]);
        assert_eq!(rebuilt.active_tab_index, 0);
        assert_eq!(ps.session_id, "21");
        assert_eq!(ps.host_pane_id.as_deref(), Some("11"));
        assert_eq!(ps.pane_path, "dev/main/shell");
        assert_eq!(ps.attention, AttentionState::NeedsAttention);
        assert_eq!(ps.attention_message.as_deref(), Some("review requested"));
        assert_eq!(ps.phase.as_deref(), Some("working"));
        assert_eq!(ps.status_text.as_deref(), Some("running tests"));
        assert_eq!(ps.queue_pending, Some(1));
        assert_eq!(ps.health_state.as_deref(), Some("running"));
        assert_eq!(ps.source.as_deref(), Some("codex"));
        assert_eq!(ps.driver_profile.as_deref(), Some("codex"));
        assert_eq!(ps.cwd.as_deref(), Some("C:/Work/WinTermDriver"));
        assert_eq!(ps.branch.as_deref(), Some("main"));
        assert!(tab.screens.contains_key(&focused));
    }

    #[test]
    fn rebuild_snapshot_replays_seeded_screen_content() {
        let state = json!({
            "name": "screen-seed-test",
            "tabs": [{
                "name": "main",
                "layout": {
                    "type": "pane",
                    "name": "shell"
                },
                "panes": [42]
            }],
            "paneStates": {
                "42": {
                    "type": "attached",
                    "sessionId": "session-42"
                }
            },
            "sessionSizes": {
                "session-42": {
                    "cols": 132,
                    "rows": 41
                }
            },
            "sessionScreens": {
                "session-42": base64_encode(b"\x1b[32mSCREEN_SEED_MARKER\x1b[0m\r\n")
            }
        });

        let rebuilt = rebuild_from_snapshot(&state, 80, 24).expect("snapshot must rebuild");
        let tab = &rebuilt.tabs[0];
        let focused = tab.layout_tree.focus();
        let session = tab
            .pane_sessions
            .get(&focused)
            .expect("focused pane should map to a session");
        let screen = rebuilt.tabs[0]
            .screens
            .get(&focused)
            .expect("focused pane should have a screen");
        let visible = screen.visible_text();

        assert_eq!(session.session_size, Some((132, 41)));
        assert_eq!(screen.cols(), 132);
        assert_eq!(screen.rows(), 41);
        assert!(
            visible.contains("SCREEN_SEED_MARKER"),
            "replayed snapshot should contain SCREEN_SEED_MARKER; got:\n{visible}"
        );
    }

    #[test]
    fn rebuild_snapshot_replays_scrollback_before_visible_seed() {
        let history = base64_encode(b"line1\r\nline2\r\nline3\r\nline4\r\nline5\r\n");
        let visible = base64_encode(b"\x1b[2J\x1b[Htail-visible\r\n");
        let state = json!({
            "name": "history-seed-test",
            "tabs": [{
                "name": "main",
                "layout": {
                    "type": "pane",
                    "name": "shell"
                },
                "panes": [42]
            }],
            "paneStates": {
                "42": {
                    "type": "attached",
                    "sessionId": "session-42"
                }
            },
            "sessionSizes": {
                "session-42": {
                    "cols": 80,
                    "rows": 2
                }
            },
            "sessionHistory": {
                "session-42": {
                    "scrollbackRows": 5,
                    "scrollbackVt": history
                }
            },
            "sessionScreens": {
                "session-42": visible
            }
        });

        let rebuilt = rebuild_from_snapshot(&state, 80, 24).expect("snapshot must rebuild");
        let tab = &rebuilt.tabs[0];
        let focused = tab.layout_tree.focus();
        let screen = rebuilt.tabs[0]
            .screens
            .get(&focused)
            .expect("focused pane should have a screen");

        assert!(screen.scrollback_len() >= 3);
        let oldest = screen
            .scrollback_row(0)
            .map(|row| {
                let mut text = String::new();
                for cell in row {
                    text.push_str(cell.text.as_str());
                }
                text.trim_end_matches(' ').to_string()
            })
            .expect("oldest scrollback row should exist");
        assert_eq!(oldest, "line1");
        assert!(
            screen.visible_text().contains("tail-visible"),
            "visible seed should still win for the current viewport; got:\n{}",
            screen.visible_text()
        );
    }

    #[test]
    fn rebuild_snapshot_restores_tab_focus_from_snapshot() {
        let state = json!({
            "name": "focus-restore-test",
            "tabs": [{
                "name": "main",
                "focus": "bottom",
                "layout": {
                    "type": "split",
                    "orientation": "vertical",
                    "ratio": 0.5,
                    "children": [
                        {
                            "type": "pane",
                            "name": "top"
                        },
                        {
                            "type": "pane",
                            "name": "bottom"
                        }
                    ]
                },
                "panes": [10, 11]
            }],
            "paneStates": {
                "10": { "type": "attached", "sessionId": 100 },
                "11": { "type": "attached", "sessionId": 101 }
            }
        });

        let rebuilt = rebuild_from_snapshot(&state, 80, 24).expect("snapshot must rebuild");
        let tab = &rebuilt.tabs[0];
        let focused = tab.layout_tree.focus();
        let session = tab
            .pane_sessions
            .get(&focused)
            .expect("focused pane should map to a session");

        assert_eq!(session.pane_path, "focus-restore-test/main/bottom");
        assert_eq!(session.session_id, "101");
    }

    #[test]
    fn rebuild_snapshot_restores_multi_pane_layout_identity_and_buffers() {
        let left_history = base64_encode(b"left-history-1\r\nleft-history-2\r\n");
        let left_visible = base64_encode(b"\x1b[2J\x1b[Hleft-visible-tail\r\n");
        let right_visible = base64_encode(b"right-startup-burst\r\nright-ready\r\n");
        let state = json!({
            "name": "restore-suite",
            "tabs": [{
                "name": "agents",
                "focus": "tests",
                "layout": {
                    "type": "split",
                    "orientation": "horizontal",
                    "ratio": 0.45,
                    "children": [
                        {
                            "type": "pane",
                            "name": "server"
                        },
                        {
                            "type": "pane",
                            "name": "tests"
                        }
                    ]
                },
                "panes": [7, 8]
            }],
            "paneStates": {
                "7": { "type": "attached", "sessionId": "left-session" },
                "8": { "type": "attached", "sessionId": "right-session" }
            },
            "sessionSizes": {
                "left-session": { "cols": 100, "rows": 3 },
                "right-session": { "cols": 100, "rows": 3 }
            },
            "sessionHistory": {
                "left-session": {
                    "scrollbackRows": 2,
                    "scrollbackVt": left_history
                }
            },
            "sessionScreens": {
                "left-session": left_visible,
                "right-session": right_visible
            }
        });

        let rebuilt = rebuild_from_snapshot(&state, 80, 24).expect("snapshot must rebuild");
        let tab = &rebuilt.tabs[0];
        let pane_paths = tab
            .pane_sessions
            .values()
            .map(|session| session.pane_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(tab.layout_tree.panes().len(), 2);
        assert!(pane_paths.contains(&"restore-suite/agents/server"));
        assert!(pane_paths.contains(&"restore-suite/agents/tests"));

        let focused = tab.layout_tree.focus();
        let focused_session = tab
            .pane_sessions
            .get(&focused)
            .expect("focused pane should map to a session");
        assert_eq!(focused_session.pane_path, "restore-suite/agents/tests");

        let server_pane = tab
            .pane_sessions
            .iter()
            .find_map(|(pane_id, session)| {
                (session.pane_path == "restore-suite/agents/server").then_some(pane_id)
            })
            .expect("server pane should exist");
        let server_screen = tab
            .screens
            .get(server_pane)
            .expect("server pane should have a screen");
        assert!(
            server_screen.visible_text().contains("left-visible-tail"),
            "server visible buffer should be replayed; got:\n{}",
            server_screen.visible_text()
        );
        let oldest = server_screen
            .scrollback_row(0)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end_matches(' ')
                    .to_string()
            })
            .expect("server scrollback should be replayed");
        assert_eq!(oldest, "left-history-1");

        let tests_screen = tab
            .screens
            .get(&focused)
            .expect("tests pane should have a screen");
        assert!(
            tests_screen.visible_text().contains("right-ready"),
            "tests startup burst should be replayed; got:\n{}",
            tests_screen.visible_text()
        );
    }
}
