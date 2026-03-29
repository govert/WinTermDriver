//! Workspace definition types — the durable, persisted layer (§9.1, §10).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

// ── WorkspaceName ──────────────────────────────────────────────────────────────

/// A validated workspace name (non-empty, no path separators).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    pub fn new(name: impl Into<String>) -> Result<Self, crate::error::CoreError> {
        let s = name.into();
        if s.is_empty() || s.contains('/') || s.contains('\\') {
            return Err(crate::error::CoreError::InvalidName(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── WorkspaceDefinition (root) ─────────────────────────────────────────────────

/// Root of a workspace definition file (§9.1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceDefinition {
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<DefaultsDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<HashMap<String, ProfileDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<BindingsDefinition>,
    /// Multi-window form — mutually exclusive with `tabs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<WindowDefinition>>,
    /// Single-window shorthand — mutually exclusive with `windows`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<TabDefinition>>,
}

// ── DefaultsDefinition (§9.1.2) ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DefaultsDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "restartPolicy"
    )]
    pub restart_policy: Option<RestartPolicy>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "scrollbackLines"
    )]
    pub scrollback_lines: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, Option<String>>>,
}

// ── RestartPolicy (§9.1.3) ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

// ── WindowDefinition (§9.1.4) ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub tabs: Vec<TabDefinition>,
}

// ── TabDefinition (§9.1.5) ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabDefinition {
    pub name: String,
    pub layout: PaneNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

// ── PaneNode (§9.1.6) ─────────────────────────────────────────────────────────

/// Tagged union: a leaf pane or a split container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PaneNode {
    Pane(PaneLeaf),
    Split(SplitNode),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLeaf {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionLaunchDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitNode {
    pub orientation: Orientation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    pub children: Vec<PaneNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

// ── SessionLaunchDefinition (§9.1.7) ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionLaunchDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, Option<String>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "startupCommand"
    )]
    pub startup_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

// ── ProfileDefinition (§9.1.8) ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileDefinition {
    #[serde(rename = "type")]
    pub profile_type: ProfileType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "identityFile"
    )]
    pub identity_file: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "useAgent"
    )]
    pub use_agent: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "remoteCommand"
    )]
    pub remote_command: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "scrollbackLines"
    )]
    pub scrollback_lines: Option<u32>,
}

// ── ProfileType (§9.1.9) ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileType {
    Powershell,
    Cmd,
    Wsl,
    Ssh,
    Custom,
}

// ── BindingPreset (§9.1.10) ───────────────────────────────────────────────────

/// Built-in keybinding preset.
///
/// When specified, the preset loads a predefined set of keys, chords, and prefix
/// as the base. User-specified `keys` and `chords` fields then override individual
/// entries from the preset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingPreset {
    /// Windows Terminal-style keybindings (default, populated by h35.3).
    WindowsTerminal,
    /// tmux-style keybindings: Ctrl+B prefix, 15 chords, 10 single-stroke keys.
    Tmux,
    /// No preset — only explicit `keys` and `chords` are active.
    None,
}

// ── BindingsDefinition (§9.1.10) ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BindingsDefinition {
    /// Built-in preset to use as the base before applying user overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<BindingPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "prefixTimeout"
    )]
    pub prefix_timeout: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chords: Option<HashMap<String, ActionReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, ActionReference>>,
}

// ── ActionReference (§9.1.11) ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionReference {
    Simple(String),
    WithArgs {
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<HashMap<String, String>>,
    },
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_name_accepted() {
        let name = WorkspaceName::new("my-workspace").unwrap();
        assert_eq!(name.as_str(), "my-workspace");
    }

    #[test]
    fn empty_name_rejected() {
        assert!(WorkspaceName::new("").is_err());
    }

    #[test]
    fn name_with_slash_rejected() {
        assert!(WorkspaceName::new("foo/bar").is_err());
    }
}
