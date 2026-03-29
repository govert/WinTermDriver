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

---

## wintermdriver-8w8.1: IPC message types and framing

All IPC message types live in `wtd-ipc::message`. Framing in `wtd-ipc::framing`.

**Envelope:** `Envelope { id: String, msg_type: String, payload: serde_json::Value }` — serializes to `{"id":"...","type":"...","payload":{...}}` per §13.5. The `msg_type` field uses `#[serde(rename = "type")]`.

**MessagePayload trait:** Every payload struct implements `MessagePayload` with a `TYPE_NAME` constant. Use `Envelope::new(id, &payload)` to construct and `envelope.extract_payload::<T>()` to extract typed payloads.

**parse_envelope:** `parse_envelope(&Envelope) -> Result<TypedMessage, ParseError>` dispatches on `msg_type` string to deserialize into the correct variant of the `TypedMessage` enum (covers all 40+ message types).

**Framing (§13.4):** `wtd_ipc::framing::encode/decode` — 4-byte u32 LE length prefix + UTF-8 JSON. Max 16 MiB (`MAX_MESSAGE_SIZE`). `read_length_prefix()` for incremental pipe reading.

**Key design decisions:**
- Single `Envelope` struct (not generic over direction) — framing layer doesn't know sender/receiver
- No separate `ClientMessage`/`HostMessage` enums — `TypedMessage` enum contains all variants; downstream can match only the ones they expect
- Payload field names use camelCase on wire (`#[serde(rename_all = "camelCase")]` on payload structs)
- IDs are `String` (not uuid crate) — caller generates UUIDs
- `ErrorCode` enum serializes to kebab-case strings (e.g. `"target-not-found"`)
- `Send.newline` defaults to `true`; `InvokeAction.args` defaults to `{}`
- State snapshots in results (e.g. `AttachWorkspaceResult.state`) use `serde_json::Value` — concrete types deferred to host implementation beads
- `IpcError` extended with `MessageTooLarge` and `FrameTooShort` variants

---

## wintermdriver-8w8.2: Session manager with restart and backoff

Session lifecycle lives in `wtd-host::session`. Backoff logic in `wtd-host::backoff`. The `wtd-host` crate is now lib+bin (has both `src/lib.rs` and `src/main.rs`).

**Key types:**
- `Session` — owns a `PtySession`, `ScreenBuffer`, reader thread, and backoff state
- `SessionState` — enum: `Creating | Running | Exited { exit_code } | Failed { error } | Restarting { attempt }`
- `SessionConfig` — `executable`, `args`, `cwd`, `env`, `restart_policy`, `startup_command`, `size`, `name`, `max_scrollback`
- `SessionError` — `Pty(PtyError)` | `NotRunning`
- `BackoffState` — tracks restart count and computes exponential delays

**Public API:**
- `Session::new(id, config)` — create in `Creating` state
- `Session::start()` — spawn ConPTY child, start reader thread, deliver startup command after 100ms
- `Session::write_input(data)` — write to child stdin
- `Session::process_pending_output()` — drain reader thread into screen buffer
- `Session::check_exit() -> Option<u32>` — poll for exit, returns exit code if exited
- `Session::should_restart() -> bool` — evaluate restart policy against current state
- `Session::next_restart_delay() -> Duration` — get next backoff delay
- `Session::restart()` — tear down old child, clear screen, spawn fresh

**Design decisions:**
- Reader thread uses raw HANDLE passed as `usize` across thread boundary (HANDLE wraps `*mut c_void` which is `!Send`)
- Output flows via `mpsc::channel<Vec<u8>>` from reader thread; `process_pending_output()` drains into ScreenBuffer
- Startup command delivered by a detached thread that sleeps 100ms then writes to the raw input handle
- `PtySession::process_handle()` added to expose the child process HANDLE for `GetExitCodeProcess`
- On restart, PtySession is dropped (closes ConPTY + handles), reader thread detects pipe close and exits, then new PTY is spawned
- `PtySession::spawn` does NOT yet accept environment variables; env from `SessionConfig` is not passed to CreateProcess (future bead needed)
- Backoff formula: `min(500 * 2^(count-1), 30000)` ms; resets after 60s stable run

---

## wintermdriver-8w8.3: Workspace instance manager

Workspace lifecycle lives in `wtd-host::workspace_instance`. Manages running workspace instances with pane-session attachments.

**Key types:**
- `WorkspaceInstance` — owns tabs, sessions, pane records, and a Windows Job Object
- `WorkspaceState` — enum: `Creating | Active | Closing | Closed | Recreating` (§27.2)
- `PaneState` — `Attached { session_id }` | `Detached { error }` (§29.2)
- `TabInstance` — runtime tab: `TabId`, name, `LayoutTree`
- `AttachSnapshot` / `TabSnapshot` — read-only state for attach (§26.2)
- `WorkspaceError` — `InvalidState`, `JobObject`, `ProfileResolution`

**Public API:**
- `WorkspaceInstance::open(id, workspace_def, global_settings, host_env, find_exe)` — create from definition
- `close()` — terminate all sessions, release job object
- `recreate(workspace_def, ...)` — tear down and re-create from definition
- `save() -> WorkspaceDefinition` — reconstruct definition from runtime state
- `attach_snapshot() -> AttachSnapshot` — read-only state snapshot
- Accessors: `sessions()`, `session()`, `pane_state()`, `pane_name()`, `tabs()`, `running_session_count()`, `failed_pane_count()`

**LayoutTree additions:**
- `LayoutTree::from_pane_node(node) -> (LayoutTree, Vec<(String, PaneId)>)` — build tree from definition with pane name mappings
- `LayoutTree::to_pane_node(leaf_fn) -> PaneNode` — reconstruct definition from runtime tree

**Session additions:**
- `Session::stop()` — public method to terminate and clean up
- `Session::config()` — access immutable config
- `Session::name()` — convenience accessor
- `Session::process_handle_raw()` — raw HANDLE as usize for Job Object assignment

**Design decisions:**
- Internal `populate()` method shared between `open()` and `recreate()` to avoid duplication
- Depth-first traversal of `PaneNode` creates sessions in same order as `LayoutTree::from_pane_node` pane mappings
- Partial failure (§29.2–29.3): failed sessions are recorded as `PaneState::Detached`, workspace still moves to Active
- Job Object created per instance; each child process added on successful start
- Session IDs are monotonically increasing across recreates (never reused)
- `save()` uses `to_pane_node` with original `SessionLaunchDefinition` stored per pane

---

## wintermdriver-8w8.4: Named pipe IPC server

Named pipe server lives in `wtd-host::ipc_server`. Security helpers in `wtd-host::pipe_security`.

**Key types:**
- `IpcServer` — tokio-based accept loop on `\\.\pipe\wtd-{SID}`, manages concurrent clients
- `ClientRegistry` — tracks connected clients with `mpsc` channels for push messages
- `ClientId` — `u64` identifier for each connected client
- `PipeSecurity` — RAII wrapper owning SECURITY_DESCRIPTOR + ACL buffers for pipe DACL
- `ServerError` — `Io | Ipc | Security`
- `RequestHandler` trait — `handle_request(client_id, envelope, msg) -> Option<Envelope>`

**Public API:**
- `IpcServer::new(pipe_name, handler) -> Result<Self, ServerError>` — create with security
- `IpcServer::run(&self, shutdown_rx) -> Result<()>` — accept loop until shutdown
- `IpcServer::broadcast_to_ui(&self, envelope)` — push to all UI clients
- `IpcServer::send_to_client(&self, client_id, envelope)` — push to specific client
- `IpcServer::clients()` — access `Arc<Mutex<ClientRegistry>>`
- `read_frame(reader) -> Result<Envelope>` / `write_frame(writer, envelope)` — async frame I/O
- `pipe_name_for_current_user() -> Result<String>` — builds `\\.\pipe\wtd-{SID}`
- `PipeSecurity::verify_client_sid(pipe_handle) -> Result<bool>` — checks connecting client's SID

**Design decisions:**
- Uses `tokio::net::windows::named_pipe::ServerOptions::create_with_security_attributes_raw` for custom DACL
- DACL built manually (`InitializeAcl` + `AddAccessAllowedAce`) — no `Win32_Security_Authorization` dependency
- SID-to-string conversion is hand-rolled (avoids `ConvertSidToStringSidW` and extra feature)
- Per-connection `tokio::spawn` with `tokio::io::split` for simultaneous read/write
- `select!` loop: reads frames from pipe AND drains push channel, writes responses directly
- Handshake handled by the server itself (not the `RequestHandler`)
- Protocol version check: rejects mismatched versions with `ErrorCode::ProtocolError`
- Client SID verified via `GetNamedPipeClientProcessId` + `OpenProcessToken` + `EqualSid`
- `PROTOCOL_VERSION = 1`, `HOST_VERSION = env!("CARGO_PKG_VERSION")`
- Shutdown via `watch::Receiver<bool>` — accept loop exits, existing connections run until client disconnects

---

## wintermdriver-8w8.5: Action system (registry + dispatcher)

Action system lives in `wtd-host::action`. Registry of named actions and dispatcher that validates args and executes them.

**Key types:**
- `ActionRegistry` — maps action names (kebab-case) to `ActionDef`; `v1_registry()` pre-populates all 36 v1 actions
- `ActionDef` — `name`, `target_type: TargetType`, `args: &[ArgDef]`, `description`
- `TargetType` — `Global | Workspace | Window | Tab | Pane`
- `ArgDef` — `name`, `arg_type: ArgType`, `required: bool`
- `ArgType` — `String | Int | Bool`
- `ActionDispatcher` — validates args via registry, resolves target pane, dispatches to `WorkspaceInstance`
- `ActionResult` — `Ok | PaneCreated { pane_id } | PaneClosed { pane_id, close_result }`
- `ActionError` — `UnknownAction | InvalidArgument | Workspace | Layout | PaneNotFound | NoActiveTab | NotImplemented`

**Public API:**
- `v1_registry() -> ActionRegistry` — all §20.3 actions registered
- `ActionRegistry::get(name)`, `validate_args(name, &Value)`, `action_names()`, `len()`
- `ActionDispatcher::new(registry, viewport)` — create with viewport rect for layout ops
- `ActionDispatcher::dispatch(workspace, action_name, args, target_pane_id) -> Result<ActionResult>`

**Currently dispatched actions:** split-right, split-down, close-pane, focus-next/prev-pane, focus-pane-{up,down,left,right}, focus-pane (by name), zoom-pane, resize-pane-{grow,shrink}-{right,down}, rename-pane, restart-session

**Not yet dispatched (return NotImplemented):** Workspace lifecycle (open/close/recreate/save-workspace), window actions, tab management (new/close/next/prev/goto/rename/move-tab), clipboard (copy/paste), UI actions (toggle-command-palette, toggle-fullscreen, enter-scrollback-mode). These need host-level or UI-level context beyond a single `WorkspaceInstance`.

**WorkspaceInstance additions:**
- `tabs_mut()` — mutable access to tabs vec
- `stop_pane_session(pane_id)` — stop and remove session for a pane
- `remove_pane(pane_id)` — remove pane record
- `find_pane_by_name(name) -> Option<PaneId>` — lookup across all panes
- `rename_pane(pane_id, new_name)` — update pane name
- `restart_pane_session(pane_id)` — stop and restart session
- `new_for_test(name)` — (cfg(test)) creates minimal instance with one tab/pane for unit tests

**Design decisions:**
- Split actions only modify the layout tree (no session created for new pane — session creation requires profile resolution, which is a host-level concern)
- Close-pane stops the session, removes from layout, then removes pane record
- Resolve target pane: explicit `target_pane_id` if given, otherwise focused pane of first (active) tab
- Pane existence checked in both pane records AND layout trees (split-created panes only exist in layout)
- Actions that require host-level context (workspace lifecycle, tab management, clipboard, UI) return `NotImplemented` for the host request handler to dispatch at a higher level

---

## wintermdriver-8w8.6: Host lifecycle (single-instance, auto-start, PID file, shutdown)

Host lifecycle lives in `wtd-host::host_lifecycle`. Auto-start/connect helpers in `wtd-ipc::connect`.

**Key types and functions:**
- `LifecycleError` — error enum for lifecycle operations
- `SingleInstanceCheck` — `Available | AlreadyRunning | StalePidCleaned`
- `data_dir()` → `%APPDATA%\WinTermDriver` (overridable via `WTD_DATA_DIR` env)
- PID file ops: `write_pid_file_in(dir)`, `read_pid_in(dir)`, `remove_pid_in(dir)`, `clean_stale_pid_in(dir)` — all accept `&Path` for test isolation; parameterless variants use default `data_dir()`
- `check_single_instance_in(pipe_name, dir)` — pipe check + stale PID cleanup
- `install_ctrl_handler(watch::Sender<bool>)` — `SetConsoleCtrlHandler` for CTRL_C/CLOSE/LOGOFF/SHUTDOWN
- `run_host(pipe_name, handler, shutdown_rx, dir)` — writes PID, runs IPC server, removes PID on exit
- `is_process_running(pid)` — `OpenProcess` + `GetExitCodeProcess` check for STILL_ACTIVE

**Auto-start helpers (`wtd-ipc::connect`):**
- `is_host_pipe_available(pipe_name)` — `WaitNamedPipeW` with 1ms timeout, non-consuming probe
- `find_host_executable()` — searches near current binary
- `start_host_detached()` — `CreateProcess` with `DETACHED_PROCESS` flag
- `ensure_host_running(pipe_name)` — check pipe → launch host → poll 50ms×100

**Host `main.rs` flow:** pipe_name → single-instance check → shutdown channel → ctrl handler → `run_host` → exit

**Design decisions:**
- Pipe name (`\\.\pipe\wtd-{SID}`) is the single-instance mutex; checked via `WaitNamedPipeW` (no pipe instance consumed)
- `pipe_name_for_current_user()` remains in `wtd-host::pipe_security`; `wtd-ipc::connect` does NOT have it (avoids duplicating SID retrieval). CLI/UI beads will need to add their own pipe name resolution or share it
- PID file functions accept `&Path dir` parameter for test isolation; tests use unique temp directories
- Ctrl handler uses `OnceLock<watch::Sender<bool>>` — can only be installed once per process
- `run_host` does NOT install the ctrl handler (caller responsibility) — keeps tests simple
- Shutdown sequence steps 1-2 (notify UI clients, close workspace instances) deferred to workspace management bead
- No `StopHost` IPC message type yet — shutdown is triggered via `watch::Sender` (ctrl handler or programmatic)
- Idle shutdown timeout (§16.3 `hostIdleShutdown`) not implemented — requires workspace instance tracking
- `main.rs` uses a `StubHandler` that returns `None` for all requests; real dispatching deferred to a future bead

---

## wintermdriver-g4u.1: Gate — YAML to running ConPTY

Integration tests in `crates/wtd-host/tests/gate_yaml_to_conpty.rs` verify the full pipeline: YAML fixture → `load_workspace_definition` → `WorkspaceInstance::open` → sessions reach `Running` with live ConPTY output.

**Fixtures:** `crates/wtd-host/tests/fixtures/simple-workspace.yaml` (single pane) and `split-workspace.yaml` (two-pane split)

**WorkspaceInstance additions:**
- `sessions_mut()` — mutable access to sessions HashMap (for draining output via `process_pending_output()`)

---

## wintermdriver-g4u.2: Gate — Input to screen buffer output

Extended `gate_yaml_to_conpty.rs` with two tests verifying the I/O round-trip:
- `input_sent_to_session_appears_in_screen_buffer` — sends `echo` via `write_input()`, polls `process_pending_output()`, asserts marker in `visible_text()`
- `multiple_inputs_appear_sequentially_in_screen_buffer` — sends two commands, verifies both markers appear in order

**Test pattern:** Use `sessions()` (immutable) for `write_input(&self)`, then `sessions_mut()` for `process_pending_output(&mut self)`. Poll with `wait_until()` helper (5s timeout, 100ms interval). cmd.exe echoes commands, so markers appear at least twice (command echo + output).

---

## wintermdriver-g4u.3: Gate — Full headless round-trip via IPC

Integration test in `crates/wtd-host/tests/gate_ipc_round_trip.rs` verifies the complete IPC pipeline: named pipe connect → handshake → OpenWorkspace → Send → Capture → assert.

**Test structure:** `GateHandler` implements `RequestHandler` with `Mutex<GateState>` for interior mutability. Handles three message types:
- `OpenWorkspace` — loads YAML fixture, creates `WorkspaceInstance`
- `Send` — resolves pane by name via `find_pane_by_name()`, writes input to session
- `Capture` — drains `process_pending_output()` for all sessions, returns `visible_text()` from target pane's session

**IPC client test pattern:** Use `connect_client()` (retry loop for pipe availability), `do_handshake()`, `poll_capture_until()` (polls Capture with a predicate and timeout). The `message::Send` payload type name-conflicts with `std::marker::Send` — import as `wtd_ipc::message::Send` or use qualified `message::Send`.

**Key insight:** When polling for echoed output, poll until the marker appears **at least twice** (once in the command echo line, once in the output line) rather than polling for first appearance then checking count separately — avoids timing races between the poll returning and the final capture.

---

## wintermdriver-in5.1: M1 Acceptance Gate

`crates/wtd-host/tests/gate_m1_acceptance.rs` — dedicated M1 milestone acceptance test (§37.5). Explicitly validates all six M1 criteria: YAML parsing, profile resolution, ConPTY launch, IPC send, screen buffer population, and capture returning expected output. Uses inline YAML (not fixture file) with its own `M1Handler`. This is the milestone sign-off test; all prior gate tests (g4u.1–g4u.3) validated individual pipeline stages.

---

## wintermdriver-u24.3: Global settings loader and merge precedence

`GlobalSettings` in `wtd-core::global_settings` expanded to full §11.2 schema.

**New types:**
- `FontConfig` — `family` ("Cascadia Mono"), `size` (12.0), `weight` ("normal")
- `ThemeConfig` — `name`, `foreground`, `background`, `cursor_color`, `selection_background`, `palette` (16-color xterm)
- `LogLevel` — `Trace | Debug | Info | Warn | Error` (default `Info`)
- `SettingsLoadError` — `Io | Yaml`

**New GlobalSettings fields:** `bindings`, `scrollback_lines` (10000), `restart_policy` (Never), `font`, `theme`, `copy_on_select` (false), `confirm_close` (true), `host_idle_shutdown` (None), `log_level` (Info)

**New public API:**
- `load_global_settings(path) -> Result<GlobalSettings, SettingsLoadError>` — missing file → defaults, empty file → defaults, partial YAML fills defaults via serde
- `default_bindings() -> BindingsDefinition` — §11.3 built-in keys (10) + chords (15) + prefix "Ctrl+B" + timeout 2000ms
- `merge_bindings(global, workspace) -> BindingsDefinition` — §11.6 merge: workspace chords/keys override same-key global entries, unoverridden preserved; workspace prefix/timeout override if set

**Design decisions:**
- `RestartPolicy` now implements `Default` (returns `Never`)
- All new fields use `#[serde(default = "...")]` so existing code constructing `GlobalSettings::default()` or deserializing partial YAML continues to work
- Existing `profile_resolver.rs` test that constructed `GlobalSettings { ... }` updated to use `..GlobalSettings::default()`

---

## wintermdriver-u24.4: Workspace definition file discovery

Workspace file discovery lives in `wtd-core::workspace_discovery` (§12).

**Key types:**
- `DiscoveredWorkspace` — `name`, `path: PathBuf`, `source: WorkspaceSource`
- `WorkspaceSource` — `Explicit | Local | User`
- `DiscoveryError` — `NotFound | ExplicitFileNotFound | Io`

**Public API:**
- `find_workspace(name, explicit_file, cwd)` — search using default user workspaces dir
- `find_workspace_in(name, explicit_file, cwd, user_dir)` — search with explicit user dir (test-friendly)
- `list_workspaces(cwd)` / `list_workspaces_in(cwd, user_dir)` — scan both sources, returns `Vec<DiscoveredWorkspace>`
- `user_workspaces_dir()` — `%APPDATA%\WinTermDriver\workspaces` (respects `WTD_DATA_DIR`)
- `ensure_dir(path)` / `ensure_user_workspaces_dir()` — create directories on first use (§12.3)

**Design decisions:**
- All functions have `_in` variants accepting explicit `user_dir: &Path` for test isolation (no env var mutation needed)
- Extension priority: `.yaml` > `.yml` > `.json` — first match in that order wins
- Listing returns both local and user entries even for the same name (per §12.4)
- `data_dir()` is private within the module — mirrors `wtd-host::host_lifecycle::data_dir()` pattern

---

## wintermdriver-rul.1: CLI command parser

All CLI parsing lives in `wtd-cli::cli` (§22.1–22.4). Uses clap derive macros.

**Key types:**
- `Cli` — top-level `#[derive(Parser)]` with global flags and `Command` subcommand
- `Command` — enum of all commands: `Open`, `Attach`, `Recreate`, `Close`, `Save`, `List`, `Focus`, `Rename`, `Action`, `Send`, `Keys`, `Capture`, `Scrollback`, `Follow`, `Inspect`, `Host`, `Completions`
- `ListCommand` — subcommands of `list`: `Workspaces`, `Instances`, `Panes { workspace }`, `Sessions { workspace }`
- `HostCommand` — subcommands of `host`: `Status`, `Stop`

**Global flags:** `--json` (bool), `--verbose` (bool), `--id <uuid>` (Option<String>) — all `global = true` so they work before or after subcommands.

**Shell completions:** Hidden `completions <shell>` subcommand using `clap_complete` crate. `print_completions(shell)` writes to stdout.

**Workspace dependency added:** `clap_complete = "4"` at workspace level.

**Design decisions:**
- `action` command uses `trailing_var_arg = true` for extra args after the action name
- `keys` requires at least one key spec (`#[arg(required = true)]`)
- `scrollback --tail` is a required `u32` flag (clap validates numeric)
- Command dispatch is not yet implemented — `main.rs` parses then exits with "not yet implemented"

---

## wintermdriver-rul.2: Target path parser and resolver

Target path parsing in `wtd-core::target`. Resolution in `wtd-host::target_resolver`.

**Key types:**
- `TargetPath` — enum: `Pane { pane }` | `WorkspacePane { workspace, pane }` | `WorkspaceTabPane { workspace, tab, pane }` | `WorkspaceWindowTabPane { workspace, window, tab, pane }`
- `TargetPathError` — `Empty | TooManySegments | EmptySegment | InvalidCharacters | TooLong`
- `ResolvedTarget` — `{ instance_id: WorkspaceInstanceId, pane_id: PaneId, canonical_path: String }`
- `ResolveError` — `Ambiguous | NotFound | NoActiveInstance | MultipleActiveInstances | WorkspaceNotFound | TabNotFound | PaneNotFound | PaneNotFoundInTab | IdNotFound`

**Public API:**
- `TargetPath::parse(path) -> Result<TargetPath, TargetPathError>` — validates §19.1 naming rules
- `resolve_target(path, &[&WorkspaceInstance]) -> Result<ResolvedTarget, ResolveError>` — resolution per §19.4
- `resolve_by_id(id_str, &[&WorkspaceInstance]) -> Result<ResolvedTarget, ResolveError>` — `--id` lookup

**WorkspaceInstance additions:**
- `find_tab_by_name(name) -> Option<&TabInstance>`
- `find_pane_in_tab(tab, pane_name) -> Option<PaneId>`
- `find_all_panes_by_name(name) -> Vec<(PaneId, String)>` — returns canonical paths for ambiguity reporting
- `canonical_pane_path(pane_id) -> Option<String>` — `workspace/tab/pane` format
- `new_for_test_multi(name, id, tab_specs)` — `#[cfg(test)]` flexible multi-tab test constructor

**Design decisions:**
- 4-segment paths: window segment is parsed but ignored during resolution (runtime doesn't track window-to-tab mapping)
- 1-segment requires exactly one active instance (§19.5); 0 or 2+ returns error
- `resolve_by_id` parses the ID string as u64 (matching current PaneId representation)
- Known issue: `LayoutTree::new()` always starts PaneIds at 1, so multi-tab workspaces have PaneId collisions in the flat `panes` HashMap — cross-tab pane-level resolution not reliable until PaneId uniqueness is addressed

---

## wintermdriver-rul.3: CLI IPC client, dispatch, and output formatting

CLI client lives in `wtd-cli::client`. Command dispatch in `wtd-cli::dispatch`. Output formatting in `wtd-cli::output`. Exit codes in `wtd-cli::exit_code`. `wtd-cli` is now a lib+bin crate (like `wtd-host`).

**Shared IPC additions (`wtd-ipc`):**
- `wtd_ipc::PROTOCOL_VERSION` — protocol version constant (was previously only in `wtd-host::ipc_server`)
- `wtd_ipc::connect::pipe_name_for_current_user()` — SID-based pipe name resolution (mirrors `wtd-host::pipe_security::pipe_name_for_current_user()`)
- `wtd_ipc::framing::read_frame_async()` / `write_frame_async()` — async length-prefixed frame I/O (mirrors `wtd-host::ipc_server::read_frame/write_frame`)
- **Note:** In windows-rs 0.58, `OpenProcessToken` is in `Win32::System::Threading`, NOT `Win32::Security` — must import from Threading explicitly

**Key types:**
- `IpcClient` — connects to host pipe, performs handshake, sends requests and reads responses
- `ClientError` — `Connect(ConnectError) | Ipc(IpcError) | Handshake(String)`
- `OutputResult` — `{ stdout, stderr, exit_code }` for testable formatting
- Exit codes: SUCCESS=0, GENERAL_ERROR=1, TARGET_NOT_FOUND=2, AMBIGUOUS_TARGET=3, HOST_START_FAILED=4, DEFINITION_ERROR=5, CONNECTION_ERROR=6, TIMEOUT=10

**Public API:**
- `IpcClient::connect_and_handshake()` — resolve pipe name, auto-start host, connect, handshake
- `IpcClient::connect_to(pipe_name)` — connect to specific pipe (for tests)
- `IpcClient::request(envelope) -> Envelope` — send request, read response
- `IpcClient::read_frame() / write_frame()` — raw frame I/O for streaming (Follow)
- `dispatch::run(cli) -> i32` — full dispatch: connect, build request, send, format, return exit code
- `output::format_response(envelope, json_mode) -> OutputResult` — text or JSON formatting

**Command dispatch mapping:**
- All CLI commands map to their corresponding IPC message types
- `message::Send` conflicts with `std::marker::Send` — use qualified `message::Send` or avoid glob importing `wtd_ipc::message::*`
- `host status` checks pipe availability locally (no IPC needed)
- `host stop` not yet implemented (no StopHost IPC message)
- `follow` sends Follow request then loops reading FollowData/FollowEnd; Ctrl+C sends CancelFollow

**Output formatting:**
- Text mode: table formatting with dynamic column widths for list commands; plain text for capture/scrollback
- JSON mode: `serde_json::to_string_pretty` on the response payload
- Error responses: message to stderr, candidates listed if present
- ErrorCode → exit code mapping: TargetNotFound/WorkspaceNotFound → 2, TargetAmbiguous → 3, others → 1

**Design decisions:**
- `IpcClient::connect_to(pipe_name)` allows tests to use custom pipe names without auto-start
- `message::Send` name conflict: dispatch.rs imports specific types, not glob, and qualifies `message::Send`
- `OutputResult` struct enables unit testing of formatting without stdout capture
- `host status` is a local check (no server connection needed), using `is_host_pipe_available`
- `FocusPane` and `RenamePane` messages receive the CLI target string as `pane_id` — host dispatch handler will need to resolve paths to PaneIds
- Action command args parsed as `key=value` pairs into `serde_json::Value::Object`

---

## wintermdriver-6en.1: Rendering technology decision

**Decision:** Win32 + DirectWrite selected as the rendering technology (ADR-001 in `docs/decisions/001-rendering-technology.md`).

**Candidates evaluated:**
- wezterm components: NO-GO — GPU renderer (`wezterm-gui`) not published as standalone crate; extraction requires forking ~15k lines
- Win32 + DirectWrite: GO (recommended) — 2-5ms/frame for realistic terminal content, 42 MB memory, zero new deps (uses existing `windows` 0.58)
- WebView2 + xterm.js: NO-GO — 80-150 MB memory per WebView2 instance, 7-12ms IPC+render pipeline, dual-language complexity

**Benchmark crate:** `crates/eval-renderer` — contains `bench_directwrite` example with five rendering modes (per-row, per-cell, run-based). Workspace member with `publish = false`.

**Windows features needed for rendering** (beyond existing workspace features):
`Foundation_Numerics`, `Win32_Graphics_Direct2D`, `Win32_Graphics_Direct2D_Common`, `Win32_Graphics_DirectWrite`, `Win32_Graphics_Dxgi_Common`, `Win32_Graphics_Gdi`, `Win32_UI_WindowsAndMessaging`

**Key API pattern for windows-rs 0.58 Direct2D:**
- `ID2D1HwndRenderTarget` does not expose inherited `ID2D1RenderTarget` methods directly — must `.cast::<ID2D1RenderTarget>()` first
- `D2D1_BRUSH_PROPERTIES` requires `Foundation_Numerics` feature (contains `Matrix3x2`)
- Use `D2D1_PRESENT_OPTIONS_IMMEDIATELY` to bypass vsync for benchmarking
