# Bead Runner Memory

Cross-bead knowledge for the WinTermDriver project.
Each bead agent reads this file for context and appends learnings for future beads.

---

## wintermdriver-elf: Cargo workspace layout

Workspace root: `Cargo.toml` at repo root, members in `crates/`.

| Crate | Type | Purpose |
|-------|------|---------|
| `wtd-core` | lib | Shared domain types: `WorkspaceName`, `SessionId`, `PaneId`, `TabId`, `WorkspaceInstanceId`, `CoreError` |
| `wtd-ipc` | lib | IPC message envelope types (`ClientMessage`, `HostMessage`), pipe name prefix, `IpcError` |
| `wtd-pty` | lib | ConPTY scaffold: `PtySize`, `PtyError` — full impl in wintermdriver-mtz.1 |
| `wtd-host` | bin (`wtd-host`) | Background host process — stub main |
| `wtd-ui` | bin (`wtd-ui`) | Graphical UI process — stub main |
| `wtd-cli` | bin (`wtd`) | Controller CLI — stub main |

**Workspace deps** (use `{ workspace = true }` in member Cargo.toml):
`serde`, `serde_json`, `serde_yaml`, `thiserror`, `anyhow`, `tokio`, `vte`, `clap`, `windows` (0.58), `wtd-core`, `wtd-ipc`, `wtd-pty`

**`windows` dep** is declared at workspace level unconditionally (Windows-only project).
Use `[target.'cfg(windows)'.dependencies]` in member crates when needed.

**rust-toolchain.toml** pins `stable` with target `x86_64-pc-windows-msvc`.

**serde_yaml 0.9** — `serde_yaml = "0.9"` resolves to 0.9.34+deprecated; this is the last stable serde_yaml release.

---

## wintermdriver-u24.1: WorkspaceDefinition types and loader

All definition types live in `wtd-core::workspace` (§9.1). Loader + validation in `wtd-core::workspace_loader`.

**Key types:**
- `WorkspaceDefinition` — root struct; `windows` and `tabs` are both `Option<Vec<...>>` (mutually exclusive)
- `PaneNode` — `#[serde(tag = "type", rename_all = "lowercase")]` enum: `Pane(PaneLeaf)` / `Split(SplitNode)`
- `ActionReference` — `#[serde(untagged)]`: `Simple(String)` or `WithArgs { action, args }`
- camelCase YAML fields use `#[serde(rename = "...")]` on snake_case Rust fields

**Public API:** `wtd_core::load_workspace_definition(file_path, content) -> Result<WorkspaceDefinition, LoadError>`

**Validation:** `LoadError::Validation { errors: Vec<ValidationError> }` — each error has `.path` (dot-notation) and `.message`. Built-in profile names (`powershell`, `cmd`, `wsl`, `ssh`, `custom`) are always valid profile references.

---

## wintermdriver-u24.2: Profile resolver and GlobalSettings

`GlobalSettings` lives in `wtd-core::global_settings`. Profile resolution in `wtd-core::profile_resolver`.

**Key types:**
- `GlobalSettings` — `default_profile: String` (default `"powershell"`), `profiles: HashMap<String, ProfileDefinition>`
- `ResolvedLaunchSpec` — `executable`, `args`, `cwd: Option<String>`, `env: HashMap<String,String>`
- `ResolveError` — `ProfileNotFound`, `CustomMissingExecutable`

**Public API:** `wtd_core::resolve_launch_spec(session, workspace_def, global_settings, host_env, find_exe) -> Result<ResolvedLaunchSpec, ResolveError>`

**Key design decisions:**
- `find_exe: impl Fn(&str) -> bool` injectable — enables `pwsh.exe` → `powershell.exe` fallback testing without real PATH check
- WSL `cwd` defaults to `None` (WSL determines its own home); all other types default to `%USERPROFILE%`
- SSH sessions do NOT get `TERM=xterm-256color` (remote negotiates TERM)
- Env layer 2 applies global `default_profile`'s env (not the resolved profile's parent), allowing global baseline env
- `%VAR%` expansion in cwd uses host_env map (no OS call)
