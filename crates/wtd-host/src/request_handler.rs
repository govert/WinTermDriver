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
use wtd_core::target::TargetPath;
use wtd_core::{find_workspace, list_workspaces, load_workspace_definition};

use wtd_ipc::message;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use crate::action::{v1_registry, ActionDispatcher};
use crate::ipc_server::{ClientId, RequestHandler};
use crate::output_broadcaster::BroadcastEvent;
use crate::target_resolver::{resolve_by_id, resolve_target};
use crate::terminal_input::encode_key_specs;
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

    /// Drain pending output from all sessions and collect broadcast events.
    ///
    /// Called periodically by the output broadcaster. Returns events for:
    /// - `Output`: raw VT bytes from ConPTY (caller base64-encodes)
    /// - `StateChanged`: session exited
    /// - `TitleChange`: VT title sequence detected
    ///
    /// `prev_titles` tracks the last-seen title per session for change detection.
    pub fn drain_session_events(
        &self,
        prev_titles: &mut HashMap<String, String>,
    ) -> Vec<BroadcastEvent> {
        let mut state = self.state.lock().unwrap();
        let mut events = Vec::new();

        for inst in state.workspaces.values_mut() {
            for (session_id, session) in inst.sessions_mut().iter_mut() {
                let sid = format!("{}", session_id.0);

                // Drain output and feed to screen buffer.
                let raw_bytes = session.process_pending_output_collecting();
                if !raw_bytes.is_empty() {
                    events.push(BroadcastEvent::Output {
                        session_id: sid.clone(),
                        data: raw_bytes,
                    });
                }

                // Detect title changes (screen buffer is up-to-date after drain).
                let new_title = session.screen().title.clone();
                let title_changed = match prev_titles.get(&sid) {
                    Some(old) => *old != new_title,
                    None => !new_title.is_empty(),
                };
                if title_changed {
                    prev_titles.insert(sid.clone(), new_title.clone());
                    events.push(BroadcastEvent::TitleChange {
                        session_id: sid.clone(),
                        title: new_title,
                    });
                }

                // Detect session exit.
                if let Some(exit_code) = session.check_exit() {
                    events.push(BroadcastEvent::StateChanged {
                        session_id: sid,
                        new_state: "exited".to_string(),
                        exit_code: Some(exit_code as i32),
                    });
                }
            }
        }

        events
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

fn request_cwd(envelope: &Envelope) -> std::path::PathBuf {
    envelope
        .payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        })
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

fn resolve_pane_for_resize<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &'a str,
) -> Option<(String, PaneId)> {
    if let Some((workspace_name, pane_id)) = resolve_invoke_action_target(workspaces, target) {
        if workspaces.contains_key(&workspace_name) {
            return Some((workspace_name, pane_id));
        }
    }
    for (workspace_name, inst) in workspaces {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((workspace_name.clone(), pane_id));
        }
    }
    None
}

fn workspace_name_for_instance_id(
    workspaces: &HashMap<String, WorkspaceInstance>,
    instance_id: &WorkspaceInstanceId,
) -> Option<String> {
    workspaces
        .iter()
        .find(|(_, inst)| inst.id() == instance_id)
        .map(|(name, _)| name.clone())
}

fn resolve_invoke_action_target(
    workspaces: &HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(String, PaneId)> {
    let instances: Vec<&WorkspaceInstance> = workspaces.values().collect();

    if let Ok(resolved) = resolve_by_id(target, &instances) {
        if let Some(workspace_name) =
            workspace_name_for_instance_id(workspaces, &resolved.instance_id)
        {
            return Some((workspace_name, resolved.pane_id));
        }
    }

    if let Ok(path) = TargetPath::parse(target) {
        if let Ok(resolved) = resolve_target(&path, &instances) {
            if let Some(workspace_name) =
                workspace_name_for_instance_id(workspaces, &resolved.instance_id)
            {
                return Some((workspace_name, resolved.pane_id));
            }
        }
    }

    for (workspace_name, inst) in workspaces {
        if let Some(pane_id) = inst.find_pane_by_name(target) {
            return Some((workspace_name.clone(), pane_id));
        }
    }

    None
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
    cwd: &std::path::Path,
) -> Result<wtd_core::workspace::WorkspaceDefinition, Envelope> {
    let explicit = file.map(|f| std::path::PathBuf::from(f));

    let discovered = find_workspace(name, explicit.as_deref(), cwd).map_err(|e| {
        error_envelope(
            "",
            ErrorCode::WorkspaceNotFound,
            &format!("workspace '{}' not found: {}", name, e),
        )
    })?;

    let content = std::fs::read_to_string(&discovered.path).map_err(|e| {
        error_envelope(
            "",
            ErrorCode::WorkspaceNotFound,
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
            ErrorCode::DefinitionError,
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
            TypedMessage::OpenWorkspace(open) => self.handle_open_workspace(envelope, open),

            TypedMessage::CloseWorkspace(close) => self.handle_close_workspace(&envelope.id, close),

            TypedMessage::AttachWorkspace(attach) => {
                self.handle_attach_workspace(&envelope.id, attach)
            }

            TypedMessage::RecreateWorkspace(recreate) => {
                self.handle_recreate_workspace(envelope, recreate)
            }

            TypedMessage::SaveWorkspace(save) => self.handle_save_workspace(&envelope.id, save),

            TypedMessage::ListWorkspaces(_) => self.handle_list_workspaces(envelope),

            TypedMessage::ListInstances(_) => self.handle_list_instances(&envelope.id),

            TypedMessage::ListPanes(lp) => self.handle_list_panes(&envelope.id, lp),

            TypedMessage::ListSessions(ls) => self.handle_list_sessions(&envelope.id, ls),

            TypedMessage::Send(send) => self.handle_send(&envelope.id, send),

            TypedMessage::Keys(keys) => self.handle_keys(&envelope.id, keys),

            TypedMessage::PaneInput(input) => self.handle_pane_input(&envelope.id, input),

            TypedMessage::Capture(capture) => self.handle_capture(&envelope.id, capture),

            TypedMessage::Scrollback(scrollback) => {
                self.handle_scrollback(&envelope.id, scrollback)
            }

            TypedMessage::Follow(follow) => self.handle_follow(&envelope.id, follow),

            TypedMessage::Inspect(inspect) => self.handle_inspect(&envelope.id, inspect),

            TypedMessage::InvokeAction(action) => self.handle_invoke_action(&envelope.id, action),

            TypedMessage::SessionInput(input) => {
                self.handle_session_input(input);
                None // fire-and-forget
            }

            TypedMessage::PaneResize(pane_resize) => {
                self.handle_pane_resize(&envelope.id, pane_resize)
            }

            TypedMessage::FocusPane(focus) => self.handle_focus_pane(&envelope.id, focus),

            TypedMessage::RenamePane(rename) => self.handle_rename_pane(&envelope.id, rename),

            _ => None,
        }
    }
}

// ── Individual handlers ──────────────────────────────────────────────────

impl HostRequestHandler {
    fn handle_open_workspace(&self, envelope: &Envelope, open: &OpenWorkspace) -> Option<Envelope> {
        let id = &envelope.id;
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
        let def = match load_workspace_from_disk(
            &open.name,
            open.file.as_deref(),
            &request_cwd(envelope),
        ) {
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
        let mut state = self.state.lock().unwrap();
        if !state.workspaces.contains_key(&attach.workspace) {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", attach.workspace),
            ));
        }
        // Drain any buffered output so the snapshot captures the latest screen state.
        if let Some(inst) = state.workspaces.get_mut(&attach.workspace) {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
        }
        let inst = state.workspaces.get(&attach.workspace).unwrap();
        let snapshot = inst.attach_snapshot();
        let state_value = serde_json::to_value(&snapshot)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
        Some(Envelope::new(
            id,
            &AttachWorkspaceResult { state: state_value },
        ))
    }

    fn handle_recreate_workspace(
        &self,
        envelope: &Envelope,
        recreate: &RecreateWorkspace,
    ) -> Option<Envelope> {
        let id = &envelope.id;
        let mut state = self.state.lock().unwrap();

        if !state.workspaces.contains_key(&recreate.workspace) {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", recreate.workspace),
            ));
        }

        let def = match load_workspace_from_disk(&recreate.workspace, None, &request_cwd(envelope))
        {
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
                let def = inst.save();

                let path = if let Some(ref file) = save.file {
                    std::path::PathBuf::from(file)
                } else {
                    let dir = match wtd_core::ensure_user_workspaces_dir() {
                        Ok(d) => d,
                        Err(e) => {
                            return Some(error_envelope(
                                id,
                                ErrorCode::InternalError,
                                &format!("failed to create workspaces directory: {}", e),
                            ));
                        }
                    };
                    dir.join(format!("{}.yaml", save.workspace))
                };

                let yaml = match serde_yaml::to_string(&def) {
                    Ok(y) => y,
                    Err(e) => {
                        return Some(error_envelope(
                            id,
                            ErrorCode::InternalError,
                            &format!("failed to serialize workspace: {}", e),
                        ));
                    }
                };

                if let Err(e) = std::fs::write(&path, &yaml) {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InternalError,
                        &format!("failed to write {}: {}", path.display(), e),
                    ));
                }

                Some(Envelope::new(id, &OkResponse {}))
            }
            None => Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", save.workspace),
            )),
        }
    }

    fn handle_list_workspaces(&self, envelope: &Envelope) -> Option<Envelope> {
        let discovered = list_workspaces(&request_cwd(envelope));

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

        Some(Envelope::new(
            &envelope.id,
            &ListWorkspacesResult { workspaces },
        ))
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

        let encoded = match encode_key_specs(&keys.keys) {
            Ok(encoded) => encoded,
            Err(e) => {
                return Some(error_envelope(
                    id,
                    ErrorCode::InvalidArgument,
                    &format!("invalid key spec: {}", e),
                ));
            }
        };

        if let Err(e) = session.write_input(&encoded) {
            return Some(error_envelope(
                id,
                ErrorCode::SessionFailed,
                &format!("write failed: {}", e),
            ));
        }

        Some(Envelope::new(id, &OkResponse {}))
    }

    fn handle_pane_input(&self, id: &str, input: &PaneInput) -> Option<Envelope> {
        let state = self.state.lock().unwrap();
        let (inst, pane_id) = match find_pane(&state.workspaces, &input.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", input.target),
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

        let bytes = match decode_base64(&input.data) {
            Some(bytes) => bytes,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::InvalidArgument,
                    "input data is not valid base64",
                ));
            }
        };

        if let Err(e) = session.write_input(&bytes) {
            return Some(error_envelope(
                id,
                ErrorCode::SessionFailed,
                &format!("write failed: {}", e),
            ));
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

        // Get the session's screen buffer and run capture_extended.
        let result = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => {
                match inst.session(session_id) {
                    Some(session) => {
                        // Compile anchor regex if provided.
                        let compiled_regex = match &capture.after_regex {
                            Some(pattern) => match regex::Regex::new(pattern) {
                                Ok(re) => Some(re),
                                Err(e) => {
                                    return Some(error_envelope(
                                        id,
                                        ErrorCode::InvalidArgument,
                                        &format!("invalid after_regex '{}': {}", pattern, e),
                                    ));
                                }
                            },
                            None => None,
                        };

                        let screen = session.screen();
                        let ext = screen.capture_extended(
                            capture.lines,
                            capture.all.unwrap_or(false),
                            capture.after.as_deref(),
                            compiled_regex.as_ref(),
                            capture.max_lines,
                            capture.count.unwrap_or(false),
                        );
                        CaptureResult {
                            text: ext.text,
                            lines: ext.lines,
                            total_lines: ext.total_lines,
                            anchor_found: ext.anchor_found,
                            cursor: Some(ext.cursor),
                        }
                    }
                    None => CaptureResult {
                        text: String::new(),
                        lines: 0,
                        total_lines: 0,
                        anchor_found: None,
                        cursor: None,
                    },
                }
            }
            _ => CaptureResult {
                text: String::new(),
                lines: 0,
                total_lines: 0,
                anchor_found: None,
                cursor: None,
            },
        };

        Some(Envelope::new(id, &result))
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

        // Resolve the pane context up front. UI callers send pane ids here,
        // while other clients may send canonical workspace/tab/pane paths or
        // bare pane names.
        let (workspace_name, target_pane) = if let Some(ref target) = action.target_pane_id {
            match resolve_invoke_action_target(&state.workspaces, target) {
                Some((workspace_name, pane_id)) => (Some(workspace_name), Some(pane_id)),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", target),
                    ));
                }
            }
        } else {
            (state.workspaces.keys().next().cloned(), None)
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

        let settings = state.settings.clone();
        let inst = state.workspaces.get_mut(&workspace_name).unwrap();

        // Handle tab lifecycle actions at this level (they need settings/env for session spawn).
        match action.action.as_str() {
            "new-tab" => {
                let name = action
                    .args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("tab-{}", inst.tabs().len() + 1));
                let tab = inst.add_tab(name);
                let pane_id = tab.layout().focus();
                let pane_name = format!("pane-{}", pane_id.0);
                let env = host_env();
                inst.spawn_session_for_pane(&pane_id, pane_name, &settings, &env, &find_exe);
                return Some(Envelope::new(
                    id,
                    &InvokeActionResult {
                        result: "tab-created".to_string(),
                        pane_id: None,
                    },
                ));
            }
            "next-tab" => {
                let count = inst.tabs().len();
                if count == 0 {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InvalidAction,
                        "no tabs available",
                    ));
                }
                let next = (inst.active_tab_index() + 1) % count;
                inst.set_active_tab(next);
                return Some(Envelope::new(
                    id,
                    &InvokeActionResult {
                        result: "tab-switched".to_string(),
                        pane_id: None,
                    },
                ));
            }
            "prev-tab" => {
                let count = inst.tabs().len();
                if count == 0 {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InvalidAction,
                        "no tabs available",
                    ));
                }
                let prev = if inst.active_tab_index() == 0 {
                    count - 1
                } else {
                    inst.active_tab_index() - 1
                };
                inst.set_active_tab(prev);
                return Some(Envelope::new(
                    id,
                    &InvokeActionResult {
                        result: "tab-switched".to_string(),
                        pane_id: None,
                    },
                ));
            }
            "goto-tab" => {
                if let Some(tab_index) = action
                    .args
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                {
                    if tab_index < inst.tabs().len() {
                        inst.set_active_tab(tab_index);
                        return Some(Envelope::new(
                            id,
                            &InvokeActionResult {
                                result: "tab-switched".to_string(),
                                pane_id: None,
                            },
                        ));
                    }
                }

                if let Some(name) = action.args.get("name").and_then(|v| v.as_str()) {
                    if let Some(index) = inst.tabs().iter().position(|tab| tab.name() == name) {
                        inst.set_active_tab(index);
                        return Some(Envelope::new(
                            id,
                            &InvokeActionResult {
                                result: "tab-switched".to_string(),
                                pane_id: None,
                            },
                        ));
                    }
                }

                return Some(error_envelope(
                    id,
                    ErrorCode::InvalidArgument,
                    "invalid goto-tab target",
                ));
            }
            "close-tab" => {
                let idx = inst.active_tab_index();
                if inst.tabs().len() > 1 {
                    inst.close_tab(idx);
                    return Some(Envelope::new(
                        id,
                        &InvokeActionResult {
                            result: "tab-closed".to_string(),
                            pane_id: None,
                        },
                    ));
                } else {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InvalidAction,
                        "cannot close the last tab",
                    ));
                }
            }
            _ => {} // Fall through to dispatcher
        }

        let (width, height) = inst.estimated_viewport_size().unwrap_or((120, 40));
        let registry = v1_registry();
        let dispatcher = ActionDispatcher::new(
            registry,
            Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
        );

        match dispatcher.dispatch(inst, &action.action, &action.args, target_pane) {
            Ok(result) => {
                use crate::action::ActionResult;

                match result {
                    ActionResult::PaneCreated { pane_id } => {
                        // Spawn a session for the newly created pane.
                        let pane_name = format!("pane-{}", pane_id.0);
                        let env = host_env();
                        inst.spawn_session_for_pane(
                            &pane_id, pane_name, &settings, &env, &find_exe,
                        );
                        Some(Envelope::new(
                            id,
                            &InvokeActionResult {
                                result: "pane-created".to_string(),
                                pane_id: Some(format!("{}", pane_id.0)),
                            },
                        ))
                    }
                    ActionResult::PaneClosed { pane_id, .. } => Some(Envelope::new(
                        id,
                        &InvokeActionResult {
                            result: "pane-closed".to_string(),
                            pane_id: Some(format!("{}", pane_id.0)),
                        },
                    )),
                    ActionResult::Ok => Some(Envelope::new(
                        id,
                        &InvokeActionResult {
                            result: "ok".to_string(),
                            pane_id: None,
                        },
                    )),
                }
            }
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

    fn handle_pane_resize(&self, id: &str, resize: &PaneResize) -> Option<Envelope> {
        if resize.cols == 0 || resize.rows == 0 {
            return Some(error_envelope(
                id,
                ErrorCode::InvalidArgument,
                "pane size must be greater than zero",
            ));
        }

        let mut state = self.state.lock().unwrap();
        let (workspace_name, pane_id) =
            match resolve_pane_for_resize(&state.workspaces, &resize.pane_id) {
                Some((name, pane_id)) => (name, pane_id),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", resize.pane_id),
                    ));
                }
            };

        let inst = state.workspaces.get_mut(&workspace_name).unwrap();
        match inst.resize_pane_session(&pane_id, resize.cols, resize.rows) {
            Ok(()) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => {
                let code = match e {
                    crate::workspace_instance::WorkspaceError::SessionOperation(_) => {
                        ErrorCode::SessionFailed
                    }
                    crate::workspace_instance::WorkspaceError::PaneNotFound(_) => {
                        ErrorCode::TargetNotFound
                    }
                    _ => ErrorCode::InternalError,
                };
                Some(error_envelope(id, code, &format!("{e}")))
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

    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
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
