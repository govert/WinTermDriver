//! Workspace definition file discovery (§12).
//!
//! Searches for workspace definition files in a well-defined order and lists
//! available workspaces from local (`.wtd/`) and user directories.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("workspace definition not found: {name}")]
    NotFound { name: String },

    #[error("explicit file not found: {path}")]
    ExplicitFileNotFound { path: PathBuf },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Source label ─────────────────────────────────────────────────────

/// Where a workspace definition was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSource {
    /// Explicit `--file <path>`.
    Explicit,
    /// `.wtd/` directory relative to CWD.
    Local,
    /// `%APPDATA%\WinTermDriver\workspaces\` user directory.
    User,
}

impl std::fmt::Display for WorkspaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceSource::Explicit => write!(f, "explicit"),
            WorkspaceSource::Local => write!(f, "local"),
            WorkspaceSource::User => write!(f, "user"),
        }
    }
}

// ── Found workspace entry ───────────────────────────────────────────

/// A discovered workspace definition file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorkspace {
    /// Workspace name (stem of the file without extension).
    pub name: String,
    /// Absolute path to the definition file.
    pub path: PathBuf,
    /// Where the file was found.
    pub source: WorkspaceSource,
}

// ── Supported extensions ────────────────────────────────────────────

const EXTENSIONS: &[&str] = &["yaml", "yml", "json"];

// ── Default user workspaces directory ───────────────────────────────

/// Returns the user workspace directory, respecting `WTD_DATA_DIR` override.
///
/// Default: `%APPDATA%\WinTermDriver\workspaces`
pub fn user_workspaces_dir() -> PathBuf {
    data_dir().join("workspaces")
}

/// Returns the WinTermDriver data directory.
///
/// Uses `WTD_DATA_DIR` env override, else `%APPDATA%\WinTermDriver`.
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WTD_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        format!(r"{}\AppData\Roaming", home)
    });
    PathBuf::from(appdata).join("WinTermDriver")
}

// ── §12.3  Directory creation ───────────────────────────────────────

/// Ensures the given directory exists, creating it on first use (§12.3).
pub fn ensure_dir(dir: &Path) -> Result<(), DiscoveryError> {
    fs::create_dir_all(dir)?;
    Ok(())
}

/// Ensures the default user workspaces directory exists (§12.3).
pub fn ensure_user_workspaces_dir() -> Result<PathBuf, DiscoveryError> {
    let dir = user_workspaces_dir();
    ensure_dir(&dir)?;
    Ok(dir)
}

// ── §12.1  Search order ─────────────────────────────────────────────

/// Find a workspace definition file by name (§12.1), using the default user
/// workspaces directory.
///
/// See [`find_workspace_in`] for the parameterized version.
pub fn find_workspace(
    name: &str,
    explicit_file: Option<&Path>,
    cwd: &Path,
) -> Result<DiscoveredWorkspace, DiscoveryError> {
    find_workspace_in(name, explicit_file, cwd, &user_workspaces_dir())
}

/// Find a workspace definition file by name (§12.1).
///
/// Search order:
/// 1. `explicit_file` — if `Some`, use that path exactly.
/// 2. `cwd/.wtd/<name>.{yaml,yml,json}` — local project definitions.
/// 3. `user_dir/<name>.{yaml,yml,json}` — user workspace directory.
pub fn find_workspace_in(
    name: &str,
    explicit_file: Option<&Path>,
    cwd: &Path,
    user_dir: &Path,
) -> Result<DiscoveredWorkspace, DiscoveryError> {
    // 1. Explicit path
    if let Some(path) = explicit_file {
        if path.is_file() {
            return Ok(DiscoveredWorkspace {
                name: name.to_string(),
                path: path.to_path_buf(),
                source: WorkspaceSource::Explicit,
            });
        }
        return Err(DiscoveryError::ExplicitFileNotFound {
            path: path.to_path_buf(),
        });
    }

    // 2. CWD .wtd/ directory
    let wtd_dir = cwd.join(".wtd");
    if let Some(found) = probe_name_in_dir(&wtd_dir, name) {
        return Ok(DiscoveredWorkspace {
            name: name.to_string(),
            path: found,
            source: WorkspaceSource::Local,
        });
    }

    // 3. User workspace directory
    if let Some(found) = probe_name_in_dir(user_dir, name) {
        return Ok(DiscoveredWorkspace {
            name: name.to_string(),
            path: found,
            source: WorkspaceSource::User,
        });
    }

    Err(DiscoveryError::NotFound {
        name: name.to_string(),
    })
}

/// Probe for `<name>.{yaml,yml,json}` in the given directory.
/// Returns the first match found (extension priority order).
fn probe_name_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    for ext in EXTENSIONS {
        let candidate = dir.join(format!("{}.{}", name, ext));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ── §12.4  Workspace listing ────────────────────────────────────────

/// List all workspace definitions from the local `.wtd/` directory and the
/// default user workspace directory (§12.4).
///
/// See [`list_workspaces_in`] for the parameterized version.
pub fn list_workspaces(cwd: &Path) -> Vec<DiscoveredWorkspace> {
    list_workspaces_in(cwd, &user_workspaces_dir())
}

/// List all workspace definitions from `.wtd/` in `cwd` and the given
/// `user_dir` (§12.4).
///
/// If a workspace name appears in both locations, both entries are returned
/// with their respective source labels.
pub fn list_workspaces_in(cwd: &Path, user_dir: &Path) -> Vec<DiscoveredWorkspace> {
    let mut results = Vec::new();

    // Scan CWD .wtd/
    let wtd_dir = cwd.join(".wtd");
    scan_dir_into(&wtd_dir, WorkspaceSource::Local, &mut results);

    // Scan user workspaces directory
    scan_dir_into(user_dir, WorkspaceSource::User, &mut results);

    results
}

/// Scan a directory for workspace definition files and append to `out`.
fn scan_dir_into(dir: &Path, source: WorkspaceSource, out: &mut Vec<DiscoveredWorkspace>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return, // directory doesn't exist or unreadable — skip silently
    };

    let mut found: Vec<DiscoveredWorkspace> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let ext = path.extension()?.to_str()?;
            if !EXTENSIONS.contains(&ext) {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_string();
            Some(DiscoveredWorkspace {
                name,
                path,
                source: source.clone(),
            })
        })
        .collect();

    // Sort by name for deterministic output
    found.sort_by(|a, b| a.name.cmp(&b.name));
    out.append(&mut found);
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a unique temp dir for this test.
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtd-disc-{}-{}", label, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── §12.1: Explicit --file path ────────────────────────────

    #[test]
    fn explicit_file_found() {
        let tmp = temp_dir("explicit-found");
        let file = tmp.join("my-ws.yaml");
        fs::write(&file, "version: 1\nname: my-ws\ntabs: []").unwrap();

        let user_dir = tmp.join("user");
        let result = find_workspace_in("my-ws", Some(&file), &tmp, &user_dir).unwrap();
        assert_eq!(result.source, WorkspaceSource::Explicit);
        assert_eq!(result.path, file);
        assert_eq!(result.name, "my-ws");

        cleanup(&tmp);
    }

    #[test]
    fn explicit_file_not_found() {
        let tmp = temp_dir("explicit-missing");
        let file = tmp.join("missing.yaml");

        let user_dir = tmp.join("user");
        let err = find_workspace_in("missing", Some(&file), &tmp, &user_dir).unwrap_err();
        assert!(matches!(err, DiscoveryError::ExplicitFileNotFound { .. }));

        cleanup(&tmp);
    }

    // ── §12.1: Search order (CWD .wtd/ before user dir) ───────

    #[test]
    fn cwd_wtd_dir_takes_precedence_over_user_dir() {
        let tmp = temp_dir("search-order");

        // Set up CWD .wtd/ directory
        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();
        fs::write(wtd_dir.join("demo.yaml"), "version: 1").unwrap();

        // Set up user directory with same name
        let user_dir = tmp.join("user-ws");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("demo.yaml"), "version: 1").unwrap();

        let result = find_workspace_in("demo", None, &tmp, &user_dir).unwrap();
        assert_eq!(result.source, WorkspaceSource::Local);
        assert_eq!(result.path, wtd_dir.join("demo.yaml"));

        cleanup(&tmp);
    }

    #[test]
    fn falls_through_to_user_dir_when_not_in_cwd() {
        let tmp = temp_dir("fallthrough");

        // No .wtd/ directory in CWD
        let user_dir = tmp.join("user-ws");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("demo.yml"), "version: 1").unwrap();

        let result = find_workspace_in("demo", None, &tmp, &user_dir).unwrap();
        assert_eq!(result.source, WorkspaceSource::User);
        assert_eq!(result.path, user_dir.join("demo.yml"));

        cleanup(&tmp);
    }

    #[test]
    fn not_found_anywhere() {
        let tmp = temp_dir("not-found");
        let user_dir = tmp.join("user-ws");
        fs::create_dir_all(&user_dir).unwrap();

        let err = find_workspace_in("nope", None, &tmp, &user_dir).unwrap_err();
        assert!(matches!(err, DiscoveryError::NotFound { .. }));

        cleanup(&tmp);
    }

    // ── Extension priority ────────────────────────────────────

    #[test]
    fn yaml_preferred_over_yml_and_json() {
        let tmp = temp_dir("ext-priority");
        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();

        fs::write(wtd_dir.join("ws.json"), "{}").unwrap();
        fs::write(wtd_dir.join("ws.yml"), "").unwrap();
        fs::write(wtd_dir.join("ws.yaml"), "").unwrap();

        let user_dir = tmp.join("user-ws");
        let result = find_workspace_in("ws", None, &tmp, &user_dir).unwrap();
        assert!(result.path.to_str().unwrap().ends_with(".yaml"));

        cleanup(&tmp);
    }

    #[test]
    fn yml_found_when_no_yaml() {
        let tmp = temp_dir("ext-yml");
        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();
        fs::write(wtd_dir.join("ws.yml"), "").unwrap();

        let user_dir = tmp.join("user-ws");
        let result = find_workspace_in("ws", None, &tmp, &user_dir).unwrap();
        assert!(result.path.to_str().unwrap().ends_with(".yml"));

        cleanup(&tmp);
    }

    #[test]
    fn json_found_when_no_yaml_or_yml() {
        let tmp = temp_dir("ext-json");
        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();
        fs::write(wtd_dir.join("ws.json"), "{}").unwrap();

        let user_dir = tmp.join("user-ws");
        let result = find_workspace_in("ws", None, &tmp, &user_dir).unwrap();
        assert!(result.path.to_str().unwrap().ends_with(".json"));

        cleanup(&tmp);
    }

    // ── §12.4: Listing ────────────────────────────────────────

    #[test]
    fn list_workspaces_from_both_sources() {
        let tmp = temp_dir("list-both");

        // Local .wtd/ directory
        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();
        fs::write(wtd_dir.join("alpha.yaml"), "").unwrap();
        fs::write(wtd_dir.join("beta.yml"), "").unwrap();

        // User directory
        let user_dir = tmp.join("user-ws");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("gamma.json"), "").unwrap();
        fs::write(user_dir.join("beta.yaml"), "").unwrap(); // same name as local

        let list = list_workspaces_in(&tmp, &user_dir);

        assert_eq!(list.len(), 4);

        // Local entries first (sorted by name)
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[0].source, WorkspaceSource::Local);

        assert_eq!(list[1].name, "beta");
        assert_eq!(list[1].source, WorkspaceSource::Local);

        // User entries next (sorted by name)
        assert_eq!(list[2].name, "beta");
        assert_eq!(list[2].source, WorkspaceSource::User);

        assert_eq!(list[3].name, "gamma");
        assert_eq!(list[3].source, WorkspaceSource::User);

        cleanup(&tmp);
    }

    #[test]
    fn list_workspaces_empty_when_no_dirs() {
        let tmp = temp_dir("list-empty");
        let user_dir = tmp.join("user-ws");
        // Neither .wtd/ nor user_dir exist

        let list = list_workspaces_in(&tmp, &user_dir);
        assert!(list.is_empty());

        cleanup(&tmp);
    }

    #[test]
    fn list_ignores_non_workspace_files() {
        let tmp = temp_dir("list-ignore");

        let wtd_dir = tmp.join(".wtd");
        fs::create_dir_all(&wtd_dir).unwrap();
        fs::write(wtd_dir.join("readme.txt"), "").unwrap();
        fs::write(wtd_dir.join("notes.md"), "").unwrap();
        fs::write(wtd_dir.join("real.yaml"), "").unwrap();

        let user_dir = tmp.join("user-ws");
        let list = list_workspaces_in(&tmp, &user_dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "real");

        cleanup(&tmp);
    }

    // ── §12.3: Directory creation ─────────────────────────────

    #[test]
    fn ensure_dir_creates_on_first_use() {
        let tmp = temp_dir("ensure-dir");
        let target = tmp.join("nested").join("workspaces");
        assert!(!target.exists());

        ensure_dir(&target).unwrap();
        assert!(target.is_dir());

        cleanup(&tmp);
    }
}
