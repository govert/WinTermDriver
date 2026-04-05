//! Global user configuration (§11).
//!
//! Provides [`GlobalSettings`] (the full §11.2 schema), [`FontConfig`] (§11.4),
//! [`ThemeConfig`] (§11.5), built-in default keybindings (§11.3), file loading,
//! and merge-precedence helpers (§11.6).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::workspace::{
    ActionReference, BindingPreset, BindingsDefinition, ProfileDefinition, RestartPolicy,
};

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

// ── Preset Keybindings ───────────────────────────────────────────────────────

/// Returns the tmux-style preset keybindings.
///
/// Ctrl+B prefix, 10 single-stroke keys, 15 chords — the legacy WTD defaults.
pub fn tmux_bindings() -> BindingsDefinition {
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
        preset: Some(BindingPreset::Tmux),
        prefix: Some("Ctrl+B".to_string()),
        prefix_timeout: Some(2000),
        chords: Some(chords),
        keys: Some(keys),
    }
}

/// Returns the Windows Terminal-style preset keybindings.
///
/// Single-stroke only — no prefix key, no chords.  Matches WT default bindings
/// where a WTD action equivalent exists (§11.3, audit in docs/WT_KEYBINDING_MAP.md).
///
/// **28 bindings total:**
/// - Tab: new/close/next/prev tab, goto-tab 1–8 (Ctrl+Alt+1–8)
/// - Pane: split right/down, focus up/down/left/right, resize in all directions
/// - Clipboard: copy/paste (primary + secondary WT bindings)
/// - UI: toggle-fullscreen, toggle-command-palette
pub fn windows_terminal_bindings() -> BindingsDefinition {
    let mut keys: HashMap<String, ActionReference> = [
        // ── Tab management ──────────────────────────────────────────────────
        (
            "Ctrl+Shift+T",
            ActionReference::Simple("new-tab".to_string()),
        ),
        (
            "Ctrl+Shift+W",
            ActionReference::Simple("close-pane".to_string()),
        ),
        ("Ctrl+Tab", ActionReference::Simple("next-tab".to_string())),
        (
            "Ctrl+Shift+Tab",
            ActionReference::Simple("prev-tab".to_string()),
        ),
        // ── Pane management ─────────────────────────────────────────────────
        // WT: alt+shift+plus → split right (WTD default uses Alt+Shift+D)
        (
            "Alt+Shift+Plus",
            ActionReference::Simple("split-right".to_string()),
        ),
        (
            "Alt+Shift+Minus",
            ActionReference::Simple("split-down".to_string()),
        ),
        (
            "Alt+Down",
            ActionReference::Simple("focus-pane-down".to_string()),
        ),
        (
            "Alt+Up",
            ActionReference::Simple("focus-pane-up".to_string()),
        ),
        (
            "Alt+Left",
            ActionReference::Simple("focus-pane-left".to_string()),
        ),
        (
            "Alt+Right",
            ActionReference::Simple("focus-pane-right".to_string()),
        ),
        (
            "Alt+Shift+Down",
            ActionReference::Simple("resize-pane-grow-down".to_string()),
        ),
        // WT resize-up = shrink from below
        (
            "Alt+Shift+Up",
            ActionReference::Simple("resize-pane-shrink-down".to_string()),
        ),
        (
            "Alt+Shift+Right",
            ActionReference::Simple("resize-pane-grow-right".to_string()),
        ),
        // WT resize-left = shrink from right
        (
            "Alt+Shift+Left",
            ActionReference::Simple("resize-pane-shrink-right".to_string()),
        ),
        // ── Clipboard ───────────────────────────────────────────────────────
        ("Ctrl+Shift+C", ActionReference::Simple("copy".to_string())),
        ("Ctrl+Shift+V", ActionReference::Simple("paste".to_string())),
        // Secondary WT bindings
        ("Ctrl+Insert", ActionReference::Simple("copy".to_string())),
        ("Shift+Insert", ActionReference::Simple("paste".to_string())),
        // ── UI ──────────────────────────────────────────────────────────────
        (
            "F11",
            ActionReference::Simple("toggle-fullscreen".to_string()),
        ),
        // WT uses Ctrl+Shift+P; WTD default uses Ctrl+Shift+Space
        (
            "Ctrl+Shift+P",
            ActionReference::Simple("toggle-command-palette".to_string()),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();

    // Ctrl+Alt+1..8 → goto-tab index 0..7 (WT uses 0-based index internally).
    for n in 1u8..=8 {
        keys.insert(
            format!("Ctrl+Alt+{n}"),
            ActionReference::WithArgs {
                action: "goto-tab".to_string(),
                args: Some(
                    [("index".to_string(), (n - 1).to_string())]
                        .into_iter()
                        .collect(),
                ),
            },
        );
    }

    BindingsDefinition {
        preset: Some(BindingPreset::WindowsTerminal),
        prefix: None,
        prefix_timeout: None,
        chords: None,
        keys: Some(keys),
    }
}

/// Expand a `BindingPreset` into its base `BindingsDefinition` (no preset field
/// set on the returned value).
fn expand_preset(preset: &BindingPreset) -> BindingsDefinition {
    match preset {
        BindingPreset::Tmux => {
            let base = tmux_bindings();
            BindingsDefinition {
                preset: None,
                ..base
            }
        }
        BindingPreset::WindowsTerminal => {
            let base = windows_terminal_bindings();
            BindingsDefinition {
                preset: None,
                ..base
            }
        }
        BindingPreset::None => BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        },
    }
}

/// Resolve the effective (fully expanded) bindings for a `BindingsDefinition`.
///
/// Expands the preset into its base keys/chords/prefix, then applies user
/// overrides from the `keys`, `chords`, `prefix`, and `prefix_timeout` fields.
/// The returned value always has `preset: None`.
pub fn effective_bindings(def: &BindingsDefinition) -> BindingsDefinition {
    let base = match &def.preset {
        Some(preset) => expand_preset(preset),
        None => BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        },
    };

    let prefix = def.prefix.clone().or(base.prefix);
    let prefix_timeout = def.prefix_timeout.or(base.prefix_timeout);
    let keys = merge_action_maps(&base.keys, &def.keys);
    let chords = merge_action_maps(&base.chords, &def.chords);

    BindingsDefinition {
        preset: None,
        prefix,
        prefix_timeout,
        chords,
        keys,
    }
}

// ── Default Keybindings (§11.3) ─────────────────────────────────────────────

/// Returns the built-in default keybindings per §11.3.
///
/// The default preset is `windows-terminal`. Use [`tmux_bindings`] to get
/// the tmux-style preset.
pub fn default_bindings() -> BindingsDefinition {
    BindingsDefinition {
        preset: Some(BindingPreset::WindowsTerminal),
        prefix: None,
        prefix_timeout: None,
        chords: None,
        keys: None,
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
/// Both `global` and `workspace` presets are expanded first.  Then workspace
/// effective keys/chords/prefix override global effective values.
///
/// - Workspace `chords` override global for the same chord key; unoverridden preserved.
/// - Workspace `keys` override global for the same key spec; unoverridden preserved.
/// - Workspace `prefix` overrides global `prefix` if set.
/// - Workspace `prefixTimeout` overrides global if set.
pub fn merge_bindings(
    global: &BindingsDefinition,
    workspace: &BindingsDefinition,
) -> BindingsDefinition {
    let eff_global = effective_bindings(global);
    let eff_workspace = effective_bindings(workspace);

    let prefix = eff_workspace.prefix.or(eff_global.prefix);
    let prefix_timeout = eff_workspace.prefix_timeout.or(eff_global.prefix_timeout);

    let chords = merge_action_maps(&eff_global.chords, &eff_workspace.chords);
    let keys = merge_action_maps(&eff_global.keys, &eff_workspace.keys);

    BindingsDefinition {
        preset: None,
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
        (None, Some(o)) => {
            // Filter out Removed entries (no base to remove from, just skip)
            let filtered: HashMap<_, _> = o
                .iter()
                .filter(|(_, v)| !matches!(v, ActionReference::Removed))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if filtered.is_empty() {
                None
            } else {
                Some(filtered)
            }
        }
        (Some(b), Some(o)) => {
            let mut merged = b.clone();
            for (k, v) in o {
                if matches!(v, ActionReference::Removed) {
                    // Null/Removed in overlay means "delete this key from the preset"
                    merged.remove(k);
                } else {
                    merged.insert(k.clone(), v.clone());
                }
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
        let settings = load_global_settings(Path::new("/nonexistent/path/settings.yaml")).unwrap();
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
    fn windows_terminal_bindings_populated() {
        let b = windows_terminal_bindings();
        assert_eq!(b.preset, Some(BindingPreset::WindowsTerminal));
        // No prefix — WT uses single-stroke bindings only.
        assert!(b.prefix.is_none());
        assert!(b.prefix_timeout.is_none());
        assert!(b.chords.is_none());

        let keys = b.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 28, "28 WT-mapped single-stroke bindings");

        // Tab management
        assert_eq!(
            keys.get("Ctrl+Shift+T"),
            Some(&ActionReference::Simple("new-tab".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Shift+W"),
            Some(&ActionReference::Simple("close-pane".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Tab"),
            Some(&ActionReference::Simple("next-tab".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Shift+Tab"),
            Some(&ActionReference::Simple("prev-tab".to_string()))
        );

        // goto-tab bindings: Ctrl+Alt+1 → index 0, Ctrl+Alt+8 → index 7
        assert_eq!(
            keys.get("Ctrl+Alt+1"),
            Some(&ActionReference::WithArgs {
                action: "goto-tab".to_string(),
                args: Some(
                    [("index".to_string(), "0".to_string())]
                        .into_iter()
                        .collect()
                ),
            })
        );
        assert_eq!(
            keys.get("Ctrl+Alt+8"),
            Some(&ActionReference::WithArgs {
                action: "goto-tab".to_string(),
                args: Some(
                    [("index".to_string(), "7".to_string())]
                        .into_iter()
                        .collect()
                ),
            })
        );
        // Ctrl+Alt+9 is NOT present (WT's "last tab" has no WTD equivalent)
        assert!(!keys.contains_key("Ctrl+Alt+9"));

        // Pane management
        assert_eq!(
            keys.get("Alt+Shift+Plus"),
            Some(&ActionReference::Simple("split-right".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Shift+Minus"),
            Some(&ActionReference::Simple("split-down".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Down"),
            Some(&ActionReference::Simple("focus-pane-down".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Up"),
            Some(&ActionReference::Simple("focus-pane-up".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Left"),
            Some(&ActionReference::Simple("focus-pane-left".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Right"),
            Some(&ActionReference::Simple("focus-pane-right".to_string()))
        );
        assert_eq!(
            keys.get("Alt+Shift+Down"),
            Some(&ActionReference::Simple(
                "resize-pane-grow-down".to_string()
            ))
        );
        assert_eq!(
            keys.get("Alt+Shift+Up"),
            Some(&ActionReference::Simple(
                "resize-pane-shrink-down".to_string()
            ))
        );
        assert_eq!(
            keys.get("Alt+Shift+Right"),
            Some(&ActionReference::Simple(
                "resize-pane-grow-right".to_string()
            ))
        );
        assert_eq!(
            keys.get("Alt+Shift+Left"),
            Some(&ActionReference::Simple(
                "resize-pane-shrink-right".to_string()
            ))
        );

        // Clipboard — primary and secondary WT bindings
        assert_eq!(
            keys.get("Ctrl+Shift+C"),
            Some(&ActionReference::Simple("copy".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Shift+V"),
            Some(&ActionReference::Simple("paste".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Insert"),
            Some(&ActionReference::Simple("copy".to_string()))
        );
        assert_eq!(
            keys.get("Shift+Insert"),
            Some(&ActionReference::Simple("paste".to_string()))
        );

        // UI
        assert_eq!(
            keys.get("F11"),
            Some(&ActionReference::Simple("toggle-fullscreen".to_string()))
        );
        // WT uses Ctrl+Shift+P for command palette (WTD default is Ctrl+Shift+Space)
        assert_eq!(
            keys.get("Ctrl+Shift+P"),
            Some(&ActionReference::Simple(
                "toggle-command-palette".to_string()
            ))
        );

        // Omitted: no scroll-line/page actions in WTD v1
        assert!(!keys.contains_key("Ctrl+Shift+Up"));
        assert!(!keys.contains_key("Ctrl+Shift+Down"));
        assert!(!keys.contains_key("Ctrl+Shift+PageUp"));
        assert!(!keys.contains_key("Ctrl+Shift+PageDown"));
        // Omitted: no find action in WTD v1
        assert!(!keys.contains_key("Ctrl+Shift+F"));
    }

    #[test]
    fn default_bindings_is_windows_terminal_preset() {
        let b = default_bindings();
        assert_eq!(
            b.preset,
            Some(BindingPreset::WindowsTerminal),
            "default preset must be windows-terminal"
        );
        // Default bindings delegate entirely to the preset; no explicit overrides.
        assert!(b.prefix.is_none());
        assert!(b.keys.is_none());
        assert!(b.chords.is_none());
    }

    #[test]
    fn tmux_bindings_populated() {
        let b = tmux_bindings();
        assert_eq!(b.preset, Some(BindingPreset::Tmux));
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

    #[test]
    fn effective_bindings_expands_tmux_preset() {
        let def = BindingsDefinition {
            preset: Some(BindingPreset::Tmux),
            ..BindingsDefinition::default()
        };
        let eff = effective_bindings(&def);
        assert!(
            eff.preset.is_none(),
            "effective bindings must have no preset"
        );
        assert_eq!(eff.prefix, Some("Ctrl+B".to_string()));
        assert_eq!(eff.prefix_timeout, Some(2000));
        let keys = eff.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 10);
        let chords = eff.chords.as_ref().unwrap();
        assert_eq!(chords.len(), 15);
    }

    #[test]
    fn effective_bindings_expands_windows_terminal_preset() {
        let def = BindingsDefinition {
            preset: Some(BindingPreset::WindowsTerminal),
            ..BindingsDefinition::default()
        };
        let eff = effective_bindings(&def);
        assert!(eff.preset.is_none());
        // WT preset is single-stroke only — no prefix, no chords.
        assert!(eff.prefix.is_none());
        assert!(eff.chords.is_none());
        // But it has 28 single-stroke keys.
        let keys = eff.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 28);
        assert_eq!(
            keys.get("Ctrl+Shift+T"),
            Some(&ActionReference::Simple("new-tab".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Shift+P"),
            Some(&ActionReference::Simple(
                "toggle-command-palette".to_string()
            ))
        );
    }

    #[test]
    fn effective_bindings_user_overrides_layer_on_preset() {
        let mut extra_key = HashMap::new();
        extra_key.insert(
            "Ctrl+N".to_string(),
            ActionReference::Simple("new-window".to_string()),
        );
        let def = BindingsDefinition {
            preset: Some(BindingPreset::Tmux),
            prefix: None,
            prefix_timeout: None,
            keys: Some(extra_key),
            chords: None,
        };
        let eff = effective_bindings(&def);
        // Tmux preset keys + user extra key.
        let keys = eff.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 11, "tmux 10 + 1 user key");
        assert_eq!(
            keys.get("Ctrl+N"),
            Some(&ActionReference::Simple("new-window".to_string()))
        );
        assert_eq!(
            keys.get("Ctrl+Shift+T"),
            Some(&ActionReference::Simple("new-tab".to_string()))
        );
    }

    #[test]
    fn effective_bindings_user_override_replaces_preset_entry() {
        let mut override_key = HashMap::new();
        override_key.insert(
            "%".to_string(),
            ActionReference::Simple("split-down".to_string()),
        );
        let def = BindingsDefinition {
            preset: Some(BindingPreset::Tmux),
            prefix: None,
            prefix_timeout: None,
            keys: None,
            chords: Some(override_key),
        };
        let eff = effective_bindings(&def);
        let chords = eff.chords.as_ref().unwrap();
        assert_eq!(chords.len(), 15, "still 15 chords");
        assert_eq!(
            chords.get("%"),
            Some(&ActionReference::Simple("split-down".to_string())),
            "user override must replace preset value"
        );
    }

    #[test]
    fn preset_none_produces_empty_effective_bindings() {
        let def = BindingsDefinition {
            preset: Some(BindingPreset::None),
            ..BindingsDefinition::default()
        };
        let eff = effective_bindings(&def);
        assert!(eff.prefix.is_none());
        assert!(eff.keys.is_none());
        assert!(eff.chords.is_none());
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
        // Bindings should be the default (windows-terminal preset, placeholder empty):
        assert_eq!(
            s.bindings.preset,
            Some(BindingPreset::WindowsTerminal),
            "default bindings must use windows-terminal preset"
        );

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
        // Use tmux preset as global to get 15 chords/10 keys to merge against.
        let global = tmux_bindings();
        let workspace = BindingsDefinition {
            preset: None,
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
        let global = tmux_bindings();
        let workspace = BindingsDefinition {
            preset: None,
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
        let global = tmux_bindings();
        let workspace = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+A".to_string()),
            prefix_timeout: Some(5000),
            chords: None,
            keys: None,
        };

        let merged = merge_bindings(&global, &workspace);
        assert_eq!(merged.prefix, Some("Ctrl+A".to_string()));
        assert_eq!(merged.prefix_timeout, Some(5000));
        // Chords and keys preserved from global (tmux preset):
        assert_eq!(merged.chords.as_ref().unwrap().len(), 15);
        assert_eq!(merged.keys.as_ref().unwrap().len(), 10);
    }

    #[test]
    fn merge_empty_workspace_preserves_global() {
        let global = tmux_bindings();
        let workspace = BindingsDefinition::default();

        let merged = merge_bindings(&global, &workspace);
        // Effective global has tmux content; workspace adds nothing.
        assert_eq!(merged.prefix, Some("Ctrl+B".to_string()));
        assert_eq!(merged.prefix_timeout, Some(2000));
        assert_eq!(merged.chords.as_ref().unwrap().len(), 15);
        assert_eq!(merged.keys.as_ref().unwrap().len(), 10);
    }

    #[test]
    fn merge_both_empty_returns_empty() {
        let merged = merge_bindings(
            &BindingsDefinition::default(),
            &BindingsDefinition::default(),
        );
        assert_eq!(merged.prefix, None);
        assert_eq!(merged.prefix_timeout, None);
        assert_eq!(merged.chords, None);
        assert_eq!(merged.keys, None);
    }

    #[test]
    fn merge_tmux_global_with_windows_terminal_workspace_preset() {
        // Workspace with windows-terminal preset overlaid on tmux global.
        let global = tmux_bindings();
        let workspace = BindingsDefinition {
            preset: Some(BindingPreset::WindowsTerminal),
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let merged = merge_bindings(&global, &workspace);

        // WT has no prefix — tmux prefix is preserved from global.
        assert_eq!(merged.prefix, Some("Ctrl+B".to_string()));
        // WT has no chords — tmux chords (15) are preserved from global.
        assert_eq!(merged.chords.as_ref().unwrap().len(), 15);

        // Keys: WT contributes 28 keys; tmux has 10.
        // 8 overlap (Ctrl+Shift+T/W/C/V, Ctrl+Tab, Ctrl+Shift+Tab, Alt+Shift+Minus, F11).
        // Tmux-unique: Ctrl+Shift+Space, Alt+Shift+D — 2 keys.
        // Merged = 28 WT + 2 tmux-unique = 30.
        let keys = merged.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 30);
        // WT-unique keys are present.
        assert!(keys.contains_key("Alt+Down"), "WT focus key present");
        assert!(keys.contains_key("Ctrl+Shift+P"), "WT palette key present");
        // Tmux-unique keys are preserved.
        assert!(
            keys.contains_key("Ctrl+Shift+Space"),
            "tmux palette key preserved"
        );
        assert!(
            keys.contains_key("Alt+Shift+D"),
            "tmux split-right key preserved"
        );
    }

    #[test]
    fn merge_workspace_tmux_preset_overrides_global_keys() {
        // Global has a custom binding; workspace switches to tmux preset.
        let mut custom_keys = HashMap::new();
        custom_keys.insert(
            "Ctrl+X".to_string(),
            ActionReference::Simple("custom".to_string()),
        );
        let global = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            keys: Some(custom_keys),
            chords: None,
        };
        let workspace = BindingsDefinition {
            preset: Some(BindingPreset::Tmux),
            prefix: None,
            prefix_timeout: None,
            keys: None,
            chords: None,
        };
        let merged = merge_bindings(&global, &workspace);
        // Global has 1 key (Ctrl+X); workspace expands to 10 tmux keys; merged is 11.
        let keys = merged.keys.as_ref().unwrap();
        assert_eq!(keys.len(), 11, "1 global + 10 tmux = 11");
        assert!(keys.contains_key("Ctrl+X"), "global Ctrl+X preserved");
        assert!(keys.contains_key("Ctrl+Shift+T"), "tmux key present");
    }
}
