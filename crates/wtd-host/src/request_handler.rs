//! Real host request handler (§8.1, §13.9–13.13).
//!
//! Replaces the `StubHandler` — dispatches all IPC request types to
//! workspace instances, sessions, and the action system.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use wtd_core::global_settings::GlobalSettings;
use wtd_core::ids::{PaneId, SessionId, WorkspaceInstanceId};
use wtd_core::layout::Rect;
use wtd_core::{load_workspace_definition, find_workspace, list_workspaces};

use wtd_ipc::message;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use crate::action::{v1_registry, ActionDispatcher};
use crate::ipc_server::{ClientId, RequestHandler};
use crate::workspace_instance::{PaneState, WorkspaceInstance};

// ── Internal state ────────────────────────────────────────────────────────

struct HostState {
    workspaces: HashMap<String, WorkspaceInstance>,
    settings: GlobalSettings,
    next_instance_id: u64,
}

// ── HostRequestHandler ───────────────────────────────────────────────────

/// Real request handler for the host process.
///
/// Owns all workspace instances and dispatches IPC messages to the
/// appropriate subsystem.
pub struct HostRequestHandler {
    state: Mutex<HostState>,
}

impl HostRequestHandler {
    /// Create a new handler with the given global settings.
    pub fn new(settings: GlobalSettings) -> Self {
        Self {
            state: Mutex::new(HostState {
                workspaces: HashMap::new(),
                settings,
                next_instance_id: 1,
            }),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn host_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(val) = std::env::var("USERPROFILE") {
        env.insert("USERPROFILE".to_string(), val);
    } else {
        env.insert("USERPROFILE".to_string(), r"C:\".to_string());
    }
    env
}

fn find_exe(name: &str) -> bool {
    matches!(
        name,
        "cmd.exe" | "powershell.exe" | "pwsh.exe" | "wsl.exe" | "ssh.exe"
    )
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

/// Find a pane by name across all open workspaces.
fn find_pane<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    for inst in workspaces.values() {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((inst, pane_id));
        }
    }
    None
}

/// Get visible text for a pane's session.
fn get_pane_screen_text(inst: &WorkspaceInstance, pane_id: &PaneId) -> String {
    match inst.pane_state(pane_id) {
        Some(PaneState::Attached { session_id }) => inst
            .session(session_id)
            .map(|s| s.screen().visible_text())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Get scrollback lines for a pane's session.
fn get_pane_scrollback(inst: &WorkspaceInstance, pane_id: &PaneId, tail: u32) -> Vec<String> {
    match inst.pane_state(pane_id) {
        Some(PaneState::Attached { session_id }) => {
            let screen = match inst.session(session_id) {
                Some(s) => s.screen(),
                None => return Vec::new(),
            };
            let total = screen.scrollback_len();
            let start = total.saturating_sub(tail as usize);
            (start..total)
                .filter_map(|idx| {
                    screen.scrollback_row(idx).map(|cells| {
                        cells
                            .iter()
                            .filter(|c| !c.wide_continuation)
                            .map(|c| c.character)
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Load a workspace definition from disk by name (with optional explicit file).
fn load_workspace_from_disk(
    name: &str,
    file: Option<&str>,
) -> Result<wtd_core::workspace::WorkspaceDefinition, Envelope> {
    let explicit = file.map(|f| std::path::PathBuf::from(f));
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let discovered = find_workspace(
        name,
        explicit.as_deref(),
        &cwd,
    )
    .map_err(|e| {
        error_envelope(
            "",
            ErrorCode::WorkspaceNotFound,
            &format!("workspace '{}' not found: {}", name, e),
        )
    })?;

    let content = std::fs::read_to_string(&discovered.path).map_err(|e| {
        error_envelope(
            "",
            ErrorCode::InternalError,
            &format!("failed to read {}: {}", discovered.path.display(), e),
        )
    })?;

    let file_name = discovered
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace.yaml".to_string());

    load_workspace_definition(&file_name, &content).map_err(|e| {
        error_envelope(
            "",
            ErrorCode::InternalError,
            &format!("failed to parse workspace: {}", e),
        )
    })
}

// ── RequestHandler impl ──────────────────────────────────────────────────

impl RequestHandler for HostRequestHandler {
    fn handle_request(
        &self,
        _client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope> {
        match msg {
            TypedMessage::OpenWorkspace(open) => {
                self.handle_open_workspace(&envelope.id, open)
            }

            TypedMessage::CloseWorkspace(close) => {
                self.handle_close_workspace(&envelope.id, close)
            }

            TypedMessage::AttachWorkspace(attach) => {
                self.handle_attach_workspace(&envelope.id, attach)
            }

            TypedMessage::RecreateWorkspace(recreate) => {
                self.handle_recreate_workspace(&envelope.id, recreate)
            }

            TypedMessage::SaveWorkspace(save) => {
                self.handle_save_workspace(&envelope.id, save)
            }

            TypedMessage::ListWorkspaces(_) => {
                self.handle_list_workspaces(&envelope.id)
            }

            TypedMessage::ListInstances(_) => {
                self.handle_list_instances(&envelope.id)
            }

            TypedMessage::ListPanes(lp) => {
                self.handle_list_panes(&envelope.id, lp)
            }

            TypedMessage::ListSessions(ls) => {
                self.handle_list_sessions(&envelope.id, ls)
            }

            TypedMessage::Send(send) => {
                self.handle_send(&envelope.id, send)
            }

            TypedMessage::Keys(keys) => {
                self.handle_keys(&envelope.id, keys)
            }

            TypedMessage::Capture(capture) => {
                self.handle_capture(&envelope.id, capture)
            }

            TypedMessage::Scrollback(scrollback) => {
                self.handle_scrollback(&envelope.id, scrollback)
            }

            TypedMessage::Follow(follow) => {
                self.handle_follow(&envelope.id, follow)
            }

            TypedMessage::Inspect(inspect) => {
                self.handle_inspect(&envelope.id, inspect)
            }

            TypedMessage::InvokeAction(action) => {
                self.handle_invoke_action(&envelope.id, action)
            }

            TypedMessage::SessionInput(input) => {
                self.handle_session_input(input);
                None // fire-and-forget
            }

            TypedMessage::FocusPane(focus) => {
                self.handle_focus_pane(&envelope.id, focus)
            }

            TypedMessage::RenamePane(rename) => {
                self.handle_rename_pane(&envelope.id, rename)
            }

            _ => None,
        }
    }
}

// ── Individual handlers ──────────────────────────────────────────────────

impl HostRequestHandler {
    fn handle_open_workspace(&self, id: &str, open: &OpenWorkspace) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();

        // Check if already open (and not requesting recreate)
        if !open.recreate {
            if let Some(inst) = state.workspaces.get(&open.name) {
                return Some(Envelope::new(
                    id,
                    &OpenWorkspaceResult {
                        instance_id: format!("{}", inst.id().0),
                        state: Value::Object(serde_json::Map::new()),
                    },
                ));
            }
        }

        // Load workspace definition
        let def = match load_workspace_from_disk(&open.name, open.file.as_deref()) {
            Ok(d) => d,
            Err(mut e) => {
                e.id = id.to_string();
                return Some(e);
            }
        };

        // If recreating, close existing first
        if open.recreate {
            if let Some(mut existing) = state.workspaces.remove(&open.name) {
                existing.close();
            }
        }

        let inst_id = state.next_instance_id;
        state.next_instance_id += 1;

        let env = host_env();
        let inst = match WorkspaceInstance::open(
            WorkspaceInstanceId(inst_id),
            &def,
            &state.settings,
            &env,
            find_exe,
        ) {
            Ok(i) => i,
            Err(e) => {
                return Some(error_envelope(
                    id,
                    ErrorCode::InternalError,
                    &format!("failed to open workspace: {}", e),
                ));
            }
        };

        let instance_id = format!("{}", inst.id().0);
        state.workspaces.insert(open.name.clone(), inst);

        Some(Envelope::new(
            id,
            &OpenWorkspaceResult {
                instance_id,
                state: Value::Object(serde_json::Map::new()),
            },
        ))
    }

    fn handle_close_workspace(&self, id: &str, close: &CloseWorkspace) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();
        match state.workspaces.remove(&close.workspace) {
            Some(mut inst) => {
                inst.close();
                Some(Envelope::new(id, &OkResponse {}))
            }
            None => Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", close.workspace),
            )),
        }
    }

    fn handle_attach_workspace(&self, id: &str, attach: &AttachWorkspace) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        match state.workspaces.get(&attach.workspace) {
            Some(_inst) => {
                // Full state population deferred to gp6.3
                Some(Envelope::new(
                    id,
                    &AttachWorkspaceResult {
                        state: Value::Object(serde_json::Map::new()),
                    },
                ))
            }
            None => Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", attach.workspace),
            )),
        }
    }

    fn handle_recreate_workspace(
        &self,
        id: &str,
        recreate: &RecreateWorkspace,
    ) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();

        if !state.workspaces.contains_key(&recreate.workspace) {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", recreate.workspace),
            ));
        }

        let def = match load_workspace_from_disk(&recreate.workspace, None) {
            Ok(d) => d,
            Err(mut e) => {
                e.id = id.to_string();
                return Some(e);
            }
        };

        let settings = state.settings.clone();
        let env = host_env();
        let inst = state.workspaces.get_mut(&recreate.workspace).unwrap();

        match inst.recreate(&def, &settings, &env, find_exe) {
            Ok(()) => Some(Envelope::new(
                id,
                &RecreateWorkspaceResult {
                    instance_id: format!("{}", inst.id().0),
                    state: Value::Object(serde_json::Map::new()),
                },
            )),
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::InternalError,
                &format!("recreate failed: {}", e),
            )),
        }
    }

    fn handle_save_workspace(&self, id: &str, save: &SaveWorkspace) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        match state.workspaces.get(&save.workspace) {
            Some(inst) => {
                let _def = inst.save();
                // File writing deferred to gp6.4
                Some(Envelope::new(id, &OkResponse {}))
            }
            None => Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", save.workspace),
            )),
        }
    }

    fn handle_list_workspaces(&self, id: &str) -> Option<Envelope> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let discovered = list_workspaces(&cwd);

        let state = self.state.lock().unwrap();

        // Merge discovered definitions with running instances
        let mut workspaces: Vec<WorkspaceInfo> = discovered
            .into_iter()
            .map(|d| WorkspaceInfo {
                name: d.name,
                source: format!("{:?}", d.source).to_lowercase(),
            })
            .collect();

        // Add running instances not in discovery
        for name in state.workspaces.keys() {
            if !workspaces.iter().any(|w| w.name == *name) {
                workspaces.push(WorkspaceInfo {
                    name: name.clone(),
                    source: "running".to_string(),
                });
            }
        }

        Some(Envelope::new(id, &ListWorkspacesResult { workspaces }))
    }

    fn handle_list_instances(&self, id: &str) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let instances: Vec<InstanceInfo> = state
            .workspaces
            .iter()
            .map(|(name, inst)| InstanceInfo {
                name: name.clone(),
                instance_id: format!("{}", inst.id().0),
            })
            .collect();
        Some(Envelope::new(id, &ListInstancesResult { instances }))
    }

    fn handle_list_panes(&self, id: &str, lp: &ListPanes) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let inst = match state.workspaces.get(&lp.workspace) {
            Some(i) => i,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::WorkspaceNotFound,
                    &format!("workspace '{}' not found", lp.workspace),
                ));
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
                        format!("detached: {}", error)
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

        Some(Envelope::new(id, &ListPanesResult { panes }))
    }

    fn handle_list_sessions(&self, id: &str, ls: &ListSessions) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let inst = match state.workspaces.get(&ls.workspace) {
            Some(i) => i,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::WorkspaceNotFound,
                    &format!("workspace '{}' not found", ls.workspace),
                ));
            }
        };

        let sessions: Vec<SessionInfo> = inst
            .sessions()
            .iter()
            .map(|(sid, session)| SessionInfo {
                session_id: format!("{}", sid.0),
                pane: session.name().to_string(),
                state: format!("{:?}", session.state()),
            })
            .collect();

        Some(Envelope::new(id, &ListSessionsResult { sessions }))
    }

    fn handle_send(&self, id: &str, send: &message::Send) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let (inst, pane_id) = match find_pane(&state.workspaces, &send.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", send.target),
                ));
            }
        };

        let session_id = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => session_id.clone(),
            _ => {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    "pane not attached",
                ));
            }
        };

        let session = match inst.session(&session_id) {
            Some(s) => s,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    "session not found",
                ));
            }
        };

        let mut input = send.text.clone();
        if send.newline {
            input.push_str("\r\n");
        }

        match session.write_input(input.as_bytes()) {
            Ok(()) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::SessionFailed,
                &format!("write failed: {}", e),
            )),
        }
    }

    fn handle_keys(&self, id: &str, keys: &Keys) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let (inst, pane_id) = match find_pane(&state.workspaces, &keys.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", keys.target),
                ));
            }
        };

        let session_id = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => session_id.clone(),
            _ => {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    "pane not attached",
                ));
            }
        };

        let session = match inst.session(&session_id) {
            Some(s) => s,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    "session not found",
                ));
            }
        };

        // Send each key spec as raw text
        for key in &keys.keys {
            if let Err(e) = session.write_input(key.as_bytes()) {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    &format!("write failed: {}", e),
                ));
            }
        }

        Some(Envelope::new(id, &OkResponse {}))
    }

    fn handle_capture(&self, id: &str, capture: &Capture) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();

        // Drain pending output from all sessions
        for inst in state.workspaces.values_mut() {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
        }

        let (inst, pane_id) = match find_pane(&state.workspaces, &capture.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", capture.target),
                ));
            }
        };

        let text = get_pane_screen_text(inst, &pane_id);
        Some(Envelope::new(id, &CaptureResult { text }))
    }

    fn handle_scrollback(&self, id: &str, scrollback: &Scrollback) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();

        // Drain pending output
        for inst in state.workspaces.values_mut() {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
        }

        let (inst, pane_id) = match find_pane(&state.workspaces, &scrollback.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", scrollback.target),
                ));
            }
        };

        let lines = get_pane_scrollback(inst, &pane_id, scrollback.tail);
        Some(Envelope::new(id, &ScrollbackResult { lines }))
    }

    fn handle_follow(&self, id: &str, follow: &Follow) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        match find_pane(&state.workspaces, &follow.target) {
            Some(_) => Some(Envelope::new(id, &OkResponse {})),
            None => Some(error_envelope(
                id,
                ErrorCode::TargetNotFound,
                &format!("pane '{}' not found", follow.target),
            )),
        }
    }

    fn handle_inspect(&self, id: &str, inspect: &Inspect) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let (inst, pane_id) = match find_pane(&state.workspaces, &inspect.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", inspect.target),
                ));
            }
        };

        let pane_name = inst.pane_name(&pane_id).unwrap_or("?");
        let session_state = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => inst
                .session(session_id)
                .map(|s| format!("{:?}", s.state()))
                .unwrap_or_else(|| "unknown".into()),
            Some(PaneState::Detached { error }) => format!("detached: {}", error),
            None => "none".into(),
        };

        let data = serde_json::json!({
            "paneName": pane_name,
            "paneId": format!("{}", pane_id),
            "sessionState": session_state,
            "workspace": inst.name(),
        });

        Some(Envelope::new(id, &InspectResult { data }))
    }

    fn handle_invoke_action(&self, id: &str, action: &InvokeAction) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();

        // Find which workspace to dispatch to.
        // Use target_pane_id to identify workspace if provided, otherwise use first workspace.
        let workspace_name = if let Some(ref target) = action.target_pane_id {
            state
                .workspaces
                .iter()
                .find(|(_, inst)| inst.find_pane_by_name(target).is_some())
                .map(|(name, _)| name.clone())
        } else {
            state.workspaces.keys().next().cloned()
        };

        let workspace_name = match workspace_name {
            Some(n) => n,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::WorkspaceNotFound,
                    "no workspace open",
                ));
            }
        };

        let inst = state.workspaces.get_mut(&workspace_name).unwrap();

        // Resolve target pane id
        let target_pane = action
            .target_pane_id
            .as_ref()
            .and_then(|name| inst.find_pane_by_name(name));

        let registry = v1_registry();
        let viewport = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let dispatcher = ActionDispatcher::new(registry, viewport);

        match dispatcher.dispatch(inst, &action.action, &action.args, target_pane) {
            Ok(_result) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => {
                let code = match &e {
                    crate::action::ActionError::UnknownAction(_) => ErrorCode::InvalidAction,
                    crate::action::ActionError::InvalidArgument { .. } => {
                        ErrorCode::InvalidArgument
                    }
                    crate::action::ActionError::PaneNotFound(_) => ErrorCode::TargetNotFound,
                    _ => ErrorCode::InternalError,
                };
                Some(error_envelope(id, code, &format!("{}", e)))
            }
        }
    }

    fn handle_session_input(&self, input: &SessionInput) {
        let state = self.state.lock().unwrap();
        let session_id = SessionId(input.session_id.parse::<u64>().unwrap_or(0));

        for inst in state.workspaces.values() {
            if let Some(session) = inst.session(&session_id) {
                // Decode base64 data
                if let Some(bytes) = decode_base64(&input.data) {
                    let _ = session.write_input(&bytes);
                }
                return;
            }
        }
    }

    fn handle_focus_pane(&self, id: &str, focus: &FocusPane) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();
        // Try to find the pane by name and set focus
        for inst in state.workspaces.values_mut() {
            if let Some(pane_id) = inst.find_pane_by_name(&focus.pane_id) {
                // Set focus in the tab's layout tree
                for tab in inst.tabs_mut() {
                    if tab.layout().panes().contains(&pane_id) {
                        let _ = tab.layout_mut().set_focus(pane_id);
                        return Some(Envelope::new(id, &OkResponse {}));
                    }
                }
            }
        }
        Some(error_envelope(
            id,
            ErrorCode::TargetNotFound,
            &format!("pane '{}' not found", focus.pane_id),
        ))
    }

    fn handle_rename_pane(&self, id: &str, rename: &RenamePane) -> Option<Envelope> {
        let mut state = self.state.lock().unwrap();
        for inst in state.workspaces.values_mut() {
            if let Some(pane_id) = inst.find_pane_by_name(&rename.pane_id) {
                inst.rename_pane(&pane_id, rename.new_name.clone());
                return Some(Envelope::new(id, &OkResponse {}));
            }
        }
        Some(error_envelope(
            id,
            ErrorCode::TargetNotFound,
            &format!("pane '{}' not found", rename.pane_id),
        ))
    }
}

// ── Base64 decode ────────────────────────────────────────────────────────

fn decode_base64(input: &str) -> Option<Vec<u8>> {
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

    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=' && b != b'\n' && b != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);

    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let v = DECODE[b as usize];
            if v == 0xFF {
                return None;
            }
            buf[i] = v;
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
    Some(out)
}
