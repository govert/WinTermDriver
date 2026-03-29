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

---

## wintermdriver-rul.4: E2E CLI command test suite

Comprehensive E2E test suite in `crates/wtd-cli/tests/e2e_commands.rs` (27 tests) per §32.2.

**Test harness:**
- `TestHost` struct — starts `IpcServer` with unique pipe name, provides `connect()` → `IpcClient`
- `E2eHandler` implements `RequestHandler` — manages workspace instances via `Mutex<E2eState>`
- Handles all message types: OpenWorkspace, CloseWorkspace, ListWorkspaces/Instances/Panes/Sessions, Send, Capture, Scrollback, Follow, Inspect, FocusPane, RenamePane, InvokeAction, Keys, AttachWorkspace, RecreateWorkspace, SaveWorkspace
- Special "AMBIGUOUS" target name triggers `ErrorCode::TargetAmbiguous` response with candidates

**Test coverage:**
- Full lifecycle: open → list workspaces/instances/panes/sessions → send → capture → scrollback → inspect → close
- Additional commands: attach, recreate, save, focus, rename, action, keys
- Error exit codes per §22.9: target-not-found (2), workspace-not-found (2), ambiguous (3) — tested for send, capture, inspect, scrollback, follow, focus, rename, keys, open, close, list panes, list sessions
- JSON output structure validation for all response types
- Concurrent input from two simultaneous IPC clients with no crash or hang

**Pattern for testing CLI output layer:**
- Use `IpcClient::connect_to(pipe_name)` for test-specific pipes (bypasses auto-start)
- Use `output::format_response(&response, json_mode)` to validate formatting and exit codes
- Use `poll_capture_until()` with predicate for async ConPTY output

**Dev-dependencies added to `wtd-cli`:** `wtd-host`, `wtd-pty`

---

## wintermdriver-6en.2: Minimal rendering prototype

`wtd-ui` is now a lib+bin crate with Direct2D + DirectWrite rendering of `ScreenBuffer` content.

**New modules:**
- `wtd_ui::renderer` — `TerminalRenderer` struct that paints a `ScreenBuffer` to an HWND render target
- `wtd_ui::window` — Win32 window creation (`create_terminal_window`), message pump, repaint/resize signaling

**Key types:**
- `TerminalRenderer` — owns D2D factory, DW factory, HWND render target, 4 text formats (regular/bold/italic/bold+italic)
- `RendererConfig` — `font_family` (default "Cascadia Mono"), `font_size` (default 14.0)

**Public API:**
- `TerminalRenderer::new(hwnd, config) -> Result<Self>` — create D2D/DW resources
- `TerminalRenderer::paint(&self, screen: &ScreenBuffer) -> Result<()>` — render full screen buffer
- `TerminalRenderer::resize(&mut self, width, height) -> Result<()>` — handle window resize
- `TerminalRenderer::cell_size() -> (f32, f32)` — cell dimensions in pixels
- `color_to_rgb(color: &Color, is_foreground: bool) -> (u8, u8, u8)` — public color conversion
- `resolve_cell_colors(cell: &Cell) -> ((u8,u8,u8), (u8,u8,u8))` — handles INVERSE and DIM attributes

**Rendering approach:**
- Run-based batching: adjacent cells with same fg color and font style are drawn in a single `DrawText` call
- Background colors: non-default backgrounds drawn as filled rectangles, also run-batched
- Attributes: bold/italic select different `IDWriteTextFormat`; underline/strikethrough drawn as `DrawLine`; inverse swaps fg/bg; dim halves fg
- Cursor: semi-transparent filled rectangle (50% opacity block cursor)
- 256-color support: full 6×6×6 cube + 24 grayscale ramp + 16 ANSI palette

**Windows features added to wtd-ui:** `Foundation_Numerics`, `Win32_Graphics_Direct2D`, `Win32_Graphics_Direct2D_Common`, `Win32_Graphics_DirectWrite`, `Win32_Graphics_Dxgi_Common`, `Win32_Graphics_Gdi`, `Win32_System_LibraryLoader`, `Win32_UI_WindowsAndMessaging`

**windows-rs 0.58 API notes for rendering:**
- `ID2D1HwndRenderTarget` must be `.cast::<ID2D1RenderTarget>()` to access `DrawText`, `CreateSolidColorBrush`, etc.
- HWND parameters don't use `Option<HWND>` — pass `hwnd` directly (not `Some(hwnd)`)
- `DefWindowProcW` is a generic function — can't be used as `lpfnWndProc` directly; wrap in an `extern "system"` function
- Font family name must be null-terminated wide string via `PCWSTR`

**Test coverage:** 30 tests (12 unit in renderer, 18 integration in `tests/render_prototype.rs`) covering color mapping, attribute resolution, VT parsing, and end-to-end rendering pipeline

**wtd-ui dependencies added:** `wtd-pty` (for ScreenBuffer access)

---

## wintermdriver-psx.1: Tab strip component

`TabStrip` lives in `wtd_ui::tab_strip`. Renders a horizontal tab bar using Direct2D + DirectWrite.

**Key types:**
- `TabStrip` — owns tab list, layout zones, drag state, DW text formats; renders via `paint(&self, rt: &ID2D1RenderTarget)`
- `Tab` — `{ id: u64, name: String }`
- `TabAction` — enum: `SwitchTo(usize) | Close(usize) | Create | Reorder { from, to } | WindowClose`

**Public API:**
- `TabStrip::new(dw_factory) -> Result<Self>` — create with DirectWrite factory
- `add_tab(name) -> u64`, `close_tab(index) -> TabAction`, `set_active(index)`, `reorder(from, to)`
- `layout(available_width)` — recompute hit zones (call after tab/size changes)
- `on_mouse_down/up/move(x, y) -> Option<TabAction>` — mouse interaction
- `paint(rt) -> Result<()>` — render within an active BeginDraw session
- `window_title(workspace_name) -> String` — "workspace — tab" format
- `height() -> f32` — constant `TAB_STRIP_HEIGHT` (32px)

**TerminalRenderer compositing additions:**
- `begin_draw()`, `clear_background()`, `end_draw()` — split paint lifecycle
- `paint_screen(screen, y_offset)` — render ScreenBuffer at vertical offset
- `render_target()` / `dw_factory()` — access D2D/DW resources for shared use
- Original `paint(screen)` still works as convenience method

**Window module additions:**
- `MouseEvent { kind, x, y }` / `MouseEventKind` — captured from WM_LBUTTONDOWN/UP/MOUSEMOVE
- `drain_mouse_events()` — drain pending mouse event queue
- `set_window_title(hwnd, title)` — update window title text

**Design decisions:**
- Tab strip uses Segoe UI 12pt (not terminal monospace) for tab labels
- Text measurement via `IDWriteFactory::CreateTextLayout` for accurate tab widths
- Tab width clamped to [80, 200] px; overflow triggers scroll arrows (20px wide)
- Drag threshold 5px before reorder activates; drop position based on zone midpoints
- Colors: dark theme matching terminal bg (#1a1a26); accent #4ec9b0 for active indicator
- `close_tab` on last tab returns `WindowClose` (caller should destroy window)
- Tab strip paints within caller's BeginDraw/EndDraw session (no own draw cycle)

---

## wintermdriver-psx.2: Pane layout component

`PaneLayout` lives in `wtd_ui::pane_layout`. Renders pane borders, splitter bars, and focus indicators. Handles mouse interaction for splitter dragging and pane focusing.

**Key types:**
- `PaneLayout` — manages pane pixel rects, splitter detection, drag state, and painting
- `PixelRect` — `{ x, y, width, height }` in f32 pixel coordinates
- `PaneLayoutAction` — enum: `FocusPane(PaneId) | Resize { pane_id, direction, cells }`
- `CursorHint` — enum: `Arrow | ResizeHorizontal | ResizeVertical`

**Public API:**
- `PaneLayout::new(cell_width, cell_height)` — create with cell dimensions
- `update(&mut self, tree, origin_x, origin_y, cols, rows)` — recompute from LayoutTree
- `paint(&self, rt, focused_pane) -> Result<()>` — render borders, splitters, focus indicator
- `pane_pixel_rect(pane_id) -> Option<PixelRect>` — get pixel rect for a pane
- `pane_pixel_rects() -> &HashMap<PaneId, PixelRect>` — all pane rects
- `on_mouse_down/move/up(x, y) -> Option<PaneLayoutAction>` — mouse interaction
- `cursor_hint(x, y) -> CursorHint` — what cursor shape to show
- `is_dragging() -> bool` — whether a splitter drag is active
- `splitter_count() -> usize` — number of detected splitters

**Splitter detection:** Derived from pane cell rects — finds shared edges between adjacent panes (right edge == left edge for vertical splitters, bottom edge == top edge for horizontal). Segments at the same position are merged.

**Splitter drag:** On mouse down near a splitter, enters drag mode. Mouse move accumulates pixel delta, converts to cell increments, and emits `PaneLayoutAction::Resize` with `ResizeDirection`. Caller applies via `LayoutTree::resize_pane()` then calls `update()`.

**Compositing pattern:**
```rust
renderer.begin_draw();
renderer.clear_background();
tab_strip.paint(renderer.render_target())?;
// paint each pane's screen buffer at its pixel rect
pane_layout.paint(renderer.render_target(), &focused_pane)?;
renderer.end_draw()?;
```

**Design decisions:**
- Splitter thickness: 2px visual, 4px hit zone each side (8px total hit area)
- Focus border: 2px accent (#4ec9b0) matching tab strip accent color
- Unfocused pane border: 1px subtle (#2d2d3a)
- Splitter color: #3c3c4b normal, #5a5a6e on hover/drag
- Orientation naming: `Orientation::Horizontal` split has a vertical splitter line (divides left/right)
- `pane_before` in SplitterInfo is the pane to the left/above — used for resize direction
- Per-pane viewport clipping implemented in psx.3

---

## wintermdriver-psx.3: Per-pane viewport rendering with cursor shapes

`TerminalRenderer::paint_pane_viewport()` renders a `ScreenBuffer` clipped to a pane's pixel rectangle using `PushAxisAlignedClip`.

**New public types:**
- `CursorShape` — enum in `wtd-pty`: `Block | Underline | Bar` (added to `Cursor` struct)
- `TextSelection` — in `wtd-ui::renderer`: `{ start_row, start_col, end_row, end_col }` with `normalised()` and `contains()`

**New public API:**
- `TerminalRenderer::paint_pane_viewport(screen, x, y, width, height, selection)` — clipped viewport rendering
- `CursorShape` — exported from `wtd_pty` root

**VT sequence added:**
- DECSCUSR (`CSI Ps SP q`): sets cursor shape (0/1/2 → Block, 3/4 → Underline, 5/6 → Bar)

**Compositing pattern (updated):**
```rust
renderer.begin_draw();
renderer.clear_background();
tab_strip.paint(renderer.render_target())?;
for pane_id in layout_tree.panes() {
    let rect = pane_layout.pane_pixel_rect(&pane_id);
    renderer.paint_pane_viewport(&screen, rect.x, rect.y, rect.width, rect.height, None)?;
}
pane_layout.paint(renderer.render_target(), &focused_pane)?;
renderer.end_draw()?;
```

**Design decisions:**
- Block cursor: 50% opacity filled rectangle (text visible underneath)
- Underline cursor: 2px solid bar at cell bottom
- Bar cursor: 2px solid bar at cell left edge
- Selection: 50% opacity filled rectangles per row, color #3a6496
- Viewport clips to exact pixel rect; only visible rows/cols rendered for performance
- `paint_screen()` still available for non-pane use cases (also uses shaped cursor now)

---

## wintermdriver-psx.5: Failed/exited pane overlay rendering

`TerminalRenderer::paint_failed_pane()` in `wtd_ui::renderer` renders centered error/exit messages in pane viewports.

**New public API:**
- `TerminalRenderer::paint_failed_pane(message, x, y, width, height) -> Result<()>` — clipped viewport overlay
- `exited_pane_message(exit_code: u32) -> String` — formats "Session exited (code N)"
- `failed_pane_message(error: &str) -> String` — formats "Session failed: error"
- `RESTART_HINT: &str` — "Press Enter to restart  ·  Ctrl+B, r"

**Compositing pattern (updated):**
```rust
for pane_id in layout_tree.panes() {
    let rect = pane_layout.pane_pixel_rect(&pane_id);
    match pane_state {
        PaneState::Attached { session_id } => {
            renderer.paint_pane_viewport(&screen, rect.x, rect.y, rect.width, rect.height, None)?;
        }
        PaneState::Detached { error } => {
            renderer.paint_failed_pane(&error, rect.x, rect.y, rect.width, rect.height)?;
        }
    }
}
```

**Design decisions:**
- Background: dark fill (#1e1e2a), slightly distinct from terminal bg (#1a1a26)
- Message color: muted red (#cc7878) — noticeable but not alarming
- Hint color: dim gray (#8c8ca0) — secondary information
- Text centered both horizontally and vertically as a two-line block
- Uses DirectWrite `CreateTextLayout` for measurement, ensuring accurate centering regardless of text length
- Clipped via D2D `PushAxisAlignedClip` (same pattern as `paint_pane_viewport`)

---

## wintermdriver-psx.6: Status bar component

`StatusBar` lives in `wtd_ui::status_bar`. Renders a bottom status bar with workspace name, pane path, prefix indicator, and session state.

**Key types:**
- `StatusBar` — owns state fields and DirectWrite resources; renders via `paint(&self, rt: &ID2D1RenderTarget, y: f32)`
- `SessionStatus` — UI-side enum: `Creating | Running | Exited { exit_code } | Failed { error } | Restarting { attempt }`

**Public API:**
- `StatusBar::new(dw_factory) -> Result<Self>` — create with DirectWrite factory
- `layout(available_width)` — recompute on resize
- `set_workspace_name(name)`, `set_pane_path(path)`, `set_session_status(status)` — update displayed state
- `set_prefix_active(bool)`, `set_prefix_label(label)` — show/hide prefix chord indicator
- `paint(rt, y) -> Result<()>` — render within an active BeginDraw session at vertical offset `y`
- `height() -> f32` — constant `STATUS_BAR_HEIGHT` (24px)

**Compositing pattern (updated):**
```rust
renderer.begin_draw();
renderer.clear_background();
tab_strip.paint(renderer.render_target())?;
// ... pane viewports and pane layout ...
status_bar.paint(renderer.render_target(), window_height - status_bar.height())?;
renderer.end_draw()?;
```

**Design decisions:**
- Uses Segoe UI 11pt (matching tab strip font choice, slightly smaller)
- Workspace name: bold, accent color (#4ec9b0) — left-aligned
- Pane path: regular weight, muted text (#b4b4b4)
- Session state: right-aligned, color-coded (green=running, yellow=exited, red=failed, gray=creating/restarting)
- Prefix badge: accent bg (#4ec9b0) with dark text, rounded rect — only visible when `prefix_active` is true
- Vertical separators between segments
- Background matches tab strip (#1e1e28); top border line for visual separation
- `SessionStatus` is a UI-side enum (not importing from `wtd-host::session`) to avoid circular dependency

---

## wintermdriver-w0y.1: Keyboard input classifier

`InputClassifier` lives in `wtd_ui::input`. Parses binding configs and classifies keyboard events per §21.1 and §21.4.

**Key types:**
- `KeySpec` — parsed key specification: `{ modifiers: Modifiers, key: KeyName }` with `parse("Ctrl+Shift+T")` and `matches(&KeyEvent)`
- `KeyName` — enum: `Char('A'..'Z')`, `Digit(0..9)`, `F(1..12)`, `Enter`, `Tab`, `Escape`, `Space`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`, `Plus`, `Minus`, punctuation variants
- `Modifiers` — flags: `CTRL | ALT | SHIFT` with bitwise ops
- `KeyEvent` — `{ key: KeyName, modifiers: Modifiers, character: Option<char> }`
- `InputAction` — enum: `PrefixKey | ChordBinding(ActionReference) | SingleStrokeBinding(ActionReference) | RawInput(Vec<u8>)`
- `InputClassifier` — built from `BindingsDefinition`, classifies via `classify(&KeyEvent, prefix_active: bool)`
- `KeySpecError` — parse errors

**Public API:**
- `InputClassifier::from_bindings(bindings) -> Result<Self, KeySpecError>` — parse all binding strings
- `InputClassifier::classify(event, prefix_active) -> InputAction` — main classification
- `InputClassifier::prefix_key()`, `prefix_timeout_ms()`, `find_chord()`, `find_single_stroke()`
- `key_event_to_bytes(event) -> Vec<u8>` — raw terminal byte conversion (VT sequences, control codes, UTF-8)
- `vk_to_key_name(vk: u16) -> Option<KeyName>` — Win32 VK code mapping
- `current_modifiers() -> Modifiers` — read modifier state from `GetKeyState`
- `vk_to_char(vk, scan_code) -> Option<char>` — character prediction via `ToUnicode`

**Window module additions:**
- `drain_key_events() -> Vec<KeyEvent>` — keyboard event queue (same pattern as `drain_mouse_events()`)
- WM_KEYDOWN handler — captures all non-modifier keys
- WM_SYSKEYDOWN handler — captures Alt combos (Alt+F4 passed through to DefWindowProc)

**Classification precedence (§21.4):**
1. Prefix key wins over single-stroke for same key
2. Chords checked only when `prefix_active == true`
3. Single-strokes checked only when `prefix_active == false`
4. Unbound keys → `RawInput` with terminal bytes

**Chord matching:** Single-character chord keys (e.g. `%`, `o`) match on `event.character`; multi-character chord keys (e.g. `Up`, `F11`) match on `event.key` (KeyName)

**Design decisions:**
- Caller manages prefix state (§21.3 state machine) and passes `prefix_active` flag — classifier is stateless
- `Win32_UI_Input_KeyboardAndMouse` feature added to wtd-ui for `GetKeyState`, `ToUnicode`, `GetKeyboardState`
- Modifier-only key presses (VK_SHIFT, VK_CONTROL, VK_MENU) are filtered out in the wndproc
- `KeyName::Char` stores uppercase-normalized letters; matching is case-insensitive via normalization

---

## wintermdriver-w0y.2: Prefix chord state machine

`PrefixStateMachine` lives in `wtd_ui::prefix_state`. Wraps `InputClassifier` and manages prefix-active / idle transitions per §21.3 and §27.4.

**Key types:**
- `PrefixStateMachine` — stateful wrapper around `InputClassifier`; tracks active/idle state and timeout
- `PrefixOutput` — enum: `DispatchAction(ActionReference) | SendToSession(Vec<u8>) | Consumed`

**Public API:**
- `PrefixStateMachine::new(classifier) -> Self` — create from classifier, pre-computes prefix key bytes and label
- `process(&mut self, event) -> PrefixOutput` — classify event with state-aware transitions
- `check_timeout(&mut self) -> bool` — returns true if timeout elapsed and state reset to idle
- `is_prefix_active() -> bool` — for status bar indicator updates
- `prefix_label() -> &str` — display label (e.g. "Ctrl+B") for status bar
- `timeout() -> Duration` — configured timeout duration
- `classifier() -> &InputClassifier` — access inner classifier

**State transitions:**
- Idle + prefix key → PrefixActive → `Consumed`
- PrefixActive + chord → Idle → `DispatchAction(action)`
- PrefixActive + prefix again → Idle → `SendToSession(prefix_bytes)` (literal prefix)
- PrefixActive + Escape (no mods) → Idle → `Consumed`
- PrefixActive + unbound key → Idle → `SendToSession(prefix_bytes + key_bytes)`
- PrefixActive + timeout → Idle (via `check_timeout()`)

**Design decisions:**
- Double-prefix detected in state machine before calling classifier (classifier skips prefix check when `prefix_active=true`)
- Escape cancel requires plain Escape (no modifiers); Ctrl+Escape is treated as unbound key
- Prefix key bytes pre-computed at construction via `key_event_to_bytes` on a synthetic `KeyEvent`
- State machine is stateless w.r.t. time source — uses `std::time::Instant`; tests use short timeouts
- Caller responsible for updating status bar via `set_prefix_active()` / `set_prefix_label()` after each `process()` / `check_timeout()` call
- When no prefix is configured, state machine never enters active state; all keys pass through as raw input or single-stroke bindings

---

## wintermdriver-w0y.3: Mouse handling

`MouseHandler` lives in `wtd_ui::mouse_handler`. Central coordinator for all mouse interactions per §21.6.

**Key types:**
- `MouseHandler` — stateful handler tracking per-pane scroll offsets, selection drags, and button state
- `MouseOutput` — enum: `FocusPane(PaneId) | SelectionChanged(PaneId, Option<TextSelection>) | PaneResize(PaneLayoutAction) | SendToSession(PaneId, Vec<u8>) | ScrollPane(PaneId, i32) | PasteClipboard(PaneId) | Tab(TabAction) | SetCursor(CursorHint)`
- `MouseButton` — enum: `Left | Middle | Right | None | WheelUp | WheelDown`

**MouseMode tracking in ScreenBuffer (`wtd-pty`):**
- `MouseMode` enum: `None | Normal (1000) | ButtonEvent (1002) | AnyEvent (1003)`
- `sgr_mouse: bool` for mode 1006 (SGR extended format)
- Handled via DECSET/DECRST in `csi_dispatch`; reset on RIS
- Exported from `wtd_pty` root

**Public API:**
- `MouseHandler::new()` / `Default`
- `handle_event(event, tab_strip, pane_layout, ..., focused_pane, mouse_modes, cell_size) -> Vec<MouseOutput>`
- `scroll_offset(pane_id) -> i32`, `selection(pane_id) -> Option<TextSelection>`
- `clear_selection(pane_id)`, `reset_scroll(pane_id)`, `clamp_scroll(pane_id, max)`, `remove_pane(pane_id)`
- `encode_mouse_event(button, press, col, row, modifier_bits, sgr) -> Vec<u8>` — VT mouse sequence
- `encode_mouse_motion(button, col, row, modifier_bits, sgr) -> Vec<u8>` — VT motion sequence

**Window module changes:**
- `MouseEventKind` expanded: `LeftDown | LeftUp | RightDown | RightUp | MiddleDown | MiddleUp | Move | Wheel(i16)`
- Wndproc handles WM_RBUTTONDOWN/UP, WM_MBUTTONDOWN/UP, WM_MOUSEWHEEL
- Existing code using `Down`/`Up` updated to `LeftDown`/`LeftUp`

**Design decisions:**
- `focused_pane` passed by reference to `handle_event` (PaneId is not Copy)
- Right-click = paste when no mouse reporting; forwards VT right-click when mouse mode active
- Scroll wheel = scrollback navigation (3 lines per notch) or VT wheel events when mouse mode active
- Wheel targets pane under cursor, not focused pane (hover-scroll)
- Selection: drag starts on left-down in non-mouse-reporting pane; finalized on left-up; single-cell click clears
- SGR format (`\x1b[<...M/m`) preferred when mouse mode active; legacy X10 also supported
- Motion reporting: AnyEvent reports all motion; ButtonEvent only reports while button held

---

## wintermdriver-w0y.4: Copy and paste with VT stripping and bracketed paste

Clipboard operations live in `wtd_ui::clipboard`.

**Key types and functions:**
- `extract_selection_text(screen, selection) -> String` — extracts plain text from `TextSelection` range in `ScreenBuffer`, skipping wide-char continuations, trimming trailing whitespace per line
- `strip_vt(text) -> String` — strips ANSI/VT escape sequences (CSI, OSC, simple ESC) as safety measure
- `copy_to_clipboard(text) -> Result<(), ClipboardError>` — Win32 clipboard write (CF_UNICODETEXT via GlobalAlloc)
- `read_from_clipboard() -> Result<String, ClipboardError>` — Win32 clipboard read
- `wrap_bracketed_paste(data) -> Vec<u8>` — wraps bytes in `ESC[200~` / `ESC[201~` markers
- `prepare_paste(text, bracketed_paste_active) -> Vec<u8>` — encode text as bytes, optionally wrapped
- `ClipboardError` — `Open | SetData | NoText | Alloc | Lock`

**ScreenBuffer additions:**
- `bracketed_paste: bool` field — tracks DECSET 2004 mode
- `bracketed_paste() -> bool` accessor
- DECSET 2004 handled in `csi_dispatch` (set/reset); reset on RIS

**Windows features added to wtd-ui:** `Win32_System_DataExchange`, `Win32_System_Memory`

**Design decisions:**
- Text extraction from cells is inherently VT-stripped (cells store parsed characters, not raw VT); `strip_vt()` is an extra safety layer
- Win32 clipboard tests must run sequentially (single test function) — clipboard is a global resource and concurrent access from parallel test threads causes heap corruption
- `copy_to_clipboard` uses `HWND(null)` for clipboard association (works from any thread context)
- `prepare_paste` takes `bracketed_paste_active: bool` — caller checks `ScreenBuffer::bracketed_paste()` and passes the flag

---

## wintermdriver-psx.4: UI IPC client and host bridge

UI IPC connection lives in `wtd_ui::host_client` (async) and `wtd_ui::host_bridge` (sync bridge).

**Key types:**
- `UiIpcClient` — async IPC client that handshakes with `clientType: "ui"`; has `connect_and_handshake()`, `connect_to(pipe)`, `request()`, and `split()` → `(UiIpcReader, UiIpcWriter)`
- `UiIpcReader` / `UiIpcWriter` — split halves for concurrent read/write after attach
- `HostBridge` — sync bridge spawning a background thread with its own tokio `current_thread` runtime
- `HostEvent` — enum: `Connected { state }`, `SessionOutput { session_id, data: Vec<u8> }`, `SessionStateChanged`, `TitleChanged`, `LayoutChanged`, `WorkspaceStateChanged`, `Error`, `Disconnected`
- `HostCommand` — enum: `SessionInput { session_id, data: Vec<u8> }`, `PaneResize { pane_id, cols, rows }`, `InvokeAction { action, target_pane_id, args }`, `Disconnect`

**Public API:**
- `HostBridge::connect(workspace_name)` — connect to host (auto-start), handshake, attach
- `HostBridge::connect_to(pipe_name, workspace_name)` — connect to specific pipe (for tests)
- `HostBridge::try_recv() -> Option<HostEvent>` — non-blocking poll for host events
- `HostBridge::send(cmd)`, `send_input()`, `send_resize()`, `send_action()` — push commands
- `UiIpcClient::connect_to(pipe_name)` — async direct client (for tests bypassing bridge)

**Base64 encoding:** Built-in encode/decode (no external dependency). `SessionOutput.data` arrives base64-encoded; bridge decodes to `Vec<u8>`. `SessionInput.data` sent as base64-encoded bytes.

**Architecture:**
- Background thread runs `current_thread` tokio runtime
- `std::sync::mpsc` channels between UI thread and IPC thread
- Separate OS thread relays `std::sync::mpsc::Receiver<HostCommand>` → `tokio::sync::mpsc` (avoids blocking the async runtime)
- After attach, pipe is split; reader task pushes `HostEvent`s, writer drains command channel

**main.rs integration:**
- `--workspace <name>` or `WTD_WORKSPACE` env var triggers IPC mode; otherwise demo mode
- Event loop drains `HostEvent`s each frame: feeds SessionOutput to ScreenBuffers, updates status bar on state changes
- Keyboard input forwarded as `SessionInput` via bridge
- Pane resizes sent as `PaneResize` on window resize
- Status bar and tab strip integrated into paint pipeline

**Dependencies added to wtd-ui:** `serde_json` (runtime), `wtd-host` (dev-only, for tests)

**Design decisions:**
- Bridge tests need `#[tokio::test(flavor = "multi_thread")]` because `wait_for_event` polling uses `std::thread::sleep` which blocks a current-thread runtime
- `UiIpcWriter`/`UiIpcReader` are separate types (not reusing CLI's `IpcClient`) because UI needs split I/O for concurrent push receive + command send
- No `base64` crate dependency; hand-rolled encode/decode for the small amount of base64 needed
- `PaneSession` mapping (pane → session) is maintained in main.rs; populated from attach state (full population deferred to host handler bead)
- Layout/tab rebuilding from `HostEvent::LayoutChanged` is stubbed (logs notification); full rebuild deferred to downstream beads

---

## wintermdriver-w0y.5: Command palette component

`CommandPalette` lives in `wtd_ui::command_palette`. Modal overlay with fuzzy search over v1 action catalog.

**Key types:**
- `CommandPalette` — owns action list, filtered results, selection state, DirectWrite resources
- `PaletteEntry` — `{ name, description, keybinding: Option<String> }`
- `PaletteResult` — enum: `Dismissed | Action(ActionReference) | Consumed`

**Public API:**
- `CommandPalette::new(dw_factory, bindings) -> Result<Self>` — create with action catalog + keybinding hints
- `show()`, `hide()`, `toggle()`, `is_visible()`
- `on_key_event(event) -> PaletteResult` — handles Escape/Enter/Up/Down/Backspace/typing
- `on_click(x, y, window_w, window_h) -> Option<PaletteResult>` — click-to-select or click-outside-to-dismiss
- `paint(rt, window_w, window_h) -> Result<()>` — render overlay within BeginDraw/EndDraw
- `fuzzy_score(query, target) -> Option<i32>` — public subsequence fuzzy match with scoring
- `build_palette_entries(bindings) -> Vec<PaletteEntry>` — all 36 v1 actions with keybinding hints
- `build_keybinding_hints(bindings) -> HashMap<String, String>` — reverse map action→keybinding

**Integration in main.rs:**
- Toggled by `Ctrl+Shift+Space` (checked in keyboard event loop before normal key handling)
- When visible, all keyboard events routed to palette; mouse clicks handled for selection/dismissal
- Selected actions dispatched via `bridge.send_action()` (or handled locally for `toggle-command-palette`)
- Painted as overlay after all other components (within same BeginDraw/EndDraw)

**Design decisions:**
- Action catalog is UI-side (not imported from `wtd-host::action` which is dev-dep only) — 36 static entries matching §20.3
- Keybinding hints built from `BindingsDefinition` reverse lookup; single-stroke preferred over chord
- Fuzzy matching: character-by-character subsequence with consecutive and word-boundary bonuses
- `D2D_RECT_F` is the correct type in windows-rs 0.58 (not `D2D1_RECT_F`)
- Prefix state machine not yet integrated — Ctrl+Shift+Space checked directly in main loop
- Target selection for arg-requiring actions not yet implemented (actions dispatch with null args)

---

## wintermdriver-jxt.1: Gate — Host output renders in pane viewport

Integration test in `crates/wtd-ui/tests/gate_host_to_pane.rs` verifies the full UI rendering pipeline: IPC connect → SessionOutput push → ScreenBuffer → Direct2D pane viewport.

**Test structure:** `GateHandler` implements `RequestHandler` handling OpenWorkspace, Send, and Capture. Test connects as UI client via `UiIpcClient::connect_to()`, uses split reader/writer.

**Pipeline verified:**
1. UI connects to host IPC as `ClientType::Ui` (via `UiIpcClient`)
2. Opens workspace with real ConPTY session, polls Capture until `GATE_MARKER` appears
3. `server.broadcast_to_ui()` pushes `SessionOutput` with base64-encoded VT bytes
4. UI reader receives push notification, decodes base64, feeds to `ScreenBuffer::advance()`
5. Verifies `visible_text()` contains expected content
6. Creates test window + `TerminalRenderer`, calls `paint_pane_viewport()` with the populated ScreenBuffer
7. Also renders ANSI-colored content (green, bold red) to verify styled rendering

**Test patterns:**
- `UiIpcClient::connect_to(pipe)` then `.split()` for concurrent reader/writer (no HostBridge needed)
- Writer sends requests, reader reads responses in sequence (no interleaved pushes during request/response)
- `server.broadcast_to_ui()` only called after all request/response interactions complete
- Base64 encode/decode implemented locally in test (same algorithm as `host_bridge.rs`)
- Hidden test window via Win32 `CreateWindowExW` + `WS_OVERLAPPEDWINDOW` (not shown)

---

## wintermdriver-jxt.3: Gate — Status bar and failed pane display

Integration tests in `crates/wtd-ui/tests/gate_status_bar_failed_pane.rs` (8 tests) verify status bar rendering and failed/exited pane overlays per §24.5, §24.8. Closes Slice 3.

**Test coverage:**
1. Status bar shows workspace name, pane path, and running state
2. Status bar renders all `SessionStatus` variants (Creating, Running, Exited, Failed, Restarting)
3. Status bar renders prefix-active indicator (Ctrl+B badge)
4. Failed pane overlay with `failed_pane_message()` + `RESTART_HINT`
5. Exited pane overlay with `exited_pane_message()` for various exit codes
6. Full composited frame: tab strip + live pane + failed pane overlay + status bar
7. Composited frame with exited pane and status bar reflecting exited state
8. Status bar state transitions (creating → running → exited → restarting cycle)

**Compositing pattern for failed/exited panes:**
```rust
// For attached panes:
renderer.paint_pane_viewport(&screen, rect.x, rect.y, rect.width, rect.height, None)?;
// For detached/failed panes:
renderer.paint_failed_pane(&message, rect.x, rect.y, rect.width, rect.height)?;
// Status bar at bottom:
status_bar.paint(renderer.render_target(), window_height - status_bar.height())?;
```

---

## wintermdriver-17h.1: M4 Acceptance Gate

`crates/wtd-ui/tests/gate_m4_acceptance.rs` — dedicated M4 milestone acceptance test (§37.5). Validates all four M4 criteria: tabs display, split panes with live ConPTY content, tab switching, pane focus indicators, and status bar with workspace/pane info.

**Test structure:** `M4Handler` implements `RequestHandler` handling OpenWorkspace and Capture. Uses single-tab split workspace YAML (avoids PaneId collision across tabs). UI-side builds two-tab TabStrip to prove tab switching. Full compositing pipeline exercised: tab strip + split pane viewports + pane borders/focus + status bar.

**Known issue confirmed:** Multi-tab workspaces have PaneId collisions because `LayoutTree::new()` always starts PaneIds at 1 in each tab. `find_pane_by_name` fails for panes in the first tab when a second tab's panes overwrite the flat `panes` HashMap. Workaround: use single-tab workspace for IPC/ConPTY verification, build multi-tab UI on the rendering side.

---

## wintermdriver-nae.1: M6 Acceptance Gate

`crates/wtd-ui/tests/gate_m6_acceptance.rs` — dedicated M6 milestone acceptance test (§36, §37.5). Validates all 10 acceptance criteria with 10 separate test functions.

**Test structure:** `M6Handler` implements `RequestHandler` handling OpenWorkspace, CloseWorkspace, AttachWorkspace, RecreateWorkspace, ListPanes, ListSessions, Send, Capture, Scrollback, Inspect, InvokeAction, Keys. Uses `TestHost` harness with unique pipe names.

**Criteria covered:**
1. §36.1 `criterion_36_1_workspace_lifecycle` — IPC: open → interact → disconnect → attach → recreate
2. §36.2 `criterion_36_2_mixed_sessions` — `resolve_launch_spec` for powershell, wsl, ssh profiles
3. §36.3 `criterion_36_3_manual_interaction` — ScreenBuffer: typing, cursor movement, paste, selection, scrollback, alternate screen
4. §36.4 `criterion_36_4_controller_interaction` — IPC: list panes, send, keys, capture, scrollback, inspect, action
5. §36.5 `criterion_36_5_semantic_naming` — TargetPath parsing + resolve_target + ambiguous error with candidates
6. §36.6 `criterion_36_6_prefix_chords` — PrefixStateMachine: Ctrl+B,% → split-right, Ctrl+B," → split-down, Ctrl+B,o → focus-next, timeout
7. §36.7 `criterion_36_7_partial_failure` — 4-pane workspace with 1 bad executable; 3 running, 1 detached
8. §36.8 `criterion_36_8_local_security` — SID-based pipe name, PipeSecurity DACL creation
9. §36.9 `criterion_36_9_workspace_as_code` — .wtd/dev.yaml found via find_workspace_in
10. §36.10 `criterion_36_10_recreation_determinism` — same YAML → same tabs/panes/layout/save()

**Key pattern notes:**
- `AttachSnapshot` does not implement `Serialize`; build JSON manually in handler
- `new_for_test_multi` is `#[cfg(test)] pub(crate)` — not accessible from integration tests; use `WorkspaceInstance::open` with YAML instead
- SID format varies (S-1-5- for NT Authority, S-1-12- for AzureAD); check for `S-1-` not `S-1-5-`

---

## wintermdriver-nae.2: M6 Performance Gate

`crates/wtd-host/tests/gate_m6_performance.rs` — validates all §30 performance targets with 6 tests.

**Tests:**
1. `capture_response_under_100ms` — IPC capture round-trip latency (20 iterations, median)
2. `keystroke_to_echo_under_50ms` — Direct session: write_input → process_pending_output → screen buffer (single char echo)
3. `workspace_open_5_sessions_under_2s` — 5-pane YAML → WorkspaceInstance::open via IPC
4. `screen_buffer_throughput_100mbps` — ScreenBuffer::advance() with colored VT content
5. `screen_buffer_frame_advance_under_16ms` — Single full-screen VT repaint (50 iterations, median)
6. `concurrent_sessions_20_plus` — 24 long-running cmd.exe sessions, startup command polling, concurrent write verification

**Design decisions:**
- Debug vs release targets: throughput (5 MB/s debug / 100 MB/s release), keystroke echo (100ms debug / 50ms release) — `cfg!(debug_assertions)` gates
- Keystroke test uses direct Session access (not IPC polling) to avoid artificial polling latency
- Concurrent test uses long-running `cmd.exe` (no `/c`) with startup commands; polls for READY markers
- Throughput test scales data volume: 10 MB debug, 100 MB release
