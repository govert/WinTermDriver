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
    PaneLeaf, PaneNode, RestartPolicy, SessionLaunchDefinition, TabDefinition,
    TerminalSizeDefinition, WorkspaceDefinition,
};
use wtd_core::{resolve_launch_spec, ResolveError};
use wtd_pty::PtySize;

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
                    focus: None,
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

    /// Add a new empty tab and return a reference to it.
    pub fn add_tab(&mut self, name: String) -> &TabInstance {
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let layout = LayoutTree::new();
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
        let session_def = SessionLaunchDefinition::default();
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
                self.panes.insert(
                    pane_id.clone(),
                    PaneRecord {
                        name: pane_name,
                        state: PaneState::Detached {
                            error: e.to_string(),
                        },
                        original_def: None,
                    },
                );
                return;
            }
        };

        let session_id = SessionId(self.next_session_id);
        self.next_session_id += 1;

        let config = SessionConfig {
            executable: resolved.executable,
            args: resolved.args,
            cwd: resolved.cwd,
            env: resolved.env,
            restart_policy: RestartPolicy::Never,
            startup_command: None,
            size: self.default_size,
            name: pane_name.clone(),
            max_scrollback: 10_000,
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
                        original_def: None,
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
                        original_def: None,
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

        let layout = LayoutTree::new();
        let pane_id = layout.focus();

        inst.panes.insert(
            pane_id,
            PaneRecord {
                name: "default".to_string(),
                state: PaneState::Detached {
                    error: "test mode".to_string(),
                },
                original_def: None,
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
            let first_pane = layout.focus();
            inst.panes.insert(
                first_pane.clone(),
                PaneRecord {
                    name: pane_names[0].to_string(),
                    state: PaneState::Detached {
                        error: "test".to_string(),
                    },
                    original_def: None,
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
                        self.panes.insert(
                            pane_id.clone(),
                            PaneRecord {
                                name: pane_name.clone(),
                                state: PaneState::Detached {
                                    error: e.to_string(),
                                },
                                original_def: session_def.clone(),
                            },
                        );
                        continue;
                    }
                };

                let session_id = SessionId(self.next_session_id);
                self.next_session_id += 1;

                let config = SessionConfig {
                    executable: resolved.executable,
                    args: resolved.args,
                    cwd: resolved.cwd,
                    env: resolved.env,
                    restart_policy: restart_policy.clone(),
                    startup_command: session_launch.startup_command.clone(),
                    size: session_launch
                        .terminal_size
                        .as_ref()
                        .map(pty_size_from_definition)
                        .unwrap_or(self.default_size),
                    name: pane_name.clone(),
                    max_scrollback,
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
    pub session_states: HashMap<SessionId, SessionState>,
    /// Current terminal title per session (OSC 2).
    pub session_titles: HashMap<SessionId, String>,
    /// Current visible terminal size per session.
    pub session_sizes: HashMap<SessionId, SessionSizeSnapshot>,
    /// Base64-encoded VT snapshot of the current visible screen per session.
    ///
    /// The UI can feed these bytes directly into `ScreenBuffer::advance()` to
    /// seed pane content immediately on attach, before any new output arrives.
    pub session_screens: HashMap<SessionId, String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSizeSnapshot {
    pub cols: u16,
    pub rows: u16,
}

/// Snapshot of a single tab's metadata and layout.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSnapshot {
    pub id: TabId,
    pub name: String,
    pub panes: Vec<PaneId>,
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_core::workspace::{Orientation, PaneLeaf, SplitNode};

    fn default_global_settings() -> GlobalSettings {
        GlobalSettings::default()
    }

    fn default_host_env() -> HashMap<String, String> {
        let mut env = HashMap::new();
        // Use real USERPROFILE so CWD resolution produces a valid path.
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
}
