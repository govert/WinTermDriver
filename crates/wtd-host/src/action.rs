//! Action system (§20).
//!
//! Provides a registry of named actions and a dispatcher that validates
//! arguments and executes them against workspace/session managers.

use std::collections::HashMap;

use serde_json::Value;

use wtd_core::ids::PaneId;
use wtd_core::layout::{CloseResult, Direction, LayoutError, Rect, ResizeDirection};

use crate::workspace_instance::{WorkspaceError, WorkspaceInstance, WorkspaceState};

// ── Target type ──────────────────────────────────────────────────────────────

/// What kind of object an action operates on (§20.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetType {
    Global,
    Workspace,
    Window,
    Tab,
    Pane,
}

impl std::fmt::Display for TargetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetType::Global => write!(f, "global"),
            TargetType::Workspace => write!(f, "workspace"),
            TargetType::Window => write!(f, "window"),
            TargetType::Tab => write!(f, "tab"),
            TargetType::Pane => write!(f, "pane"),
        }
    }
}

// ── Argument definition ──────────────────────────────────────────────────────

/// Type of an action argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    String,
    Int,
    Bool,
}

impl std::fmt::Display for ArgType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgType::String => write!(f, "string"),
            ArgType::Int => write!(f, "int"),
            ArgType::Bool => write!(f, "bool"),
        }
    }
}

/// Definition of a single action argument.
#[derive(Debug, Clone)]
pub struct ArgDef {
    pub name: &'static str,
    pub arg_type: ArgType,
    pub required: bool,
}

// ── Action definition ────────────────────────────────────────────────────────

/// Static definition of an action (§20.1).
#[derive(Debug, Clone)]
pub struct ActionDef {
    pub name: &'static str,
    pub target_type: TargetType,
    pub args: &'static [ArgDef],
    pub description: &'static str,
}

// ── Action registry ──────────────────────────────────────────────────────────

/// Registry of all known actions, keyed by name (§20.2).
pub struct ActionRegistry {
    actions: HashMap<&'static str, ActionDef>,
}

impl ActionRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            actions: HashMap::new(),
        }
    }

    /// Register an action definition.
    pub fn register(&mut self, def: ActionDef) {
        self.actions.insert(def.name, def);
    }

    /// Look up an action by name.
    pub fn get(&self, name: &str) -> Option<&ActionDef> {
        self.actions.get(name)
    }

    /// Return all registered action names.
    pub fn action_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.actions.keys().copied().collect();
        names.sort();
        names
    }

    /// Number of registered actions.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Validate arguments against an action's definition.
    /// Returns a list of validation errors (empty = valid).
    pub fn validate_args(&self, action_name: &str, args: &Value) -> Result<(), ActionError> {
        let def = self
            .actions
            .get(action_name)
            .ok_or_else(|| ActionError::UnknownAction(action_name.to_string()))?;

        let obj = match args {
            Value::Object(map) => map,
            Value::Null if def.args.is_empty() => return Ok(()),
            Value::Null => {
                // Check required args
                for arg_def in def.args {
                    if arg_def.required {
                        return Err(ActionError::InvalidArgument(format!(
                            "missing required argument '{}'",
                            arg_def.name,
                        )));
                    }
                }
                return Ok(());
            }
            _ => {
                return Err(ActionError::InvalidArgument(
                    "args must be an object".to_string(),
                ));
            }
        };

        // Check required args are present and types match
        for arg_def in def.args {
            if let Some(val) = obj.get(arg_def.name) {
                if !matches_type(val, arg_def.arg_type) {
                    return Err(ActionError::InvalidArgument(format!(
                        "argument '{}' must be {}",
                        arg_def.name, arg_def.arg_type,
                    )));
                }
            } else if arg_def.required {
                return Err(ActionError::InvalidArgument(format!(
                    "missing required argument '{}'",
                    arg_def.name,
                )));
            }
        }

        // Check for unknown args
        let known: Vec<&str> = def.args.iter().map(|a| a.name).collect();
        for key in obj.keys() {
            if !known.contains(&key.as_str()) {
                return Err(ActionError::InvalidArgument(format!(
                    "unknown argument '{}'",
                    key,
                )));
            }
        }

        Ok(())
    }
}

fn matches_type(val: &Value, expected: ArgType) -> bool {
    match expected {
        ArgType::String => val.is_string(),
        ArgType::Int => val.is_i64() || val.is_u64(),
        ArgType::Bool => val.is_boolean(),
    }
}

// ── v1 catalog (§20.3) ──────────────────────────────────────────────────────

// Argument definition constants used across actions.
const ARG_NAME_STRING: ArgDef = ArgDef {
    name: "name",
    arg_type: ArgType::String,
    required: true,
};
const ARG_NAME_STRING_OPT: ArgDef = ArgDef {
    name: "name",
    arg_type: ArgType::String,
    required: false,
};
const ARG_FILE_OPT: ArgDef = ArgDef {
    name: "file",
    arg_type: ArgType::String,
    required: false,
};
const ARG_PROFILE_OPT: ArgDef = ArgDef {
    name: "profile",
    arg_type: ArgType::String,
    required: false,
};
const ARG_PROFILE_STRING: ArgDef = ArgDef {
    name: "profile",
    arg_type: ArgType::String,
    required: true,
};
const ARG_AMOUNT_OPT: ArgDef = ArgDef {
    name: "amount",
    arg_type: ArgType::Int,
    required: false,
};

// Static arg slices for each action.
static OPEN_WORKSPACE_ARGS: &[ArgDef] = &[
    ARG_NAME_STRING,
    ARG_FILE_OPT,
    ArgDef {
        name: "recreate",
        arg_type: ArgType::Bool,
        required: false,
    },
];
static CLOSE_WORKSPACE_ARGS: &[ArgDef] = &[ArgDef {
    name: "kill",
    arg_type: ArgType::Bool,
    required: false,
}];
static SAVE_WORKSPACE_ARGS: &[ArgDef] = &[ARG_FILE_OPT];
static NO_ARGS: &[ArgDef] = &[];
static NEW_TAB_ARGS: &[ArgDef] = &[ARG_PROFILE_OPT];
static GOTO_TAB_ARGS: &[ArgDef] = &[
    ArgDef {
        name: "index",
        arg_type: ArgType::Int,
        required: false,
    },
    ARG_NAME_STRING_OPT,
];
static RENAME_TAB_ARGS: &[ArgDef] = &[ARG_NAME_STRING];
static SPLIT_ARGS: &[ArgDef] = &[ARG_PROFILE_OPT];
static CHANGE_PROFILE_ARGS: &[ArgDef] = &[ARG_PROFILE_STRING];
static FOCUS_PANE_BY_NAME_ARGS: &[ArgDef] = &[ARG_NAME_STRING];
static RENAME_PANE_ARGS: &[ArgDef] = &[ARG_NAME_STRING];
static RESIZE_ARGS: &[ArgDef] = &[ARG_AMOUNT_OPT];

/// Create a registry pre-populated with all v1 actions (§20.3).
pub fn v1_registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();

    // Workspace lifecycle actions
    r.register(ActionDef {
        name: "open-workspace",
        target_type: TargetType::Global,
        args: OPEN_WORKSPACE_ARGS,
        description: "Open or attach to a workspace",
    });
    r.register(ActionDef {
        name: "close-workspace",
        target_type: TargetType::Workspace,
        args: CLOSE_WORKSPACE_ARGS,
        description: "Close workspace UI. If kill=true, destroy instance.",
    });
    r.register(ActionDef {
        name: "recreate-workspace",
        target_type: TargetType::Workspace,
        args: NO_ARGS,
        description: "Tear down and recreate instance from definition",
    });
    r.register(ActionDef {
        name: "save-workspace",
        target_type: TargetType::Workspace,
        args: SAVE_WORKSPACE_ARGS,
        description: "Save current workspace state as definition",
    });

    // Window actions
    r.register(ActionDef {
        name: "new-window",
        target_type: TargetType::Workspace,
        args: NO_ARGS,
        description: "Create a new window in the workspace",
    });
    r.register(ActionDef {
        name: "close-window",
        target_type: TargetType::Window,
        args: NO_ARGS,
        description: "Close window and all its tabs/panes/sessions",
    });

    // Tab actions
    r.register(ActionDef {
        name: "new-tab",
        target_type: TargetType::Window,
        args: NEW_TAB_ARGS,
        description: "Create a new tab with a single pane",
    });
    r.register(ActionDef {
        name: "close-tab",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Close tab and all its panes/sessions",
    });
    r.register(ActionDef {
        name: "next-tab",
        target_type: TargetType::Window,
        args: NO_ARGS,
        description: "Switch to the next tab",
    });
    r.register(ActionDef {
        name: "prev-tab",
        target_type: TargetType::Window,
        args: NO_ARGS,
        description: "Switch to the previous tab",
    });
    r.register(ActionDef {
        name: "goto-tab",
        target_type: TargetType::Window,
        args: GOTO_TAB_ARGS,
        description: "Switch to tab by index (0-based) or name",
    });
    r.register(ActionDef {
        name: "rename-tab",
        target_type: TargetType::Tab,
        args: RENAME_TAB_ARGS,
        description: "Rename the tab",
    });
    r.register(ActionDef {
        name: "move-tab-left",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move tab one position left in the tab strip",
    });
    r.register(ActionDef {
        name: "move-tab-right",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move tab one position right",
    });

    // Pane actions
    r.register(ActionDef {
        name: "split-right",
        target_type: TargetType::Pane,
        args: SPLIT_ARGS,
        description: "Split focused pane horizontally, new pane on right",
    });
    r.register(ActionDef {
        name: "split-down",
        target_type: TargetType::Pane,
        args: SPLIT_ARGS,
        description: "Split focused pane vertically, new pane below",
    });
    r.register(ActionDef {
        name: "close-pane",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Close pane and kill its session",
    });
    r.register(ActionDef {
        name: "focus-next-pane",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus to next pane",
    });
    r.register(ActionDef {
        name: "focus-prev-pane",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus to previous pane",
    });
    r.register(ActionDef {
        name: "focus-pane-up",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus up",
    });
    r.register(ActionDef {
        name: "focus-pane-down",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus down",
    });
    r.register(ActionDef {
        name: "focus-pane-left",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus left",
    });
    r.register(ActionDef {
        name: "focus-pane-right",
        target_type: TargetType::Tab,
        args: NO_ARGS,
        description: "Move focus right",
    });
    r.register(ActionDef {
        name: "focus-pane",
        target_type: TargetType::Workspace,
        args: FOCUS_PANE_BY_NAME_ARGS,
        description: "Move focus to named pane",
    });
    r.register(ActionDef {
        name: "zoom-pane",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Toggle pane zoom",
    });
    r.register(ActionDef {
        name: "rename-pane",
        target_type: TargetType::Pane,
        args: RENAME_PANE_ARGS,
        description: "Rename pane",
    });
    r.register(ActionDef {
        name: "change-profile",
        target_type: TargetType::Pane,
        args: CHANGE_PROFILE_ARGS,
        description: "Relaunch pane using the selected profile",
    });
    r.register(ActionDef {
        name: "resize-pane-grow-right",
        target_type: TargetType::Pane,
        args: RESIZE_ARGS,
        description: "Grow pane to the right",
    });
    r.register(ActionDef {
        name: "resize-pane-grow-down",
        target_type: TargetType::Pane,
        args: RESIZE_ARGS,
        description: "Grow pane downward",
    });
    r.register(ActionDef {
        name: "resize-pane-shrink-right",
        target_type: TargetType::Pane,
        args: RESIZE_ARGS,
        description: "Shrink pane from the right",
    });
    r.register(ActionDef {
        name: "resize-pane-shrink-down",
        target_type: TargetType::Pane,
        args: RESIZE_ARGS,
        description: "Shrink pane from above",
    });

    // Session actions
    r.register(ActionDef {
        name: "restart-session",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Kill current session and launch a new one from the same definition",
    });

    // Clipboard actions
    r.register(ActionDef {
        name: "copy",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Copy selected text to clipboard",
    });
    r.register(ActionDef {
        name: "paste",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Paste clipboard content as input to the session",
    });

    // UI actions
    r.register(ActionDef {
        name: "toggle-command-palette",
        target_type: TargetType::Global,
        args: NO_ARGS,
        description: "Open or close the command palette",
    });
    r.register(ActionDef {
        name: "toggle-fullscreen",
        target_type: TargetType::Window,
        args: NO_ARGS,
        description: "Toggle window fullscreen",
    });
    r.register(ActionDef {
        name: "enter-scrollback-mode",
        target_type: TargetType::Pane,
        args: NO_ARGS,
        description: "Enter scrollback navigation mode",
    });

    r
}

// ── Dispatch errors ──────────────────────────────────────────────────────────

/// Errors from action dispatch.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("unknown action '{0}'")]
    UnknownAction(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    #[error("layout error: {0}")]
    Layout(#[from] LayoutError),

    #[error("target pane not found: {0}")]
    PaneNotFound(PaneId),

    #[error("no active tab")]
    NoActiveTab,

    #[error("action '{0}' not implemented")]
    NotImplemented(String),
}

// ── Action dispatcher ────────────────────────────────────────────────────────

/// Processes actions from all sources identically (§20.2).
///
/// The dispatcher validates arguments against the registry, resolves the
/// target, and executes the action on the workspace instance.
pub struct ActionDispatcher {
    registry: ActionRegistry,
    /// Total rect for layout computations (character cells).
    viewport: Rect,
}

impl ActionDispatcher {
    /// Create a dispatcher with the v1 action registry and a viewport size.
    pub fn new(registry: ActionRegistry, viewport: Rect) -> Self {
        Self { registry, viewport }
    }

    /// Access the underlying registry.
    pub fn registry(&self) -> &ActionRegistry {
        &self.registry
    }

    /// Update the viewport size (e.g. on window resize).
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Dispatch an action by name with the given arguments.
    ///
    /// `target_pane_id` is the pane context (from `InvokeAction.target_pane_id`);
    /// for pane-targeted actions, if None the focused pane of the active tab is used.
    pub fn dispatch(
        &self,
        workspace: &mut WorkspaceInstance,
        action_name: &str,
        args: &Value,
        target_pane_id: Option<PaneId>,
    ) -> Result<ActionResult, ActionError> {
        // Validate the action exists
        let def = self
            .registry
            .get(action_name)
            .ok_or_else(|| ActionError::UnknownAction(action_name.to_string()))?;

        // Validate arguments
        self.registry.validate_args(action_name, args)?;

        // Check workspace is active for most actions
        if def.target_type != TargetType::Global && *workspace.state() != WorkspaceState::Active {
            return Err(ActionError::Workspace(WorkspaceError::InvalidState(
                workspace.state().clone(),
            )));
        }

        match action_name {
            // ── Pane split actions ────────────────────────────────────────
            "split-right" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                let new_pane = tab.layout_mut().split_right(pane_id)?;
                Ok(ActionResult::PaneCreated { pane_id: new_pane })
            }
            "split-down" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                let new_pane = tab.layout_mut().split_down(pane_id)?;
                Ok(ActionResult::PaneCreated { pane_id: new_pane })
            }

            // ── Close pane ───────────────────────────────────────────────
            "close-pane" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                // Stop the session attached to this pane
                workspace.stop_pane_session(&pane_id);
                // Remove from layout
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                let result = tab.layout_mut().close_pane(pane_id.clone())?;
                workspace.remove_pane(&pane_id);
                Ok(ActionResult::PaneClosed {
                    pane_id,
                    close_result: result,
                })
            }

            // ── Focus actions ────────────────────────────────────────────
            "focus-next-pane" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut().focus_next();
                Ok(ActionResult::Ok)
            }
            "focus-prev-pane" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut().focus_prev();
                Ok(ActionResult::Ok)
            }
            "focus-pane-up" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut()
                    .focus_direction(Direction::Up, self.viewport);
                Ok(ActionResult::Ok)
            }
            "focus-pane-down" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut()
                    .focus_direction(Direction::Down, self.viewport);
                Ok(ActionResult::Ok)
            }
            "focus-pane-left" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut()
                    .focus_direction(Direction::Left, self.viewport);
                Ok(ActionResult::Ok)
            }
            "focus-pane-right" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut()
                    .focus_direction(Direction::Right, self.viewport);
                Ok(ActionResult::Ok)
            }
            "focus-pane" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let pane_id = workspace
                    .find_pane_by_name(name)
                    .ok_or_else(|| ActionError::PaneNotFound(PaneId(0)))?;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                tab.layout_mut().set_focus(pane_id)?;
                Ok(ActionResult::Ok)
            }

            // ── Zoom ─────────────────────────────────────────────────────
            "zoom-pane" => {
                let tab = active_tab_mut(workspace)?;
                tab.layout_mut().toggle_zoom();
                Ok(ActionResult::Ok)
            }

            // ── Resize ───────────────────────────────────────────────────
            "resize-pane-grow-right" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(1) as u16;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                tab.layout_mut().resize_pane(
                    pane_id,
                    ResizeDirection::GrowRight,
                    amount,
                    self.viewport,
                )?;
                Ok(ActionResult::Ok)
            }
            "resize-pane-grow-down" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(1) as u16;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                tab.layout_mut().resize_pane(
                    pane_id,
                    ResizeDirection::GrowDown,
                    amount,
                    self.viewport,
                )?;
                Ok(ActionResult::Ok)
            }
            "resize-pane-shrink-right" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(1) as u16;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                tab.layout_mut().resize_pane(
                    pane_id,
                    ResizeDirection::ShrinkRight,
                    amount,
                    self.viewport,
                )?;
                Ok(ActionResult::Ok)
            }
            "resize-pane-shrink-down" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(1) as u16;
                let tab = find_tab_for_pane_mut(workspace, &pane_id)?;
                tab.layout_mut().resize_pane(
                    pane_id,
                    ResizeDirection::ShrinkDown,
                    amount,
                    self.viewport,
                )?;
                Ok(ActionResult::Ok)
            }

            // ── Rename pane ──────────────────────────────────────────────
            "rename-pane" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                workspace.rename_pane(&pane_id, name);
                Ok(ActionResult::Ok)
            }

            // ── Restart session ──────────────────────────────────────────
            "restart-session" => {
                let pane_id = self.resolve_pane(workspace, target_pane_id)?;
                workspace.restart_pane_session(&pane_id)?;
                Ok(ActionResult::Ok)
            }

            // Actions that require full host context (workspace lifecycle,
            // window/tab management, clipboard, UI) are dispatched at a
            // higher level. Return NotImplemented so the host can handle
            // or report them.
            other => Err(ActionError::NotImplemented(other.to_string())),
        }
    }

    /// Resolve the pane to act on: explicit target, or focused pane of active tab.
    fn resolve_pane(
        &self,
        workspace: &WorkspaceInstance,
        target: Option<PaneId>,
    ) -> Result<PaneId, ActionError> {
        if let Some(id) = target {
            // Verify the pane exists in pane records or in a layout tree
            let in_panes = workspace.pane_state(&id).is_some();
            let in_layout = workspace
                .tabs()
                .iter()
                .any(|t| t.layout().panes().contains(&id));
            if !in_panes && !in_layout {
                return Err(ActionError::PaneNotFound(id));
            }
            Ok(id)
        } else {
            // Use focused pane of first tab (active tab)
            let tab = workspace.tabs().first().ok_or(ActionError::NoActiveTab)?;
            Ok(tab.layout().focus())
        }
    }
}

// ── Action result ────────────────────────────────────────────────────────────

/// Outcome of a successfully dispatched action.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionResult {
    /// Action completed with no specific return value.
    Ok,
    /// A new pane was created (from split).
    PaneCreated { pane_id: PaneId },
    /// A pane was closed.
    PaneClosed {
        pane_id: PaneId,
        close_result: CloseResult,
    },
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Find the tab containing a pane and return a mutable reference.
fn find_tab_for_pane_mut<'a>(
    workspace: &'a mut WorkspaceInstance,
    pane_id: &PaneId,
) -> Result<&'a mut crate::workspace_instance::TabInstance, ActionError> {
    for tab in workspace.tabs_mut() {
        if tab.layout().panes().contains(pane_id) {
            return Ok(tab);
        }
    }
    Err(ActionError::PaneNotFound(pane_id.clone()))
}

/// Get the active (first) tab with a mutable reference.
fn active_tab_mut(
    workspace: &mut WorkspaceInstance,
) -> Result<&mut crate::workspace_instance::TabInstance, ActionError> {
    let idx = workspace.active_tab_index();
    workspace
        .tabs_mut()
        .get_mut(idx)
        .ok_or(ActionError::NoActiveTab)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_registry_has_all_actions() {
        let r = v1_registry();
        // §20.3 plus change-profile totals 37 actions.
        assert_eq!(r.len(), 37);
    }

    #[test]
    fn lookup_by_name() {
        let r = v1_registry();
        let def = r.get("split-right").unwrap();
        assert_eq!(def.target_type, TargetType::Pane);
        assert_eq!(def.args.len(), 1); // profile?
        assert_eq!(
            def.description,
            "Split focused pane horizontally, new pane on right"
        );
    }

    #[test]
    fn lookup_unknown_action_returns_none() {
        let r = v1_registry();
        assert!(r.get("nonexistent-action").is_none());
    }

    #[test]
    fn validate_args_no_args_action() {
        let r = v1_registry();
        assert!(r.validate_args("close-pane", &json!({})).is_ok());
        assert!(r.validate_args("close-pane", &Value::Null).is_ok());
    }

    #[test]
    fn validate_args_optional_args() {
        let r = v1_registry();
        // split-right with no args is fine (profile is optional)
        assert!(r.validate_args("split-right", &json!({})).is_ok());
        // with valid profile arg
        assert!(r
            .validate_args("split-right", &json!({"profile": "cmd"}))
            .is_ok());
        assert!(r
            .validate_args("new-tab", &json!({"profile": "powershell"}))
            .is_ok());
    }

    #[test]
    fn validate_change_profile_requires_profile_arg() {
        let r = v1_registry();
        assert!(r
            .validate_args("change-profile", &json!({"profile": "cmd"}))
            .is_ok());
        let err = r
            .validate_args("change-profile", &json!({}))
            .expect_err("missing profile should fail");
        assert!(
            err.to_string()
                .contains("missing required argument 'profile'"),
            "got: {}",
            err
        );
    }

    #[test]
    fn validate_args_required_arg_missing() {
        let r = v1_registry();
        // rename-tab requires "name"
        let result = r.validate_args("rename-tab", &json!({}));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("missing required argument 'name'"),
            "got: {}",
            err
        );
    }

    #[test]
    fn validate_args_wrong_type() {
        let r = v1_registry();
        // rename-tab name should be string, not int
        let result = r.validate_args("rename-tab", &json!({"name": 42}));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be string"));
    }

    #[test]
    fn validate_args_unknown_arg() {
        let r = v1_registry();
        let result = r.validate_args("close-pane", &json!({"bogus": true}));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown argument"));
    }

    #[test]
    fn validate_unknown_action_returns_error() {
        let r = v1_registry();
        let result = r.validate_args("does-not-exist", &json!({}));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActionError::UnknownAction(_)));
    }

    #[test]
    fn validate_open_workspace_args() {
        let r = v1_registry();
        // name is required
        assert!(r.validate_args("open-workspace", &json!({})).is_err());
        assert!(r
            .validate_args("open-workspace", &json!({"name": "test"}))
            .is_ok());
        assert!(r
            .validate_args(
                "open-workspace",
                &json!({"name": "test", "file": "a.yaml", "recreate": true})
            )
            .is_ok());
    }

    #[test]
    fn validate_goto_tab_args() {
        let r = v1_registry();
        // both args optional
        assert!(r.validate_args("goto-tab", &json!({})).is_ok());
        assert!(r.validate_args("goto-tab", &json!({"index": 2})).is_ok());
        assert!(r
            .validate_args("goto-tab", &json!({"name": "main"}))
            .is_ok());
        // wrong type
        assert!(r
            .validate_args("goto-tab", &json!({"index": "two"}))
            .is_err());
    }

    #[test]
    fn action_names_sorted() {
        let r = v1_registry();
        let names = r.action_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn all_target_types_represented() {
        let r = v1_registry();
        let types: std::collections::HashSet<TargetType> = r
            .action_names()
            .iter()
            .filter_map(|n| r.get(n))
            .map(|d| d.target_type)
            .collect();
        assert!(types.contains(&TargetType::Global));
        assert!(types.contains(&TargetType::Workspace));
        assert!(types.contains(&TargetType::Window));
        assert!(types.contains(&TargetType::Tab));
        assert!(types.contains(&TargetType::Pane));
    }

    // ── Dispatcher tests using layout tree directly ──────────────────────

    #[test]
    fn dispatch_unknown_action_error() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();
        let result = dispatcher.dispatch(&mut workspace, "nonexistent", &json!({}), None);
        assert!(matches!(result, Err(ActionError::UnknownAction(_))));
    }

    #[test]
    fn dispatch_invalid_args_error() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();
        // rename-tab requires "name" string arg
        let result = dispatcher.dispatch(&mut workspace, "rename-tab", &json!({"name": 42}), None);
        assert!(matches!(result, Err(ActionError::InvalidArgument(_))));
    }

    #[test]
    fn dispatch_split_right_modifies_layout() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();

        // Initially one pane
        assert_eq!(workspace.tabs()[0].layout().pane_count(), 1);

        let result = dispatcher.dispatch(&mut workspace, "split-right", &json!({}), None);
        assert!(result.is_ok());
        match result.unwrap() {
            ActionResult::PaneCreated { pane_id } => {
                assert!(pane_id.0 > 0);
            }
            other => panic!("expected PaneCreated, got {:?}", other),
        }

        // Now two panes
        assert_eq!(workspace.tabs()[0].layout().pane_count(), 2);
    }

    #[test]
    fn dispatch_split_down_modifies_layout() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();

        let result = dispatcher.dispatch(&mut workspace, "split-down", &json!({}), None);
        assert!(result.is_ok());
        assert_eq!(workspace.tabs()[0].layout().pane_count(), 2);
    }

    #[test]
    fn dispatch_close_pane_removes_from_layout() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();

        // Split first to have 2 panes
        dispatcher
            .dispatch(&mut workspace, "split-right", &json!({}), None)
            .unwrap();
        assert_eq!(workspace.tabs()[0].layout().pane_count(), 2);

        let panes = workspace.tabs()[0].layout().panes();
        let target = panes[1].clone();

        let result = dispatcher.dispatch(
            &mut workspace,
            "close-pane",
            &json!({}),
            Some(target.clone()),
        );
        assert!(result.is_ok());
        match result.unwrap() {
            ActionResult::PaneClosed {
                pane_id,
                close_result,
            } => {
                assert_eq!(pane_id, target);
                assert!(matches!(close_result, CloseResult::Closed { .. }));
            }
            other => panic!("expected PaneClosed, got {:?}", other),
        }

        assert_eq!(workspace.tabs()[0].layout().pane_count(), 1);
    }

    #[test]
    fn dispatch_focus_next_cycles() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();

        // Split to get 2 panes
        dispatcher
            .dispatch(&mut workspace, "split-right", &json!({}), None)
            .unwrap();

        let original_focus = workspace.tabs()[0].layout().focus();

        dispatcher
            .dispatch(&mut workspace, "focus-next-pane", &json!({}), None)
            .unwrap();

        let new_focus = workspace.tabs()[0].layout().focus();
        assert_ne!(original_focus, new_focus);
    }

    #[test]
    fn dispatch_zoom_toggles() {
        let dispatcher = ActionDispatcher::new(v1_registry(), Rect::new(0, 0, 120, 40));
        let mut workspace = test_workspace();

        assert!(!workspace.tabs()[0].layout().is_zoomed());

        dispatcher
            .dispatch(&mut workspace, "zoom-pane", &json!({}), None)
            .unwrap();
        assert!(workspace.tabs()[0].layout().is_zoomed());

        dispatcher
            .dispatch(&mut workspace, "zoom-pane", &json!({}), None)
            .unwrap();
        assert!(!workspace.tabs()[0].layout().is_zoomed());
    }

    // ── Test helper: minimal workspace with one tab/pane ─────────────────

    fn test_workspace() -> WorkspaceInstance {
        WorkspaceInstance::new_for_test("test-ws")
    }
}
