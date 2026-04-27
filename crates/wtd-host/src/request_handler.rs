//! Real host request handler (§8.1, §13.9–13.13).
//!
//! Replaces the `StubHandler` — dispatches all IPC request types to
//! workspace instances, sessions, and the action system.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use wtd_core::global_settings::GlobalSettings;
use wtd_core::ids::{PaneId, SessionId, WorkspaceInstanceId};
use wtd_core::layout::Rect;
use wtd_core::target::TargetPath;
use wtd_core::workspace::{
    PaneDriverProfile, PaneLeaf, PaneNode, SessionLaunchDefinition, TabDefinition,
    WorkspaceDefinition,
};
use wtd_core::{find_workspace, list_workspaces, load_workspace_definition};

use wtd_ipc::message;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

use crate::action::{v1_registry, ActionDispatcher};
use crate::ipc_server::{ClientId, RequestHandler};
use crate::output_broadcaster::progress_info_from_screen;
use crate::output_broadcaster::BroadcastEvent;
use crate::prompt_driver::{
    build_prompt_input_plan, encode_send_input, pane_driver_definition_from_effective,
    resolve_pane_driver,
};
use crate::target_resolver::{resolve_by_id, resolve_target};
use crate::terminal_input::encode_key_specs;
use crate::workspace_instance::{PaneState, WorkspaceInstance};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VtMouseButton {
    Left,
    Middle,
    Right,
    None,
    WheelUp,
    WheelDown,
}

impl VtMouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::None => 3,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
        }
    }
}

// ── Internal state ────────────────────────────────────────────────────────

struct HostState {
    workspaces: HashMap<String, WorkspaceInstance>,
    settings: GlobalSettings,
    next_instance_id: u64,
    pending_broadcasts: Vec<BroadcastEvent>,
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
                pending_broadcasts: Vec::new(),
            }),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, HostState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::error!("host state mutex poisoned; recovering lock state");
                poisoned.into_inner()
            }
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
        prev_progress: &mut HashMap<String, Option<wtd_ipc::message::ProgressInfo>>,
    ) -> Vec<BroadcastEvent> {
        let mut state = self.lock_state();
        let mut events = Vec::new();
        events.append(&mut state.pending_broadcasts);

        for (workspace_name, inst) in state.workspaces.iter_mut() {
            let session_ids: Vec<SessionId> = inst.sessions().keys().cloned().collect();
            for session_id in session_ids {
                let sid = format!("{}", session_id.0);
                let scope_key = scoped_session_key(workspace_name, &sid);
                let pane_id = inst.pane_for_session(&session_id);

                let Some(session) = inst.session_mut(&session_id) else {
                    continue;
                };

                // Drain output and feed to screen buffer.
                let raw_bytes = session.process_pending_output_collecting();
                if !raw_bytes.is_empty() {
                    events.push(BroadcastEvent::Output {
                        workspace: workspace_name.clone(),
                        session_id: sid.clone(),
                        data: raw_bytes,
                    });
                }

                // Detect title changes (screen buffer is up-to-date after drain).
                let new_title = session.screen().title.clone();
                let title_changed = match prev_titles.get(&scope_key) {
                    Some(old) => *old != new_title,
                    None => !new_title.is_empty(),
                };
                if title_changed {
                    prev_titles.insert(scope_key.clone(), new_title.clone());
                    events.push(BroadcastEvent::TitleChange {
                        workspace: workspace_name.clone(),
                        session_id: sid.clone(),
                        title: new_title,
                    });
                }

                let new_progress = progress_info_from_screen(session.screen().progress());
                let progress_changed = prev_progress.get(&scope_key) != Some(&new_progress);
                if progress_changed {
                    prev_progress.insert(scope_key.clone(), new_progress.clone());
                    events.push(BroadcastEvent::ProgressChange {
                        workspace: workspace_name.clone(),
                        session_id: sid.clone(),
                        progress: new_progress,
                    });
                }

                // Detect session exit.
                if let Some(exit_code) = session.check_exit() {
                    events.push(BroadcastEvent::StateChanged {
                        workspace: workspace_name.clone(),
                        session_id: sid,
                        new_state: "exited".to_string(),
                        exit_code: Some(exit_code as i32),
                    });
                }

                let notifications = session.screen_mut().drain_notifications();
                if let Some(pane_id) = pane_id {
                    for notification in notifications {
                        let message = Some(notification.message);
                        let source = Some("osc".to_string());
                        if inst
                            .set_pane_attention(
                                &pane_id,
                                AttentionState::NeedsAttention,
                                message.clone(),
                                source.clone(),
                            )
                            .is_ok()
                        {
                            events.push(BroadcastEvent::AttentionChange {
                                workspace: workspace_name.clone(),
                                pane_id: Some(format!("{}", pane_id.0)),
                                state: AttentionState::NeedsAttention,
                                message,
                                source,
                            });
                        }
                    }
                }
            }
        }

        events
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn host_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.entry("USERPROFILE".to_string())
        .or_insert_with(|| r"C:\".to_string());
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

fn scoped_session_key(workspace: &str, session_id: &str) -> String {
    format!("{workspace}:{session_id}")
}

/// Find a pane by name across all open workspaces.
fn find_pane<'a>(
    workspaces: &'a HashMap<String, WorkspaceInstance>,
    target: &str,
) -> Option<(&'a WorkspaceInstance, PaneId)> {
    let (workspace_name, pane_id) = resolve_invoke_action_target(workspaces, target)?;
    workspaces.get(&workspace_name).map(|inst| (inst, pane_id))
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
                    screen
                        .scrollback_row(idx)
                        .map(|cells| wtd_pty::cells_to_string(cells).trim_end().to_string())
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn wait_signature(inst: &WorkspaceInstance, pane_id: &PaneId, recent_lines: u32) -> String {
    let attention = inst
        .pane_attention(pane_id)
        .and_then(|attention| serde_json::to_string(attention).ok())
        .unwrap_or_default();
    let metadata = inst
        .pane_metadata(pane_id)
        .and_then(|metadata| serde_json::to_string(metadata).ok())
        .unwrap_or_default();
    let recent_output = get_pane_scrollback(inst, pane_id, recent_lines).join("\n");
    format!("{attention}|{metadata}|{recent_output}")
}

fn wait_condition_matches(condition: WaitCondition, data: &Value) -> bool {
    let attention_state = data["attention"]["state"].as_str().unwrap_or("active");
    let metadata = &data["metadata"];
    let phase = metadata["phase"].as_str().unwrap_or_default();
    let completion = metadata["completion"].as_str().unwrap_or_default();
    let queue_pending = metadata["queuePending"].as_u64();

    match condition {
        WaitCondition::Idle => phase == "idle",
        WaitCondition::Done => {
            attention_state == "done" || phase == "done" || !completion.is_empty()
        }
        WaitCondition::NeedsAttention => attention_state == "needs_attention",
        WaitCondition::Error => attention_state == "error" || phase == "error",
        WaitCondition::QueueEmpty => queue_pending.unwrap_or(0) == 0,
        WaitCondition::StateChange => data["stateChanged"].as_bool().unwrap_or(false),
    }
}

fn mouse_mode_name(screen: &wtd_pty::ScreenBuffer) -> &'static str {
    match screen.mouse_mode() {
        wtd_pty::MouseMode::None => "none",
        wtd_pty::MouseMode::Normal => "normal",
        wtd_pty::MouseMode::ButtonEvent => "button-event",
        wtd_pty::MouseMode::AnyEvent => "any-event",
    }
}

fn cursor_shape_name(screen: &wtd_pty::ScreenBuffer) -> &'static str {
    match screen.cursor().shape {
        wtd_pty::CursorShape::Block => "block",
        wtd_pty::CursorShape::Underline => "underline",
        wtd_pty::CursorShape::Bar => "bar",
    }
}

fn encode_mouse_modifiers(shift: bool, alt: bool, ctrl: bool) -> u8 {
    let mut bits = 0u8;
    if shift {
        bits |= 4;
    }
    if alt {
        bits |= 8;
    }
    if ctrl {
        bits |= 16;
    }
    bits
}

fn encode_mouse_event(
    button: VtMouseButton,
    press: bool,
    col: usize,
    row: usize,
    modifier_bits: u8,
    sgr: bool,
) -> Vec<u8> {
    let cb = button.code() | modifier_bits;
    if sgr {
        let suffix = if press { 'M' } else { 'm' };
        format!("\x1b[<{};{};{}{}", cb, col + 1, row + 1, suffix).into_bytes()
    } else {
        let cb = if press { cb + 32 } else { 3 + 32 };
        let cx = ((col + 1) as u8).saturating_add(32);
        let cy = ((row + 1) as u8).saturating_add(32);
        vec![0x1b, b'[', b'M', cb, cx, cy]
    }
}

fn encode_mouse_motion(
    button: VtMouseButton,
    col: usize,
    row: usize,
    modifier_bits: u8,
    sgr: bool,
) -> Vec<u8> {
    let cb = button.code() | modifier_bits | 32;
    if sgr {
        format!("\x1b[<{};{};{}M", cb, col + 1, row + 1).into_bytes()
    } else {
        let cb = cb + 32;
        let cx = ((col + 1) as u8).saturating_add(32);
        let cy = ((row + 1) as u8).saturating_add(32);
        vec![0x1b, b'[', b'M', cb, cx, cy]
    }
}

fn map_mouse_button(button: Option<message::MouseButton>) -> VtMouseButton {
    match button.unwrap_or(message::MouseButton::None) {
        message::MouseButton::Left => VtMouseButton::Left,
        message::MouseButton::Middle => VtMouseButton::Middle,
        message::MouseButton::Right => VtMouseButton::Right,
        message::MouseButton::None => VtMouseButton::None,
    }
}

fn encode_mouse_input(mouse: &message::Mouse, sgr: bool) -> Result<Vec<u8>, &'static str> {
    if mouse.repeat == 0 {
        return Err("repeat must be at least 1");
    }

    let button = map_mouse_button(mouse.button);
    let modifiers = encode_mouse_modifiers(mouse.shift, mouse.alt, mouse.ctrl);
    let col = mouse.col as usize;
    let row = mouse.row as usize;

    let mut bytes = Vec::new();
    match mouse.kind {
        message::MouseKind::Press => {
            if button == VtMouseButton::None {
                return Err("press requires --button left|middle|right");
            }
            bytes.extend(encode_mouse_event(button, true, col, row, modifiers, sgr));
        }
        message::MouseKind::Release => {
            if button == VtMouseButton::None {
                return Err("release requires --button left|middle|right");
            }
            bytes.extend(encode_mouse_event(button, false, col, row, modifiers, sgr));
        }
        message::MouseKind::Click => {
            if button == VtMouseButton::None {
                return Err("click requires --button left|middle|right");
            }
            bytes.extend(encode_mouse_event(button, true, col, row, modifiers, sgr));
            bytes.extend(encode_mouse_event(button, false, col, row, modifiers, sgr));
        }
        message::MouseKind::Move => {
            bytes.extend(encode_mouse_motion(button, col, row, modifiers, sgr));
        }
        message::MouseKind::WheelUp => {
            for _ in 0..mouse.repeat {
                bytes.extend(encode_mouse_event(
                    VtMouseButton::WheelUp,
                    true,
                    col,
                    row,
                    modifiers,
                    sgr,
                ));
            }
        }
        message::MouseKind::WheelDown => {
            for _ in 0..mouse.repeat {
                bytes.extend(encode_mouse_event(
                    VtMouseButton::WheelDown,
                    true,
                    col,
                    row,
                    modifiers,
                    sgr,
                ));
            }
        }
    }

    Ok(bytes)
}

fn screen_metadata(screen: &wtd_pty::ScreenBuffer) -> serde_json::Value {
    serde_json::json!({
        "cols": u16::try_from(screen.cols()).unwrap_or(u16::MAX),
        "rows": u16::try_from(screen.rows()).unwrap_or(u16::MAX),
        "onAlternate": screen.on_alternate(),
        "title": if screen.title.is_empty() { Value::Null } else { Value::String(screen.title.clone()) },
        "progress": progress_info_from_screen(screen.progress()),
        "mouseMode": mouse_mode_name(screen),
        "sgrMouse": screen.sgr_mouse(),
        "bracketedPaste": screen.bracketed_paste(),
        "cursorRow": u16::try_from(screen.cursor().row).unwrap_or(u16::MAX),
        "cursorCol": u16::try_from(screen.cursor().col).unwrap_or(u16::MAX),
        "cursorVisible": screen.cursor().visible,
        "cursorShape": cursor_shape_name(screen),
    })
}

/// Create an ad-hoc workspace definition from a profile name and global settings.
///
/// Used when the user runs `wtd` / `wtd open` / `wtd open --profile <name>` without
/// a workspace YAML file.
fn synthesize_default_workspace(
    name: &str,
    profile: Option<&str>,
    settings: &GlobalSettings,
) -> WorkspaceDefinition {
    let effective_profile = profile.unwrap_or(settings.default_profile.as_str());
    WorkspaceDefinition {
        version: 1,
        name: name.to_string(),
        description: None,
        defaults: None,
        profiles: None,
        bindings: None,
        windows: None,
        tabs: Some(vec![TabDefinition {
            name: "main".to_string(),
            layout: PaneNode::Pane(PaneLeaf {
                name: "shell".to_string(),
                session: Some(SessionLaunchDefinition {
                    profile: Some(effective_profile.to_string()),
                    cwd: None,
                    env: None,
                    startup_command: None,
                    title: None,
                    args: None,
                    terminal_size: None,
                    driver: None,
                }),
            }),
            focus: None,
        }]),
    }
}

fn parse_driver_profile(value: &str) -> Option<PaneDriverProfile> {
    match value {
        "plain" => Some(PaneDriverProfile::Plain),
        "codex" => Some(PaneDriverProfile::Codex),
        "pi" => Some(PaneDriverProfile::Pi),
        "claude-code" => Some(PaneDriverProfile::ClaudeCode),
        "gemini-cli" => Some(PaneDriverProfile::GeminiCli),
        "copilot-cli" => Some(PaneDriverProfile::CopilotCli),
        _ => None,
    }
}

fn session_def_with_profile(profile: &str) -> SessionLaunchDefinition {
    SessionLaunchDefinition {
        profile: Some(profile.to_string()),
        ..Default::default()
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

fn save_workspace_definition_to_file(
    inst: &WorkspaceInstance,
    workspace: &str,
    file: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let def = inst.save();

    let path = if let Some(file) = file {
        std::path::PathBuf::from(file)
    } else {
        let dir = wtd_core::ensure_user_workspaces_dir()
            .map_err(|e| format!("failed to create workspaces directory: {e}"))?;
        dir.join(format!("{workspace}.yaml"))
    };

    let yaml =
        serde_yaml::to_string(&def).map_err(|e| format!("failed to serialize workspace: {e}"))?;

    std::fs::write(&path, &yaml).map_err(|e| format!("failed to write {}: {e}", path.display()))?;

    Ok(path)
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

            TypedMessage::Prompt(prompt) => self.handle_prompt(&envelope.id, prompt),

            TypedMessage::Keys(keys) => self.handle_keys(&envelope.id, keys),

            TypedMessage::Mouse(mouse) => self.handle_mouse(&envelope.id, mouse),

            TypedMessage::PaneInput(input) => self.handle_pane_input(&envelope.id, input),

            TypedMessage::Capture(capture) => self.handle_capture(&envelope.id, capture),

            TypedMessage::Scrollback(scrollback) => {
                self.handle_scrollback(&envelope.id, scrollback)
            }

            TypedMessage::WaitPane(wait) => self.handle_wait_pane(&envelope.id, wait),

            TypedMessage::Follow(follow) => self.handle_follow(&envelope.id, follow),

            TypedMessage::Inspect(inspect) => self.handle_inspect(&envelope.id, inspect),

            TypedMessage::ConfigurePane(configure) => {
                self.handle_configure_pane(&envelope.id, configure)
            }

            TypedMessage::Notify(notify) => self.handle_notify(&envelope.id, notify),

            TypedMessage::ClearAttention(clear) => self.handle_clear_attention(&envelope.id, clear),

            TypedMessage::SetPaneStatus(status) => {
                self.handle_set_pane_status(&envelope.id, status)
            }

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
        let mut state = self.lock_state();

        // Derive effective workspace name.
        let ws_name = match (&open.name, &open.profile) {
            (Some(name), _) => name.clone(),
            (None, Some(prof)) => prof.clone(),
            (None, None) => "default".to_string(),
        };

        // Check if already open (and not requesting recreate)
        if !open.recreate {
            if let Some(inst) = state.workspaces.get(&ws_name) {
                return Some(Envelope::new(
                    id,
                    &OpenWorkspaceResult {
                        instance_id: format!("{}", inst.id().0),
                        state: Value::Object(serde_json::Map::new()),
                    },
                ));
            }
        }

        // Load or synthesize workspace definition.
        let def = if open.file.is_some() || (open.name.is_some() && open.profile.is_none()) {
            // File-based path: look up workspace definition on disk.
            match load_workspace_from_disk(&ws_name, open.file.as_deref(), &request_cwd(envelope)) {
                Ok(d) => d,
                Err(mut e) => {
                    e.id = id.to_string();
                    return Some(e);
                }
            }
        } else {
            // Ad-hoc path: synthesize from defaults / named profile.
            synthesize_default_workspace(&ws_name, open.profile.as_deref(), &state.settings)
        };

        // If recreating, close existing first
        if open.recreate {
            if let Some(mut existing) = state.workspaces.remove(&ws_name) {
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
        state.workspaces.insert(ws_name, inst);

        Some(Envelope::new(
            id,
            &OpenWorkspaceResult {
                instance_id,
                state: Value::Object(serde_json::Map::new()),
            },
        ))
    }

    fn handle_close_workspace(&self, id: &str, close: &CloseWorkspace) -> Option<Envelope> {
        let mut state = self.lock_state();
        match state.workspaces.remove(&close.workspace) {
            Some(mut inst) => {
                inst.close();
                state
                    .pending_broadcasts
                    .push(BroadcastEvent::WorkspaceState {
                        workspace: close.workspace.clone(),
                        new_state: "closing".to_string(),
                    });
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
        let mut state = self.lock_state();
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
        let Some(inst) = state.workspaces.get(&attach.workspace) else {
            tracing::error!(workspace = %attach.workspace, "workspace disappeared during attach handling");
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", attach.workspace),
            ));
        };
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
        let mut state = self.lock_state();

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
        let Some(inst) = state.workspaces.get_mut(&recreate.workspace) else {
            tracing::error!(workspace = %recreate.workspace, "workspace disappeared during recreate handling");
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", recreate.workspace),
            ));
        };

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
        let state = self.lock_state();
        match state.workspaces.get(&save.workspace) {
            Some(inst) => {
                match save_workspace_definition_to_file(inst, &save.workspace, save.file.as_deref())
                {
                    Ok(_) => Some(Envelope::new(id, &OkResponse {})),
                    Err(message) => Some(error_envelope(id, ErrorCode::InternalError, &message)),
                }
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

        let state = self.lock_state();

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
        let state = self.lock_state();
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
        let state = self.lock_state();
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
                let attention = inst.pane_attention(&pane_id).cloned().unwrap_or_default();
                panes.push(PaneInfo {
                    name,
                    tab: tab.name().to_string(),
                    session_state,
                    attention: attention.state,
                    attention_message: attention.message,
                });
            }
        }

        Some(Envelope::new(id, &ListPanesResult { panes }))
    }

    fn handle_list_sessions(&self, id: &str, ls: &ListSessions) -> Option<Envelope> {
        let state = self.lock_state();
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
        let state = self.lock_state();
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

        let input = encode_send_input(&send.text, send.newline, session.screen().bracketed_paste());

        match session.write_input(&input) {
            Ok(()) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::SessionFailed,
                &format!("write failed: {}", e),
            )),
        }
    }

    fn handle_prompt(&self, id: &str, prompt: &message::Prompt) -> Option<Envelope> {
        let state = self.lock_state();
        let (inst, pane_id) = match find_pane(&state.workspaces, &prompt.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", prompt.target),
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

        let driver = inst
            .pane_driver(&pane_id)
            .cloned()
            .unwrap_or_else(|| resolve_pane_driver(None, None));
        let plan = match build_prompt_input_plan(
            &prompt.text,
            &driver,
            session.screen().bracketed_paste(),
        ) {
            Ok(plan) => plan,
            Err(e) => {
                return Some(error_envelope(
                    id,
                    ErrorCode::InvalidArgument,
                    &e.to_string(),
                ));
            }
        };

        match session.write_input(&plan.body) {
            Ok(()) => {}
            Err(e) => {
                return Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    &format!("write failed: {}", e),
                ));
            }
        }

        if plan.submit_delay_ms > 0 {
            match session
                .schedule_write_input(plan.submit, Duration::from_millis(plan.submit_delay_ms))
            {
                Ok(()) => Some(Envelope::new(id, &OkResponse {})),
                Err(e) => Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    &format!("write failed: {}", e),
                )),
            }
        } else {
            match session.write_input(&plan.submit) {
                Ok(()) => Some(Envelope::new(id, &OkResponse {})),
                Err(e) => Some(error_envelope(
                    id,
                    ErrorCode::SessionFailed,
                    &format!("write failed: {}", e),
                )),
            }
        }
    }

    fn handle_keys(&self, id: &str, keys: &Keys) -> Option<Envelope> {
        let state = self.lock_state();
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

    fn handle_mouse(&self, id: &str, mouse: &message::Mouse) -> Option<Envelope> {
        let state = self.lock_state();
        let (inst, pane_id) = match find_pane(&state.workspaces, &mouse.target) {
            Some(r) => r,
            None => {
                return Some(error_envelope(
                    id,
                    ErrorCode::TargetNotFound,
                    &format!("pane '{}' not found", mouse.target),
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

        let screen = session.screen();
        if screen.mouse_mode() == wtd_pty::MouseMode::None && !mouse.force {
            return Some(error_envelope(
                id,
                ErrorCode::InvalidArgument,
                "pane is not advertising VT mouse mode; use --force to inject anyway",
            ));
        }

        let bytes = match encode_mouse_input(mouse, screen.sgr_mouse()) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Some(error_envelope(id, ErrorCode::InvalidArgument, e));
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

    fn handle_pane_input(&self, id: &str, input: &PaneInput) -> Option<Envelope> {
        let state = self.lock_state();
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
        let mut state = self.lock_state();

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
                        let metadata = screen_metadata(screen);
                        let ext = screen.capture_extended(
                            capture.lines,
                            capture.all.unwrap_or(false),
                            capture.after.as_deref(),
                            compiled_regex.as_ref(),
                            capture.max_lines,
                            capture.count.unwrap_or(false),
                        );
                        let text = if capture.count.unwrap_or(false) {
                            String::new()
                        } else if capture.vt.unwrap_or(false) {
                            String::from_utf8_lossy(&screen.to_vt_snapshot()).into_owned()
                        } else {
                            ext.text
                        };
                        CaptureResult {
                            text,
                            lines: ext.lines,
                            total_lines: ext.total_lines,
                            anchor_found: ext.anchor_found,
                            cursor: Some(ext.cursor),
                            cols: metadata["cols"].as_u64().unwrap_or(0) as u16,
                            rows: metadata["rows"].as_u64().unwrap_or(0) as u16,
                            on_alternate: metadata["onAlternate"].as_bool().unwrap_or(false),
                            title: metadata["title"].as_str().map(str::to_owned),
                            progress: metadata
                                .get("progress")
                                .and_then(|v| serde_json::from_value(v.clone()).ok()),
                            mouse_mode: metadata["mouseMode"].as_str().map(str::to_owned),
                            sgr_mouse: metadata["sgrMouse"].as_bool().unwrap_or(false),
                            bracketed_paste: metadata["bracketedPaste"].as_bool().unwrap_or(false),
                            cursor_row: metadata["cursorRow"].as_u64().map(|v| v as u16),
                            cursor_col: metadata["cursorCol"].as_u64().map(|v| v as u16),
                            cursor_visible: metadata["cursorVisible"].as_bool(),
                            cursor_shape: metadata["cursorShape"].as_str().map(str::to_owned),
                        }
                    }
                    None => CaptureResult {
                        text: String::new(),
                        lines: 0,
                        total_lines: 0,
                        anchor_found: None,
                        cursor: None,
                        cols: 0,
                        rows: 0,
                        on_alternate: false,
                        title: None,
                        progress: None,
                        mouse_mode: None,
                        sgr_mouse: false,
                        bracketed_paste: false,
                        cursor_row: None,
                        cursor_col: None,
                        cursor_visible: None,
                        cursor_shape: None,
                    },
                }
            }
            _ => CaptureResult {
                text: String::new(),
                lines: 0,
                total_lines: 0,
                anchor_found: None,
                cursor: None,
                cols: 0,
                rows: 0,
                on_alternate: false,
                title: None,
                progress: None,
                mouse_mode: None,
                sgr_mouse: false,
                bracketed_paste: false,
                cursor_row: None,
                cursor_col: None,
                cursor_visible: None,
                cursor_shape: None,
            },
        };

        Some(Envelope::new(id, &result))
    }

    fn handle_scrollback(&self, id: &str, scrollback: &Scrollback) -> Option<Envelope> {
        let mut state = self.lock_state();

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

    fn handle_wait_pane(&self, id: &str, wait: &WaitPane) -> Option<Envelope> {
        let timeout = std::time::Duration::from_millis(wait.timeout_ms.unwrap_or(30_000));
        let poll = std::time::Duration::from_millis(wait.poll_ms.unwrap_or(250).max(10));
        let recent_lines = wait.recent_lines.unwrap_or(40);
        let started = std::time::Instant::now();
        let initial = self.wait_state_signature(&wait.target, recent_lines);

        loop {
            match self.wait_pane_snapshot(&wait.target, recent_lines, initial.as_deref()) {
                Ok((_signature, data)) => {
                    let condition_matched = wait_condition_matches(wait.condition, &data);
                    if condition_matched {
                        return Some(Envelope::new(
                            id,
                            &WaitPaneResult {
                                matched: true,
                                condition: wait.condition,
                                target: wait.target.clone(),
                                data,
                            },
                        ));
                    }
                    if started.elapsed() >= timeout {
                        return Some(Envelope::new(
                            id,
                            &WaitPaneResult {
                                matched: false,
                                condition: wait.condition,
                                target: wait.target.clone(),
                                data,
                            },
                        ));
                    }
                }
                Err(message) => {
                    return Some(error_envelope(id, ErrorCode::TargetNotFound, &message));
                }
            }
            std::thread::sleep(poll);
        }
    }

    fn wait_state_signature(&self, target: &str, recent_lines: u32) -> Option<String> {
        let mut state = self.lock_state();
        for inst in state.workspaces.values_mut() {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
        }
        let (inst, pane_id) = find_pane(&state.workspaces, target)?;
        Some(wait_signature(inst, &pane_id, recent_lines))
    }

    fn wait_pane_snapshot(
        &self,
        target: &str,
        recent_lines: u32,
        initial_signature: Option<&str>,
    ) -> Result<(Option<String>, Value), String> {
        let mut state = self.lock_state();
        for inst in state.workspaces.values_mut() {
            for session in inst.sessions_mut().values_mut() {
                session.process_pending_output();
            }
        }
        let (inst, pane_id) = find_pane(&state.workspaces, target)
            .ok_or_else(|| format!("pane '{target}' not found"))?;
        let signature = wait_signature(inst, &pane_id, recent_lines);
        let pane_name = inst.pane_name(&pane_id).unwrap_or("?").to_string();
        let attention = inst
            .pane_attention(&pane_id)
            .and_then(|attention| serde_json::to_value(attention).ok())
            .unwrap_or_else(|| serde_json::json!({ "state": "active" }));
        let metadata = inst
            .pane_metadata(&pane_id)
            .and_then(|metadata| serde_json::to_value(metadata).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let recent_output = get_pane_scrollback(inst, &pane_id, recent_lines);
        let changed = initial_signature
            .map(|initial| initial != signature)
            .unwrap_or(false);
        let data = serde_json::json!({
            "paneName": pane_name,
            "paneId": format!("{}", pane_id),
            "workspace": inst.name(),
            "attention": attention,
            "metadata": metadata,
            "recentOutput": recent_output,
            "stateChanged": changed,
        });
        Ok((Some(signature), data))
    }

    fn handle_follow(&self, id: &str, follow: &Follow) -> Option<Envelope> {
        let state = self.lock_state();
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
        let state = self.lock_state();
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
        let mut data = serde_json::json!({
            "paneName": pane_name,
            "paneId": format!("{}", pane_id),
            "workspace": inst.name(),
        });

        let session_state = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => {
                if let Some(session) = inst.session(session_id) {
                    if let Some(obj) = data.as_object_mut() {
                        obj.insert(
                            "sessionId".to_string(),
                            Value::String(session_id.to_string()),
                        );
                        if let Some(screen_obj) = screen_metadata(session.screen()).as_object() {
                            for (key, value) in screen_obj {
                                obj.insert(key.clone(), value.clone());
                            }
                        }
                    }
                    format!("{:?}", session.state())
                } else {
                    "unknown".into()
                }
            }
            Some(PaneState::Detached { error }) => format!("detached: {}", error),
            None => "none".into(),
        };
        if let Some(obj) = data.as_object_mut() {
            obj.insert("sessionState".to_string(), Value::String(session_state));
            if let Some(attention) = inst.pane_attention(&pane_id) {
                obj.insert(
                    "attention".to_string(),
                    serde_json::to_value(attention)
                        .unwrap_or_else(|_| serde_json::json!({ "state": "active" })),
                );
            }
            if let Some(metadata) = inst.pane_metadata(&pane_id) {
                let mut value =
                    serde_json::to_value(metadata).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(meta_obj) = value.as_object_mut() {
                    if let Some(PaneState::Attached { session_id }) = inst.pane_state(&pane_id) {
                        if let Some(session) = inst.session(session_id) {
                            meta_obj.insert(
                                "cwd".to_string(),
                                session
                                    .config()
                                    .cwd
                                    .as_ref()
                                    .map(|cwd| Value::String(cwd.clone()))
                                    .unwrap_or(Value::Null),
                            );
                            meta_obj.insert(
                                "progress".to_string(),
                                serde_json::to_value(progress_info_from_screen(
                                    session.screen().progress(),
                                ))
                                .unwrap_or(Value::Null),
                            );
                        }
                    }
                    if let Some(driver) = inst.pane_driver(&pane_id) {
                        meta_obj.insert(
                            "driverProfile".to_string(),
                            Value::String(driver.profile.clone()),
                        );
                    }
                }
                obj.insert("metadata".to_string(), value);
            }
            if let Some(driver) = inst.pane_driver(&pane_id) {
                obj.insert(
                    "driverProfile".to_string(),
                    Value::String(driver.profile.clone()),
                );
                obj.insert(
                    "submitKey".to_string(),
                    Value::String(driver.submit_key.clone()),
                );
                obj.insert(
                    "softBreakKey".to_string(),
                    driver
                        .soft_break_key
                        .as_ref()
                        .map(|key| Value::String(key.clone()))
                        .unwrap_or(Value::Null),
                );
            }
        }

        Some(Envelope::new(id, &InspectResult { data }))
    }

    fn handle_notify(&self, id: &str, notify: &Notify) -> Option<Envelope> {
        let mut state = self.lock_state();
        let (workspace_name, pane_id) =
            match resolve_pane_for_resize(&state.workspaces, &notify.target) {
                Some((workspace_name, pane_id)) => (workspace_name, pane_id),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", notify.target),
                    ));
                }
            };

        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };

        match inst.set_pane_attention(
            &pane_id,
            notify.state,
            notify.message.clone(),
            notify.source.clone(),
        ) {
            Ok(record) => {
                state
                    .pending_broadcasts
                    .push(BroadcastEvent::AttentionChange {
                        workspace: workspace_name,
                        pane_id: Some(format!("{}", pane_id.0)),
                        state: record.state,
                        message: record.message,
                        source: record.source,
                    });
                Some(Envelope::new(id, &OkResponse {}))
            }
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::InternalError,
                &format!("failed to set attention: {}", e),
            )),
        }
    }

    fn handle_clear_attention(&self, id: &str, clear: &ClearAttention) -> Option<Envelope> {
        let mut state = self.lock_state();
        let (workspace_name, pane_id) =
            match resolve_pane_for_resize(&state.workspaces, &clear.target) {
                Some((workspace_name, pane_id)) => (workspace_name, pane_id),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", clear.target),
                    ));
                }
            };

        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };

        match inst.clear_pane_attention(&pane_id) {
            Ok(record) => {
                state
                    .pending_broadcasts
                    .push(BroadcastEvent::AttentionChange {
                        workspace: workspace_name,
                        pane_id: Some(format!("{}", pane_id.0)),
                        state: record.state,
                        message: record.message,
                        source: record.source,
                    });
                Some(Envelope::new(id, &OkResponse {}))
            }
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::InternalError,
                &format!("failed to clear attention: {}", e),
            )),
        }
    }

    fn handle_set_pane_status(&self, id: &str, status: &SetPaneStatus) -> Option<Envelope> {
        let mut state = self.lock_state();
        let (workspace_name, pane_id) =
            match resolve_pane_for_resize(&state.workspaces, &status.target) {
                Some((workspace_name, pane_id)) => (workspace_name, pane_id),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", status.target),
                    ));
                }
            };

        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };

        match inst.set_pane_metadata(
            &pane_id,
            status.phase.clone(),
            status.status_text.clone(),
            status.queue_pending,
            status.completion.clone(),
            status.source.clone(),
        ) {
            Ok(_) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::InternalError,
                &format!("failed to set pane status: {}", e),
            )),
        }
    }

    fn handle_configure_pane(&self, id: &str, configure: &ConfigurePane) -> Option<Envelope> {
        let mut state = self.lock_state();
        let (workspace_name, pane_id) =
            match resolve_pane_for_resize(&state.workspaces, &configure.target) {
                Some((workspace_name, pane_id)) => (workspace_name, pane_id),
                None => {
                    return Some(error_envelope(
                        id,
                        ErrorCode::TargetNotFound,
                        &format!("pane '{}' not found", configure.target),
                    ));
                }
            };

        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            tracing::error!(workspace = %workspace_name, "workspace disappeared during configure-pane handling");
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };

        if !configure.clear_driver
            && configure.driver_profile.is_none()
            && configure.submit_key.is_none()
            && configure.soft_break_key.is_none()
            && !configure.clear_soft_break
        {
            return Some(error_envelope(
                id,
                ErrorCode::InvalidArgument,
                "configure-pane requires at least one driver setting",
            ));
        }

        if configure.clear_driver
            && (configure.driver_profile.is_some()
                || configure.submit_key.is_some()
                || configure.soft_break_key.is_some()
                || configure.clear_soft_break)
        {
            return Some(error_envelope(
                id,
                ErrorCode::InvalidArgument,
                "--clear-driver cannot be combined with other pane driver settings",
            ));
        }

        let driver_definition = if configure.clear_driver {
            None
        } else {
            let current = inst
                .pane_driver(&pane_id)
                .cloned()
                .unwrap_or_else(|| resolve_pane_driver(None, None));
            let mut driver = pane_driver_definition_from_effective(&current);

            if let Some(profile) = &configure.driver_profile {
                let parsed = match parse_driver_profile(profile) {
                    Some(parsed) => parsed,
                    None => {
                        return Some(error_envelope(
                            id,
                            ErrorCode::InvalidArgument,
                            &format!(
                                "unknown driver profile '{}'; expected plain, codex, pi, claude-code, gemini-cli, or copilot-cli",
                                profile
                            ),
                        ));
                    }
                };
                driver.profile = Some(parsed);
            }
            if let Some(submit_key) = &configure.submit_key {
                driver.submit_key = Some(submit_key.clone());
            }
            if let Some(soft_break_key) = &configure.soft_break_key {
                driver.soft_break_key = Some(soft_break_key.clone());
                driver.disable_soft_break = false;
            }
            if configure.clear_soft_break {
                driver.soft_break_key = None;
                driver.disable_soft_break = true;
            }

            Some(driver)
        };

        let effective_driver = if let Some(ref driver_definition) = driver_definition {
            let session_def = SessionLaunchDefinition {
                driver: Some(driver_definition.clone()),
                ..Default::default()
            };
            resolve_pane_driver(Some(&session_def), None)
        } else {
            resolve_pane_driver(None, None)
        };

        match inst.set_pane_driver(&pane_id, driver_definition, effective_driver) {
            Ok(()) => Some(Envelope::new(id, &OkResponse {})),
            Err(e) => Some(error_envelope(
                id,
                ErrorCode::InternalError,
                &format!("failed to configure pane: {}", e),
            )),
        }
    }

    fn handle_invoke_action(&self, id: &str, action: &InvokeAction) -> Option<Envelope> {
        let mut state = self.lock_state();

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
        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            tracing::error!(workspace = %workspace_name, "workspace disappeared during invoke-action handling");
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };
        let active_pane = || {
            inst.tabs()
                .get(inst.active_tab_index())
                .map(|tab| tab.layout().focus())
        };

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
                if let Some(profile) = action.args.get("profile").and_then(|v| v.as_str()) {
                    inst.spawn_session_for_pane_with_definition(
                        &pane_id,
                        pane_name,
                        session_def_with_profile(profile),
                        &settings,
                        &env,
                        &find_exe,
                    );
                } else {
                    inst.spawn_session_for_pane(&pane_id, pane_name, &settings, &env, &find_exe);
                }
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
            "rename-tab" => {
                let name = match action.args.get("name").and_then(|v| v.as_str()) {
                    Some(name) => name.to_string(),
                    None => {
                        return Some(error_envelope(
                            id,
                            ErrorCode::InvalidArgument,
                            "missing rename-tab name",
                        ));
                    }
                };
                let tab_index = target_pane
                    .as_ref()
                    .and_then(|pane_id| {
                        inst.tabs()
                            .iter()
                            .position(|tab| tab.layout().panes().contains(pane_id))
                    })
                    .unwrap_or_else(|| inst.active_tab_index());
                if tab_index >= inst.tabs().len() {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InvalidAction,
                        "no tabs available",
                    ));
                }
                inst.rename_tab(tab_index, name);
                return Some(Envelope::new(
                    id,
                    &InvokeActionResult {
                        result: "tab-renamed".to_string(),
                        pane_id: None,
                    },
                ));
            }
            "save-workspace" => {
                if let Err(e) = v1_registry().validate_args("save-workspace", &action.args) {
                    return Some(error_envelope(
                        id,
                        ErrorCode::InvalidArgument,
                        &e.to_string(),
                    ));
                }
                let file = action.args.get("file").and_then(|v| v.as_str());
                match save_workspace_definition_to_file(inst, &workspace_name, file) {
                    Ok(path) => {
                        return Some(Envelope::new(
                            id,
                            &InvokeActionResult {
                                result: format!("workspace-saved:{}", path.display()),
                                pane_id: None,
                            },
                        ));
                    }
                    Err(message) => {
                        return Some(error_envelope(id, ErrorCode::InternalError, &message));
                    }
                }
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
            "change-profile" => {
                let profile = match action.args.get("profile").and_then(|v| v.as_str()) {
                    Some(profile) => profile,
                    None => {
                        return Some(error_envelope(
                            id,
                            ErrorCode::InvalidArgument,
                            "missing change-profile profile",
                        ));
                    }
                };
                let pane_id = match target_pane.clone().or_else(active_pane) {
                    Some(pane_id) => pane_id,
                    None => {
                        return Some(error_envelope(
                            id,
                            ErrorCode::TargetNotFound,
                            "no pane available for change-profile",
                        ));
                    }
                };
                let pane_name = inst
                    .pane_name(&pane_id)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("pane-{}", pane_id.0));
                let mut session_def = inst
                    .pane_original_def(&pane_id)
                    .cloned()
                    .flatten()
                    .unwrap_or_default();
                session_def.profile = Some(profile.to_string());
                let env = host_env();
                inst.stop_pane_session(&pane_id);
                inst.spawn_session_for_pane_with_definition(
                    &pane_id,
                    pane_name,
                    session_def,
                    &settings,
                    &env,
                    &find_exe,
                );
                return Some(Envelope::new(
                    id,
                    &InvokeActionResult {
                        result: "pane-profile-changed".to_string(),
                        pane_id: Some(format!("{}", pane_id.0)),
                    },
                ));
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

        match dispatcher.dispatch(inst, &action.action, &action.args, target_pane.clone()) {
            Ok(result) => {
                use crate::action::ActionResult;

                match result {
                    ActionResult::PaneCreated { pane_id } => {
                        // Spawn a session for the newly created pane.
                        let pane_name = format!("pane-{}", pane_id.0);
                        let env = host_env();
                        let session_def = match action.args.get("profile").and_then(|v| v.as_str())
                        {
                            Some(profile) => session_def_with_profile(profile),
                            None => target_pane
                                .as_ref()
                                .and_then(|source_pane| inst.pane_original_def(source_pane))
                                .cloned()
                                .flatten()
                                .unwrap_or_default(),
                        };
                        inst.spawn_session_for_pane_with_definition(
                            &pane_id,
                            pane_name,
                            session_def,
                            &settings,
                            &env,
                            &find_exe,
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

        let mut state = self.lock_state();
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

        let Some(inst) = state.workspaces.get_mut(&workspace_name) else {
            tracing::error!(workspace = %workspace_name, "workspace disappeared during pane-resize handling");
            return Some(error_envelope(
                id,
                ErrorCode::WorkspaceNotFound,
                &format!("workspace '{}' not found", workspace_name),
            ));
        };
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
        let state = self.lock_state();
        let Some(inst) = state.workspaces.get(&input.workspace) else {
            return;
        };
        let session_id = SessionId(input.session_id.parse::<u64>().unwrap_or(0));
        if let Some(session) = inst.session(&session_id) {
            if let Some(bytes) = decode_base64(&input.data) {
                let _ = session.write_input(&bytes);
            }
        }
    }

    fn handle_focus_pane(&self, id: &str, focus: &FocusPane) -> Option<Envelope> {
        let mut state = self.lock_state();
        let Some((workspace_name, pane_id)) =
            resolve_invoke_action_target(&state.workspaces, &focus.pane_id)
        else {
            return Some(error_envelope(
                id,
                ErrorCode::TargetNotFound,
                &format!("pane '{}' not found", focus.pane_id),
            ));
        };

        if let Some(inst) = state.workspaces.get_mut(&workspace_name) {
            for tab in inst.tabs_mut() {
                if tab.layout().panes().contains(&pane_id) {
                    let _ = tab.layout_mut().set_focus(pane_id);
                    return Some(Envelope::new(id, &OkResponse {}));
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
        let mut state = self.lock_state();
        let Some((workspace_name, pane_id)) =
            resolve_invoke_action_target(&state.workspaces, &rename.pane_id)
        else {
            return Some(error_envelope(
                id,
                ErrorCode::TargetNotFound,
                &format!("pane '{}' not found", rename.pane_id),
            ));
        };

        if let Some(inst) = state.workspaces.get_mut(&workspace_name) {
            inst.rename_pane(&pane_id, rename.new_name.clone());
            return Some(Envelope::new(id, &OkResponse {}));
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::{
        encode_mouse_input, encode_send_input, parse_driver_profile, scoped_session_key,
        HostRequestHandler,
    };
    use crate::output_broadcaster::BroadcastEvent;
    use crate::workspace_instance::WorkspaceInstance;
    use serde_json::json;
    use wtd_core::global_settings::GlobalSettings;
    use wtd_core::workspace::{
        PaneLeaf, PaneNode, SessionLaunchDefinition, TabDefinition, WorkspaceDefinition,
    };
    use wtd_ipc::message::{
        AttentionState, ClearAttention, Inspect, InspectResult, Mouse, MouseButton, MouseKind,
        Notify, SetPaneStatus, WaitCondition, WaitPane, WaitPaneResult,
    };

    fn encode_b64(input: &[u8]) -> String {
        const CHARS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
            out.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(CHARS[(triple & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn parse_driver_profile_accepts_pi() {
        assert_eq!(
            parse_driver_profile("pi"),
            Some(wtd_core::workspace::PaneDriverProfile::Pi)
        );
    }

    #[test]
    fn encode_send_input_plain_text_without_newline() {
        assert_eq!(encode_send_input("abc", false, false), b"abc");
    }

    #[test]
    fn encode_send_input_uses_carriage_return_for_newline() {
        assert_eq!(encode_send_input("abc", true, false), b"abc\r");
    }

    #[test]
    fn encode_send_input_wraps_bracketed_paste_for_bulk_text() {
        assert_eq!(
            encode_send_input("abc", false, true),
            b"\x1b[200~abc\x1b[201~"
        );
    }

    #[test]
    fn encode_send_input_appends_enter_after_bracketed_paste() {
        assert_eq!(
            encode_send_input("abc", true, true),
            b"\x1b[200~abc\x1b[201~\r"
        );
    }

    #[test]
    fn encode_send_input_does_not_wrap_single_char_input() {
        assert_eq!(encode_send_input("a", false, true), b"a");
    }

    #[test]
    fn encode_mouse_click_sgr() {
        let mouse = Mouse {
            target: "w/p".into(),
            kind: MouseKind::Click,
            col: 4,
            row: 2,
            button: Some(MouseButton::Left),
            shift: false,
            alt: false,
            ctrl: false,
            repeat: 1,
            force: false,
        };
        assert_eq!(
            encode_mouse_input(&mouse, true).unwrap(),
            b"\x1b[<0;5;3M\x1b[<0;5;3m"
        );
    }

    #[test]
    fn encode_mouse_wheel_repeat_legacy() {
        let mouse = Mouse {
            target: "w/p".into(),
            kind: MouseKind::WheelDown,
            col: 1,
            row: 1,
            button: None,
            shift: false,
            alt: true,
            ctrl: false,
            repeat: 2,
            force: false,
        };
        assert_eq!(
            encode_mouse_input(&mouse, false).unwrap(),
            vec![0x1b, b'[', b'M', 105, 34, 34, 0x1b, b'[', b'M', 105, 34, 34]
        );
    }

    #[test]
    fn encode_mouse_click_requires_button() {
        let mouse = Mouse {
            target: "w/p".into(),
            kind: MouseKind::Click,
            col: 0,
            row: 0,
            button: None,
            shift: false,
            alt: false,
            ctrl: false,
            repeat: 1,
            force: false,
        };
        assert_eq!(
            encode_mouse_input(&mouse, true).unwrap_err(),
            "click requires --button left|middle|right"
        );
    }

    #[test]
    fn scoped_session_key_includes_workspace_namespace() {
        assert_eq!(scoped_session_key("dev", "1"), "dev:1");
        assert_eq!(scoped_session_key("ops", "1"), "ops:1");
    }

    #[test]
    fn notify_and_clear_attention_update_pane_state_and_broadcast() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert(
                "alpha".to_string(),
                WorkspaceInstance::new_for_test_multi("alpha", 1, &[("main", &["shell"])]),
            );
        }

        let response = handler.handle_notify(
            "notify-1",
            &Notify {
                target: "alpha/main/shell".to_string(),
                state: AttentionState::NeedsAttention,
                message: Some("input requested".to_string()),
                source: Some("pi".to_string()),
            },
        );
        assert!(response.is_some());

        {
            let state = handler.state.lock().unwrap();
            let inst = state.workspaces.get("alpha").unwrap();
            let pane_id = inst.find_pane_by_name("shell").unwrap();
            let attention = inst.pane_attention(&pane_id).unwrap();
            assert_eq!(attention.state, AttentionState::NeedsAttention);
            assert_eq!(attention.message.as_deref(), Some("input requested"));
            assert_eq!(state.pending_broadcasts.len(), 1);
        }

        let response = handler.handle_clear_attention(
            "clear-1",
            &ClearAttention {
                target: "shell".to_string(),
            },
        );
        assert!(response.is_some());

        let state = handler.state.lock().unwrap();
        let inst = state.workspaces.get("alpha").unwrap();
        let pane_id = inst.find_pane_by_name("shell").unwrap();
        let attention = inst.pane_attention(&pane_id).unwrap();
        assert_eq!(attention.state, AttentionState::Active);
        assert!(attention.message.is_none());
        assert_eq!(state.pending_broadcasts.len(), 2);
    }

    #[test]
    fn set_pane_status_updates_metadata_and_inspect_output() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert(
                "alpha".to_string(),
                WorkspaceInstance::new_for_test_multi("alpha", 1, &[("main", &["shell"])]),
            );
        }

        let response = handler.handle_set_pane_status(
            "status-1",
            &SetPaneStatus {
                target: "alpha/main/shell".to_string(),
                phase: Some("working".to_string()),
                status_text: Some("running tests".to_string()),
                progress: None,
                queue_pending: Some(2),
                completion: None,
                source: Some("codex".to_string()),
            },
        );
        assert!(response.is_some());

        let inspect = handler
            .handle_inspect(
                "inspect-1",
                &Inspect {
                    target: "alpha/main/shell".to_string(),
                },
            )
            .unwrap();
        let result: InspectResult = inspect.extract_payload().unwrap();
        assert_eq!(result.data["metadata"]["phase"], "working");
        assert_eq!(result.data["metadata"]["statusText"], "running tests");
        assert_eq!(result.data["metadata"]["queuePending"], 2);
        assert_eq!(result.data["metadata"]["source"], "codex");
    }

    #[test]
    fn wait_pane_succeeds_with_current_metadata_and_snapshot() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert(
                "alpha".to_string(),
                WorkspaceInstance::new_for_test_multi("alpha", 1, &[("main", &["shell"])]),
            );
        }
        handler.handle_set_pane_status(
            "status-1",
            &SetPaneStatus {
                target: "alpha/main/shell".to_string(),
                phase: Some("done".to_string()),
                status_text: Some("tests passed".to_string()),
                progress: None,
                queue_pending: Some(0),
                completion: Some("success".to_string()),
                source: Some("codex".to_string()),
            },
        );

        let response = handler
            .handle_wait_pane(
                "wait-1",
                &WaitPane {
                    target: "alpha/main/shell".to_string(),
                    condition: WaitCondition::Done,
                    timeout_ms: Some(1),
                    poll_ms: Some(1),
                    recent_lines: Some(5),
                },
            )
            .unwrap();
        let result: WaitPaneResult = response.extract_payload().unwrap();
        assert!(result.matched);
        assert_eq!(result.condition, WaitCondition::Done);
        assert_eq!(result.data["metadata"]["phase"], "done");
        assert_eq!(result.data["metadata"]["statusText"], "tests passed");
    }

    #[test]
    fn wait_pane_timeout_returns_state_snapshot() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert(
                "alpha".to_string(),
                WorkspaceInstance::new_for_test_multi("alpha", 1, &[("main", &["shell"])]),
            );
        }

        let response = handler
            .handle_wait_pane(
                "wait-1",
                &WaitPane {
                    target: "alpha/main/shell".to_string(),
                    condition: WaitCondition::Error,
                    timeout_ms: Some(1),
                    poll_ms: Some(1),
                    recent_lines: Some(5),
                },
            )
            .unwrap();
        let result: WaitPaneResult = response.extract_payload().unwrap();
        assert!(!result.matched);
        assert_eq!(result.condition, WaitCondition::Error);
        assert_eq!(result.data["attention"]["state"], "active");
        assert!(result.data["metadata"].is_object());
        assert!(result.data["recentOutput"].is_array());
    }

    #[test]
    fn wait_pane_snapshot_reports_state_change_after_metadata_update() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert(
                "alpha".to_string(),
                WorkspaceInstance::new_for_test_multi("alpha", 1, &[("main", &["shell"])]),
            );
        }
        let initial = handler
            .wait_state_signature("alpha/main/shell", 5)
            .expect("pane should resolve");
        handler.handle_set_pane_status(
            "status-1",
            &SetPaneStatus {
                target: "alpha/main/shell".to_string(),
                phase: Some("working".to_string()),
                status_text: Some("running tests".to_string()),
                progress: None,
                queue_pending: Some(1),
                completion: None,
                source: Some("pi".to_string()),
            },
        );

        let (_, data) = handler
            .wait_pane_snapshot("alpha/main/shell", 5, Some(&initial))
            .unwrap();
        assert_eq!(data["stateChanged"], true);
        assert!(super::wait_condition_matches(
            WaitCondition::StateChange,
            &data
        ));
        assert_eq!(data["metadata"]["source"], "pi");
    }

    fn single_session_workspace(
        name: &str,
        session: SessionLaunchDefinition,
    ) -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: name.to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Pane(PaneLeaf {
                    name: "shell".to_string(),
                    session: Some(session),
                }),
                focus: None,
            }]),
        }
    }

    fn add_workspace(handler: &HostRequestHandler, workspace: WorkspaceDefinition) {
        let settings = GlobalSettings::default();
        let env: HashMap<String, String> = std::env::vars().collect();
        let instance = WorkspaceInstance::open(
            wtd_core::ids::WorkspaceInstanceId(1),
            &workspace,
            &settings,
            &env,
            super::find_exe,
        )
        .expect("workspace should open");
        handler
            .state
            .lock()
            .unwrap()
            .workspaces
            .insert(workspace.name.clone(), instance);
    }

    fn invoke_action(
        handler: &HostRequestHandler,
        action: &str,
        target_pane_id: Option<&str>,
        args: serde_json::Value,
    ) {
        let request = wtd_ipc::message::InvokeAction {
            action: action.to_string(),
            target_pane_id: target_pane_id.map(str::to_string),
            args,
        };
        let response = handler.handle_invoke_action("test", &request);
        assert!(response.is_some(), "expected action response");
    }

    #[test]
    fn new_tab_with_profile_spawns_selected_profile() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        add_workspace(
            &handler,
            single_session_workspace(
                "alpha",
                SessionLaunchDefinition {
                    profile: Some("powershell".to_string()),
                    ..Default::default()
                },
            ),
        );

        invoke_action(&handler, "new-tab", None, json!({"profile": "cmd"}));

        let state = handler.state.lock().unwrap();
        let inst = state.workspaces.get("alpha").expect("workspace");
        assert_eq!(inst.tabs().len(), 2);
        let pane_id = inst.tabs()[1].layout().focus();
        let def = inst
            .pane_original_def(&pane_id)
            .cloned()
            .flatten()
            .expect("pane definition");
        assert_eq!(def.profile.as_deref(), Some("cmd"));
    }

    #[test]
    fn split_with_profile_does_not_inherit_source_startup_command() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        add_workspace(
            &handler,
            single_session_workspace(
                "alpha",
                SessionLaunchDefinition {
                    profile: Some("powershell".to_string()),
                    startup_command: Some("codex".to_string()),
                    ..Default::default()
                },
            ),
        );

        invoke_action(
            &handler,
            "split-right",
            Some("alpha/main/shell"),
            json!({"profile": "cmd"}),
        );

        let state = handler.state.lock().unwrap();
        let inst = state.workspaces.get("alpha").expect("workspace");
        let source = inst.find_pane_by_name("shell").expect("source pane");
        let new_pane = inst.tabs()[0]
            .layout()
            .panes()
            .into_iter()
            .find(|pane_id| *pane_id != source)
            .expect("new pane");
        let def = inst
            .pane_original_def(&new_pane)
            .cloned()
            .flatten()
            .expect("new pane definition");
        assert_eq!(def.profile.as_deref(), Some("cmd"));
        assert_eq!(def.startup_command, None);
    }

    #[test]
    fn change_profile_preserves_existing_launch_overrides() {
        let handler = HostRequestHandler::new(GlobalSettings::default());
        add_workspace(
            &handler,
            single_session_workspace(
                "alpha",
                SessionLaunchDefinition {
                    profile: Some("powershell".to_string()),
                    cwd: Some("C:/Work/WinTermDriver".to_string()),
                    startup_command: Some("codex".to_string()),
                    ..Default::default()
                },
            ),
        );

        invoke_action(
            &handler,
            "change-profile",
            Some("alpha/main/shell"),
            json!({"profile": "cmd"}),
        );

        let state = handler.state.lock().unwrap();
        let inst = state.workspaces.get("alpha").expect("workspace");
        let pane_id = inst.find_pane_by_name("shell").expect("pane");
        let def = inst
            .pane_original_def(&pane_id)
            .cloned()
            .flatten()
            .expect("pane definition");
        assert_eq!(def.profile.as_deref(), Some("cmd"));
        assert_eq!(def.cwd.as_deref(), Some("C:/Work/WinTermDriver"));
        assert_eq!(def.startup_command.as_deref(), Some("codex"));
    }

    #[test]
    fn session_input_routes_within_named_workspace() {
        fn single_cmd_workspace(name: &str) -> WorkspaceDefinition {
            WorkspaceDefinition {
                version: 1,
                name: name.to_string(),
                description: None,
                defaults: None,
                profiles: None,
                bindings: None,
                windows: None,
                tabs: Some(vec![TabDefinition {
                    name: "main".to_string(),
                    layout: PaneNode::Pane(PaneLeaf {
                        name: "shell".to_string(),
                        session: Some(SessionLaunchDefinition {
                            profile: Some("cmd".to_string()),
                            ..Default::default()
                        }),
                    }),
                    focus: None,
                }]),
            }
        }

        let settings = GlobalSettings::default();
        let env: HashMap<String, String> = std::env::vars().collect();
        let alpha = WorkspaceInstance::open(
            wtd_core::ids::WorkspaceInstanceId(1),
            &single_cmd_workspace("alpha"),
            &settings,
            &env,
            super::find_exe,
        )
        .expect("alpha workspace should open");
        let beta = WorkspaceInstance::open(
            wtd_core::ids::WorkspaceInstanceId(2),
            &single_cmd_workspace("beta"),
            &settings,
            &env,
            super::find_exe,
        )
        .expect("beta workspace should open");

        let alpha_session = alpha.sessions().keys().next().expect("alpha session id").0;
        let beta_session = beta.sessions().keys().next().expect("beta session id").0;
        assert_eq!(alpha_session, 1, "expected overlapping test session ids");
        assert_eq!(beta_session, 1, "expected overlapping test session ids");

        let handler = HostRequestHandler::new(settings);
        {
            let mut state = handler.state.lock().unwrap();
            state.workspaces.insert("alpha".into(), alpha);
            state.workspaces.insert("beta".into(), beta);
        }

        let mut prev_titles = HashMap::new();
        let mut prev_progress = HashMap::new();
        std::thread::sleep(Duration::from_millis(200));
        let _ = handler.drain_session_events(&mut prev_titles, &mut prev_progress);

        let marker = "WTD_SESSION_SCOPE_TEST_7QH2";
        handler.handle_session_input(&wtd_ipc::message::SessionInput {
            workspace: "beta".into(),
            session_id: "1".into(),
            data: encode_b64(format!("echo {marker}\r").as_bytes()),
        });

        std::thread::sleep(Duration::from_millis(250));
        let events = handler.drain_session_events(&mut prev_titles, &mut prev_progress);

        let mut saw_beta_marker = false;
        let mut saw_alpha_marker = false;
        for event in events {
            if let BroadcastEvent::Output {
                workspace, data, ..
            } = event
            {
                let text = String::from_utf8_lossy(&data);
                if text.contains(marker) {
                    if workspace == "beta" {
                        saw_beta_marker = true;
                    }
                    if workspace == "alpha" {
                        saw_alpha_marker = true;
                    }
                }
            }
        }

        assert!(
            saw_beta_marker,
            "expected scoped input output in beta workspace"
        );
        assert!(!saw_alpha_marker, "input leaked into alpha workspace");
    }
}
