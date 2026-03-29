//! Global user configuration (§11).
//!
//! Provides [`GlobalSettings`] (the full §11.2 schema), [`FontConfig`] (§11.4),
//! [`ThemeConfig`] (§11.5), built-in default keybindings (§11.3), file loading,
//! and merge-precedence helpers (§11.6).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::workspace::{ActionReference, BindingsDefinition, ProfileDefinition, RestartPolicy};

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SettingsLoadError {
    #[error("I/O error reading settings file: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error in settings file: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

// ── LogLevel ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

// ── FontConfig (§11.4) ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontConfig {
    #[serde(default = "default_font_family")]
    pub family: String,
    #[serde(default = "default_font_size")]
    pub size: f64,
    #[serde(default = "default_font_weight")]
    pub weight: String,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: default_font_family(),
            size: default_font_size(),
            weight: default_font_weight(),
        }
    }
}

fn default_font_family() -> String {
    "Cascadia Mono".to_string()
}
fn default_font_size() -> f64 {
    12.0
}
fn default_font_weight() -> String {
    "normal".to_string()
}

// ── ThemeConfig (§11.5) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "default_theme_name")]
    pub name: String,
    #[serde(default = "default_foreground")]
    pub foreground: String,
    #[serde(default = "default_background")]
    pub background: String,
    #[serde(rename = "cursorColor", default = "default_cursor_color")]
    pub cursor_color: String,
    #[serde(rename = "selectionBackground", default = "default_selection_bg")]
    pub selection_background: String,
    #[serde(default = "default_palette")]
    pub palette: Vec<String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: default_theme_name(),
            foreground: default_foreground(),
            background: default_background(),
            cursor_color: default_cursor_color(),
            selection_background: default_selection_bg(),
            palette: default_palette(),
        }
    }
}

fn default_theme_name() -> String {
    "default".to_string()
}
fn default_foreground() -> String {
    "#CCCCCC".to_string()
}
fn default_background() -> String {
    "#0C0C0C".to_string()
}
fn default_cursor_color() -> String {
    "#FFFFFF".to_string()
}
fn default_selection_bg() -> String {
    "#FFFFFF".to_string()
}
fn default_palette() -> Vec<String> {
    vec![
        "#0C0C0C".to_string(), // 0  Black
        "#C50F1F".to_string(), // 1  Red
        "#13A10E".to_string(), // 2  Green
        "#C19C00".to_string(), // 3  Yellow
        "#0037DA".to_string(), // 4  Blue
        "#881798".to_string(), // 5  Magenta
        "#3A96DD".to_string(), // 6  Cyan
        "#CCCCCC".to_string(), // 7  White
        "#767676".to_string(), // 8  Bright Black
        "#E74856".to_string(), // 9  Bright Red
        "#16C60C".to_string(), // 10 Bright Green
        "#F9F1A5".to_string(), // 11 Bright Yellow
        "#3B78FF".to_string(), // 12 Bright Blue
        "#B4009E".to_string(), // 13 Bright Magenta
        "#61D6D6".to_string(), // 14 Bright Cyan
        "#F2F2F2".to_string(), // 15 Bright White
    ]
}

// ── GlobalSettings (§11.2) ──────────────────────────────────────────────────

/// Global user configuration loaded from `%APPDATA%\WinTermDriver\settings.yaml` (§11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalSettings {
    /// Built-in profile type or globally defined profile to use when no profile is
    /// specified in the session or workspace defaults. Default: `"powershell"`.
    #[serde(rename = "defaultProfile", default = "default_profile_name")]
    pub default_profile: String,

    /// Globally defined profiles available to all workspaces (§11.2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profiles: HashMap<String, ProfileDefinition>,

    /// Global keybinding configuration (§11.3).
    #[serde(default = "default_bindings")]
    pub bindings: BindingsDefinition,

    /// Default scrollback buffer size. Default: 10000.
    #[serde(rename = "scrollbackLines", default = "default_scrollback_lines")]
    pub scrollback_lines: u32,

    /// Default restart policy. Default: `never`.
    #[serde(rename = "restartPolicy", default)]
    pub restart_policy: RestartPolicy,

    /// Terminal font configuration (§11.4).
    #[serde(default)]
    pub font: FontConfig,

    /// Color theme configuration (§11.5).
    #[serde(default)]
    pub theme: ThemeConfig,

    /// If true, selecting text automatically copies to clipboard. Default: false.
    #[serde(rename = "copyOnSelect", default)]
    pub copy_on_select: bool,

    /// If true, closing a window with running sessions shows a confirmation. Default: true.
    #[serde(rename = "confirmClose", default = "default_confirm_close")]
    pub confirm_close: bool,

    /// Seconds of idle time before host auto-shuts down. Null means never.
    #[serde(rename = "hostIdleShutdown", default)]
    pub host_idle_shutdown: Option<u64>,

    /// Logging level. Default: `info`.
    #[serde(rename = "logLevel", default)]
    pub log_level: LogLevel,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            default_profile: default_profile_name(),
            profiles: HashMap::new(),
            bindings: default_bindings(),
            scrollback_lines: default_scrollback_lines(),
            restart_policy: RestartPolicy::default(),
            font: FontConfig::default(),
            theme: ThemeConfig::default(),
            copy_on_select: false,
            confirm_close: true,
            host_idle_shutdown: None,
            log_level: LogLevel::default(),
        }
    }
}

fn default_profile_name() -> String {
    "powershell".to_string()
}

fn default_scrollback_lines() -> u32 {
    10000
}

fn default_confirm_close() -> bool {
    true
}

// ── Default Keybindings (§11.3) ─────────────────────────────────────────────

/// Returns the built-in default keybindings per §11.3.
pub fn default_bindings() -> BindingsDefinition {
    let keys: HashMap<String, ActionReference> = [
        ("Ctrl+Shift+T", "new-tab"),
        ("Ctrl+Shift+W", "close-pane"),
        ("Ctrl+Shift+Space", "toggle-command-palette"),
        ("Ctrl+Shift+C", "copy"),
        ("Ctrl+Shift+V", "paste"),
        ("Ctrl+Tab", "next-tab"),
        ("Ctrl+Shift+Tab", "prev-tab"),
        ("Alt+Shift+D", "split-right"),
        ("Alt+Shift+Minus", "split-down"),
        ("F11", "toggle-fullscreen"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), ActionReference::Simple(v.to_string())))
    .collect();

    let chords: HashMap<String, ActionReference> = [
        ("%", "split-right"),
        ("\"", "split-down"),
        ("o", "focus-next-pane"),
        ("c", "new-tab"),
        (",", "rename-pane"),
        ("x", "close-pane"),
        ("z", "zoom-pane"),
        ("n", "next-tab"),
        ("p", "prev-tab"),
        ("d", "close-workspace"),
        ("Up", "focus-pane-up"),
        ("Down", "focus-pane-down"),
        ("Left", "focus-pane-left"),
        ("Right", "focus-pane-right"),
        ("[", "enter-scrollback-mode"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), ActionReference::Simple(v.to_string())))
    .collect();

    BindingsDefinition {
        prefix: Some("Ctrl+B".to_string()),
        prefix_timeout: Some(2000),
        chords: Some(chords),
        keys: Some(keys),
    }
}

// ── Settings Loading ────────────────────────────────────────────────────────

/// Load global settings from the given file path.
///
/// If the file does not exist, returns built-in defaults.
/// If the file exists but is empty, returns built-in defaults.
/// Fields missing from the file get their default values via serde.
pub fn load_global_settings(path: &Path) -> Result<GlobalSettings, SettingsLoadError> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return Ok(GlobalSettings::default());
            }
            let settings: GlobalSettings = serde_yaml::from_str(trimmed)?;
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GlobalSettings::default()),
        Err(e) => Err(SettingsLoadError::Io(e)),
    }
}

// ── Keybinding Merge (§11.6) ────────────────────────────────────────────────

/// Merge workspace-level bindings on top of global bindings per §11.6.
///
/// - Workspace `chords` override global for the same chord key; unoverridden preserved.
/// - Workspace `keys` override global for the same key spec; unoverridden preserved.
/// - Workspace `prefix` overrides global `prefix` if set.
/// - Workspace `prefixTimeout` overrides global if set.
pub fn merge_bindings(
    global: &BindingsDefinition,
    workspace: &BindingsDefinition,
) -> BindingsDefinition {
    let prefix = workspace.prefix.clone().or_else(|| global.prefix.clone());
    let prefix_timeout = workspace.prefix_timeout.or(global.prefix_timeout);

    let chords = merge_action_maps(&global.chords, &workspace.chords);
    let keys = merge_action_maps(&global.keys, &workspace.keys);

    BindingsDefinition {
        prefix,
        prefix_timeout,
        chords,
        keys,
    }
}

fn merge_action_maps(
    base: &Option<HashMap<String, ActionReference>>,
    overlay: &Option<HashMap<String, ActionReference>>,
) -> Option<HashMap<String, ActionReference>> {
    match (base, overlay) {
        (None, None) => None,
        (Some(b), None) => Some(b.clone()),
        (None, Some(o)) => Some(o.clone()),
        (Some(b), Some(o)) => {
            let mut merged = b.clone();
            for (k, v) in o {
                merged.insert(k.clone(), v.clone());
            }
            Some(merged)
        }
    }
}

// ── RestartPolicy default ───────────────────────────────────────────────────

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::Never
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Missing file → defaults ─────────────────────────────────────────

    #[test]
    fn missing_file_returns_defaults() {
        let settings =
            load_global_settings(Path::new("/nonexistent/path/settings.yaml")).unwrap();
        assert_eq!(settings, GlobalSettings::default());
    }

    #[test]
    fn defaults_have_expected_values() {
        let s = GlobalSettings::default();
        assert_eq!(s.default_profile, "powershell");
        assert!(s.profiles.is_empty());
        assert_eq!(s.scrollback_lines, 10000);
        assert_eq!(s.restart_policy, RestartPolicy::Never);
        assert!(!s.copy_on_select);
        assert!(s.confirm_close);
        assert_eq!(s.host_idle_shutdown, None);
        assert_eq!(s.log_level, LogLevel::Info);
    }

    // ── Font defaults ───────────────────────────────────────────────────

    #[test]
    fn font_defaults() {
        let f = FontConfig::default();
        assert_eq!(f.family, "Cascadia Mono");
        assert!((f.size - 12.0).abs() < f64::EPSILON);
        assert_eq!(f.weight, "normal");
    }

    // ── Theme defaults ──────────────────────────────────────────────────

    #[test]
    fn theme_defaults() {
        let t = ThemeConfig::default();
        assert_eq!(t.name, "default");
        assert_eq!(t.foreground, "#CCCCCC");
        assert_eq!(t.background, "#0C0C0C");
        assert_eq!(t.cursor_color, "#FFFFFF");
        assert_eq!(t.selection_background, "#FFFFFF");
        assert_eq!(t.palette.len(), 16);
    }

    // ── Default keybindings ─────────────────────────────────────────────

    #[test]
    fn default_bindings_populated() {
        let b = default_bindings();
        assert_eq!(b.prefix, Some("Ctrl+B".to_string()));
        assert_eq!(b.prefix_timeout, Some(2000));

        let keys = b.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 10);
        assert_eq!(
            keys.get("Ctrl+Shift+T"),
            Some(&ActionReference::Simple("new-tab".to_string()))
        );
        assert_eq!(
            keys.get("F11"),
            Some(&ActionReference::Simple("toggle-fullscreen".to_string()))
        );

        let chords = b.chords.as_ref().unwrap();
        assert_eq!(chords.len(), 15);
        assert_eq!(
            chords.get("%"),
            Some(&ActionReference::Simple("split-right".to_string()))
        );
        assert_eq!(
            chords.get("["),
            Some(&ActionReference::Simple(
                "enter-scrollback-mode".to_string()
            ))
        );
    }

    // ── Partial overrides (some fields set, others default) ─────────────

    #[test]
    fn partial_yaml_fills_defaults() {
        let yaml = r#"
defaultProfile: "cmd"
scrollbackLines: 5000
"#;
        let dir = std::env::temp_dir().join("wtd_test_partial");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, yaml).unwrap();

        let s = load_global_settings(&path).unwrap();
        assert_eq!(s.default_profile, "cmd");
        assert_eq!(s.scrollback_lines, 5000);
        // Unset fields get defaults:
        assert_eq!(s.restart_policy, RestartPolicy::Never);
        assert!(s.confirm_close);
        assert_eq!(s.font, FontConfig::default());
        assert_eq!(s.theme, ThemeConfig::default());
        assert_eq!(s.log_level, LogLevel::Info);
        // Bindings should be defaults since not overridden:
        let keys = s.bindings.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 10);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_font_override() {
        let yaml = r#"
font:
  family: "JetBrains Mono"
"#;
        let dir = std::env::temp_dir().join("wtd_test_font_partial");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, yaml).unwrap();

        let s = load_global_settings(&path).unwrap();
        assert_eq!(s.font.family, "JetBrains Mono");
        assert!((s.font.size - 12.0).abs() < f64::EPSILON); // default
        assert_eq!(s.font.weight, "normal"); // default

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_theme_override() {
        let yaml = r##"
theme:
  name: "solarized"
  foreground: "#839496"
"##;
        let dir = std::env::temp_dir().join("wtd_test_theme_partial");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, yaml).unwrap();

        let s = load_global_settings(&path).unwrap();
        assert_eq!(s.theme.name, "solarized");
        assert_eq!(s.theme.foreground, "#839496");
        assert_eq!(s.theme.background, "#0C0C0C"); // default

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_returns_defaults() {
        let dir = std::env::temp_dir().join("wtd_test_empty_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, "").unwrap();

        let s = load_global_settings(&path).unwrap();
        assert_eq!(s, GlobalSettings::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Full YAML round-trip ────────────────────────────────────────────

    #[test]
    fn full_settings_yaml_round_trip() {
        let yaml = r##"
defaultProfile: "wsl"
scrollbackLines: 20000
restartPolicy: on-failure
copyOnSelect: true
confirmClose: false
hostIdleShutdown: 300
logLevel: debug
font:
  family: "Fira Code"
  size: 14.0
  weight: bold
theme:
  name: "monokai"
  foreground: "#F8F8F2"
  background: "#272822"
  cursorColor: "#F8F8F0"
  selectionBackground: "#49483E"
bindings:
  prefix: "Ctrl+A"
  prefixTimeout: 3000
"##;
        let dir = std::env::temp_dir().join("wtd_test_full_rt");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.yaml");
        std::fs::write(&path, yaml).unwrap();

        let s = load_global_settings(&path).unwrap();
        assert_eq!(s.default_profile, "wsl");
        assert_eq!(s.scrollback_lines, 20000);
        assert_eq!(s.restart_policy, RestartPolicy::OnFailure);
        assert!(s.copy_on_select);
        assert!(!s.confirm_close);
        assert_eq!(s.host_idle_shutdown, Some(300));
        assert_eq!(s.log_level, LogLevel::Debug);
        assert_eq!(s.font.family, "Fira Code");
        assert!((s.font.size - 14.0).abs() < f64::EPSILON);
        assert_eq!(s.font.weight, "bold");
        assert_eq!(s.theme.name, "monokai");
        assert_eq!(s.bindings.prefix, Some("Ctrl+A".to_string()));
        assert_eq!(s.bindings.prefix_timeout, Some(3000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Keybinding merge precedence (§11.6) ─────────────────────────────

    #[test]
    fn merge_workspace_chords_override_global() {
        let global = default_bindings();
        let workspace = BindingsDefinition {
            prefix: None,
            prefix_timeout: None,
            chords: Some(
                [(
                    "%".to_string(),
                    ActionReference::Simple("split-down".to_string()),
                )]
                .into_iter()
                .collect(),
            ),
            keys: None,
        };

        let merged = merge_bindings(&global, &workspace);
        let chords = merged.chords.as_ref().unwrap();

        // Overridden chord:
        assert_eq!(
            chords.get("%"),
            Some(&ActionReference::Simple("split-down".to_string()))
        );
        // Preserved global chords:
        assert_eq!(
            chords.get("o"),
            Some(&ActionReference::Simple("focus-next-pane".to_string()))
        );
        assert_eq!(
            chords.get("["),
            Some(&ActionReference::Simple(
                "enter-scrollback-mode".to_string()
            ))
        );
        // All 15 global chords present (1 overridden, 14 preserved):
        assert_eq!(chords.len(), 15);
    }

    #[test]
    fn merge_workspace_keys_override_global() {
        let global = default_bindings();
        let workspace = BindingsDefinition {
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: Some(
                [
                    (
                        "Ctrl+Shift+T".to_string(),
                        ActionReference::Simple("custom-action".to_string()),
                    ),
                    (
                        "Ctrl+N".to_string(),
                        ActionReference::Simple("new-window".to_string()),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
        };

        let merged = merge_bindings(&global, &workspace);
        let keys = merged.keys.as_ref().unwrap();

        // Overridden:
        assert_eq!(
            keys.get("Ctrl+Shift+T"),
            Some(&ActionReference::Simple("custom-action".to_string()))
        );
        // New workspace-only key:
        assert_eq!(
            keys.get("Ctrl+N"),
            Some(&ActionReference::Simple("new-window".to_string()))
        );
        // Preserved global key:
        assert_eq!(
            keys.get("F11"),
            Some(&ActionReference::Simple("toggle-fullscreen".to_string()))
        );
        // 10 global + 1 new = 11
        assert_eq!(keys.len(), 11);
    }

    #[test]
    fn merge_workspace_prefix_overrides_global() {
        let global = default_bindings();
        let workspace = BindingsDefinition {
            prefix: Some("Ctrl+A".to_string()),
            prefix_timeout: Some(5000),
            chords: None,
            keys: None,
        };

        let merged = merge_bindings(&global, &workspace);
        assert_eq!(merged.prefix, Some("Ctrl+A".to_string()));
        assert_eq!(merged.prefix_timeout, Some(5000));
        // Chords and keys preserved from global:
        assert_eq!(merged.chords.as_ref().unwrap().len(), 15);
        assert_eq!(merged.keys.as_ref().unwrap().len(), 10);
    }

    #[test]
    fn merge_empty_workspace_preserves_global() {
        let global = default_bindings();
        let workspace = BindingsDefinition::default();

        let merged = merge_bindings(&global, &workspace);
        assert_eq!(merged.prefix, global.prefix);
        assert_eq!(merged.prefix_timeout, global.prefix_timeout);
        assert_eq!(merged.chords, global.chords);
        assert_eq!(merged.keys, global.keys);
    }

    #[test]
    fn merge_both_empty_returns_empty() {
        let merged = merge_bindings(&BindingsDefinition::default(), &BindingsDefinition::default());
        assert_eq!(merged.prefix, None);
        assert_eq!(merged.prefix_timeout, None);
        assert_eq!(merged.chords, None);
        assert_eq!(merged.keys, None);
    }
}
