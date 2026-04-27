//! Workspace instance management (§26, §27.2).
//!
//! A [`WorkspaceInstance`] owns the runtime state for one running workspace:
//! layout trees, sessions, pane attachments, and a Windows Job Object for
//! child-process cleanup.

use std::collections::HashMap;

use wtd_core::global_settings::GlobalSettings;
use wtd_core::ids::{PaneId, SessionId, TabId, WorkspaceInstanceId};
use wtd_core::layout::LayoutTree;
use wtd_core::workspace::{
    PaneDriverDefinition, PaneLeaf, PaneNode, RestartPolicy, SessionLaunchDefinition,
    TabDefinition, TerminalSizeDefinition, WorkspaceDefinition,
};
use wtd_core::{resolve_launch_spec, ResolveError};
use wtd_ipc::message::AttentionState;
use wtd_pty::PtySize;

use crate::output_broadcaster::progress_info_from_screen;
use crate::prompt_driver::{
    infer_pane_driver_profile, resolve_pane_driver, resolve_pane_driver_with_inference,
    EffectivePaneDriver,
};
use crate::session::{Session, SessionConfig, SessionState};

#[cfg(windows)]
use wtd_pty::JobObject;

const DEFAULT_PTY_COLS: u16 = 80;
const DEFAULT_PTY_ROWS: u16 = 24;

// ── Workspace state machine (§27.2) ─────────────────────────────────────────

/// Lifecycle state of a workspace instance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceState {
    /// Sessions are being launched.
    Creating,
    /// All initial sessions have been attempted. Workspace is operational.
    Active,
    /// Sessions are being terminated and resources released.
    Closing,
    /// Instance is destroyed.
    Closed,
    /// Existing instance is being torn down before re-creation.
    Recreating,
}

// ── Per-pane state ──────────────────────────────────────────────────────────

/// Attachment state of a pane to a session (§29.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PaneState {
    /// Session is running and attached to this pane.
    Attached {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
    },
    /// Session failed to launch; error is displayed in the pane.
    Detached { error: String },
}

// ── Tab instance ────────────────────────────────────────────────────────────

/// Runtime state for one tab in a workspace.
pub struct TabInstance {
    id: TabId,
    name: String,
    layout: LayoutTree,
}

impl TabInstance {
    pub fn id(&self) -> &TabId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub fn layout(&self) -> &LayoutTree {
        &self.layout
    }

    pub fn layout_mut(&mut self) -> &mut LayoutTree {
        &mut self.layout
    }
}

// ── Error ───────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace is in {0:?} state, cannot perform this operation")]
    InvalidState(WorkspaceState),

    #[error("job object creation failed: {0}")]
    JobObject(String),

    #[error("pane \"{0}\" not found")]
    PaneNotFound(String),

    #[error("session operation failed: {0}")]
    SessionOperation(String),

    #[error("profile resolution failed: {0}")]
    ProfileResolution(#[from] ResolveError),
}

// ── Internal pane record ────────────────────────────────────────────────────

struct PaneRecord {
    name: String,
    state: PaneState,
    original_def: Option<SessionLaunchDefinition>,
    driver: EffectivePaneDriver,
    attention: AttentionRecord,
    metadata: PaneMetadataRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionRecord {
    pub state: AttentionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Default for AttentionRecord {
    fn default() -> Self {
        Self {
            state: AttentionState::Active,
            message: None,
            source: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneMetadataRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_pending: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

// ── WorkspaceInstance ───────────────────────────────────────────────────────

/// A running workspace instance (§26, §27.2).
///
/// Owns the layout trees, sessions, pane-to-session attachments, and (on
/// Windows) a Job Object that ensures child processes are killed if the host
/// exits unexpectedly.
pub struct WorkspaceInstance {
    id: WorkspaceInstanceId,
    name: String,
    state: WorkspaceState,
    tabs: Vec<TabInstance>,
    active_tab_index: usize,
    next_pane_id: u64,
    sessions: HashMap<SessionId, Session>,
    panes: HashMap<PaneId, PaneRecord>,
    #[cfg(windows)]
    job: Option<JobObject>,
    default_size: PtySize,
    next_session_id: u64,
    next_tab_id: u64,
}

impl WorkspaceInstance {
    /// Create and start a workspace instance from a definition (§26.1).
    ///
    /// Depth-first traverses the layout to create one session per pane.
    /// If some sessions fail, the workspace still opens with those panes in
    /// `Detached` state (§29.2–29.3).
    pub fn open(
        id: WorkspaceInstanceId,
        workspace_def: &WorkspaceDefinition,
        global_settings: &GlobalSettings,
        host_env: &HashMap<String, String>,
        find_exe: impl Fn(&str) -> bool,
    ) -> Result<Self, WorkspaceError> {
        let mut inst = Self {
            id,
            name: workspace_def.name.clone(),
            state: WorkspaceState::Creating,
            tabs: Vec::new(),
            active_tab_index: 0,
            next_pane_id: 1,
            sessions: HashMap::new(),
            panes: HashMap::new(),
            #[cfg(windows)]
            job: None,
            default_size: PtySize::new(DEFAULT_PTY_COLS, DEFAULT_PTY_ROWS),
            next_session_id: 1,
            next_tab_id: 1,
        };
        inst.populate(workspace_def, global_settings, host_env, &find_exe)?;
        Ok(inst)
    }

    /// Attach to an existing instance — returns current state (§26.2).
    ///
    /// This is a read-only snapshot; the caller uses the returned data to
    /// set up its UI.
    pub fn attach_snapshot(&self) -> AttachSnapshot {
        AttachSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            state: self.state.clone(),
            active_tab_index: self.active_tab_index,
            tabs: self
                .tabs
                .iter()
                .map(|t| TabSnapshot {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    panes: t.layout.panes(),
                    focus: self
                        .panes
                        .get(&t.layout.focus())
                        .map(|rec| rec.name.clone()),
                    layout: t.layout.to_pane_node(|pane_id| {
                        if let Some(rec) = self.panes.get(pane_id) {
                            PaneLeaf {
                                name: rec.name.clone(),
                                session: None,
                            }
                        } else {
                            PaneLeaf {
                                name: format!("pane-{}", pane_id),
                                session: None,
                            }
                        }
                    }),
                })
                .collect(),
            pane_states: self
                .panes
                .iter()
                .map(|(id, rec)| (id.clone(), rec.state.clone()))
                .collect(),
            pane_attention: self
                .panes
                .iter()
                .map(|(id, rec)| (id.clone(), rec.attention.clone()))
                .collect(),
            pane_metadata: self
                .panes
                .iter()
                .map(|(id, rec)| {
                    let mut metadata = serde_json::to_value(&rec.metadata)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = metadata.as_object_mut() {
                        if let PaneState::Attached { session_id } = &rec.state {
                            if let Some(session) = self.sessions.get(session_id) {
                                obj.insert(
                                    "cwd".to_string(),
                                    session
                                        .config()
                                        .cwd
                                        .as_ref()
                                        .map(|cwd| serde_json::Value::String(cwd.clone()))
                                        .unwrap_or(serde_json::Value::Null),
                                );
                                obj.insert(
                                    "progress".to_string(),
                                    serde_json::to_value(progress_info_from_screen(
                                        session.screen().progress(),
                                    ))
                                    .unwrap_or(serde_json::Value::Null),
                                );
                            }
                        }
                        obj.insert(
                            "driverProfile".to_string(),
                            serde_json::Value::String(rec.driver.profile.clone()),
                        );
                    }
                    (id.clone(), metadata)
                })
                .collect(),
            workspace_attention: self.workspace_attention(),
            session_states: self
                .sessions
                .iter()
                .map(|(id, s)| (id.clone(), s.state().clone()))
                .collect(),
            session_titles: self
                .sessions
                .iter()
                .map(|(id, s)| (id.clone(), s.screen().title.clone()))
                .collect(),
            session_progress: self
                .sessions
                .iter()
                .filter_map(|(id, s)| {
                    progress_info_from_screen(s.screen().progress()).map(|progress| {
                        (
                            id.clone(),
                            SessionProgressSnapshot {
                                state: progress.state,
                                value: progress.value,
                            },
                        )
                    })
                })
                .collect(),
            session_sizes: self
                .sessions
                .iter()
                .map(|(id, s)| {
                    (
                        id.clone(),
                        SessionSizeSnapshot {
                            cols: s.screen().cols().try_into().unwrap_or(u16::MAX),
                            rows: s.screen().rows().try_into().unwrap_or(u16::MAX),
                        },
                    )
                })
                .collect(),
            session_screens: self
                .sessions
                .iter()
                .map(|(id, s)| {
                    let vt = s.screen().to_vt_snapshot();
                    (id.clone(), crate::output_broadcaster::encode_base64(&vt))
                })
                .collect(),
            session_history: self
                .sessions
                .iter()
                .filter_map(|(id, s)| {
                    let scrollback_rows = s.screen().scrollback_len();
                    if scrollback_rows == 0 {
                        return None;
                    }
                    let vt = s.screen().to_vt_scrollback();
                    Some((
                        id.clone(),
                        SessionHistorySnapshot {
                            scrollback_rows: u32::try_from(scrollback_rows).unwrap_or(u32::MAX),
                            scrollback_vt: crate::output_broadcaster::encode_base64(&vt),
                        },
                    ))
                })
                .collect(),
        }
    }

    /// Close the workspace: terminate all sessions and release resources (§26.3 --kill).
    pub fn close(&mut self) {
        self.state = WorkspaceState::Closing;
        self.tear_down();
        self.state = WorkspaceState::Closed;
    }

    /// Recreate the workspace from its definition (§26.4).
    pub fn recreate(
        &mut self,
        workspace_def: &WorkspaceDefinition,
        global_settings: &GlobalSettings,
        host_env: &HashMap<String, String>,
        find_exe: impl Fn(&str) -> bool,
    ) -> Result<(), WorkspaceError> {
        self.state = WorkspaceState::Recreating;
        self.tear_down();
        self.name = workspace_def.name.clone();
        self.populate(workspace_def, global_settings, host_env, &find_exe)?;
        Ok(())
    }

    /// Save the current layout and session config as a workspace definition (§26.5).
    pub fn save(&self) -> WorkspaceDefinition {
        let tabs: Vec<TabDefinition> = self
            .tabs
            .iter()
            .map(|tab| {
                let layout = tab.layout.to_pane_node(|pane_id| {
                    if let Some(rec) = self.panes.get(pane_id) {
                        PaneLeaf {
                            name: rec.name.clone(),
                            session: rec.original_def.clone(),
                        }
                    } else {
                        PaneLeaf {
                            name: format!("pane-{}", pane_id),
                            session: None,
                        }
                    }
                });
                TabDefinition {
                    name: tab.name.clone(),
                    layout,
                    focus: self
                        .panes
                        .get(&tab.layout.focus())
                        .map(|rec| rec.name.clone()),
                }
            })
            .collect();

        WorkspaceDefinition {
            version: 1,
            name: self.name.clone(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(tabs),
        }
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    pub fn id(&self) -> &WorkspaceInstanceId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> &WorkspaceState {
        &self.state
    }

    pub fn tabs(&self) -> &[TabInstance] {
        &self.tabs
    }

    pub fn tabs_mut(&mut self) -> &mut Vec<TabInstance> {
        &mut self.tabs
    }

    pub fn session(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn session_mut(&mut self, id: &SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    pub fn sessions(&self) -> &HashMap<SessionId, Session> {
        &self.sessions
    }

    pub fn sessions_mut(&mut self) -> &mut HashMap<SessionId, Session> {
        &mut self.sessions
    }

    pub fn pane_state(&self, id: &PaneId) -> Option<&PaneState> {
        self.panes.get(id).map(|r| &r.state)
    }

    pub fn pane_name(&self, id: &PaneId) -> Option<&str> {
        self.panes.get(id).map(|r| r.name.as_str())
    }

    pub fn pane_driver(&self, id: &PaneId) -> Option<&EffectivePaneDriver> {
        self.panes.get(id).map(|r| &r.driver)
    }

    pub fn pane_for_session(&self, session_id: &SessionId) -> Option<PaneId> {
        self.panes
            .iter()
            .find_map(|(pane_id, rec)| match &rec.state {
                PaneState::Attached {
                    session_id: pane_session_id,
                } if pane_session_id == session_id => Some(pane_id.clone()),
                _ => None,
            })
    }

    pub fn pane_attention(&self, id: &PaneId) -> Option<&AttentionRecord> {
        self.panes.get(id).map(|r| &r.attention)
    }

    pub fn pane_metadata(&self, id: &PaneId) -> Option<&PaneMetadataRecord> {
        self.panes.get(id).map(|r| &r.metadata)
    }

    pub fn set_pane_metadata(
        &mut self,
        pane_id: &PaneId,
        phase: Option<String>,
        status_text: Option<String>,
        queue_pending: Option<u32>,
        completion: Option<String>,
        source: Option<String>,
    ) -> Result<PaneMetadataRecord, WorkspaceError> {
        let Some(rec) = self.panes.get_mut(pane_id) else {
            return Err(WorkspaceError::PaneNotFound(format!("{}", pane_id.0)));
        };
        if phase.is_some() {
            rec.metadata.phase = phase;
        }
        if status_text.is_some() {
            rec.metadata.status_text = status_text;
        }
        if queue_pending.is_some() {
            rec.metadata.queue_pending = queue_pending;
        }
        if completion.is_some() {
            rec.metadata.completion = completion;
        }
        if source.is_some() {
            rec.metadata.source = source;
        }
        Ok(rec.metadata.clone())
    }

    pub fn workspace_attention(&self) -> AttentionRecord {
        let mut aggregate = AttentionRecord::default();
        for rec in self.panes.values() {
            match rec.attention.state {
                AttentionState::Error => return rec.attention.clone(),
                AttentionState::NeedsAttention => {
                    if aggregate.state != AttentionState::NeedsAttention {
                        aggregate = rec.attention.clone();
                    }
                }
                AttentionState::Done => {
                    if aggregate.state == AttentionState::Active {
                        aggregate = rec.attention.clone();
                    }
                }
                AttentionState::Active => {}
            }
        }
        aggregate
    }

    pub fn set_pane_attention(
        &mut self,
        pane_id: &PaneId,
        state: AttentionState,
        message: Option<String>,
        source: Option<String>,
    ) -> Result<AttentionRecord, WorkspaceError> {
        let Some(rec) = self.panes.get_mut(pane_id) else {
            return Err(WorkspaceError::PaneNotFound(format!("{}", pane_id.0)));
        };
        rec.attention = AttentionRecord {
            state,
            message,
            source,
        };
        Ok(rec.attention.clone())
    }

    pub fn clear_pane_attention(
        &mut self,
        pane_id: &PaneId,
    ) -> Result<AttentionRecord, WorkspaceError> {
        self.set_pane_attention(pane_id, AttentionState::Active, None, None)
    }

    pub fn pane_original_def(&self, id: &PaneId) -> Option<&Option<SessionLaunchDefinition>> {
        self.panes.get(id).map(|r| &r.original_def)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn running_session_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|s| *s.state() == SessionState::Running)
            .count()
    }

    pub fn failed_pane_count(&self) -> usize {
        self.panes
            .values()
            .filter(|r| matches!(r.state, PaneState::Detached { .. }))
            .count()
    }

    pub fn active_tab_index(&self) -> usize {
        self.active_tab_index
    }

    pub fn set_active_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab_index = index;
        }
    }

    fn alloc_workspace_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        id
    }

    /// Add a new empty tab and return a reference to it.
    pub fn add_tab(&mut self, name: String) -> &TabInstance {
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let mut layout = LayoutTree::new();
        layout.reassign_pane_ids(|| self.alloc_workspace_pane_id());
        let pane_id = layout.focus();
        let pane_name = format!("pane-{}", pane_id.0);
        self.panes.insert(
            pane_id,
            PaneRecord {
                name: pane_name,
                state: PaneState::Detached {
                    error: "pending session".to_string(),
                },
                original_def: None,
                driver: resolve_pane_driver(None, None),
                attention: AttentionRecord::default(),
                metadata: PaneMetadataRecord::default(),
            },
        );
        self.tabs.push(TabInstance {
            id: tab_id,
            name,
            layout,
        });
        self.active_tab_index = self.tabs.len() - 1;
        self.tabs.last().unwrap()
    }

    /// Close a tab by index, stopping all its sessions.
    pub fn close_tab(&mut self, tab_index: usize) {
        if tab_index >= self.tabs.len() || self.tabs.len() <= 1 {
            return; // Don't close the last tab.
        }
        let tab = &self.tabs[tab_index];
        let pane_ids: Vec<PaneId> = tab.layout().panes();
        for pane_id in &pane_ids {
            self.stop_pane_session(pane_id);
            self.panes.remove(pane_id);
        }
        self.tabs.remove(tab_index);
        if self.active_tab_index >= self.tabs.len() {
            self.active_tab_index = self.tabs.len() - 1;
        } else if self.active_tab_index > tab_index {
            self.active_tab_index -= 1;
        }
    }

    pub fn rename_tab(&mut self, tab_index: usize, name: String) {
        if let Some(tab) = self.tabs.get_mut(tab_index) {
            tab.set_name(name);
        }
    }

    // ── Target-resolution helpers ────────────────────────────────────────────

    /// Find a tab by name.
    pub fn find_tab_by_name(&self, name: &str) -> Option<&TabInstance> {
        self.tabs.iter().find(|t| t.name() == name)
    }

    /// Find a pane by name within a specific tab.
    pub fn find_pane_in_tab(&self, tab: &TabInstance, pane_name: &str) -> Option<PaneId> {
        for pane_id in tab.layout().panes() {
            if self.pane_name(&pane_id).map_or(false, |n| n == pane_name) {
                return Some(pane_id);
            }
        }
        None
    }

    /// Find all panes matching a name, returning `(PaneId, canonical_path)` pairs.
    ///
    /// Canonical path format: `workspace/tab/pane`.
    pub fn find_all_panes_by_name(&self, name: &str) -> Vec<(PaneId, String)> {
        let mut results = Vec::new();
        for tab in &self.tabs {
            for pane_id in tab.layout().panes() {
                if self.pane_name(&pane_id).map_or(false, |n| n == name) {
                    let path = format!("{}/{}/{}", self.name, tab.name(), name);
                    results.push((pane_id, path));
                }
            }
        }
        results
    }

    /// Build the canonical path for a pane (`workspace/tab/pane`).
    pub fn canonical_pane_path(&self, pane_id: &PaneId) -> Option<String> {
        let pane_name = self.pane_name(pane_id)?;
        for tab in &self.tabs {
            if tab.layout().panes().contains(pane_id) {
                return Some(format!("{}/{}/{}", self.name, tab.name(), pane_name));
            }
        }
        None
    }

    // ── Action-support methods ────────────────────────────────────────────────

    /// Stop the session attached to a pane (if any).
    pub fn stop_pane_session(&mut self, pane_id: &PaneId) {
        if let Some(rec) = self.panes.get(pane_id) {
            if let PaneState::Attached { session_id } = &rec.state {
                let sid = session_id.clone();
                if let Some(session) = self.sessions.get_mut(&sid) {
                    session.stop();
                }
                self.sessions.remove(&sid);
            }
        }
    }

    /// Remove pane record (after layout removal).
    pub fn remove_pane(&mut self, pane_id: &PaneId) {
        self.panes.remove(pane_id);
    }

    /// Find a pane by name (across all tabs).
    pub fn find_pane_by_name(&self, name: &str) -> Option<PaneId> {
        for (id, rec) in &self.panes {
            if rec.name == name {
                return Some(id.clone());
            }
        }
        None
    }

    /// Rename a pane.
    pub fn rename_pane(&mut self, pane_id: &PaneId, new_name: String) {
        if let Some(rec) = self.panes.get_mut(pane_id) {
            rec.name = new_name;
        }
    }

    pub fn set_pane_driver(
        &mut self,
        pane_id: &PaneId,
        driver_definition: Option<PaneDriverDefinition>,
        effective_driver: EffectivePaneDriver,
    ) -> Result<(), WorkspaceError> {
        let session_id = match self.panes.get_mut(pane_id) {
            Some(rec) => {
                rec.driver = effective_driver.clone();
                let session_id = match &rec.state {
                    PaneState::Attached { session_id } => Some(session_id.clone()),
                    PaneState::Detached { .. } => None,
                };
                match (rec.original_def.as_mut(), driver_definition.clone()) {
                    (Some(def), driver) => def.driver = driver,
                    (None, Some(driver)) => {
                        rec.original_def = Some(SessionLaunchDefinition {
                            driver: Some(driver),
                            ..Default::default()
                        });
                    }
                    (None, None) => {}
                }
                if rec.original_def.as_ref() == Some(&SessionLaunchDefinition::default()) {
                    rec.original_def = None;
                }
                session_id
            }
            None => {
                return Err(WorkspaceError::PaneNotFound(format!("{}", pane_id.0)));
            }
        };

        if let Some(session_id) = session_id {
            let session = self
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| WorkspaceError::SessionOperation("session not found".to_string()))?;
            session.config_mut().driver = effective_driver;
        }

        Ok(())
    }

    /// Resize the PTY/screen associated with a pane.
    pub fn resize_pane_session(
        &mut self,
        pane_id: &PaneId,
        cols: u16,
        rows: u16,
    ) -> Result<(), WorkspaceError> {
        let session_id = match self.panes.get(pane_id) {
            Some(rec) => match &rec.state {
                PaneState::Attached { session_id } => session_id.clone(),
                PaneState::Detached { .. } => {
                    return Err(WorkspaceError::SessionOperation(
                        "pane is detached".to_string(),
                    ));
                }
            },
            None => {
                return Err(WorkspaceError::PaneNotFound(format!("{}", pane_id.0)));
            }
        };

        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| WorkspaceError::SessionOperation("session not found".to_string()))?;

        session
            .resize(cols, rows)
            .map_err(|e| WorkspaceError::SessionOperation(format!("resize failed: {}", e)))
    }

    /// Restart the session in a pane.
    pub fn restart_pane_session(&mut self, pane_id: &PaneId) -> Result<(), WorkspaceError> {
        let session_id = match self.panes.get(pane_id) {
            Some(rec) => match &rec.state {
                PaneState::Attached { session_id } => session_id.clone(),
                PaneState::Detached { .. } => {
                    return Err(WorkspaceError::InvalidState(WorkspaceState::Active));
                }
            },
            None => return Err(WorkspaceError::InvalidState(WorkspaceState::Active)),
        };
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.stop();
            session
                .restart()
                .map_err(|e| WorkspaceError::JobObject(format!("restart failed: {}", e)))?;
        }
        Ok(())
    }

    /// Estimate an effective viewport from the largest known session dimensions.
    ///
    /// Used by the action dispatcher as a fallback when exact UI geometry
    /// isn't available in this runtime layer.
    pub fn estimated_viewport_size(&self) -> Option<(u16, u16)> {
        let mut cols = 0u16;
        let mut rows = 0u16;

        for session in self.sessions.values() {
            cols = cols.max(session.screen().cols().try_into().ok()?);
            rows = rows.max(session.screen().rows().try_into().ok()?);
        }

        if cols == 0 || rows == 0 {
            None
        } else {
            Some((cols, rows))
        }
    }

    /// Spawn a session for a pane created by a split action.
    ///
    /// Uses the default profile from `global_settings`. Registers the pane
    /// record and session; on failure the pane is recorded as `Detached`.
    pub fn spawn_session_for_pane(
        &mut self,
        pane_id: &PaneId,
        pane_name: String,
        global_settings: &GlobalSettings,
        host_env: &HashMap<String, String>,
        find_exe: &impl Fn(&str) -> bool,
    ) {
        self.spawn_session_for_pane_with_definition(
            pane_id,
            pane_name,
            SessionLaunchDefinition::default(),
            global_settings,
            host_env,
            find_exe,
        );
    }

    /// Spawn a session for a pane using an explicit launch definition template.
    pub fn spawn_session_for_pane_with_definition(
        &mut self,
        pane_id: &PaneId,
        pane_name: String,
        session_def: SessionLaunchDefinition,
        global_settings: &GlobalSettings,
        host_env: &HashMap<String, String>,
        find_exe: &impl Fn(&str) -> bool,
    ) {
        // Minimal workspace def — split panes use the global default profile.
        let workspace_def = WorkspaceDefinition {
            version: 1,
            name: self.name.clone(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: None,
        };
        let resolved = match resolve_launch_spec(
            &session_def,
            &workspace_def,
            global_settings,
            host_env,
            find_exe,
        ) {
            Ok(r) => r,
            Err(e) => {
                let driver = resolve_pane_driver_with_inference(
                    Some(&session_def),
                    Some(&workspace_def),
                    infer_pane_driver_profile(session_def.startup_command.as_deref(), None),
                );
                self.panes.insert(
                    pane_id.clone(),
                    PaneRecord {
                        name: pane_name,
                        state: PaneState::Detached {
                            error: e.to_string(),
                        },
                        original_def: Some(session_def),
                        driver,
                        attention: AttentionRecord::default(),
                        metadata: PaneMetadataRecord::default(),
                    },
                );
                return;
            }
        };
        let driver = resolve_pane_driver_with_inference(
            Some(&session_def),
            Some(&workspace_def),
            infer_pane_driver_profile(
                session_def.startup_command.as_deref(),
                Some(&resolved.executable),
            ),
        );

        let session_id = SessionId(self.next_session_id);
        self.next_session_id += 1;
        let mut session_env = resolved.env;
        apply_runtime_terminal_env(
            &mut session_env,
            &self.name,
            &pane_name,
            &session_id,
            &driver,
        );

        let config = SessionConfig {
            executable: resolved.executable,
            args: resolved.args,
            cwd: resolved.cwd,
            env: session_env,
            restart_policy: RestartPolicy::Never,
            startup_command: session_def.startup_command.clone(),
            size: session_def
                .terminal_size
                .as_ref()
                .map(pty_size_from_definition)
                .unwrap_or(self.default_size),
            name: pane_name.clone(),
            max_scrollback: 10_000,
            driver: driver.clone(),
        };

        let mut session = Session::new(session_id.clone(), config);

        match session.start() {
            Ok(()) => {
                #[cfg(windows)]
                {
                    self.add_to_job(&session);
                }
                self.panes.insert(
                    pane_id.clone(),
                    PaneRecord {
                        name: pane_name,
                        state: PaneState::Attached {
                            session_id: session_id.clone(),
                        },
                        original_def: Some(session_def.clone()),
                        driver,
                        attention: AttentionRecord::default(),
                        metadata: PaneMetadataRecord::default(),
                    },
                );
            }
            Err(e) => {
                self.panes.insert(
                    pane_id.clone(),
                    PaneRecord {
                        name: pane_name,
                        state: PaneState::Detached {
                            error: e.to_string(),
                        },
                        original_def: Some(session_def.clone()),
                        driver,
                        attention: AttentionRecord::default(),
                        metadata: PaneMetadataRecord::default(),
                    },
                );
            }
        }

        self.sessions.insert(session_id, session);
    }

    /// Create a minimal instance for testing (no real sessions or job objects).
    #[cfg(test)]
    pub fn new_for_test(name: &str) -> Self {
        use wtd_core::layout::LayoutTree;

        let mut inst = Self {
            id: WorkspaceInstanceId(1),
            name: name.to_string(),
            state: WorkspaceState::Active,
            tabs: Vec::new(),
            active_tab_index: 0,
            next_pane_id: 1,
            sessions: HashMap::new(),
            panes: HashMap::new(),
            #[cfg(windows)]
            job: None,
            default_size: PtySize::new(DEFAULT_PTY_COLS, DEFAULT_PTY_ROWS),
            next_session_id: 1,
            next_tab_id: 1,
        };

        let tab_id = TabId(inst.next_tab_id);
        inst.next_tab_id += 1;

        let mut layout = LayoutTree::new();
        layout.reassign_pane_ids(|| inst.alloc_workspace_pane_id());
        let pane_id = layout.focus();

        inst.panes.insert(
            pane_id,
            PaneRecord {
                name: "default".to_string(),
                state: PaneState::Detached {
                    error: "test mode".to_string(),
                },
                original_def: None,
                driver: resolve_pane_driver(None, None),
                attention: AttentionRecord::default(),
                metadata: PaneMetadataRecord::default(),
            },
        );

        inst.tabs.push(TabInstance {
            id: tab_id,
            name: "main".to_string(),
            layout,
        });

        inst.state = WorkspaceState::Active;
        inst
    }

    /// Create an instance with multiple tabs and named panes for testing.
    ///
    /// `tab_specs` is `[(tab_name, [pane_name, ...])]`. Panes are created
    /// via right-splits within each tab. No real sessions or job objects.
    #[cfg(test)]
    pub(crate) fn new_for_test_multi(
        name: &str,
        instance_id: u64,
        tab_specs: &[(&str, &[&str])],
    ) -> Self {
        use wtd_core::layout::LayoutTree;

        let mut inst = Self {
            id: WorkspaceInstanceId(instance_id),
            name: name.to_string(),
            state: WorkspaceState::Active,
            tabs: Vec::new(),
            active_tab_index: 0,
            next_pane_id: 1,
            sessions: HashMap::new(),
            panes: HashMap::new(),
            #[cfg(windows)]
            job: None,
            default_size: PtySize::new(DEFAULT_PTY_COLS, DEFAULT_PTY_ROWS),
            next_session_id: 1,
            next_tab_id: 1,
        };

        for (tab_name, pane_names) in tab_specs {
            assert!(!pane_names.is_empty(), "tab must have at least one pane");

            let tab_id = TabId(inst.next_tab_id);
            inst.next_tab_id += 1;

            let mut layout = LayoutTree::new();
            layout.reassign_pane_ids(|| inst.alloc_workspace_pane_id());
            let first_pane = layout.focus();
            inst.panes.insert(
                first_pane.clone(),
                PaneRecord {
                    name: pane_names[0].to_string(),
                    state: PaneState::Detached {
                        error: "test".to_string(),
                    },
                    original_def: None,
                    driver: resolve_pane_driver(None, None),
                    attention: AttentionRecord::default(),
                    metadata: PaneMetadataRecord::default(),
                },
            );

            let mut last_pane = first_pane;
            for pane_name in &pane_names[1..] {
                let new_pane = layout.split_right(last_pane).expect("split_right in test");
                inst.panes.insert(
                    new_pane.clone(),
                    PaneRecord {
                        name: pane_name.to_string(),
                        state: PaneState::Detached {
                            error: "test".to_string(),
                        },
                        original_def: None,
                        driver: resolve_pane_driver(None, None),
                        attention: AttentionRecord::default(),
                        metadata: PaneMetadataRecord::default(),
                    },
                );
                last_pane = new_pane;
            }

            inst.tabs.push(TabInstance {
                id: tab_id,
                name: tab_name.to_string(),
                layout,
            });
        }

        inst
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    fn tear_down(&mut self) {
        for session in self.sessions.values_mut() {
            session.stop();
        }
        self.sessions.clear();
        self.tabs.clear();
        self.panes.clear();
        #[cfg(windows)]
        {
            self.job = None;
        }
    }

    /// Create job object, build tabs, resolve profiles, create sessions.
    fn populate(
        &mut self,
        workspace_def: &WorkspaceDefinition,
        global_settings: &GlobalSettings,
        host_env: &HashMap<String, String>,
        find_exe: &impl Fn(&str) -> bool,
    ) -> Result<(), WorkspaceError> {
        self.state = WorkspaceState::Creating;

        #[cfg(windows)]
        {
            self.job =
                Some(JobObject::new().map_err(|e| WorkspaceError::JobObject(e.to_string()))?);
        }

        let restart_policy = workspace_def
            .defaults
            .as_ref()
            .and_then(|d| d.restart_policy.clone())
            .unwrap_or(RestartPolicy::Never);
        self.default_size = workspace_def
            .defaults
            .as_ref()
            .and_then(|d| d.terminal_size.as_ref())
            .map(pty_size_from_definition)
            .unwrap_or_else(|| PtySize::new(DEFAULT_PTY_COLS, DEFAULT_PTY_ROWS));

        let max_scrollback = workspace_def
            .defaults
            .as_ref()
            .and_then(|d| d.scrollback_lines)
            .map(|s| s as usize)
            .unwrap_or(10_000);

        let tab_defs = collect_tabs(workspace_def);

        for tab_def in &tab_defs {
            let tab_id = TabId(self.next_tab_id);
            self.next_tab_id += 1;

            let (mut layout, pane_mappings) = LayoutTree::from_pane_node(&tab_def.layout);
            let pane_id_mapping = layout.reassign_pane_ids(|| self.alloc_workspace_pane_id());
            let pane_mappings: Vec<(String, PaneId)> = pane_mappings
                .into_iter()
                .map(|(name, pane_id)| {
                    let mapped = pane_id_mapping
                        .get(&pane_id)
                        .cloned()
                        .expect("pane remap must cover every layout pane");
                    (name, mapped)
                })
                .collect();
            let pane_defs = collect_pane_defs(&tab_def.layout);

            for ((pane_name, pane_id), (_, session_def)) in
                pane_mappings.iter().zip(pane_defs.iter())
            {
                let session_launch = session_def.as_ref().cloned().unwrap_or_default();
                let resolved = match resolve_launch_spec(
                    &session_launch,
                    workspace_def,
                    global_settings,
                    host_env,
                    find_exe,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        let driver = resolve_pane_driver_with_inference(
                            Some(&session_launch),
                            Some(workspace_def),
                            infer_pane_driver_profile(
                                session_launch.startup_command.as_deref(),
                                None,
                            ),
                        );
                        self.panes.insert(
                            pane_id.clone(),
                            PaneRecord {
                                name: pane_name.clone(),
                                state: PaneState::Detached {
                                    error: e.to_string(),
                                },
                                original_def: session_def.clone(),
                                driver,
                                attention: AttentionRecord::default(),
                                metadata: PaneMetadataRecord::default(),
                            },
                        );
                        continue;
                    }
                };
                let driver = resolve_pane_driver_with_inference(
                    Some(&session_launch),
                    Some(workspace_def),
                    infer_pane_driver_profile(
                        session_launch.startup_command.as_deref(),
                        Some(&resolved.executable),
                    ),
                );

                let session_id = SessionId(self.next_session_id);
                self.next_session_id += 1;
                let mut session_env = resolved.env;
                apply_runtime_terminal_env(
                    &mut session_env,
                    &self.name,
                    pane_name,
                    &session_id,
                    &driver,
                );

                let config = SessionConfig {
                    executable: resolved.executable,
                    args: resolved.args,
                    cwd: resolved.cwd,
                    env: session_env,
                    restart_policy: restart_policy.clone(),
                    startup_command: session_launch.startup_command.clone(),
                    size: session_launch
                        .terminal_size
                        .as_ref()
                        .map(pty_size_from_definition)
                        .unwrap_or(self.default_size),
                    name: pane_name.clone(),
                    max_scrollback,
                    driver: driver.clone(),
                };

                let mut session = Session::new(session_id.clone(), config);

                match session.start() {
                    Ok(()) => {
                        #[cfg(windows)]
                        {
                            self.add_to_job(&session);
                        }
                        self.panes.insert(
                            pane_id.clone(),
                            PaneRecord {
                                name: pane_name.clone(),
                                state: PaneState::Attached {
                                    session_id: session_id.clone(),
                                },
                                original_def: session_def.clone(),
                                driver,
                                attention: AttentionRecord::default(),
                                metadata: PaneMetadataRecord::default(),
                            },
                        );
                    }
                    Err(e) => {
                        self.panes.insert(
                            pane_id.clone(),
                            PaneRecord {
                                name: pane_name.clone(),
                                state: PaneState::Detached {
                                    error: e.to_string(),
                                },
                                original_def: session_def.clone(),
                                driver,
                                attention: AttentionRecord::default(),
                                metadata: PaneMetadataRecord::default(),
                            },
                        );
                    }
                }

                self.sessions.insert(session_id, session);
            }

            // Set focus to named pane if specified in the definition.
            if let Some(ref focus_name) = tab_def.focus {
                for (name, pane_id) in &pane_mappings {
                    if name == focus_name {
                        let _ = layout.set_focus(pane_id.clone());
                        break;
                    }
                }
            }

            self.tabs.push(TabInstance {
                id: tab_id,
                name: tab_def.name.clone(),
                layout,
            });
        }

        self.state = WorkspaceState::Active;
        Ok(())
    }

    #[cfg(windows)]
    fn add_to_job(&self, session: &Session) {
        if let Some(ref job) = self.job {
            if let Some(handle_raw) = session.process_handle_raw() {
                use windows::Win32::Foundation::HANDLE;
                let handle = HANDLE(handle_raw as *mut std::ffi::c_void);
                let _ = job.add_process(handle);
            }
        }
    }
}

// ── Attach snapshot ─────────────────────────────────────────────────────────

/// Read-only snapshot of a workspace instance for attach (§26.2).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachSnapshot {
    pub id: WorkspaceInstanceId,
    pub name: String,
    pub state: WorkspaceState,
    pub active_tab_index: usize,
    pub tabs: Vec<TabSnapshot>,
    pub pane_states: HashMap<PaneId, PaneState>,
    pub pane_attention: HashMap<PaneId, AttentionRecord>,
    pub pane_metadata: HashMap<PaneId, serde_json::Value>,
    pub workspace_attention: AttentionRecord,
    pub session_states: HashMap<SessionId, SessionState>,
    /// Current terminal title per session (OSC 2).
    pub session_titles: HashMap<SessionId, String>,
    /// Current terminal progress per session (OSC 9;4).
    pub session_progress: HashMap<SessionId, SessionProgressSnapshot>,
    /// Current visible terminal size per session.
    pub session_sizes: HashMap<SessionId, SessionSizeSnapshot>,
    /// Base64-encoded VT snapshot of the current visible screen per session.
    ///
    /// The UI can feed these bytes directly into `ScreenBuffer::advance()` to
    /// seed pane content immediately on attach, before any new output arrives.
    pub session_screens: HashMap<SessionId, String>,
    /// Replayable retained scrollback per session.
    ///
    /// The UI can replay `scrollback_vt` into a fresh `ScreenBuffer` before
    /// applying `session_screens` so newly attached panes preserve local history.
    pub session_history: HashMap<SessionId, SessionHistorySnapshot>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSizeSnapshot {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgressSnapshot {
    pub state: wtd_ipc::message::ProgressState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<u8>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistorySnapshot {
    pub scrollback_rows: u32,
    pub scrollback_vt: String,
}

/// Snapshot of a single tab's metadata and layout.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSnapshot {
    pub id: TabId,
    pub name: String,
    pub panes: Vec<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// Full layout tree as a serializable PaneNode (same schema as workspace YAML).
    pub layout: PaneNode,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Extract a flat list of tab definitions from either `tabs` or `windows`.
fn collect_tabs(def: &WorkspaceDefinition) -> Vec<&TabDefinition> {
    if let Some(ref tabs) = def.tabs {
        tabs.iter().collect()
    } else if let Some(ref windows) = def.windows {
        windows.iter().flat_map(|w| w.tabs.iter()).collect()
    } else {
        Vec::new()
    }
}

/// Collect `(pane_name, Option<SessionLaunchDefinition>)` in depth-first order.
fn collect_pane_defs(node: &PaneNode) -> Vec<(String, Option<SessionLaunchDefinition>)> {
    let mut result = Vec::new();
    collect_pane_defs_recursive(node, &mut result);
    result
}

fn collect_pane_defs_recursive(
    node: &PaneNode,
    result: &mut Vec<(String, Option<SessionLaunchDefinition>)>,
) {
    match node {
        PaneNode::Pane(leaf) => {
            result.push((leaf.name.clone(), leaf.session.clone()));
        }
        PaneNode::Split(split) => {
            for child in &split.children {
                collect_pane_defs_recursive(child, result);
            }
        }
    }
}

fn pty_size_from_definition(size: &TerminalSizeDefinition) -> PtySize {
    PtySize::new(size.cols.max(1), size.rows.max(1))
}

fn apply_runtime_terminal_env(
    env: &mut HashMap<String, String>,
    workspace_name: &str,
    pane_name: &str,
    session_id: &SessionId,
    driver: &crate::prompt_driver::EffectivePaneDriver,
) {
    let workspace_slug = terminal_identity_slug(workspace_name);
    let pane_slug = terminal_identity_slug(pane_name);

    env.insert("TERM_PROGRAM".to_string(), "Windows_Terminal".to_string());
    env.insert(
        "TERM_PROGRAM_VERSION".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert(
        "WT_SESSION".to_string(),
        format!("wtd-{}-{}", workspace_slug, session_id),
    );
    env.insert(
        "WT_PROFILE_ID".to_string(),
        format!("wtd://{}/{}", workspace_slug, pane_slug),
    );
    env.insert("WT_WINDOW_ID".to_string(), "1".to_string());

    env.insert("WTD_TERMINAL".to_string(), "1".to_string());
    env.insert("WTD_WORKSPACE".to_string(), workspace_name.to_string());
    env.insert("WTD_PANE".to_string(), pane_name.to_string());
    env.insert("WTD_SESSION_ID".to_string(), session_id.to_string());

    env.insert("WTD_AGENT_HOST".to_string(), "1".to_string());
    env.insert("WTD_AGENT_DRIVER".to_string(), driver.profile.clone());
    env.insert(
        "WTD_AGENT_MULTILINE_MODE".to_string(),
        driver.multiline_mode_name().to_string(),
    );
    env.insert(
        "WTD_AGENT_PASTE_MODE".to_string(),
        driver.paste_mode_name().to_string(),
    );
    env.insert(
        "WTD_AGENT_SUBMIT_KEY".to_string(),
        driver.submit_key.clone(),
    );
    env.insert("WTD_AGENT_HYPERLINKS".to_string(), "osc8".to_string());
    env.insert(
        "WTD_AGENT_IMAGES".to_string(),
        "kitty-placeholder".to_string(),
    );
    if let Some(soft_break_key) = &driver.soft_break_key {
        env.insert(
            "WTD_AGENT_SOFT_BREAK_KEY".to_string(),
            soft_break_key.clone(),
        );
    } else {
        env.remove("WTD_AGENT_SOFT_BREAK_KEY");
    }
}

fn terminal_identity_slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_was_sep = false;
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            last_was_sep = false;
            ch.to_ascii_lowercase()
        } else if !last_was_sep {
            last_was_sep = true;
            '-'
        } else {
            continue;
        };
        slug.push(normalized);
    }
    slug.trim_matches('-').to_string()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_core::layout::{Direction, Rect};
    use wtd_core::workspace::{Orientation, PaneLeaf, SplitNode};

    fn default_global_settings() -> GlobalSettings {
        GlobalSettings::default()
    }

    fn default_host_env() -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.entry("USERPROFILE".to_string())
            .or_insert_with(|| r"C:\".to_string());
        env
    }

    fn find_exe_windows(name: &str) -> bool {
        matches!(name, "cmd.exe" | "powershell.exe" | "pwsh.exe")
    }

    fn simple_workspace_def() -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "test-simple".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Pane(PaneLeaf {
                    name: "editor".to_string(),
                    session: Some(SessionLaunchDefinition {
                        profile: Some("cmd".to_string()),
                        startup_command: Some("echo hello".to_string()),
                        ..Default::default()
                    }),
                }),
                focus: None,
            }]),
        }
    }

    fn split_workspace_def() -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "test-split".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Split(SplitNode {
                    orientation: Orientation::Vertical,
                    ratio: Some(0.6),
                    children: vec![
                        PaneNode::Pane(PaneLeaf {
                            name: "top".to_string(),
                            session: Some(SessionLaunchDefinition {
                                profile: Some("cmd".to_string()),
                                ..Default::default()
                            }),
                        }),
                        PaneNode::Pane(PaneLeaf {
                            name: "bottom".to_string(),
                            session: Some(SessionLaunchDefinition {
                                profile: Some("cmd".to_string()),
                                ..Default::default()
                            }),
                        }),
                    ],
                }),
                focus: Some("bottom".to_string()),
            }]),
        }
    }

    fn dnacalc_pi_workspace_def() -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "DnaCalc-pi".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Split(SplitNode {
                    orientation: Orientation::Horizontal,
                    ratio: Some(0.5),
                    children: vec![
                        PaneNode::Split(SplitNode {
                            orientation: Orientation::Vertical,
                            ratio: Some(0.5),
                            children: vec![
                                PaneNode::Split(SplitNode {
                                    orientation: Orientation::Horizontal,
                                    ratio: Some(0.5),
                                    children: vec![
                                        PaneNode::Pane(PaneLeaf {
                                            name: "Foundation".to_string(),
                                            session: None,
                                        }),
                                        PaneNode::Pane(PaneLeaf {
                                            name: "OxReplay".to_string(),
                                            session: None,
                                        }),
                                    ],
                                }),
                                PaneNode::Pane(PaneLeaf {
                                    name: "OxXlPlay".to_string(),
                                    session: None,
                                }),
                            ],
                        }),
                        PaneNode::Split(SplitNode {
                            orientation: Orientation::Vertical,
                            ratio: Some(0.5),
                            children: vec![
                                PaneNode::Pane(PaneLeaf {
                                    name: "DnaOneCalc".to_string(),
                                    session: None,
                                }),
                                PaneNode::Split(SplitNode {
                                    orientation: Orientation::Horizontal,
                                    ratio: Some(0.5),
                                    children: vec![
                                        PaneNode::Pane(PaneLeaf {
                                            name: "OxFml".to_string(),
                                            session: None,
                                        }),
                                        PaneNode::Pane(PaneLeaf {
                                            name: "OxFunc".to_string(),
                                            session: None,
                                        }),
                                    ],
                                }),
                            ],
                        }),
                    ],
                }),
                focus: Some("Foundation".to_string()),
            }]),
        }
    }

    fn startup_command_workspace_def(startup_command: &str) -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "test-startup-command".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Pane(PaneLeaf {
                    name: "agent".to_string(),
                    session: Some(SessionLaunchDefinition {
                        profile: Some("cmd".to_string()),
                        startup_command: Some(startup_command.to_string()),
                        ..Default::default()
                    }),
                }),
                focus: None,
            }]),
        }
    }

    fn partial_failure_workspace_def() -> WorkspaceDefinition {
        use wtd_core::workspace::{ProfileDefinition, ProfileType};

        let mut profiles = HashMap::new();
        profiles.insert(
            "bad-exe".to_string(),
            ProfileDefinition {
                profile_type: ProfileType::Custom,
                executable: Some("nonexistent_exe_12345".to_string()),
                args: None,
                cwd: None,
                env: None,
                title: None,
                distribution: None,
                host: None,
                user: None,
                port: None,
                identity_file: None,
                use_agent: None,
                remote_command: None,
                scrollback_lines: None,
            },
        );

        WorkspaceDefinition {
            version: 1,
            name: "test-partial".to_string(),
            description: None,
            defaults: None,
            profiles: Some(profiles),
            bindings: None,
            windows: None,
            tabs: Some(vec![TabDefinition {
                name: "main".to_string(),
                layout: PaneNode::Split(SplitNode {
                    orientation: Orientation::Horizontal,
                    children: vec![
                        PaneNode::Pane(PaneLeaf {
                            name: "good".to_string(),
                            session: Some(SessionLaunchDefinition {
                                profile: Some("cmd".to_string()),
                                ..Default::default()
                            }),
                        }),
                        PaneNode::Pane(PaneLeaf {
                            name: "bad".to_string(),
                            session: Some(SessionLaunchDefinition {
                                profile: Some("bad-exe".to_string()),
                                ..Default::default()
                            }),
                        }),
                    ],
                    ratio: None,
                }),
                focus: None,
            }]),
        }
    }

    // ── collect_tabs / collect_pane_defs ─────────────────────────────────────

    #[test]
    fn collect_tabs_from_tabs_field() {
        let def = simple_workspace_def();
        let tabs = collect_tabs(&def);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "main");
    }

    #[test]
    fn collect_tabs_from_windows_field() {
        use wtd_core::workspace::WindowDefinition;

        let def = WorkspaceDefinition {
            version: 1,
            name: "multi-win".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: Some(vec![
                WindowDefinition {
                    name: Some("win1".to_string()),
                    tabs: vec![TabDefinition {
                        name: "tab1".to_string(),
                        layout: PaneNode::Pane(PaneLeaf {
                            name: "p1".to_string(),
                            session: None,
                        }),
                        focus: None,
                    }],
                },
                WindowDefinition {
                    name: Some("win2".to_string()),
                    tabs: vec![TabDefinition {
                        name: "tab2".to_string(),
                        layout: PaneNode::Pane(PaneLeaf {
                            name: "p2".to_string(),
                            session: None,
                        }),
                        focus: None,
                    }],
                },
            ]),
            tabs: None,
        };
        let tabs = collect_tabs(&def);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].name, "tab1");
        assert_eq!(tabs[1].name, "tab2");
    }

    #[test]
    fn collect_pane_defs_depth_first() {
        let node = PaneNode::Split(SplitNode {
            orientation: Orientation::Vertical,
            ratio: None,
            children: vec![
                PaneNode::Pane(PaneLeaf {
                    name: "a".to_string(),
                    session: None,
                }),
                PaneNode::Split(SplitNode {
                    orientation: Orientation::Horizontal,
                    ratio: None,
                    children: vec![
                        PaneNode::Pane(PaneLeaf {
                            name: "b".to_string(),
                            session: None,
                        }),
                        PaneNode::Pane(PaneLeaf {
                            name: "c".to_string(),
                            session: None,
                        }),
                    ],
                }),
            ],
        });
        let defs = collect_pane_defs(&node);
        let names: Vec<&str> = defs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn rename_tab_updates_selected_tab_name() {
        let mut inst = WorkspaceInstance::new_for_test_multi(
            "test-rename",
            1,
            &[("main", &["editor"]), ("logs", &["tail"])],
        );
        inst.rename_tab(1, "debug".to_string());
        assert_eq!(inst.tabs()[0].name(), "main");
        assert_eq!(inst.tabs()[1].name(), "debug");
    }

    #[test]
    fn pane_ids_are_unique_across_tabs() {
        let inst = WorkspaceInstance::new_for_test_multi(
            "test-pane-ids",
            1,
            &[("main", &["editor"]), ("logs", &["tail"])],
        );
        let first = inst.tabs()[0].layout().focus();
        let second = inst.tabs()[1].layout().focus();
        assert_ne!(first, second);
        assert!(inst.pane_state(&first).is_some());
        assert!(inst.pane_state(&second).is_some());
    }

    #[test]
    fn close_tab_before_active_shifts_active_index_left() {
        let mut inst = WorkspaceInstance::new_for_test_multi(
            "test-close-tab",
            1,
            &[("one", &["a"]), ("two", &["b"]), ("three", &["c"])],
        );
        inst.set_active_tab(2);
        inst.close_tab(1);
        assert_eq!(inst.active_tab_index(), 1);
        assert_eq!(inst.tabs()[inst.active_tab_index()].name(), "three");
    }

    // ── Integration tests (spawn real processes) ────────────────────────────

    #[cfg(windows)]
    #[test]
    fn open_simple_workspace_sessions_run() {
        let def = simple_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(1), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        assert_eq!(*inst.state(), WorkspaceState::Active);
        assert_eq!(inst.tabs().len(), 1);
        assert_eq!(inst.tabs()[0].name(), "main");
        assert_eq!(inst.session_count(), 1);

        // The session should be Running or Exited (cmd /c exits quickly).
        for (_id, session) in inst.sessions() {
            assert!(
                matches!(
                    session.state(),
                    SessionState::Running | SessionState::Exited { .. }
                ),
                "session state: {:?}",
                session.state()
            );
        }

        // Pane should be attached
        let pane_ids = inst.tabs()[0].layout().panes();
        assert_eq!(pane_ids.len(), 1);
        assert!(matches!(
            inst.pane_state(&pane_ids[0]),
            Some(PaneState::Attached { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn open_split_workspace_multiple_sessions() {
        let def = split_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(2), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        assert_eq!(*inst.state(), WorkspaceState::Active);
        assert_eq!(inst.session_count(), 2);

        // Both panes should be attached
        let pane_ids = inst.tabs()[0].layout().panes();
        assert_eq!(pane_ids.len(), 2);
        for pane_id in &pane_ids {
            assert!(
                matches!(inst.pane_state(pane_id), Some(PaneState::Attached { .. })),
                "pane {:?} should be attached",
                pane_id
            );
        }

        // Focus should be on "bottom" pane (second in depth-first order)
        assert_eq!(inst.tabs()[0].layout().focus(), pane_ids[1]);
    }

    #[cfg(windows)]
    #[test]
    fn partial_failure_other_sessions_run() {
        let def = partial_failure_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(3), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed despite partial failure");

        assert_eq!(*inst.state(), WorkspaceState::Active);
        // Two sessions attempted (one failed at start, one succeeded)
        assert_eq!(inst.session_count(), 2);

        // At least one pane should be attached, one detached
        let pane_ids = inst.tabs()[0].layout().panes();
        assert_eq!(pane_ids.len(), 2);

        let mut attached = 0;
        let mut detached = 0;
        for pane_id in &pane_ids {
            match inst.pane_state(pane_id) {
                Some(PaneState::Attached { .. }) => attached += 1,
                Some(PaneState::Detached { error }) => {
                    detached += 1;
                    assert!(!error.is_empty(), "detached pane should have error message");
                }
                None => panic!("pane {:?} has no state", pane_id),
            }
        }

        assert_eq!(attached, 1, "one pane should be attached");
        assert_eq!(detached, 1, "one pane should be detached");

        // The good session should be Running or Exited
        let running = inst.running_session_count();
        let failed = inst.failed_pane_count();
        let _ = running; // cmd.exe may have exited already
        assert_eq!(failed, 1);
    }

    #[cfg(windows)]
    #[test]
    fn close_workspace_terminates_sessions() {
        let def = simple_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(4), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        assert_eq!(*inst.state(), WorkspaceState::Active);

        inst.close();

        assert_eq!(*inst.state(), WorkspaceState::Closed);
        assert_eq!(inst.session_count(), 0);
        assert_eq!(inst.tabs().len(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn recreate_workspace_fresh_sessions() {
        let def = simple_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(5), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        // Capture original session IDs
        let original_ids: Vec<SessionId> = inst.sessions().keys().cloned().collect();
        assert_eq!(original_ids.len(), 1);

        inst.recreate(&def, &gs, &env, find_exe_windows)
            .expect("recreate should succeed");

        assert_eq!(*inst.state(), WorkspaceState::Active);
        assert_eq!(inst.session_count(), 1);
        assert_eq!(inst.tabs().len(), 1);

        // Session IDs should be different (fresh sessions)
        let new_ids: Vec<SessionId> = inst.sessions().keys().cloned().collect();
        assert_ne!(
            original_ids, new_ids,
            "recreate should produce new session IDs"
        );
    }

    #[cfg(windows)]
    #[test]
    fn save_reconstructs_definition() {
        let def = split_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(6), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let saved = inst.save();

        assert_eq!(saved.name, "test-split");
        assert_eq!(saved.version, 1);
        let tabs = saved.tabs.as_ref().expect("should have tabs");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "main");
        assert_eq!(tabs[0].focus.as_deref(), Some("bottom"));

        // The layout should be a split with two pane children
        match &tabs[0].layout {
            PaneNode::Split(split) => {
                assert_eq!(split.orientation, Orientation::Vertical);
                assert!(split.ratio.unwrap() > 0.5); // was 0.6
                assert_eq!(split.children.len(), 2);
                match &split.children[0] {
                    PaneNode::Pane(leaf) => assert_eq!(leaf.name, "top"),
                    other => panic!("expected pane, got {:?}", other),
                }
                match &split.children[1] {
                    PaneNode::Pane(leaf) => assert_eq!(leaf.name, "bottom"),
                    other => panic!("expected pane, got {:?}", other),
                }
            }
            other => panic!("expected split, got {:?}", other),
        }
    }

    #[test]
    fn attach_snapshot_captures_state() {
        // Unit test: construct minimal instance manually to test snapshot.
        let def = simple_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        // We test the snapshot structure; on non-windows the sessions won't
        // actually spawn, but the data structures are still exercised.
        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(7), &def, &gs, &env, find_exe_windows);

        // On non-windows, open will still succeed (sessions fail gracefully).
        if let Ok(inst) = inst {
            let snap = inst.attach_snapshot();
            assert_eq!(snap.name, "test-simple");
            assert_eq!(snap.state, WorkspaceState::Active);
            assert_eq!(snap.tabs.len(), 1);
            assert_eq!(snap.active_tab_index, 0);
        }
    }

    #[test]
    fn attach_snapshot_includes_focused_pane_name() {
        let def = split_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(8), &def, &gs, &env, find_exe_windows);

        if let Ok(inst) = inst {
            let snap = inst.attach_snapshot();
            assert_eq!(snap.tabs.len(), 1);
            assert_eq!(snap.tabs[0].focus.as_deref(), Some("bottom"));
        }
    }

    #[test]
    fn repeated_resize_on_dnacalc_pi_top_left_pane_keeps_snapshot_and_save_stable() {
        let def = dnacalc_pi_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(9), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let target = inst
            .find_pane_by_name("Foundation")
            .expect("Foundation pane should exist");
        inst.tabs_mut()[0]
            .layout_mut()
            .set_focus(target.clone())
            .expect("focus should set");

        for dir in [
            Direction::Right,
            Direction::Down,
            Direction::Right,
            Direction::Down,
        ] {
            inst.tabs_mut()[0]
                .layout_mut()
                .resize_pane_toward(target.clone(), dir, 2, Rect::new(0, 0, 120, 40))
                .expect("resize should succeed");

            let snapshot = inst.attach_snapshot();
            assert_eq!(snapshot.tabs.len(), 1);
            serde_json::to_value(&snapshot).expect("snapshot should serialize after resize");

            let saved = inst.save();
            let tabs = saved
                .tabs
                .as_ref()
                .expect("saved workspace should have tabs");
            assert_eq!(tabs.len(), 1);
        }
    }

    #[test]
    fn runtime_terminal_env_stamps_wt_and_wtd_identity() {
        let mut env = HashMap::new();
        env.insert("TERM_PROGRAM".to_string(), "stale".to_string());
        env.insert("WT_SESSION".to_string(), "stale".to_string());

        let driver = resolve_pane_driver_with_inference(
            None,
            None,
            Some(wtd_core::workspace::PaneDriverProfile::Pi),
        );

        apply_runtime_terminal_env(
            &mut env,
            "DnaCalc Workspace",
            "Foundation Pane",
            &SessionId(42),
            &driver,
        );

        assert_eq!(
            env.get("TERM_PROGRAM").map(String::as_str),
            Some("Windows_Terminal")
        );
        assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
        assert_eq!(
            env.get("WT_SESSION").map(String::as_str),
            Some("wtd-dnacalc-workspace-42")
        );
        assert_eq!(
            env.get("WT_PROFILE_ID").map(String::as_str),
            Some("wtd://dnacalc-workspace/foundation-pane")
        );
        assert_eq!(env.get("WT_WINDOW_ID").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("WTD_WORKSPACE").map(String::as_str),
            Some("DnaCalc Workspace")
        );
        assert_eq!(
            env.get("WTD_PANE").map(String::as_str),
            Some("Foundation Pane")
        );
        assert_eq!(env.get("WTD_SESSION_ID").map(String::as_str), Some("42"));
        assert_eq!(env.get("WTD_AGENT_HOST").map(String::as_str), Some("1"));
        assert_eq!(env.get("WTD_AGENT_DRIVER").map(String::as_str), Some("pi"));
        assert_eq!(
            env.get("WTD_AGENT_MULTILINE_MODE").map(String::as_str),
            Some("soft-break-key")
        );
        assert_eq!(
            env.get("WTD_AGENT_PASTE_MODE").map(String::as_str),
            Some("bracketed-if-enabled")
        );
        assert_eq!(
            env.get("WTD_AGENT_SUBMIT_KEY").map(String::as_str),
            Some("Enter")
        );
        assert_eq!(
            env.get("WTD_AGENT_HYPERLINKS").map(String::as_str),
            Some("osc8")
        );
        assert_eq!(
            env.get("WTD_AGENT_IMAGES").map(String::as_str),
            Some("kitty-placeholder")
        );
        assert_eq!(
            env.get("WTD_AGENT_SOFT_BREAK_KEY").map(String::as_str),
            Some("Shift+Enter")
        );
    }

    #[cfg(windows)]
    #[test]
    fn launched_sessions_receive_runtime_terminal_identity() {
        let def = simple_workspace_def();
        let gs = default_global_settings();
        let env = default_host_env();

        let inst =
            WorkspaceInstance::open(WorkspaceInstanceId(9), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let pane_id = inst.tabs()[0].layout().focus();
        let session_id = match inst.pane_state(&pane_id) {
            Some(PaneState::Attached { session_id }) => session_id.clone(),
            other => panic!("expected attached pane, got {other:?}"),
        };
        let session = inst.session(&session_id).expect("session should exist");
        let session_env = &session.config().env;

        assert_eq!(
            session_env.get("TERM_PROGRAM").map(String::as_str),
            Some("Windows_Terminal")
        );
        assert_eq!(
            session_env.get("COLORTERM").map(String::as_str),
            Some("truecolor")
        );
        assert_eq!(
            session_env.get("WT_SESSION").map(String::as_str),
            Some("wtd-test-simple-1")
        );
        assert_eq!(
            session_env.get("WT_PROFILE_ID").map(String::as_str),
            Some("wtd://test-simple/editor")
        );
        assert_eq!(
            session_env.get("WTD_WORKSPACE").map(String::as_str),
            Some("test-simple")
        );
        assert_eq!(
            session_env.get("WTD_PANE").map(String::as_str),
            Some("editor")
        );
        assert_eq!(
            session_env.get("WTD_SESSION_ID").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            session_env.get("WTD_AGENT_HOST").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            session_env.get("WTD_AGENT_DRIVER").map(String::as_str),
            Some("plain")
        );
        assert_eq!(
            session_env.get("WTD_AGENT_HYPERLINKS").map(String::as_str),
            Some("osc8")
        );
        assert_eq!(
            session_env.get("WTD_AGENT_IMAGES").map(String::as_str),
            Some("kitty-placeholder")
        );
    }

    #[cfg(windows)]
    #[test]
    fn launched_sessions_infer_codex_driver_from_startup_command() {
        let def = startup_command_workspace_def("codex --dangerously-skip-permissions");
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(10), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let pane_id = inst.find_pane_by_name("agent").expect("agent pane");
        let driver = inst.pane_driver(&pane_id).expect("driver");
        assert_eq!(driver.profile, "codex");
        assert_eq!(driver.submit_key, "Enter");
        assert_eq!(driver.soft_break_key, None);

        inst.close();
    }

    #[cfg(windows)]
    #[test]
    fn launched_sessions_infer_pi_driver_from_startup_command() {
        let def = startup_command_workspace_def("pi");
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(11), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let pane_id = inst.find_pane_by_name("agent").expect("agent pane");
        let driver = inst.pane_driver(&pane_id).expect("driver");
        assert_eq!(driver.profile, "pi");
        assert_eq!(driver.submit_key, "Enter");
        assert_eq!(driver.soft_break_key.as_deref(), Some("Shift+Enter"));

        inst.close();
    }

    #[cfg(windows)]
    #[test]
    fn split_spawn_preserves_inherited_startup_command() {
        let marker = "WTD_SPLIT_STARTUP_MARKER_Q07";
        let def = startup_command_workspace_def(&format!("echo {marker}"));
        let gs = default_global_settings();
        let env = default_host_env();

        let mut inst =
            WorkspaceInstance::open(WorkspaceInstanceId(11), &def, &gs, &env, find_exe_windows)
                .expect("open should succeed");

        let source_pane = inst.find_pane_by_name("agent").expect("source pane");
        let session_def = inst
            .pane_original_def(&source_pane)
            .cloned()
            .flatten()
            .expect("source session definition");
        let child_pane = inst.alloc_workspace_pane_id();
        inst.spawn_session_for_pane_with_definition(
            &child_pane,
            "child".to_string(),
            session_def.clone(),
            &gs,
            &env,
            &find_exe_windows,
        );

        let child_session_id = match inst.pane_state(&child_pane) {
            Some(PaneState::Attached { session_id }) => session_id.clone(),
            other => panic!("expected attached child pane, got {other:?}"),
        };
        let child_session = inst
            .session(&child_session_id)
            .expect("child session should exist");
        assert_eq!(
            child_session.config().startup_command.as_deref(),
            session_def.startup_command.as_deref()
        );

        std::thread::sleep(std::time::Duration::from_millis(300));
        let child_session = inst
            .session_mut(&child_session_id)
            .expect("child session should exist");
        child_session.process_pending_output();
        let visible = child_session.screen().visible_text();
        assert!(
            visible.contains(marker),
            "split-spawned session should execute inherited startup command, got:\n{}",
            visible
        );

        inst.close();
    }
}
