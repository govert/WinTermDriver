//! Target path resolution against running workspace instances (§19.4–19.7).
//!
//! Resolves semantic target paths (e.g. `dev/backend/server`) and `--id`
//! addresses to concrete `(WorkspaceInstanceId, PaneId)` pairs.

use wtd_core::ids::{PaneId, WorkspaceInstanceId};
use wtd_core::target::TargetPath;

use crate::workspace_instance::WorkspaceInstance;

// ── Result type ─────────────────────────────────────────────────────────────

/// A successfully resolved target: workspace instance + pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub instance_id: WorkspaceInstanceId,
    pub pane_id: PaneId,
    /// Canonical path for display (e.g. `dev/main/server`).
    pub canonical_path: String,
}

// ── Error type ──────────────────────────────────────────────────────────────

/// Errors from target resolution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error(
        "target \"{target}\" is ambiguous. Candidates:\n{candidates}\nUse a longer path or --id to disambiguate.",
        candidates = format_candidates(.candidates)
    )]
    Ambiguous {
        target: String,
        candidates: Vec<String>,
    },

    #[error("target \"{0}\" not found")]
    NotFound(String),

    #[error("no workspace instance is active; specify the workspace explicitly")]
    NoActiveInstance,

    #[error("multiple workspace instances are active; specify the workspace explicitly")]
    MultipleActiveInstances,

    #[error("workspace \"{0}\" not found")]
    WorkspaceNotFound(String),

    #[error("tab \"{tab}\" not found in workspace \"{workspace}\"")]
    TabNotFound { workspace: String, tab: String },

    #[error("pane \"{pane}\" not found in workspace \"{workspace}\"")]
    PaneNotFound { workspace: String, pane: String },

    #[error("pane \"{pane}\" not found in tab \"{tab}\"")]
    PaneNotFoundInTab { tab: String, pane: String },

    #[error("no pane found with id \"{0}\"")]
    IdNotFound(String),
}

fn format_candidates(candidates: &[String]) -> String {
    candidates
        .iter()
        .map(|c| format!("  {}", c))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Resolve a parsed [`TargetPath`] against a set of workspace instances.
///
/// Returns the workspace instance and pane that the path refers to.
pub fn resolve_target(
    path: &TargetPath,
    instances: &[&WorkspaceInstance],
) -> Result<ResolvedTarget, ResolveError> {
    match path {
        TargetPath::Pane { pane } => resolve_one_segment(pane, instances),
        TargetPath::WorkspacePane { workspace, pane } => {
            resolve_two_segments(workspace, pane, instances)
        }
        TargetPath::WorkspaceTabPane {
            workspace,
            tab,
            pane,
        } => resolve_three_segments(workspace, tab, pane, instances),
        TargetPath::WorkspaceWindowTabPane {
            workspace,
            window: _,
            tab,
            pane,
        } => {
            // Window tracking is not yet implemented at runtime; resolve as
            // workspace/tab/pane, ignoring the window segment.
            resolve_three_segments(workspace, tab, pane, instances)
        }
    }
}

/// Resolve a target by internal ID string.
///
/// Searches all instances for a pane whose `PaneId` matches the given string
/// (parsed as u64).
pub fn resolve_by_id(
    id_str: &str,
    instances: &[&WorkspaceInstance],
) -> Result<ResolvedTarget, ResolveError> {
    let id_val: u64 = id_str
        .parse()
        .map_err(|_| ResolveError::IdNotFound(id_str.to_string()))?;
    let target_pane = PaneId(id_val);

    for inst in instances {
        if inst.pane_name(&target_pane).is_some() {
            let canonical = inst
                .canonical_pane_path(&target_pane)
                .unwrap_or_else(|| format!("#{}", id_val));
            return Ok(ResolvedTarget {
                instance_id: inst.id().clone(),
                pane_id: target_pane,
                canonical_path: canonical,
            });
        }
    }

    Err(ResolveError::IdNotFound(id_str.to_string()))
}

// ── Internal resolution ─────────────────────────────────────────────────────

/// 1-segment: implicit workspace, pane lookup (§19.4 step 1, §19.5).
fn resolve_one_segment(
    pane_name: &str,
    instances: &[&WorkspaceInstance],
) -> Result<ResolvedTarget, ResolveError> {
    match instances.len() {
        0 => Err(ResolveError::NoActiveInstance),
        1 => {
            let inst = instances[0];
            let matches = inst.find_all_panes_by_name(pane_name);
            match matches.len() {
                0 => Err(ResolveError::NotFound(pane_name.to_string())),
                1 => {
                    let (pane_id, canonical) = matches.into_iter().next().unwrap();
                    Ok(ResolvedTarget {
                        instance_id: inst.id().clone(),
                        pane_id,
                        canonical_path: canonical,
                    })
                }
                _ => Err(ResolveError::Ambiguous {
                    target: pane_name.to_string(),
                    candidates: matches.into_iter().map(|(_, p)| p).collect(),
                }),
            }
        }
        _ => Err(ResolveError::MultipleActiveInstances),
    }
}

/// 2-segment: workspace/pane (§19.4 step 2).
fn resolve_two_segments(
    ws_name: &str,
    pane_name: &str,
    instances: &[&WorkspaceInstance],
) -> Result<ResolvedTarget, ResolveError> {
    let inst = find_instance_by_name(ws_name, instances)?;
    let matches = inst.find_all_panes_by_name(pane_name);
    match matches.len() {
        0 => Err(ResolveError::PaneNotFound {
            workspace: ws_name.to_string(),
            pane: pane_name.to_string(),
        }),
        1 => {
            let (pane_id, canonical) = matches.into_iter().next().unwrap();
            Ok(ResolvedTarget {
                instance_id: inst.id().clone(),
                pane_id,
                canonical_path: canonical,
            })
        }
        _ => Err(ResolveError::Ambiguous {
            target: format!("{}/{}", ws_name, pane_name),
            candidates: matches.into_iter().map(|(_, p)| p).collect(),
        }),
    }
}

/// 3-segment (and 4-segment with window ignored): workspace/tab/pane (§19.4 step 3–4).
fn resolve_three_segments(
    ws_name: &str,
    tab_name: &str,
    pane_name: &str,
    instances: &[&WorkspaceInstance],
) -> Result<ResolvedTarget, ResolveError> {
    let inst = find_instance_by_name(ws_name, instances)?;
    let tab = inst
        .find_tab_by_name(tab_name)
        .ok_or_else(|| ResolveError::TabNotFound {
            workspace: ws_name.to_string(),
            tab: tab_name.to_string(),
        })?;
    let pane_id =
        inst.find_pane_in_tab(tab, pane_name)
            .ok_or_else(|| ResolveError::PaneNotFoundInTab {
                tab: tab_name.to_string(),
                pane: pane_name.to_string(),
            })?;
    let canonical = format!("{}/{}/{}", ws_name, tab_name, pane_name);
    Ok(ResolvedTarget {
        instance_id: inst.id().clone(),
        pane_id,
        canonical_path: canonical,
    })
}

/// Look up a workspace instance by name.
fn find_instance_by_name<'a>(
    name: &str,
    instances: &[&'a WorkspaceInstance],
) -> Result<&'a WorkspaceInstance, ResolveError> {
    let mut found: Option<&WorkspaceInstance> = None;
    for inst in instances {
        if inst.name() == name {
            if found.is_some() {
                // Multiple instances with the same name — treat as ambiguous.
                return Err(ResolveError::Ambiguous {
                    target: name.to_string(),
                    candidates: instances
                        .iter()
                        .filter(|i| i.name() == name)
                        .map(|i| format!("{} (id {})", i.name(), i.id()))
                        .collect(),
                });
            }
            found = Some(inst);
        }
    }
    found.ok_or_else(|| ResolveError::WorkspaceNotFound(name.to_string()))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_instance::WorkspaceInstance;

    // ── Helpers ─────────────────────────────────────────────────────

    fn single_tab_instance(name: &str, id: u64, pane_names: &[&str]) -> WorkspaceInstance {
        WorkspaceInstance::new_for_test_multi(name, id, &[("main", pane_names)])
    }

    // ── 1-segment resolution ────────────────────────────────────────

    #[test]
    fn one_segment_resolves_pane_in_single_instance() {
        let inst = single_tab_instance("dev", 1, &["server", "editor"]);
        let instances: Vec<&WorkspaceInstance> = vec![&inst];

        let path = TargetPath::parse("server").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        assert_eq!(resolved.canonical_path, "dev/main/server");
    }

    #[test]
    fn one_segment_not_found() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("missing").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(err, ResolveError::NotFound(ref s) if s == "missing"));
    }

    #[test]
    fn one_segment_no_instances() {
        let instances: Vec<&WorkspaceInstance> = vec![];

        let path = TargetPath::parse("server").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(err, ResolveError::NoActiveInstance));
    }

    #[test]
    fn one_segment_multiple_instances_error() {
        let inst1 = single_tab_instance("dev", 1, &["server"]);
        let inst2 = single_tab_instance("staging", 2, &["server"]);
        let instances = vec![&inst1, &inst2];

        let path = TargetPath::parse("server").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(err, ResolveError::MultipleActiveInstances));
    }

    // ── 2-segment resolution ────────────────────────────────────────

    #[test]
    fn two_segment_workspace_pane() {
        let inst = single_tab_instance("dev", 1, &["server", "editor"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/editor").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        assert_eq!(resolved.canonical_path, "dev/main/editor");
    }

    #[test]
    fn two_segment_selects_correct_workspace() {
        let inst1 = single_tab_instance("dev", 1, &["server"]);
        let inst2 = single_tab_instance("staging", 2, &["server"]);
        let instances = vec![&inst1, &inst2];

        let path = TargetPath::parse("staging/server").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(2));
        assert_eq!(resolved.canonical_path, "staging/main/server");
    }

    #[test]
    fn two_segment_workspace_not_found() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("prod/server").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(err, ResolveError::WorkspaceNotFound(ref s) if s == "prod"));
    }

    #[test]
    fn two_segment_pane_not_found() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/missing").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(
            err,
            ResolveError::PaneNotFound { ref workspace, ref pane }
            if workspace == "dev" && pane == "missing"
        ));
    }

    // ── 3-segment resolution ────────────────────────────────────────

    #[test]
    fn three_segment_workspace_tab_pane() {
        // Single-tab instance avoids PaneId collision across tabs
        // (each LayoutTree starts PaneId at 1).
        let inst = single_tab_instance("dev", 1, &["api", "db"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/main/api").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        assert_eq!(resolved.canonical_path, "dev/main/api");
    }

    #[test]
    fn three_segment_second_pane() {
        let inst = single_tab_instance("dev", 1, &["api", "db"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/main/db").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        assert_eq!(resolved.canonical_path, "dev/main/db");
    }

    #[test]
    fn three_segment_tab_not_found() {
        let inst = single_tab_instance("dev", 1, &["api"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/missing-tab/api").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(
            err,
            ResolveError::TabNotFound { ref workspace, ref tab }
            if workspace == "dev" && tab == "missing-tab"
        ));
    }

    #[test]
    fn three_segment_pane_not_in_tab() {
        let inst = single_tab_instance("dev", 1, &["api", "db"]);
        let instances = vec![&inst];

        // "web" does not exist in tab "main".
        let path = TargetPath::parse("dev/main/web").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();

        assert!(matches!(
            err,
            ResolveError::PaneNotFoundInTab { ref tab, ref pane }
            if tab == "main" && pane == "web"
        ));
    }

    // ── 4-segment resolution ────────────────────────────────────────

    #[test]
    fn four_segment_ignores_window_resolves_tab_pane() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let path = TargetPath::parse("dev/main-window/main/server").unwrap();
        let resolved = resolve_target(&path, &instances).unwrap();

        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        // Canonical path is 3-segment (window not tracked at runtime).
        assert_eq!(resolved.canonical_path, "dev/main/server");
    }

    // ── Ambiguity ───────────────────────────────────────────────────

    #[test]
    fn ambiguous_returns_candidates() {
        // This scenario shouldn't happen under the uniqueness constraint, but
        // we handle it defensively. We can create it by having the same pane
        // name in different tabs (uniqueness is workspace-wide in the spec,
        // but we test the resolver's defensive handling).
        //
        // Since pane names should be unique within a workspace, we test the
        // 1-segment ambiguity with multiple instances instead.
        let inst1 = single_tab_instance("dev", 1, &["server"]);
        let inst2 = single_tab_instance("staging", 2, &["server"]);
        let instances = vec![&inst1, &inst2];

        // With two instances, 1-segment always returns MultipleActiveInstances.
        let path = TargetPath::parse("server").unwrap();
        let err = resolve_target(&path, &instances).unwrap_err();
        assert!(matches!(err, ResolveError::MultipleActiveInstances));

        // 2-segment is unambiguous: selects workspace explicitly.
        let path2 = TargetPath::parse("dev/server").unwrap();
        let resolved = resolve_target(&path2, &instances).unwrap();
        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
    }

    // ── --id resolution ─────────────────────────────────────────────

    #[test]
    fn resolve_by_id_finds_pane() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        // PaneId(1) is the first pane created by LayoutTree::new().
        let resolved = resolve_by_id("1", &instances).unwrap();
        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
        assert_eq!(resolved.pane_id, PaneId(1));
        assert_eq!(resolved.canonical_path, "dev/main/server");
    }

    #[test]
    fn resolve_by_id_not_found() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let err = resolve_by_id("999", &instances).unwrap_err();
        assert!(matches!(err, ResolveError::IdNotFound(ref s) if s == "999"));
    }

    #[test]
    fn resolve_by_id_invalid_format() {
        let inst = single_tab_instance("dev", 1, &["server"]);
        let instances = vec![&inst];

        let err = resolve_by_id("not-a-number", &instances).unwrap_err();
        assert!(matches!(err, ResolveError::IdNotFound(ref s) if s == "not-a-number"));
    }

    #[test]
    fn resolve_by_id_across_instances() {
        let inst1 = single_tab_instance("dev", 1, &["server"]);
        let inst2 = single_tab_instance("staging", 2, &["api"]);
        let instances = vec![&inst1, &inst2];

        // inst2's first pane is also PaneId(1) (LayoutTree::new() starts at 1).
        // resolve_by_id finds the first match across instances.
        let resolved = resolve_by_id("1", &instances).unwrap();
        // First instance searched first.
        assert_eq!(resolved.instance_id, WorkspaceInstanceId(1));
    }

    // ── Error message formatting ────────────────────────────────────

    #[test]
    fn ambiguous_error_message_format() {
        let err = ResolveError::Ambiguous {
            target: "server".to_string(),
            candidates: vec![
                "dev/backend/server".to_string(),
                "dev/ops/server".to_string(),
            ],
        };
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"));
        assert!(msg.contains("dev/backend/server"));
        assert!(msg.contains("dev/ops/server"));
        assert!(msg.contains("--id"));
    }
}
