//! Profile resolution — converts workspace/global settings + a session launch
//! definition into a fully concrete [`ResolvedLaunchSpec`] (§25).

use std::collections::HashMap;

use thiserror::Error;

use crate::global_settings::GlobalSettings;
use crate::workspace::{ProfileDefinition, ProfileType, SessionLaunchDefinition, WorkspaceDefinition};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error, PartialEq)]
pub enum ResolveError {
    #[error("profile '{name}' not found in workspace profiles, global settings, or built-in types")]
    ProfileNotFound { name: String },
    #[error("custom profile '{name}' requires an 'executable' field")]
    CustomMissingExecutable { name: String },
}

// ── Output ────────────────────────────────────────────────────────────────────

/// A fully resolved set of parameters needed to launch a terminal session (§25).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLaunchSpec {
    /// Executable path or name.
    pub executable: String,
    /// Command-line arguments for the executable.
    pub args: Vec<String>,
    /// Working directory. `None` means use the process default (e.g., WSL home).
    pub cwd: Option<String>,
    /// Resolved environment (after merging all layers and removing null-valued keys).
    pub env: HashMap<String, String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Resolve a session launch definition into a concrete [`ResolvedLaunchSpec`].
///
/// Resolution follows the chain described in §25.1 and §25.3.
///
/// # Parameters
/// - `session` — per-pane session overrides (may be an empty default).
/// - `workspace_def` — parsed workspace definition; provides local profile definitions
///   and `defaults`.
/// - `global_settings` — global user settings; provides globally defined profiles and
///   the `defaultProfile` name.
/// - `host_env` — environment inherited from `wtd-host`; used as the base layer for
///   the merged environment.
/// - `find_exe` — predicate that returns `true` if the named executable is found on
///   PATH. Used for the `pwsh.exe` → `powershell.exe` PowerShell fallback.
pub fn resolve_launch_spec(
    session: &SessionLaunchDefinition,
    workspace_def: &WorkspaceDefinition,
    global_settings: &GlobalSettings,
    host_env: &HashMap<String, String>,
    find_exe: impl Fn(&str) -> bool,
) -> Result<ResolvedLaunchSpec, ResolveError> {
    // ── 1. Determine effective profile name ───────────────────────────────────
    // Priority: session → workspace defaults → global default_profile.
    let profile_name: &str = session
        .profile
        .as_deref()
        .or_else(|| workspace_def.defaults.as_ref()?.profile.as_deref())
        .unwrap_or(global_settings.default_profile.as_str());

    // ── 2. Locate the profile definition ─────────────────────────────────────
    // Chain: workspace profiles → global profiles → built-in name (§25.1).
    let (profile_type, profile_def) = lookup_profile(profile_name, workspace_def, global_settings)?;

    // ── 3. Resolve executable ─────────────────────────────────────────────────
    let executable = resolve_executable(profile_name, &profile_type, profile_def, &find_exe)?;

    // ── 4. Resolve args ───────────────────────────────────────────────────────
    // session.args overrides profile/built-in args.
    let base_args = resolve_args(&profile_type, profile_def);
    let args = session.args.clone().unwrap_or(base_args);

    // ── 5. Resolve cwd ────────────────────────────────────────────────────────
    let cwd = resolve_cwd(session, workspace_def, profile_def, &profile_type, host_env);

    // ── 6. Resolve merged env (§25.3) ─────────────────────────────────────────
    let env = resolve_env(session, workspace_def, global_settings, profile_def, &profile_type, host_env);

    Ok(ResolvedLaunchSpec { executable, args, cwd, env })
}

// ── Profile lookup (§25.1) ────────────────────────────────────────────────────

/// Returns the resolved `(ProfileType, Option<&ProfileDefinition>)`.
///
/// Built-in names resolve to their type with no definition object.
fn lookup_profile<'a>(
    name: &str,
    workspace_def: &'a WorkspaceDefinition,
    global_settings: &'a GlobalSettings,
) -> Result<(ProfileType, Option<&'a ProfileDefinition>), ResolveError> {
    // 1. Workspace profiles take priority.
    if let Some(def) = workspace_def.profiles.as_ref().and_then(|p| p.get(name)) {
        return Ok((def.profile_type.clone(), Some(def)));
    }
    // 2. Global settings profiles.
    if let Some(def) = global_settings.profiles.get(name) {
        return Ok((def.profile_type.clone(), Some(def)));
    }
    // 3. Built-in type names.
    match name {
        "powershell" => Ok((ProfileType::Powershell, None)),
        "cmd"        => Ok((ProfileType::Cmd, None)),
        "wsl"        => Ok((ProfileType::Wsl, None)),
        "ssh"        => Ok((ProfileType::Ssh, None)),
        "custom"     => Ok((ProfileType::Custom, None)),
        _ => Err(ResolveError::ProfileNotFound { name: name.to_string() }),
    }
}

// ── Executable (§25.2) ───────────────────────────────────────────────────────

fn resolve_executable(
    profile_name: &str,
    profile_type: &ProfileType,
    profile_def: Option<&ProfileDefinition>,
    find_exe: &impl Fn(&str) -> bool,
) -> Result<String, ResolveError> {
    // Explicit executable in the profile definition takes priority.
    if let Some(exe) = profile_def.and_then(|d| d.executable.as_deref()) {
        return Ok(exe.to_string());
    }
    // Built-in defaults.
    match profile_type {
        ProfileType::Powershell => {
            // Prefer PowerShell 7+ (pwsh.exe); fall back to Windows PowerShell 5.1.
            if find_exe("pwsh.exe") {
                Ok("pwsh.exe".to_string())
            } else {
                Ok("powershell.exe".to_string())
            }
        }
        ProfileType::Cmd    => Ok("cmd.exe".to_string()),
        ProfileType::Wsl    => Ok("wsl.exe".to_string()),
        ProfileType::Ssh    => Ok("ssh.exe".to_string()),
        ProfileType::Custom => Err(ResolveError::CustomMissingExecutable {
            name: profile_name.to_string(),
        }),
    }
}

// ── Arguments (§25.2) ────────────────────────────────────────────────────────

fn resolve_args(profile_type: &ProfileType, profile_def: Option<&ProfileDefinition>) -> Vec<String> {
    // Profile definition args override built-in defaults.
    if let Some(args) = profile_def.and_then(|d| d.args.as_ref()) {
        return args.clone();
    }
    match profile_type {
        ProfileType::Powershell => vec!["-NoLogo".to_string()],
        ProfileType::Cmd        => vec![],
        ProfileType::Custom     => vec![],
        ProfileType::Wsl => {
            if let Some(distro) = profile_def.and_then(|d| d.distribution.as_deref()) {
                vec!["-d".to_string(), distro.to_string()]
            } else {
                vec![]
            }
        }
        ProfileType::Ssh => build_ssh_args(profile_def),
    }
}

/// Construct SSH arguments from profile fields (§25.2 SSH row).
fn build_ssh_args(profile_def: Option<&ProfileDefinition>) -> Vec<String> {
    let Some(def) = profile_def else { return vec![] };
    let Some(host) = &def.host else { return vec![] };

    let mut args: Vec<String> = Vec::new();

    // Prepend -i identityFile if set.
    if let Some(id_file) = &def.identity_file {
        args.push("-i".to_string());
        args.push(id_file.clone());
    }

    // Prepend -o IdentitiesOnly=yes if useAgent == false.
    if def.use_agent == Some(false) {
        args.push("-o".to_string());
        args.push("IdentitiesOnly=yes".to_string());
    }

    // user@host or just host.
    let user_host = match &def.user {
        Some(user) => format!("{user}@{host}"),
        None       => host.clone(),
    };
    args.push(user_host);

    // -p port
    if let Some(port) = def.port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }

    // remote command as final arg.
    if let Some(cmd) = &def.remote_command {
        args.push(cmd.clone());
    }

    args
}

// ── CWD ──────────────────────────────────────────────────────────────────────

fn resolve_cwd(
    session: &SessionLaunchDefinition,
    workspace_def: &WorkspaceDefinition,
    profile_def: Option<&ProfileDefinition>,
    profile_type: &ProfileType,
    host_env: &HashMap<String, String>,
) -> Option<String> {
    // Priority: session → workspace defaults → profile → built-in default.
    let raw: Option<String> = session
        .cwd
        .as_deref()
        .or_else(|| workspace_def.defaults.as_ref()?.cwd.as_deref())
        .or_else(|| profile_def?.cwd.as_deref())
        .map(|s| s.to_string())
        .or_else(|| builtin_cwd(profile_type));

    raw.map(|c| expand_env_vars(&c, host_env))
}

/// Returns the built-in default cwd for a profile type.
///
/// WSL returns `None` because the home directory is determined by WSL itself.
/// All other types default to `%USERPROFILE%`.
fn builtin_cwd(profile_type: &ProfileType) -> Option<String> {
    match profile_type {
        ProfileType::Wsl => None,
        _                => Some("%USERPROFILE%".to_string()),
    }
}

// ── Environment merge (§25.3) ─────────────────────────────────────────────────

fn resolve_env(
    session: &SessionLaunchDefinition,
    workspace_def: &WorkspaceDefinition,
    global_settings: &GlobalSettings,
    profile_def: Option<&ProfileDefinition>,
    profile_type: &ProfileType,
    host_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    // Layer 1: host process environment.
    let mut env: HashMap<String, String> = host_env.clone();

    // Layer 2: global default profile env (if defined in global settings).
    if let Some(global_default) = global_settings.profiles.get(&global_settings.default_profile) {
        if let Some(e) = &global_default.env {
            apply_layer(&mut env, e);
        }
    }

    // Layer 3: workspace defaults.env.
    if let Some(defaults) = &workspace_def.defaults {
        if let Some(e) = &defaults.env {
            apply_layer(&mut env, e);
        }
    }

    // Layer 4: resolved profile env.
    if let Some(def) = profile_def {
        if let Some(e) = &def.env {
            apply_layer(&mut env, e);
        }
    }

    // Layer 5: per-session env overrides.
    if let Some(e) = &session.env {
        apply_layer(&mut env, e);
    }

    // Layer 6: TERM=xterm-256color for all non-SSH sessions.
    if *profile_type != ProfileType::Ssh {
        env.insert("TERM".to_string(), "xterm-256color".to_string());
    }

    env
}

/// Apply one env layer: `Some(value)` sets/overwrites a key; `None` removes it.
fn apply_layer(env: &mut HashMap<String, String>, layer: &HashMap<String, Option<String>>) {
    for (k, v) in layer {
        match v {
            Some(val) => { env.insert(k.clone(), val.clone()); }
            None      => { env.remove(k); }
        }
    }
}

// ── Env var expansion ─────────────────────────────────────────────────────────

/// Expands Windows-style `%VAR%` references using the supplied environment map.
/// Unknown variables are replaced with an empty string.
fn expand_env_vars(s: &str, env: &HashMap<String, String>) -> String {
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..]; // skip opening '%'
        if let Some(end) = rest.find('%') {
            let var_name = &rest[..end];
            if var_name.is_empty() {
                // "%%" → literal "%"
                result.push('%');
            } else {
                let value = env.get(var_name).map(|v| v.as_str()).unwrap_or("");
                result.push_str(value);
            }
            rest = &rest[end + 1..]; // skip past closing '%'
        } else {
            // No closing '%' — treat as literal.
            result.push('%');
        }
    }
    result.push_str(rest);
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{DefaultsDefinition, ProfileDefinition, ProfileType};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn empty_workspace() -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "test".to_string(),
            description: None,
            defaults: None,
            profiles: None,
            bindings: None,
            windows: None,
            tabs: None,
        }
    }

    fn empty_session() -> SessionLaunchDefinition {
        SessionLaunchDefinition::default()
    }

    fn session_with_profile(name: &str) -> SessionLaunchDefinition {
        SessionLaunchDefinition {
            profile: Some(name.to_string()),
            ..Default::default()
        }
    }

    fn host_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn resolve(
        session: &SessionLaunchDefinition,
        workspace: &WorkspaceDefinition,
        global: &GlobalSettings,
        host: &HashMap<String, String>,
        pwsh_available: bool,
    ) -> ResolvedLaunchSpec {
        resolve_launch_spec(session, workspace, global, host, |exe| {
            exe == "pwsh.exe" && pwsh_available
        })
        .unwrap()
    }

    // ── §25.2 Built-in profile defaults: PowerShell ───────────────────────────

    #[test]
    fn powershell_pwsh_available() {
        let spec = resolve(
            &session_with_profile("powershell"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            true,
        );
        assert_eq!(spec.executable, "pwsh.exe");
        assert_eq!(spec.args, vec!["-NoLogo"]);
        assert_eq!(spec.cwd, Some("C:\\Users\\test".to_string()));
    }

    #[test]
    fn powershell_pwsh_fallback_to_powershell_exe() {
        let spec = resolve(
            &session_with_profile("powershell"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false, // pwsh.exe NOT available
        );
        assert_eq!(spec.executable, "powershell.exe");
        assert_eq!(spec.args, vec!["-NoLogo"]);
    }

    // ── §25.2 Built-in profile defaults: cmd ─────────────────────────────────

    #[test]
    fn cmd_defaults() {
        let spec = resolve(
            &session_with_profile("cmd"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "cmd.exe");
        assert!(spec.args.is_empty());
        assert_eq!(spec.cwd, Some("C:\\Users\\test".to_string()));
    }

    // ── §25.2 Built-in profile defaults: WSL ─────────────────────────────────

    #[test]
    fn wsl_no_distribution() {
        let spec = resolve(
            &session_with_profile("wsl"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &HashMap::new(),
            false,
        );
        assert_eq!(spec.executable, "wsl.exe");
        assert!(spec.args.is_empty());
        assert_eq!(spec.cwd, None); // WSL home determined by WSL
    }

    #[test]
    fn wsl_with_distribution() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "ubuntu".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Wsl,
                    distribution: Some("Ubuntu-24.04".to_string()),
                    executable: None,
                    args: None,
                    cwd: None,
                    env: None,
                    title: None,
                    host: None,
                    user: None,
                    port: None,
                    identity_file: None,
                    use_agent: None,
                    remote_command: None,
                    scrollback_lines: None,
                },
            );
            m
        });
        let spec = resolve(
            &session_with_profile("ubuntu"),
            &workspace,
            &GlobalSettings::default(),
            &HashMap::new(),
            false,
        );
        assert_eq!(spec.executable, "wsl.exe");
        assert_eq!(spec.args, vec!["-d", "Ubuntu-24.04"]);
    }

    // ── §25.2 Built-in profile defaults: SSH ─────────────────────────────────

    #[test]
    fn ssh_basic() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "prod".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Ssh,
                    host: Some("prod-box".to_string()),
                    user: Some("deploy".to_string()),
                    port: Some(22),
                    executable: None,
                    args: None,
                    cwd: None,
                    env: None,
                    title: None,
                    distribution: None,
                    identity_file: None,
                    use_agent: None,
                    remote_command: None,
                    scrollback_lines: None,
                },
            );
            m
        });
        let spec = resolve(
            &session_with_profile("prod"),
            &workspace,
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "ssh.exe");
        assert_eq!(spec.args, vec!["deploy@prod-box", "-p", "22"]);
        assert_eq!(spec.cwd, Some("C:\\Users\\test".to_string()));
    }

    #[test]
    fn ssh_with_identity_file_and_no_agent() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "secure".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Ssh,
                    host: Some("secure-box".to_string()),
                    user: Some("admin".to_string()),
                    port: None,
                    identity_file: Some("~/.ssh/id_rsa".to_string()),
                    use_agent: Some(false),
                    remote_command: Some("bash".to_string()),
                    executable: None,
                    args: None,
                    cwd: None,
                    env: None,
                    title: None,
                    distribution: None,
                    scrollback_lines: None,
                },
            );
            m
        });
        let spec = resolve(
            &session_with_profile("secure"),
            &workspace,
            &GlobalSettings::default(),
            &HashMap::new(),
            false,
        );
        assert_eq!(
            spec.args,
            vec!["-i", "~/.ssh/id_rsa", "-o", "IdentitiesOnly=yes", "admin@secure-box", "bash"]
        );
    }

    // ── §25.2 Built-in profile defaults: Custom ───────────────────────────────

    #[test]
    fn custom_with_explicit_executable() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "fish".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Custom,
                    executable: Some("C:\\tools\\fish.exe".to_string()),
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
            m
        });
        let spec = resolve(
            &session_with_profile("fish"),
            &workspace,
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "C:\\tools\\fish.exe");
        assert!(spec.args.is_empty());
    }

    #[test]
    fn custom_missing_executable_is_error() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "noexe".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Custom,
                    executable: None, // missing!
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
            m
        });
        let result = resolve_launch_spec(
            &session_with_profile("noexe"),
            &workspace,
            &GlobalSettings::default(),
            &HashMap::new(),
            |_| false,
        );
        assert_eq!(result, Err(ResolveError::CustomMissingExecutable { name: "noexe".to_string() }));
    }

    // ── §25.1 Profile lookup fallback chain ───────────────────────────────────

    #[test]
    fn profile_resolved_from_workspace() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "my-pwsh".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Powershell,
                    executable: Some("pwsh.exe".to_string()),
                    args: Some(vec!["-NoLogo".to_string(), "-NonInteractive".to_string()]),
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
            m
        });
        let spec = resolve(
            &session_with_profile("my-pwsh"),
            &workspace,
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        // Workspace profile takes priority.
        assert_eq!(spec.executable, "pwsh.exe");
        assert_eq!(spec.args, vec!["-NoLogo", "-NonInteractive"]);
    }

    #[test]
    fn profile_resolved_from_global_when_not_in_workspace() {
        let mut global = GlobalSettings::default();
        global.profiles.insert(
            "global-cmd".to_string(),
            ProfileDefinition {
                profile_type: ProfileType::Cmd,
                executable: Some("cmd.exe".to_string()),
                args: Some(vec!["/K".to_string()]),
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

        let spec = resolve(
            &session_with_profile("global-cmd"),
            &empty_workspace(), // not in workspace
            &global,
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "cmd.exe");
        assert_eq!(spec.args, vec!["/K"]);
    }

    #[test]
    fn profile_resolved_from_builtin_when_not_in_workspace_or_global() {
        // "powershell" is a built-in — no definition needed.
        let spec = resolve(
            &session_with_profile("powershell"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            true,
        );
        assert_eq!(spec.executable, "pwsh.exe");
        assert_eq!(spec.args, vec!["-NoLogo"]);
    }

    #[test]
    fn workspace_profile_shadows_global_profile() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "shared".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Cmd,
                    executable: Some("workspace-cmd.exe".to_string()),
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
            m
        });

        let mut global = GlobalSettings::default();
        global.profiles.insert(
            "shared".to_string(),
            ProfileDefinition {
                profile_type: ProfileType::Cmd,
                executable: Some("global-cmd.exe".to_string()),
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

        let spec = resolve(
            &session_with_profile("shared"),
            &workspace,
            &global,
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        // Workspace definition wins.
        assert_eq!(spec.executable, "workspace-cmd.exe");
    }

    #[test]
    fn unknown_profile_is_error() {
        let result = resolve_launch_spec(
            &session_with_profile("does-not-exist"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &HashMap::new(),
            |_| false,
        );
        assert_eq!(
            result,
            Err(ResolveError::ProfileNotFound { name: "does-not-exist".to_string() })
        );
    }

    // ── Profile name fallback: session → workspace default → global default ───

    #[test]
    fn profile_name_from_workspace_default_when_session_has_none() {
        let mut workspace = empty_workspace();
        workspace.defaults = Some(DefaultsDefinition {
            profile: Some("cmd".to_string()),
            ..Default::default()
        });
        // Session has no profile → uses workspace default "cmd".
        let spec = resolve(
            &empty_session(),
            &workspace,
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "cmd.exe");
    }

    #[test]
    fn profile_name_from_global_default_when_session_and_workspace_have_none() {
        let mut global = GlobalSettings::default();
        global.default_profile = "cmd".to_string();
        // Session and workspace both have no profile → uses global default "cmd".
        let spec = resolve(
            &empty_session(),
            &empty_workspace(),
            &global,
            &host_env(&[("USERPROFILE", "C:\\Users\\test")]),
            false,
        );
        assert_eq!(spec.executable, "cmd.exe");
    }

    // ── §25.3 Environment merge ───────────────────────────────────────────────

    #[test]
    fn env_merge_all_layers_with_null_removal() {
        // Construct a scenario that exercises all layers:
        //   host:               { A=host,    B=host }
        //   global default env: { B=global,  C=global }
        //   workspace defaults: { C=null → remove, D=workspace }
        //   profile env:        { D=null → remove, E=profile }
        //   session env:        { E=null → remove, F=session }
        //   TERM auto-inserted  (non-SSH)
        //
        // Expected final env:
        //   { A=host, B=global, F=session, TERM=xterm-256color }

        let host = host_env(&[("A", "host"), ("B", "host")]);

        let mut global = GlobalSettings {
            default_profile: "my-default".to_string(),
            profiles: HashMap::new(),
        };
        global.profiles.insert(
            "my-default".to_string(),
            ProfileDefinition {
                profile_type: ProfileType::Cmd,
                executable: Some("cmd.exe".to_string()),
                args: None,
                cwd: None,
                env: Some({
                    let mut e = HashMap::new();
                    e.insert("B".to_string(), Some("global".to_string()));
                    e.insert("C".to_string(), Some("global".to_string()));
                    e
                }),
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

        let mut workspace = empty_workspace();
        workspace.defaults = Some(DefaultsDefinition {
            profile: Some("my-default".to_string()),
            env: Some({
                let mut e = HashMap::new();
                e.insert("C".to_string(), None); // null → remove
                e.insert("D".to_string(), Some("workspace".to_string()));
                e
            }),
            ..Default::default()
        });
        // Also define the profile in workspace to set layer-4 env.
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "my-default".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Cmd,
                    executable: Some("cmd.exe".to_string()),
                    args: None,
                    cwd: None,
                    env: Some({
                        let mut e = HashMap::new();
                        e.insert("D".to_string(), None); // null → remove
                        e.insert("E".to_string(), Some("profile".to_string()));
                        e
                    }),
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
            m
        });

        let session = SessionLaunchDefinition {
            profile: Some("my-default".to_string()),
            env: Some({
                let mut e = HashMap::new();
                e.insert("E".to_string(), None); // null → remove
                e.insert("F".to_string(), Some("session".to_string()));
                e
            }),
            ..Default::default()
        };

        let spec = resolve_launch_spec(&session, &workspace, &global, &host, |_| false).unwrap();

        assert_eq!(spec.env.get("A"), Some(&"host".to_string()),   "A should be from host");
        assert_eq!(spec.env.get("B"), Some(&"global".to_string()), "B should be overridden by global profile");
        assert!(!spec.env.contains_key("C"),                       "C should be removed by workspace defaults null");
        assert!(!spec.env.contains_key("D"),                       "D should be removed by profile null");
        assert!(!spec.env.contains_key("E"),                       "E should be removed by session null");
        assert_eq!(spec.env.get("F"), Some(&"session".to_string()), "F should be from session");
        assert_eq!(spec.env.get("TERM"), Some(&"xterm-256color".to_string()), "TERM auto-inserted for non-SSH");
    }

    #[test]
    fn ssh_sessions_do_not_get_term_variable() {
        let mut workspace = empty_workspace();
        workspace.profiles = Some({
            let mut m = HashMap::new();
            m.insert(
                "remote".to_string(),
                ProfileDefinition {
                    profile_type: ProfileType::Ssh,
                    host: Some("server".to_string()),
                    user: Some("user".to_string()),
                    port: None,
                    executable: None,
                    args: None,
                    cwd: None,
                    env: None,
                    title: None,
                    distribution: None,
                    identity_file: None,
                    use_agent: None,
                    remote_command: None,
                    scrollback_lines: None,
                },
            );
            m
        });
        let spec = resolve(
            &session_with_profile("remote"),
            &workspace,
            &GlobalSettings::default(),
            &HashMap::new(),
            false,
        );
        assert!(!spec.env.contains_key("TERM"), "SSH sessions must not get TERM");
    }

    #[test]
    fn session_env_can_remove_host_env_key() {
        let host = host_env(&[("SECRET", "value"), ("KEEP", "yes")]);
        let session = SessionLaunchDefinition {
            profile: Some("cmd".to_string()),
            env: Some({
                let mut e = HashMap::new();
                e.insert("SECRET".to_string(), None); // null → remove
                e
            }),
            ..Default::default()
        };
        let spec = resolve_launch_spec(&session, &empty_workspace(), &GlobalSettings::default(), &host, |_| false).unwrap();
        assert!(!spec.env.contains_key("SECRET"), "SECRET should be removed");
        assert_eq!(spec.env.get("KEEP"), Some(&"yes".to_string()), "KEEP should remain");
    }

    // ── CWD resolution ────────────────────────────────────────────────────────

    #[test]
    fn cwd_expanded_from_userprofile() {
        let spec = resolve(
            &session_with_profile("cmd"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\alice")]),
            false,
        );
        assert_eq!(spec.cwd, Some("C:\\Users\\alice".to_string()));
    }

    #[test]
    fn session_cwd_overrides_builtin_default() {
        let session = SessionLaunchDefinition {
            profile: Some("cmd".to_string()),
            cwd: Some("D:\\projects".to_string()),
            ..Default::default()
        };
        let spec = resolve_launch_spec(
            &session,
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\alice")]),
            |_| false,
        )
        .unwrap();
        assert_eq!(spec.cwd, Some("D:\\projects".to_string()));
    }

    #[test]
    fn workspace_defaults_cwd_used_when_session_has_none() {
        let mut workspace = empty_workspace();
        workspace.defaults = Some(DefaultsDefinition {
            cwd: Some("D:\\workspace".to_string()),
            ..Default::default()
        });
        let spec = resolve(
            &session_with_profile("cmd"),
            &workspace,
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\alice")]),
            false,
        );
        assert_eq!(spec.cwd, Some("D:\\workspace".to_string()));
    }

    #[test]
    fn wsl_cwd_is_none_without_explicit_override() {
        let spec = resolve(
            &session_with_profile("wsl"),
            &empty_workspace(),
            &GlobalSettings::default(),
            &host_env(&[("USERPROFILE", "C:\\Users\\alice")]),
            false,
        );
        assert_eq!(spec.cwd, None);
    }

    // ── session.args overrides profile args ───────────────────────────────────

    #[test]
    fn session_args_override_profile_args() {
        let session = SessionLaunchDefinition {
            profile: Some("powershell".to_string()),
            args: Some(vec!["-Command".to_string(), "Get-Process".to_string()]),
            ..Default::default()
        };
        let spec = resolve_launch_spec(
            &session,
            &empty_workspace(),
            &GlobalSettings::default(),
            &HashMap::new(),
            |_| true, // pwsh.exe available
        )
        .unwrap();
        assert_eq!(spec.args, vec!["-Command", "Get-Process"]);
    }

    // ── expand_env_vars unit tests ─────────────────────────────────────────────

    #[test]
    fn expand_known_var() {
        let env = host_env(&[("USERPROFILE", "C:\\Users\\bob")]);
        assert_eq!(expand_env_vars("%USERPROFILE%", &env), "C:\\Users\\bob");
    }

    #[test]
    fn expand_unknown_var_becomes_empty() {
        let env = HashMap::new();
        assert_eq!(expand_env_vars("%UNKNOWN%", &env), "");
    }

    #[test]
    fn expand_percent_percent_becomes_literal_percent() {
        let env = HashMap::new();
        assert_eq!(expand_env_vars("100%%", &env), "100%");
    }

    #[test]
    fn expand_var_in_path() {
        let env = host_env(&[("USERPROFILE", "C:\\Users\\bob")]);
        assert_eq!(
            expand_env_vars("%USERPROFILE%\\projects", &env),
            "C:\\Users\\bob\\projects"
        );
    }
}
