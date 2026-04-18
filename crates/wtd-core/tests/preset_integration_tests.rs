//! Integration tests for the keybinding preset system (§11.3, §11.6).
//!
//! Verifies preset loading, expansion, override layering, and workspace-level
//! override of global presets — all exercised through the YAML loading path.
//!
//! Bead: wintermdriver-h35.4

use std::collections::HashMap;
use std::path::Path;

use wtd_core::global_settings::{effective_bindings, load_global_settings, merge_bindings};
use wtd_core::workspace::{ActionReference, BindingPreset, BindingsDefinition};
use wtd_core::GlobalSettings;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Write `content` to a temporary file under `dir` and return the path.
fn write_settings(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Load settings YAML, expand its bindings with `effective_bindings`, and
/// return the fully-expanded `BindingsDefinition` (preset: None, all
/// keys/chords/prefix resolved).
fn load_and_expand(yaml: &str, dir: &Path, file: &str) -> BindingsDefinition {
    let path = write_settings(dir, file, yaml);
    let settings = load_global_settings(&path).expect("settings must load");
    effective_bindings(&settings.bindings)
}

// ── Test 1: windows-terminal preset produces full 28-key WT binding set ──────

#[test]
fn preset_windows_terminal_produces_full_wt_binding_set() {
    let yaml = r#"
bindings:
  preset: windows-terminal
"#;
    let dir = std::env::temp_dir().join("wtd_h354_wt");
    let eff = load_and_expand(yaml, &dir, "settings.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    // Preset expanded — no preset marker remains.
    assert!(
        eff.preset.is_none(),
        "effective bindings must have no preset"
    );

    // WT preset is single-stroke only.
    assert!(eff.prefix.is_none(), "WT has no prefix key");
    assert!(eff.chords.is_none(), "WT has no chord bindings");

    let keys = eff.keys.as_ref().expect("WT keys must be present");

    // Check the JSON fixture count: 20 base bindings + 8 goto-tab = 28.
    assert_eq!(keys.len(), 28, "windows-terminal preset must have 28 keys");

    // ── Spot-check against docs/wt_preset_bindings.json ──────────────────

    // Tab management
    let expect_simple = |k: &str, action: &str| {
        assert_eq!(
            keys.get(k),
            Some(&ActionReference::Simple(action.to_string())),
            "expected {k} → {action}"
        );
    };

    expect_simple("Ctrl+Shift+T", "new-tab");
    expect_simple("Ctrl+Shift+W", "close-pane");
    expect_simple("Ctrl+Shift+C", "copy");
    expect_simple("Ctrl+Shift+V", "paste");
    expect_simple("Ctrl+Tab", "next-tab");
    expect_simple("Ctrl+Shift+Tab", "prev-tab");
    expect_simple("Ctrl+Shift+P", "toggle-command-palette");
    expect_simple("F11", "toggle-fullscreen");

    // Pane management
    expect_simple("Alt+Shift+Plus", "split-right");
    expect_simple("Alt+Shift+Minus", "split-down");
    expect_simple("Alt+Down", "focus-pane-down");
    expect_simple("Alt+Up", "focus-pane-up");
    expect_simple("Alt+Left", "focus-pane-left");
    expect_simple("Alt+Right", "focus-pane-right");
    expect_simple("Alt+Shift+Down", "resize-pane-down");
    expect_simple("Alt+Shift+Up", "resize-pane-up");
    expect_simple("Alt+Shift+Right", "resize-pane-right");
    expect_simple("Alt+Shift+Left", "resize-pane-left");

    // Secondary clipboard bindings
    expect_simple("Ctrl+Insert", "copy");
    expect_simple("Shift+Insert", "paste");

    // goto-tab: Ctrl+Alt+1 → index "0", Ctrl+Alt+8 → index "7"
    let goto1 = keys.get("Ctrl+Alt+1").expect("Ctrl+Alt+1 must be present");
    match goto1 {
        ActionReference::WithArgs { action, args } => {
            assert_eq!(action, "goto-tab");
            let idx = args.as_ref().unwrap().get("index").unwrap();
            assert_eq!(idx, "0", "Ctrl+Alt+1 → index 0");
        }
        other => panic!("expected WithArgs for goto-tab, got {other:?}"),
    }

    let goto8 = keys.get("Ctrl+Alt+8").expect("Ctrl+Alt+8 must be present");
    match goto8 {
        ActionReference::WithArgs { action, args } => {
            assert_eq!(action, "goto-tab");
            let idx = args.as_ref().unwrap().get("index").unwrap();
            assert_eq!(idx, "7", "Ctrl+Alt+8 → index 7");
        }
        other => panic!("expected WithArgs for goto-tab, got {other:?}"),
    }

    // Ctrl+Alt+9 must NOT be present (WT "last tab" has no WTD equivalent).
    assert!(!keys.contains_key("Ctrl+Alt+9"), "no Ctrl+Alt+9 binding");

    // Bindings omitted from the fixture must not appear.
    assert!(
        !keys.contains_key("Ctrl+Shift+Up"),
        "no scroll-line binding"
    );
    assert!(!keys.contains_key("Ctrl+Shift+F"), "no find binding");
}

// ── Test 2: tmux preset produces Ctrl+B chord-based bindings ─────────────────

#[test]
fn preset_tmux_produces_legacy_ctrlb_chord_bindings() {
    let yaml = r#"
bindings:
  preset: tmux
"#;
    let dir = std::env::temp_dir().join("wtd_h354_tmux");
    let eff = load_and_expand(yaml, &dir, "settings.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    // Prefix key and timeout
    assert_eq!(
        eff.prefix,
        Some("Ctrl+B".to_string()),
        "tmux preset must have Ctrl+B prefix"
    );
    assert_eq!(
        eff.prefix_timeout,
        Some(2000),
        "tmux preset must have 2000ms timeout"
    );

    // 10 single-stroke keys
    let keys = eff.keys.as_ref().expect("tmux must have keys");
    assert_eq!(
        keys.len(),
        10,
        "tmux preset must have 10 single-stroke keys"
    );
    assert_eq!(
        keys.get("Ctrl+Shift+T"),
        Some(&ActionReference::Simple("new-tab".to_string()))
    );
    assert_eq!(
        keys.get("F11"),
        Some(&ActionReference::Simple("toggle-fullscreen".to_string()))
    );

    // 15 chord bindings
    let chords = eff.chords.as_ref().expect("tmux must have chords");
    assert_eq!(chords.len(), 15, "tmux preset must have 15 chord bindings");
    assert_eq!(
        chords.get("%"),
        Some(&ActionReference::Simple("split-right".to_string()))
    );
    assert_eq!(
        chords.get("\""),
        Some(&ActionReference::Simple("split-down".to_string()))
    );
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
}

// ── Test 3: none preset produces empty bindings ───────────────────────────────

#[test]
fn preset_none_produces_empty_bindings() {
    let yaml = r#"
bindings:
  preset: none
"#;
    let dir = std::env::temp_dir().join("wtd_h354_none");
    let eff = load_and_expand(yaml, &dir, "settings.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(eff.preset.is_none());
    assert!(eff.prefix.is_none(), "none preset has no prefix");
    assert!(eff.prefix_timeout.is_none());
    assert!(eff.keys.is_none(), "none preset has no keys");
    assert!(eff.chords.is_none(), "none preset has no chords");
}

// ── Test 4: user overrides on top of a preset add/replace individual bindings ─

#[test]
fn user_overrides_layer_on_top_of_preset() {
    // Add a new key and override an existing one on top of the tmux preset.
    let yaml = r#"
bindings:
  preset: tmux
  keys:
    Ctrl+Shift+T: my-custom-new-tab
    Ctrl+N: new-window
"#;
    let dir = std::env::temp_dir().join("wtd_h354_override");
    let eff = load_and_expand(yaml, &dir, "settings.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    // Prefix and chords are preserved from the tmux preset.
    assert_eq!(eff.prefix, Some("Ctrl+B".to_string()));
    let chords = eff.chords.as_ref().unwrap();
    assert_eq!(chords.len(), 15);

    let keys = eff.keys.as_ref().unwrap();
    // tmux has 10 keys; user adds Ctrl+N (new); total = 11.
    assert_eq!(keys.len(), 11, "10 tmux + 1 new user key = 11");

    // Override: tmux Ctrl+Shift+T was new-tab; now custom.
    assert_eq!(
        keys.get("Ctrl+Shift+T"),
        Some(&ActionReference::Simple("my-custom-new-tab".to_string())),
        "user override must replace preset binding"
    );

    // Addition: Ctrl+N is a new user-only binding.
    assert_eq!(
        keys.get("Ctrl+N"),
        Some(&ActionReference::Simple("new-window".to_string())),
        "user addition must be present"
    );

    // Preserved: F11 (toggle-fullscreen) from tmux preset.
    assert_eq!(
        keys.get("F11"),
        Some(&ActionReference::Simple("toggle-fullscreen".to_string())),
        "unoverridden preset binding must be preserved"
    );
}

// ── Test 5: workspace-level preset overrides global preset ────────────────────

#[test]
fn workspace_preset_overrides_global_preset() {
    // Global uses tmux; workspace definition uses windows-terminal preset.
    // merge_bindings(global, workspace) → workspace preset's keys win for
    // overlapping entries; workspace adds WT-unique keys.
    let global_yaml = r#"
bindings:
  preset: tmux
"#;
    let workspace_bindings = BindingsDefinition {
        preset: Some(BindingPreset::WindowsTerminal),
        prefix: None,
        prefix_timeout: None,
        keys: None,
        chords: None,
    };

    let dir = std::env::temp_dir().join("wtd_h354_ws_override");
    let global_path = write_settings(&dir, "settings.yaml", global_yaml);
    let global_settings = load_global_settings(&global_path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let merged = merge_bindings(&global_settings.bindings, &workspace_bindings);

    // WT preset has no prefix; but tmux global has Ctrl+B — global prefix preserved.
    assert_eq!(
        merged.prefix,
        Some("Ctrl+B".to_string()),
        "global tmux prefix preserved when workspace has no prefix"
    );

    // WT has no chords; tmux global chords (15) are preserved.
    let chords = merged.chords.as_ref().unwrap();
    assert_eq!(
        chords.len(),
        15,
        "tmux global chords preserved when workspace has no chords"
    );

    // Keys: WT 28 + tmux-unique 2 (Ctrl+Shift+Space, Alt+Shift+D) = 30.
    let keys = merged.keys.as_ref().unwrap();
    assert_eq!(keys.len(), 30, "WT 28 + 2 tmux-unique = 30");

    // WT-specific keys are present.
    assert!(
        keys.contains_key("Alt+Down"),
        "WT focus-pane-down binding must be present"
    );
    assert!(
        keys.contains_key("Ctrl+Shift+P"),
        "WT toggle-command-palette binding must be present"
    );

    // tmux-unique keys (not in WT) are preserved.
    assert!(
        keys.contains_key("Ctrl+Shift+Space"),
        "tmux toggle-command-palette key preserved"
    );
    assert!(
        keys.contains_key("Alt+Shift+D"),
        "tmux split-right key preserved"
    );
}

// ── Test 6: removing a binding from a preset via null/Removed ─────────────────

#[test]
fn removing_binding_from_preset_via_null() {
    // Setting a key to `null` in YAML removes it from the effective binding set.
    let yaml = r#"
bindings:
  preset: windows-terminal
  keys:
    F11: ~
    Ctrl+Shift+P: ~
"#;
    let dir = std::env::temp_dir().join("wtd_h354_remove");
    let eff = load_and_expand(yaml, &dir, "settings.yaml");
    let _ = std::fs::remove_dir_all(&dir);

    // WT has 28 keys; two are removed → 26 remain.
    let keys = eff.keys.as_ref().unwrap();
    assert_eq!(keys.len(), 26, "28 WT keys minus 2 removed = 26");

    // Removed keys must be absent.
    assert!(!keys.contains_key("F11"), "F11 must be removed");
    assert!(
        !keys.contains_key("Ctrl+Shift+P"),
        "Ctrl+Shift+P must be removed"
    );

    // Other WT keys are still present.
    assert_eq!(
        keys.get("Ctrl+Shift+T"),
        Some(&ActionReference::Simple("new-tab".to_string())),
        "non-removed key must remain"
    );
    assert_eq!(
        keys.get("Alt+Down"),
        Some(&ActionReference::Simple("focus-pane-down".to_string())),
        "non-removed key must remain"
    );
}

// ── Test 7: default GlobalSettings uses windows-terminal preset ───────────────

#[test]
fn default_global_settings_uses_windows_terminal_preset() {
    // Verify via the built-in default (no YAML file).
    let settings = GlobalSettings::default();
    assert_eq!(
        settings.bindings.preset,
        Some(BindingPreset::WindowsTerminal),
        "default GlobalSettings must use windows-terminal preset"
    );

    // Expanding the default bindings produces the full 28-key WT set.
    let eff = effective_bindings(&settings.bindings);
    let keys = eff.keys.as_ref().expect("expanded WT keys must be present");
    assert_eq!(
        keys.len(),
        28,
        "expanded default bindings must have 28 WT keys"
    );
    assert_eq!(
        keys.get("Ctrl+Shift+T"),
        Some(&ActionReference::Simple("new-tab".to_string()))
    );

    // Also verify via a YAML file with no explicit bindings section.
    let dir = std::env::temp_dir().join("wtd_h354_default");
    let yaml_path = write_settings(&dir, "settings.yaml", "defaultProfile: powershell\n");
    let loaded = load_global_settings(&yaml_path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        loaded.bindings.preset,
        Some(BindingPreset::WindowsTerminal),
        "settings loaded without bindings section must default to windows-terminal"
    );

    let loaded_eff = effective_bindings(&loaded.bindings);
    let loaded_keys = loaded_eff.keys.as_ref().unwrap();
    assert_eq!(
        loaded_keys.len(),
        28,
        "loaded default bindings must expand to 28 keys"
    );
}

// ── Bonus: verify Removed variant round-trips through serde_yaml ──────────────

#[test]
fn action_reference_removed_deserializes_from_null() {
    // Direct YAML deserialization of a map that contains a null value.
    let yaml = r#"
F11: ~
Ctrl+T: new-tab
"#;
    let map: HashMap<String, ActionReference> =
        serde_yaml::from_str(yaml).expect("must deserialize");
    assert_eq!(map.get("F11"), Some(&ActionReference::Removed));
    assert_eq!(
        map.get("Ctrl+T"),
        Some(&ActionReference::Simple("new-tab".to_string()))
    );
}

#[test]
fn action_reference_removed_serializes_to_null() {
    let mut map: HashMap<String, ActionReference> = HashMap::new();
    map.insert("F11".to_string(), ActionReference::Removed);
    map.insert(
        "Ctrl+T".to_string(),
        ActionReference::Simple("new-tab".to_string()),
    );
    let yaml = serde_yaml::to_string(&map).expect("must serialize");
    // The YAML should contain a null (~ or null) for F11.
    assert!(
        yaml.contains("~") || yaml.contains("null"),
        "Removed must serialize as null, got: {yaml}"
    );
}
