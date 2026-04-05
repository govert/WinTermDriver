//! Loading and validation of workspace definition files (§10).

use std::collections::HashSet;
use thiserror::Error;

use crate::workspace::{PaneNode, TabDefinition, WindowDefinition, WorkspaceDefinition};

// ── Error types ───────────────────────────────────────────────────────────────

/// An error encountered while loading a workspace definition file.
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("{file_path}: parse error: {source}")]
    Parse {
        file_path: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("{file_path}: validation failed\n{}", format_errors(errors))]
    Validation {
        file_path: String,
        errors: Vec<ValidationError>,
    },
}

fn format_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| format!("  {}: {}", e.path, e.message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A single validation error, with a field path and human-readable message.
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// Dot-notation path to the offending field (e.g. `windows[0].tabs[1].focus`).
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

// ── Public loader ─────────────────────────────────────────────────────────────

/// Parse and validate a workspace definition from YAML/JSON text.
///
/// `file_path` is used only for error messages.
pub fn load_workspace_definition(
    file_path: &str,
    content: &str,
) -> Result<WorkspaceDefinition, LoadError> {
    let def: WorkspaceDefinition = serde_yaml::from_str(content).map_err(|e| LoadError::Parse {
        file_path: file_path.to_string(),
        source: e,
    })?;

    let errors = validate(&def);
    if !errors.is_empty() {
        return Err(LoadError::Validation {
            file_path: file_path.to_string(),
            errors,
        });
    }

    Ok(def)
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Built-in profile type names that are always valid profile references.
const BUILTIN_PROFILES: &[&str] = &["powershell", "cmd", "wsl", "ssh", "custom"];

fn validate(def: &WorkspaceDefinition) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut v = Validator::new(&mut errors);
    v.root(def);
    errors
}

fn validate_terminal_size(
    size: &crate::workspace::TerminalSizeDefinition,
    path: &str,
    errors: &mut Vec<ValidationError>,
) {
    if size.cols == 0 {
        errors.push(ValidationError {
            path: format!("{path}.cols"),
            message: "must be greater than 0".to_string(),
        });
    }

    if size.rows == 0 {
        errors.push(ValidationError {
            path: format!("{path}.rows"),
            message: "must be greater than 0".to_string(),
        });
    }
}

struct Validator<'a> {
    errors: &'a mut Vec<ValidationError>,
    /// Global pane name uniqueness (Rule 5).
    pane_names: HashSet<String>,
}

impl<'a> Validator<'a> {
    fn new(errors: &'a mut Vec<ValidationError>) -> Self {
        Self {
            errors,
            pane_names: HashSet::new(),
        }
    }

    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError {
            path: path.into(),
            message: message.into(),
        });
    }

    fn root(&mut self, def: &WorkspaceDefinition) {
        // Rule 1: version == 1
        if def.version != 1 {
            self.push("version", format!("expected 1, found {}", def.version));
        }

        // Rule 2: name matches [a-zA-Z0-9_-]{1,64}
        validate_ident(&def.name, "name", self.errors);

        // Rule 3: windows and tabs are mutually exclusive
        if def.windows.is_some() && def.tabs.is_some() {
            self.push(
                "windows/tabs",
                "'windows' and 'tabs' are mutually exclusive — use one or the other",
            );
        }

        let profile_names: HashSet<String> = def
            .profiles
            .as_ref()
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();

        if let Some(defaults) = &def.defaults {
            if let Some(size) = &defaults.terminal_size {
                validate_terminal_size(size, "defaults.terminalSize", self.errors);
            }
        }

        if let Some(windows) = &def.windows {
            for (i, w) in windows.iter().enumerate() {
                self.window(w, &format!("windows[{i}]"), &profile_names);
            }
        }

        if let Some(tabs) = &def.tabs {
            let mut tab_names: HashSet<String> = HashSet::new();
            for (i, t) in tabs.iter().enumerate() {
                let path = format!("tabs[{i}]");
                // Rule 4: unique tab names within parent
                if !tab_names.insert(t.name.clone()) {
                    self.push(
                        format!("{path}.name"),
                        format!("duplicate tab name '{}'", t.name),
                    );
                }
                validate_ident(&t.name, &format!("{path}.name"), self.errors);
                self.tab(t, &path, &profile_names);
            }
        }
    }

    fn window(&mut self, w: &WindowDefinition, path: &str, profiles: &HashSet<String>) {
        if let Some(name) = &w.name {
            validate_ident(name, &format!("{path}.name"), self.errors);
        }

        // Rule 4: unique tab names within this window
        let mut tab_names: HashSet<String> = HashSet::new();
        for (i, t) in w.tabs.iter().enumerate() {
            let tpath = format!("{path}.tabs[{i}]");
            if !tab_names.insert(t.name.clone()) {
                self.push(
                    format!("{tpath}.name"),
                    format!("duplicate tab name '{}'", t.name),
                );
            }
            validate_ident(&t.name, &format!("{tpath}.name"), self.errors);
            self.tab(t, &tpath, profiles);
        }
    }

    fn tab(&mut self, t: &TabDefinition, path: &str, profiles: &HashSet<String>) {
        let mut tab_panes: HashSet<String> = HashSet::new();
        self.pane_node(
            &t.layout,
            &format!("{path}.layout"),
            profiles,
            &mut tab_panes,
        );

        // Rule 7: focus references an existing pane in this tab
        if let Some(focus) = &t.focus {
            if !tab_panes.contains(focus.as_str()) {
                self.push(
                    format!("{path}.focus"),
                    format!("references unknown pane '{focus}'"),
                );
            }
        }
    }

    fn pane_node(
        &mut self,
        node: &PaneNode,
        path: &str,
        profiles: &HashSet<String>,
        tab_panes: &mut HashSet<String>,
    ) {
        match node {
            PaneNode::Pane(leaf) => {
                validate_ident(&leaf.name, &format!("{path}.name"), self.errors);

                // Rule 5: pane names unique across the entire workspace
                if !self.pane_names.insert(leaf.name.clone()) {
                    self.push(
                        format!("{path}.name"),
                        format!(
                            "duplicate pane name '{}' — pane names must be unique across the workspace",
                            leaf.name
                        ),
                    );
                }
                tab_panes.insert(leaf.name.clone());

                // Rule 6: profile reference is defined or built-in
                if let Some(session) = &leaf.session {
                    if let Some(ref_name) = &session.profile {
                        validate_profile_ref(
                            ref_name,
                            &format!("{path}.session.profile"),
                            profiles,
                            self.errors,
                        );
                    }
                    if let Some(size) = &session.terminal_size {
                        validate_terminal_size(
                            size,
                            &format!("{path}.session.terminalSize"),
                            self.errors,
                        );
                    }
                }
            }

            PaneNode::Split(split) => {
                // Rule 8: exactly 2 children
                if split.children.len() != 2 {
                    self.push(
                        format!("{path}.children"),
                        format!(
                            "split must have exactly 2 children, found {}",
                            split.children.len()
                        ),
                    );
                }

                // Rule 9: ratio in [0.1, 0.9]
                if let Some(ratio) = split.ratio {
                    if !(0.1..=0.9).contains(&ratio) {
                        self.push(
                            format!("{path}.ratio"),
                            format!("ratio {ratio} is out of range 0.1–0.9"),
                        );
                    }
                }

                for (i, child) in split.children.iter().enumerate() {
                    self.pane_node(child, &format!("{path}.children[{i}]"), profiles, tab_panes);
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn validate_ident(name: &str, path: &str, errors: &mut Vec<ValidationError>) {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!("'{name}' must match [a-zA-Z0-9_-]{{1,64}}"),
        });
    }
}

fn validate_profile_ref(
    name: &str,
    path: &str,
    defined: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if !defined.contains(name) && !BUILTIN_PROFILES.contains(&name) {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!("profile '{name}' is not defined in profiles or a built-in type"),
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(yaml: &str) -> WorkspaceDefinition {
        load_workspace_definition("test.yaml", yaml)
            .unwrap_or_else(|e| panic!("expected Ok, got: {e}"))
    }

    fn err_messages(yaml: &str) -> Vec<String> {
        match load_workspace_definition("test.yaml", yaml) {
            Ok(_) => panic!("expected Err"),
            Err(LoadError::Validation { errors, .. }) => {
                errors.into_iter().map(|e| e.message).collect()
            }
            Err(e) => panic!("expected validation error, got: {e}"),
        }
    }

    // ── §10.5 Minimal example ─────────────────────────────────────────────────

    #[test]
    fn minimal_example_parses() {
        let yaml = r#"
version: 1
name: quick
tabs:
  - name: main
    layout:
      type: pane
      name: shell
"#;
        let def = ok(yaml);
        assert_eq!(def.version, 1);
        assert_eq!(def.name, "quick");
        assert!(def.windows.is_none());
        let tabs = def.tabs.as_ref().unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "main");
        match &tabs[0].layout {
            PaneNode::Pane(leaf) => assert_eq!(leaf.name, "shell"),
            _ => panic!("expected pane"),
        }
    }

    // ── §10.4 Complete example ────────────────────────────────────────────────

    #[test]
    fn complete_example_parses() {
        let yaml = r#"
version: 1
name: dev
description: Development cockpit for main product

defaults:
  profile: pwsh
  restartPolicy: on-failure
  scrollbackLines: 20000

profiles:
  pwsh:
    type: powershell
    executable: pwsh.exe
    title: "{name} — PowerShell"
  ubuntu:
    type: wsl
    distribution: Ubuntu-24.04
  prodssh:
    type: ssh
    host: prod-box
    user: deploy
    port: 22
    useAgent: true

bindings:
  prefix: "Ctrl+B"
  prefixTimeout: 2000
  chords:
    "%": split-right
    "o": focus-next-pane
    "c": new-tab

windows:
  - name: main
    tabs:
      - name: backend
        layout:
          type: split
          orientation: horizontal
          ratio: 0.5
          children:
            - type: pane
              name: editor
              session:
                profile: pwsh
                cwd: "C:\\src\\app"
            - type: split
              orientation: vertical
              ratio: 0.6
              children:
                - type: pane
                  name: server
                  session:
                    profile: pwsh
                    startupCommand: dotnet watch run
                - type: pane
                  name: tests
                  session:
                    profile: pwsh
        focus: editor
      - name: ops
        layout:
          type: split
          orientation: vertical
          children:
            - type: pane
              name: prod-shell
              session:
                profile: prodssh
            - type: pane
              name: prod-logs
              session:
                profile: prodssh
                startupCommand: "journalctl -f -u myservice"
"#;
        let def = ok(yaml);
        assert_eq!(def.name, "dev");
        let windows = def.windows.as_ref().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].tabs.len(), 2);
        assert_eq!(windows[0].tabs[0].focus.as_deref(), Some("editor"));
    }

    // ── Round-trip ────────────────────────────────────────────────────────────

    #[test]
    fn roundtrip_minimal() {
        let yaml = r#"
version: 1
name: quick
tabs:
  - name: main
    layout:
      type: pane
      name: shell
"#;
        let first = ok(yaml);
        let serialized = serde_yaml::to_string(&first).unwrap();
        let second = ok(&serialized);
        assert_eq!(first, second);
    }

    #[test]
    fn roundtrip_complete() {
        let yaml = r#"
version: 1
name: dev
profiles:
  pwsh:
    type: powershell
windows:
  - name: main
    tabs:
      - name: code
        layout:
          type: split
          orientation: horizontal
          ratio: 0.5
          children:
            - type: pane
              name: left
              session:
                profile: pwsh
            - type: pane
              name: right
        focus: left
"#;
        let first = ok(yaml);
        let serialized = serde_yaml::to_string(&first).unwrap();
        let second = ok(&serialized);
        assert_eq!(first, second);
    }

    // ── Rule 1: version ───────────────────────────────────────────────────────

    #[test]
    fn wrong_version_rejected() {
        let yaml = "version: 2\nname: x\ntabs:\n  - name: t\n    layout:\n      type: pane\n      name: p\n";
        let msgs = err_messages(yaml);
        assert!(msgs
            .iter()
            .any(|m| m.contains("version") || m.contains("expected 1")));
    }

    // ── Rule 2: name format ───────────────────────────────────────────────────

    #[test]
    fn invalid_name_chars_rejected() {
        let yaml = "version: 1\nname: \"my workspace\"\ntabs:\n  - name: t\n    layout:\n      type: pane\n      name: p\n";
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("must match")));
    }

    #[test]
    fn name_too_long_rejected() {
        let long = "a".repeat(65);
        let yaml = format!("version: 1\nname: {long}\ntabs:\n  - name: t\n    layout:\n      type: pane\n      name: p\n");
        let msgs = err_messages(&yaml);
        assert!(msgs.iter().any(|m| m.contains("must match")));
    }

    // ── Rule 3: windows/tabs mutual exclusivity ───────────────────────────────

    #[test]
    fn windows_and_tabs_both_rejected() {
        let yaml = r#"
version: 1
name: x
windows:
  - tabs:
      - name: t
        layout:
          type: pane
          name: p
tabs:
  - name: t2
    layout:
      type: pane
      name: p2
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("mutually exclusive")));
    }

    // ── Rule 4: duplicate tab names ───────────────────────────────────────────

    #[test]
    fn duplicate_tab_name_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: dup
    layout:
      type: pane
      name: p1
  - name: dup
    layout:
      type: pane
      name: p2
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("duplicate tab name")));
    }

    // ── Rule 5: duplicate pane names ─────────────────────────────────────────

    #[test]
    fn duplicate_pane_name_across_tabs_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t1
    layout:
      type: pane
      name: shell
  - name: t2
    layout:
      type: pane
      name: shell
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("duplicate pane name")));
    }

    // ── Rule 6: profile references ────────────────────────────────────────────

    #[test]
    fn undefined_profile_ref_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: pane
      name: p
      session:
        profile: nonexistent
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("not defined")));
    }

    #[test]
    fn builtin_profile_ref_accepted() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: pane
      name: p
      session:
        profile: powershell
"#;
        ok(yaml); // must not panic
    }

    #[test]
    fn defined_profile_ref_accepted() {
        let yaml = r#"
version: 1
name: x
profiles:
  mypwsh:
    type: powershell
tabs:
  - name: t
    layout:
      type: pane
      name: p
      session:
        profile: mypwsh
"#;
        ok(yaml);
    }

    // ── Rule 7: focus reference ───────────────────────────────────────────────

    #[test]
    fn invalid_focus_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: pane
      name: p
    focus: ghost
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("unknown pane")));
    }

    // ── Rule 8: split child count ─────────────────────────────────────────────

    #[test]
    fn split_one_child_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: split
      orientation: horizontal
      children:
        - type: pane
          name: only
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("exactly 2 children")));
    }

    #[test]
    fn split_three_children_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: split
      orientation: horizontal
      children:
        - type: pane
          name: a
        - type: pane
          name: b
        - type: pane
          name: c
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("exactly 2 children")));
    }

    // ── Rule 9: ratio range ───────────────────────────────────────────────────

    #[test]
    fn ratio_too_low_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: split
      orientation: horizontal
      ratio: 0.05
      children:
        - type: pane
          name: a
        - type: pane
          name: b
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("out of range")));
    }

    #[test]
    fn ratio_too_high_rejected() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: split
      orientation: horizontal
      ratio: 0.95
      children:
        - type: pane
          name: a
        - type: pane
          name: b
"#;
        let msgs = err_messages(yaml);
        assert!(msgs.iter().any(|m| m.contains("out of range")));
    }

    #[test]
    fn ratio_boundary_accepted() {
        let yaml = r#"
version: 1
name: x
tabs:
  - name: t
    layout:
      type: split
      orientation: horizontal
      ratio: 0.1
      children:
        - type: pane
          name: a
        - type: pane
          name: b
"#;
        ok(yaml);
    }

    // ── Multiple errors reported together ─────────────────────────────────────

    #[test]
    fn multiple_errors_collected() {
        let yaml = r#"
version: 99
name: "bad name!"
tabs:
  - name: t
    layout:
      type: pane
      name: p
"#;
        match load_workspace_definition("test.yaml", yaml) {
            Err(LoadError::Validation { errors, .. }) => {
                assert!(
                    errors.len() >= 2,
                    "expected at least 2 errors, got {}",
                    errors.len()
                );
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
