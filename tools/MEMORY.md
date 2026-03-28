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
