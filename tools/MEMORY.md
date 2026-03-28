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

---

## wintermdriver-mtz.2: VT screen buffer

`ScreenBuffer` lives in `wtd-pty::screen`. Re-exported from `wtd_pty` root.

**Key types:**
- `ScreenBuffer` — owns primary/alternate `Grid`, scrollback `VecDeque<Vec<Cell>>`, `Cursor`, SGR pen, title
- `Cell` — `character: char`, `fg/bg: Color`, `attrs: CellAttrs`, `wide: bool`, `wide_continuation: bool`
- `Color` — `Default | Ansi(u8) | AnsiBright(u8) | Indexed(u8) | Rgb(u8,u8,u8)`
- `CellAttrs` — bitfield `u16` with constants BOLD, DIM, ITALIC, UNDERLINE, BLINK, INVERSE, HIDDEN, STRIKETHROUGH

**Public API:**
- `ScreenBuffer::new(cols, rows, max_scrollback)` — create
- `ScreenBuffer::advance(&mut self, bytes: &[u8])` — feed raw PTY bytes
- `ScreenBuffer::cell(row, col) -> Option<&Cell>` — read a cell
- `ScreenBuffer::visible_text() -> String` — full screen as newline-separated text
- `ScreenBuffer::row_text(row) -> Option<String>`
- `ScreenBuffer::scrollback_len()`, `scrollback_row(idx)`
- `ScreenBuffer::cursor() -> &Cursor`, `ScreenBuffer::title: String`

**Design notes:**
- vte `Perform` is implemented directly on `ScreenBuffer`; parser is swapped out during `advance()` to avoid double-borrow
- Alternate screen (DEC `?1049h/l`) clears on entry; primary is preserved untouched
- Scrollback only accumulates from primary screen top-margin scrolls; alternate screen scrolls use a dummy sink
- Wide-char detection uses a hand-rolled Unicode range table (no external dep); covers CJK, Hangul, fullwidth forms, emoji
- Scroll region (DECSTBM `r`) is respected for cursor movement bounds and SU/SD

---

## wintermdriver-mtz.3: Binary pane layout tree

`LayoutTree` lives in `wtd-core::layout`. Arena-based mutable binary tree for per-tab pane layouts.

**Key types:**
- `LayoutTree` — arena-backed binary tree; leaves are panes (`PaneId`), internal nodes are splits (`Orientation` + ratio)
- `Rect` — `{x, y, width, height}` in character cells (`u16`)
- `Direction` — `Up | Down | Left | Right` for spatial focus
- `ResizeDirection` — `GrowRight | GrowDown | ShrinkRight | ShrinkDown`
- `CloseResult` — `Closed { new_focus }` or `LastClosed`
- `LayoutError` — `PaneNotFound(PaneId)`

**Public API:**
- `LayoutTree::new()` — single pane (`PaneId(1)`), focused
- `split_right(target) -> Result<PaneId>` / `split_down(target) -> Result<PaneId>` — replace leaf with split + new pane
- `close_pane(target) -> Result<CloseResult>` — remove leaf, promote sibling, update focus
- `resize_pane(target, dir, cells, total_rect) -> Result<()>` — adjust nearest ancestor split ratio, clamped to min sizes
- `focus()`, `set_focus(target)`, `focus_next()`, `focus_prev()`, `focus_direction(dir, total_rect)`
- `toggle_zoom()`, `is_zoomed()`, `zoomed_pane()`
- `compute_rects(total_rect) -> HashMap<PaneId, Rect>` — layout computation (zoomed pane fills entire area)
- `panes() -> Vec<PaneId>` (depth-first order), `pane_count()`

**Design decisions:**
- Reuses `Orientation` from `wtd_core::workspace` (same enum for definition and runtime)
- Arena with `Vec<Option<Node>>` + free list; nodes have parent pointers for O(depth) operations
- Split inserts new split node at the original leaf's slot (preserves parent/root references without updating them)
- PaneId generation is internal (counter starting at 1); downstream beads can map PaneIds to sessions
- Resize finds nearest ancestor split with matching orientation; adjusts ratio accounting for which child the pane is in
- Clamping uses recursive `min_dim()`: stacked same-orientation splits sum minimums, perpendicular splits take max
- Min pane size: 2 cols × 1 row (§18.4); ratio additionally bounded to [0.1, 0.9] (§18.3)
- Directional focus uses Euclidean distance² between geometric centres
