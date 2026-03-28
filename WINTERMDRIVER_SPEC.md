# WinTermDriver — Engineering Specification

**Document status:** Engineering specification — ready for work-item breakdown
**Version:** 2.0
**Audience:** Architecture, implementation, QA, product/design, advanced users
**Primary platform:** Windows 10 1809+ (build 17763+)
**Language:** Rust (stable toolchain, MSVC target)
**PTY backend:** Windows ConPTY

---

## Table of Contents

1. [Purpose and Mission](#1-purpose-and-mission)
2. [Product Statement](#2-product-statement)
3. [Design Goals](#3-design-goals)
4. [Non-Goals](#4-non-goals)
5. [Primary Use Cases](#5-primary-use-cases)
6. [Platform Requirements](#6-platform-requirements)
7. [Technology Decisions](#7-technology-decisions)
8. [Architectural Overview](#8-architectural-overview)
9. [Object Model](#9-object-model)
10. [Workspace Definition Format](#10-workspace-definition-format)
11. [Global Configuration](#11-global-configuration)
12. [Workspace File Discovery](#12-workspace-file-discovery)
13. [IPC Architecture](#13-ipc-architecture)
14. [PTY and Terminal Emulation](#14-pty-and-terminal-emulation)
15. [Output Buffer Architecture](#15-output-buffer-architecture)
16. [Host Process Lifecycle](#16-host-process-lifecycle)
17. [Session Model](#17-session-model)
18. [Layout Model](#18-layout-model)
19. [Naming and Addressing](#19-naming-and-addressing)
20. [Action System](#20-action-system)
21. [Input Model](#21-input-model)
22. [Controller Model (CLI)](#22-controller-model-cli)
23. [Output and Inspection Model](#23-output-and-inspection-model)
24. [UI Architecture](#24-ui-architecture)
25. [Profile System](#25-profile-system)
26. [Workspace Lifecycle Operations](#26-workspace-lifecycle-operations)
27. [State Machines](#27-state-machines)
28. [Security Model](#28-security-model)
29. [Error Handling](#29-error-handling)
30. [Performance Requirements](#30-performance-requirements)
31. [Logging and Diagnostics](#31-logging-and-diagnostics)
32. [Testability Requirements](#32-testability-requirements)
33. [Versioning and Migration](#33-versioning-and-migration)
34. [WT Codebase Reuse Map](#34-wt-codebase-reuse-map)
35. [Required Invariants](#35-required-invariants)
36. [Acceptance Criteria](#36-acceptance-criteria)
37. [Bead-Ready Work Breakdown](#37-bead-ready-work-breakdown)

---

## 1. Purpose and Mission

### 1.1 Purpose

WinTermDriver is a Windows-native terminal workspace manager for defining, launching, viewing, and controlling collections of console sessions arranged into windows, tabs, and panes.

WinTermDriver combines three ideas:

1. A real terminal UI with tabs and split panes that feel close to Windows Terminal.
2. A persistent workspace model that can save and recreate named layouts and session launch definitions.
3. A controller plane that can drive any pane or session programmatically without breaking ordinary interactive use.

The tool is intended for development, operations, scripting, remote SSH work, and multi-session console orchestration on Windows.

### 1.2 Mission Statement

WinTermDriver shall let a user define, open, recreate, interact with, and automate named terminal workspaces on Windows using a WT-like visual model and an NTM/tmux-like control model, while keeping every pane fully usable as a normal terminal in its own right.

### 1.3 Durability Model

WinTermDriver does not attempt to serialize full live console application state. Its durability model is based on workspace definition and controlled recreation, not process checkpoint/restore. The Workspace Definition file is the primary durable artifact. Workspace Instances are transient and host-managed.

---

## 2. Product Statement

WinTermDriver provides:

- Named workspaces with human-editable definition files.
- Named sessions and panes addressable by semantic paths.
- Tabbed windows with split panes in a WT-like visual model.
- Re-openable and recreatable terminal workspaces from saved definitions.
- Keyboard, mouse, and command-palette interaction.
- Tmux-like chorded keybindings with a configurable prefix key.
- A CLI controller (`wtd`) that can send text, send keys, invoke host actions, capture output, and inspect terminal state.
- Uniform support for local shells (PowerShell, cmd), WSL shells, SSH sessions, and custom commands.

---

## 3. Design Goals

### 3.1 Real terminal first

Every visible pane shall function as a normal interactive terminal by keyboard and mouse, without dependence on the controller CLI. No pane requires controller activity to be usable.

### 3.2 Controller is additive

The controller shall augment terminal sessions, not replace direct interactive use. A user who never touches the CLI shall have a fully functional terminal application.

### 3.3 Semantic naming

Users shall address meaningful roles such as `dev/server` and `ops/prod-logs`, not only positional pane numbers. Semantic names are the primary addressing mechanism; internal opaque IDs are secondary.

### 3.4 Workspace durability

Saved workspaces shall be durable and easily recreatable. The Workspace Definition file is the primary durable artifact. Opening a workspace from its definition shall always produce the same logical structure and session roles.

### 3.5 Layout and runtime separation

The saved layout and launch definition (Workspace Definition) shall be strictly distinct from the transient runtime realization (Workspace Instance). The definition is version-controllable; the instance is disposable.

### 3.6 Windows-native behavior

The tool shall feel natural on Windows: native window management, standard clipboard, familiar keyboard conventions, per-user security model using Windows ACLs.

### 3.7 WT-like UX

The visual and interaction model shall resemble Windows Terminal where it helps familiarity: windows, tabs, panes, resize, focus movement, selection, copy/paste, profiles, and settings-driven customization.

### 3.8 Tmux-like commandability

The action model and keyboard system shall support prefix chords such as `Ctrl+B,%`. The product shall provide tmux-like defaults for pane/tab management.

### 3.9 Uniform action model

Keyboard bindings, chorded bindings, command palette entries, and controller CLI actions shall all dispatch through one logical action system with identical semantics regardless of invocation source.

### 3.10 Safe local control

The control plane shall be local-only and per-user by default, enforced by Windows per-user named pipe ACLs.

---

## 4. Non-Goals

### 4.1 Full process snapshot/restore

WinTermDriver shall not checkpoint arbitrary process memory or restore hidden internal state of running console applications.

### 4.2 Remote multi-user orchestration

WinTermDriver shall not expose remote control APIs across the network by default. Remote control is out of scope unless explicitly and separately designed in a future version.

### 4.3 Cross-user session sharing

WinTermDriver shall not allow one Windows user to attach to another user's sessions.

### 4.4 Full Windows Terminal compatibility

WinTermDriver shall not attempt exact schema, code, or CLI compatibility with Windows Terminal. WT is a design reference, not a compatibility target.

### 4.5 Full tmux protocol compatibility

WinTermDriver shall not emulate tmux protocol or guarantee compatibility with tmux scripts. The prefix chord system is inspired by tmux, not wire-compatible with it.

### 4.6 Rich plugin ecosystem in v1

Extension points may exist, but a general plugin platform is not part of the v1 design.

### 4.7 Durable scrollback as a core promise

Scrollback exists while sessions live and is exportable via `capture`/`scrollback` commands, but long-term archival of all pane output is not a defining capability. Scrollback buffers are destroyed when sessions end.

### 4.8 Session mirroring in v1

A single session displayed simultaneously in multiple panes (tmux mirror mode) is not supported in v1. The cardinality is strictly one session to at most one pane.

### 4.9 Headless detached sessions in v1

Closing a pane kills its session in v1. There is no concept of a headless detached session that continues running without an associated pane. Sessions exist only within the context of a workspace instance's pane structure.

---

## 5. Primary Use Cases

### 5.1 Development cockpit

A user opens a workspace with panes such as `dev/editor`, `dev/server`, `dev/tests`, and `dev/logs`. The user interacts manually in some panes and drives others via the controller. Example: `wtd send dev/server "dotnet watch run"` starts the server while the user works in the editor pane.

### 5.2 Mixed local and remote workspace

A workspace contains local PowerShell, WSL Ubuntu shell, SSH session to staging, and SSH log tail on production. All appear within one coherent window/tab/pane structure. The controller can drive any of them by semantic name.

### 5.3 Quick recreation

A user closes the UI, later reopens the workspace with `wtd open dev`, and gets the correct set of sessions and layout recreated. The workspace definition is the source of truth; recreation is deterministic.

### 5.4 Automation-assisted work

A script sends commands to a named pane via `wtd send`, captures output via `wtd capture`, waits for expected text, then triggers another action. Example: a CI-local script that starts a server, waits for "Listening on port 5000", then runs integration tests.

### 5.5 Tmux-like navigation

A user presses `Ctrl+B,%` to split a pane right, `Ctrl+B,"` to split down, `Ctrl+B,o` to cycle focus, and `Ctrl+B,c` to create a new tab — all without leaving the keyboard.

### 5.6 Workspace-as-code

A workspace is defined in a `.wtd/dev.yaml` file in a project repository. Any team member clones the repo, runs `wtd open dev`, and gets an identical workspace layout with the correct sessions, profiles, and directories.

---

## 6. Platform Requirements

### 6.1 Minimum operating system

Windows 10 version 1809 (build 17763). This is the minimum version that supports the ConPTY API (`CreatePseudoConsole`).

### 6.2 Recommended operating system

Windows 10 21H2 or later, or Windows 11. Later versions have improved ConPTY behavior, better Virtual Terminal passthrough, and more reliable resize handling.

### 6.3 Runtime dependencies

No .NET, JRE, or Python runtime dependency. WinTermDriver ships as native Rust binaries with no managed runtime.

### 6.4 Optional dependencies

- WSL installed for WSL profile support. The `wsl.exe` command must be available on PATH.
- SSH client (`ssh.exe`) for SSH profile support. OpenSSH ships as an optional feature in Windows 10 1809+ and is installed by default in recent Windows 11 builds. WinTermDriver does not bundle its own SSH client.

### 6.5 Build toolchain

Rust stable toolchain targeting `x86_64-pc-windows-msvc`. The MSVC toolchain is required for `windows-rs` crate bindings to Win32 APIs.

---

## 7. Technology Decisions

### 7.1 Language and toolchain

Rust (stable), MSVC target. All three binaries (`wtd-host`, `wtd-ui`, `wtd`) are native Windows executables.

### 7.2 PTY backend

Windows ConPTY via the `windows-rs` crate. The host process calls `CreatePseudoConsole`, owns the PTY handles, and manages the child process lifecycle. See §14 for full PTY architecture.

### 7.3 Async runtime

Tokio. Used for IPC server/client, PTY I/O multiplexing, output forwarding, and timer management (prefix chord timeout, restart backoff). All three processes use Tokio where async I/O is needed.

### 7.4 IPC transport

Windows named pipes via `windows-rs` and `tokio`. The named pipe path is `\\.\pipe\wtd-{user-SID}`. See §13 for full IPC architecture.

### 7.5 VT parsing

The `vte` crate for VT sequence parsing in the host-side screen buffer. The host maintains a per-session terminal state machine for `capture`/`scrollback`/`follow` support. See §14.4 for the dual-path output model.

### 7.6 Screen buffer

A custom screen buffer implementation modeled on `alacritty_terminal`'s grid/storage approach: a fixed-size active screen (rows × cols) plus a scrollback ring buffer of configurable depth. The `alacritty_terminal` crate's architecture is the primary design reference for this component.

### 7.7 Serialization

`serde` with `serde_yaml` for workspace definitions and global settings. `serde_json` for IPC message framing. The workspace definition format uses YAML for human editability; the IPC protocol uses JSON for debuggability in v1, with a path to a binary format (MessagePack or CBOR) in a future version if performance requires it.

### 7.8 CLI parsing

`clap` crate for the `wtd` CLI argument parsing. Clap provides structured subcommands, typed arguments, shell completion generation, and help text.

### 7.9 UI rendering — evaluation required

The UI rendering technology is a critical decision with the following candidates, listed in recommended evaluation order:

1. **wezterm's rendering components** (`wezterm-gui`, `wezterm-term`). Rust-native, GPU-accelerated (OpenGL), proven terminal renderer. Highest potential for code reuse. Evaluation criteria: can the renderer be extracted and embedded in a custom window/tab/pane management framework?

2. **Win32 + DirectWrite custom renderer.** Most Windows-native. Uses `windows-rs` bindings for `IDWriteFactory`, `ID2D1RenderTarget`, and Direct2D. High implementation effort, maximum native integration. WT's DirectWrite/DX renderer is the design reference.

3. **WebView2 + xterm.js.** Fastest path to a working terminal renderer. Embeds Chromium-based WebView2 (ships with Windows 11, installable on Windows 10) and uses xterm.js for terminal rendering. Adds ~100MB runtime dependency. Latency between Rust host and JS renderer adds complexity.

The spec does not prescribe a choice. The evaluation shall be completed as a time-boxed spike (see §37) before UI implementation begins. The evaluation deliverable is a written decision document with benchmarks.

### 7.10 Window and tab management

Regardless of rendering technology, the window/tab/pane chrome (title bars, tab strips, splitter bars, status bar) is built using Win32 window management via `windows-rs`. Tab strip and pane splitter rendering may use the same rendering technology as the terminal content, or may use standard Win32 controls.

### 7.11 WT codebase role

The Windows Terminal codebase is used as:

- **Algorithm reference** for ConPTY interaction patterns (`ConPtyConnection` class), VT parser correctness, layout tree splitting and resize distribution logic, and settings schema design.
- **Behavioral reference** for expected terminal behavior: how WT handles resize, selection, clipboard, scrollback, and TUI applications.
- **Not used via FFI.** No C++ interop. All referenced algorithms are re-implemented in Rust.

See §34 for the detailed reuse map.

---

## 8. Architectural Overview

WinTermDriver consists of three principal processes with well-defined responsibilities and communication boundaries.

### 8.1 `wtd-host` — Host Process

A per-user background process. Singleton per user (enforced by named pipe ownership). Responsible for:

- Loading, validating, and saving workspace definition files.
- Managing the lifecycle of workspace instances (create, attach, recreate, close).
- Creating and owning ConPTY instances and child processes.
- Maintaining per-session screen buffers (active screen + scrollback ring).
- Forwarding raw VT output bytes to attached UI clients.
- Processing input from UI clients (keystrokes, paste, resize) and controller clients (send, keys).
- Executing actions dispatched from any source (UI keybinding, UI chord, UI palette, CLI).
- Serving the IPC named pipe for UI and CLI connections.
- Managing session restart policy and backoff.
- Tracking workspace instance state, session state, and pane-to-session attachments.
- Providing metadata and inspection data to clients.

### 8.2 `wtd-ui` — UI Process

A graphical Windows application. Multiple instances may run simultaneously (one per window, or one process managing multiple windows — implementation choice). Responsible for:

- Creating and managing native Windows windows.
- Rendering the tab strip, pane splitter bars, and status bar.
- Rendering terminal content in each pane viewport using VT bytes received from the host.
- Handling keyboard input: classifying keystrokes as raw terminal input, single-stroke host bindings, or prefix chord sequences, and dispatching accordingly.
- Handling mouse input: selection, pane focus, pane resize, tab switching.
- Implementing the command palette UI.
- Implementing copy/paste and clipboard integration.
- Reporting pane resize events to the host.
- Connecting to `wtd-host` via the named pipe IPC channel.

### 8.3 `wtd` — Controller CLI

A command-line tool. Each invocation is a short-lived process that connects to the host, issues a command, receives a response, and exits. Long-running commands (`follow`) maintain the connection until interrupted. Responsible for:

- Parsing command-line arguments into structured commands.
- Connecting to `wtd-host` via the named pipe.
- Sending command messages and receiving responses.
- Formatting output for the terminal (text or JSON).
- Auto-starting `wtd-host` if not running (see §16.2).
- Returning structured exit codes (see §22.9).

### 8.4 Separation of concerns

| Concern | Owner | Boundary |
|---------|-------|----------|
| PTY creation and ownership | `wtd-host` | Only the host calls `CreatePseudoConsole` and owns PTY handles |
| Session lifecycle | `wtd-host` | Only the host creates, restarts, and kills sessions |
| Workspace instance state | `wtd-host` | Only the host manages the runtime registry |
| Screen buffer and scrollback | `wtd-host` | Only the host maintains parsed screen state |
| Action dispatch | `wtd-host` | All actions from all sources resolve in the host |
| Window/tab/pane rendering | `wtd-ui` | Only the UI creates windows and renders content |
| Keyboard/mouse input classification | `wtd-ui` | The UI classifies input and routes it (raw input → host session, action → host action system) |
| Command parsing | `wtd` | Only the CLI parses command-line arguments |

---

## 9. Object Model

This section defines the complete object model. All behavioral sections reference these definitions. The object model has two layers: the durable definition layer (persisted to YAML files) and the runtime instance layer (transient, host-managed).

### 9.1 Durable Layer — Workspace Definition

A Workspace Definition is a durable, saved description of intended structure and launch behavior. It is the primary version-controllable artifact.

#### 9.1.1 WorkspaceDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `version` | integer | yes | Schema version. Currently `1`. |
| `name` | string | yes | Unique workspace name. Valid characters: `[a-zA-Z0-9_-]`. Max 64 characters. |
| `description` | string | no | Human-readable description. |
| `defaults` | DefaultsDefinition | no | Default values inherited by sessions. |
| `profiles` | map\<string, ProfileDefinition\> | no | Named reusable launch recipes. Keys follow the same naming rules as workspace names. |
| `bindings` | BindingsDefinition | no | Workspace-local keybinding overrides. |
| `windows` | list\<WindowDefinition\> | no | Window definitions. If omitted, all tabs are placed in a single default window. |
| `tabs` | list\<TabDefinition\> | no | Shorthand: if `windows` is omitted, `tabs` defines tabs for the implicit default window. Mutually exclusive with `windows`. |

#### 9.1.2 DefaultsDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `profile` | string | no | Name of the default profile for sessions that don't specify one. |
| `restartPolicy` | RestartPolicy | no | Default restart policy. Default: `never`. |
| `scrollbackLines` | integer | no | Default scrollback buffer size in lines. Default: `10000`. |
| `cwd` | string | no | Default startup directory. |
| `env` | map\<string, string or null\> | no | Default environment variable overrides. `null` value removes a variable. |

#### 9.1.3 RestartPolicy (enum)

| Value | On process exit (code = 0) | On process exit (code ≠ 0) | On workspace open (instance exists) |
|-------|---------------------------|---------------------------|-------------------------------------|
| `never` | Show exit code in pane. Pane enters Exited state. Wait for manual restart. | Same as exit code 0. | Attach to existing instance. |
| `on-failure` | Show exit code in pane. Pane enters Exited state. | Restart session with backoff. | Attach to existing instance. |
| `always` | Restart session with backoff. | Restart session with backoff. | Attach to existing instance. |

Restart backoff: delay starts at 500ms, doubles on each consecutive restart, capped at 30 seconds. Resets to 500ms after a session runs for more than 60 seconds without exiting.

#### 9.1.4 WindowDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | no | Window name. If omitted, auto-generated as `window-1`, `window-2`, etc. Valid characters: `[a-zA-Z0-9_-]`. |
| `tabs` | list\<TabDefinition\> | yes | Tabs in this window, ordered as defined. |

Window placement (position, size, monitor) is runtime state managed by the OS window manager. The workspace definition does not specify window geometry.

#### 9.1.5 TabDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tab name. Must be unique within the parent window. Valid characters: `[a-zA-Z0-9_-]`. Max 64 characters. |
| `layout` | PaneNode | yes | Root of the pane layout tree. |
| `focus` | string | no | Semantic name of the pane that receives initial focus. If omitted, the first pane in tree order receives focus. |

Tab order within a window is preserved as defined. Tabs created at runtime (via `new-tab` action) are appended after the last tab.

#### 9.1.6 PaneNode (tagged union)

A PaneNode is either a leaf pane or a split container. The layout tree is recursive.

**Leaf pane:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"pane"` | yes | Discriminator. |
| `name` | string | yes | Semantic pane name. Must be unique within the workspace. Valid characters: `[a-zA-Z0-9_-]`. Max 64 characters. |
| `session` | SessionLaunchDefinition | no | Session launch definition. If omitted, uses the default profile with no startup command. |

**Split container:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"split"` | yes | Discriminator. |
| `orientation` | `"horizontal"` or `"vertical"` | yes | `horizontal` = children arranged left-to-right. `vertical` = children arranged top-to-bottom. |
| `ratio` | float | no | Size ratio of the first child relative to the total. Range 0.1–0.9. Default: `0.5`. |
| `children` | list\<PaneNode\> | yes | Exactly 2 children. Each child is either a pane or a nested split. |

Note: split containers always have exactly 2 children. A three-way split is represented as a nested binary tree. The `ratio` applies to the first child; the second child gets `1 - ratio`.

#### 9.1.7 SessionLaunchDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `profile` | string | no | Name of a profile to use. If omitted, uses the workspace default profile. |
| `cwd` | string | no | Startup working directory. Overrides profile and workspace defaults. Supports environment variable expansion (e.g., `%USERPROFILE%\src`). |
| `env` | map\<string, string or null\> | no | Environment variable overrides. Merged on top of profile env. `null` removes a variable. |
| `startupCommand` | string | no | Command to execute after the shell starts. Sent as text input to the session after launch, followed by a newline. |
| `title` | string | no | Initial pane/session title. Overrides profile title template. |
| `args` | list\<string\> | no | Additional arguments appended to the profile's executable command. |

#### 9.1.8 ProfileDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | ProfileType | yes | Profile type discriminator. |
| `executable` | string | no | Path to executable. Required for `custom` type. Optional for others (has built-in defaults). |
| `args` | list\<string\> | no | Default arguments. |
| `cwd` | string | no | Default startup directory. |
| `env` | map\<string, string or null\> | no | Default environment variable overrides. |
| `title` | string | no | Title template. May contain `{name}`, `{profile}`, `{cwd}` placeholders. |
| `distribution` | string | no | WSL distribution name. Required for `wsl` type. |
| `host` | string | no | SSH target host. Required for `ssh` type. |
| `user` | string | no | SSH user. Used by `ssh` type. |
| `port` | integer | no | SSH port. Used by `ssh` type. Default: `22`. |
| `identityFile` | string | no | Path to SSH identity file. Used by `ssh` type. Passed as `-i` to `ssh.exe`. |
| `useAgent` | boolean | no | SSH agent usage. Used by `ssh` type. Default: `true`. If `false`, passes `-o IdentitiesOnly=yes` to `ssh.exe`. |
| `remoteCommand` | string | no | SSH remote command. Used by `ssh` type. If set, passed as the command argument to `ssh.exe`. |
| `scrollbackLines` | integer | no | Scrollback buffer size in lines. Overrides workspace default. |

#### 9.1.9 ProfileType (enum)

| Value | Default executable | Description |
|-------|--------------------|-------------|
| `powershell` | `pwsh.exe`, falling back to `powershell.exe` | PowerShell session. |
| `cmd` | `cmd.exe` | Classic Command Prompt. |
| `wsl` | `wsl.exe -d {distribution}` | WSL distribution shell. |
| `ssh` | `ssh.exe {user}@{host} -p {port}` | SSH client session. |
| `custom` | (must be specified) | Arbitrary executable. |

#### 9.1.10 BindingsDefinition

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `prefix` | KeySpec | no | Prefix key for chord sequences. Default: `Ctrl+B`. |
| `prefixTimeout` | integer | no | Milliseconds to wait for chord completion after prefix. Default: `2000`. |
| `chords` | map\<string, ActionReference\> | no | Map of chord key to action name. Keys are single characters or key names. |
| `keys` | map\<KeySpec, ActionReference\> | no | Map of single-stroke keybinding to action name. |

#### 9.1.11 ActionReference

Either a simple action name string (e.g., `"split-right"`) or a structured object for actions with arguments:

```yaml
# Simple:
"%": split-right

# With arguments:
"%":
  action: split-right
  args:
    profile: ubuntu
```

### 9.2 Runtime Layer — Workspace Instance

The runtime layer exists only in `wtd-host` memory. It is never persisted. It is disposable — recreating a workspace instance from its definition is always possible.

#### 9.2.1 WorkspaceInstance

| Field | Type | Description |
|-------|------|-------------|
| `instanceId` | UUID | Unique runtime identifier. Generated on creation. |
| `definitionName` | string | Name of the WorkspaceDefinition this instance was created from. |
| `definitionPath` | PathBuf | File path of the definition file used. |
| `state` | WorkspaceInstanceState | Current lifecycle state (see §27.2). |
| `windows` | list\<WindowRuntime\> | Runtime window objects. |
| `sessions` | map\<SessionId, SessionRuntime\> | All sessions in this instance, keyed by session ID. |
| `uiAttachments` | set\<ConnectionId\> | Set of connected UI client IPC connections. |
| `createdAt` | DateTime | Instance creation timestamp. |

#### 9.2.2 WindowRuntime

| Field | Type | Description |
|-------|------|-------------|
| `windowId` | UUID | Runtime identifier. |
| `name` | string | Window name (from definition or auto-generated). |
| `tabs` | list\<TabRuntime\> | Ordered list of tabs. |

#### 9.2.3 TabRuntime

| Field | Type | Description |
|-------|------|-------------|
| `tabId` | UUID | Runtime identifier. |
| `name` | string | Tab name. |
| `layoutRoot` | PaneRuntimeNode | Root of the runtime pane layout tree. |
| `focusedPaneId` | PaneId | ID of the currently focused pane within this tab. |
| `isZoomed` | boolean | Whether a pane is currently zoomed to fill the tab. |
| `zoomedPaneId` | PaneId or null | If zoomed, the ID of the zoomed pane. |

#### 9.2.4 PaneRuntimeNode (tagged union)

**Leaf pane:**

| Field | Type | Description |
|-------|------|-------------|
| `paneId` | UUID | Runtime identifier. |
| `name` | string | Semantic pane name. |
| `state` | PaneState | Current pane state (see §27.4). |
| `attachedSessionId` | SessionId or null | ID of the attached session. Null if detached. |

**Split container:**

| Field | Type | Description |
|-------|------|-------------|
| `splitId` | UUID | Runtime identifier for the split node. |
| `orientation` | horizontal or vertical | Split direction. |
| `ratio` | float | Current size ratio (may differ from definition if user has resized). |
| `children` | (PaneRuntimeNode, PaneRuntimeNode) | Exactly two children. |

#### 9.2.5 SessionRuntime

| Field | Type | Description |
|-------|------|-------------|
| `sessionId` | UUID | Runtime identifier. |
| `name` | string | Semantic name (typically matches the pane name that launched it). |
| `state` | SessionState | Current lifecycle state (see §27.1). |
| `profileType` | ProfileType | Type of profile used to launch. |
| `executable` | string | Resolved executable path. |
| `args` | list\<string\> | Resolved arguments. |
| `cwd` | string | Resolved startup directory. |
| `env` | map\<string, string\> | Resolved environment (fully merged). |
| `startupCommand` | string or null | Startup command, if any. |
| `title` | string | Current session title (may be updated by VT escape sequences). |
| `ptyHandle` | ConPTY handle | Owned ConPTY pseudo-console handle. |
| `processHandle` | HANDLE | Owned child process handle. |
| `processId` | u32 | Child process PID. |
| `screenBuffer` | ScreenBuffer | Active screen + scrollback ring buffer. |
| `exitCode` | i32 or null | Last exit code, if session has exited. |
| `restartPolicy` | RestartPolicy | Effective restart policy for this session. |
| `restartCount` | u32 | Number of consecutive restarts (for backoff calculation). |
| `lastStartTime` | DateTime | When the session was last started. |
| `cols` | u16 | Current terminal width in columns. |
| `rows` | u16 | Current terminal height in rows. |

#### 9.2.6 Entity Relationships

```
WorkspaceInstance
  ├── WindowRuntime[] (ordered)
  │     └── TabRuntime[] (ordered)
  │           └── PaneRuntimeNode (tree)
  │                 └── Leaf: PaneRuntime (name, state, attachedSessionId)
  │                 └── Split: (orientation, ratio, child1, child2)
  └── SessionRuntime[] (by sessionId)
        └── ScreenBuffer (active screen + scrollback ring)

Pane → Session: 1:1 or 1:0 (detached/exited pane has no session)
Session → Pane: 1:1 or 1:0 (orphan sessions are not supported in v1)
```

---

## 10. Workspace Definition Format

### 10.1 Serialization

YAML is the canonical format. JSON is also accepted (any valid JSON is valid YAML). The file extension determines the parser: `.yaml`, `.yml`, or `.json`.

### 10.2 Schema version

The `version` field is required and must be the integer `1`. Files with a higher version number than the tool supports shall be rejected with a clear error message naming the expected and found versions.

### 10.3 Validation rules

On load, the host shall validate:

1. `version` is present and equals `1`.
2. `name` is present and matches `[a-zA-Z0-9_-]{1,64}`.
3. Either `windows` or `tabs` is present (not both).
4. All tab names are unique within their parent window.
5. All pane names are unique within the entire workspace.
6. All `profile` references in session definitions refer to profiles defined in the `profiles` section or built-in profile types.
7. All `focus` references point to existing pane names within the tab.
8. Split nodes have exactly 2 children.
9. `ratio` values are in range 0.1–0.9.
10. No circular references exist (not possible with the tree structure, but validated defensively).

Validation errors shall report the file path, line number (if available from the YAML parser), field path, and a human-readable error message.

### 10.4 Complete example

```yaml
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
    identityFile: "%USERPROFILE%\\.ssh\\prod_key"
    useAgent: true

bindings:
  prefix: Ctrl+B
  prefixTimeout: 2000
  chords:
    "%": split-right
    "\"": split-down
    o: focus-next-pane
    c: new-tab
    ",": rename-pane
    x: close-pane
    z: zoom-pane
    n: next-tab
    p: prev-tab
    d: close-workspace
  keys:
    Ctrl+Shift+T: new-tab
    Ctrl+Shift+W: close-pane

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
                    cwd: "C:\\src\\app"
                    startupCommand: dotnet watch run
                - type: pane
                  name: tests
                  session:
                    profile: pwsh
                    cwd: "C:\\src\\app"
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
                startupCommand: journalctl -f -u myservice
```

### 10.5 Minimal example

```yaml
version: 1
name: quick
tabs:
  - name: main
    layout:
      type: pane
      name: shell
```

This creates a single window, single tab, single pane workspace using the built-in default PowerShell profile.

---

## 11. Global Configuration

### 11.1 Settings file

Global user settings are stored in `%APPDATA%\WinTermDriver\settings.yaml`.

### 11.2 Settings schema

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `defaultProfile` | string | `"powershell"` | Built-in profile type or name of a globally defined profile to use when no profile is specified. |
| `profiles` | map\<string, ProfileDefinition\> | (empty) | Globally defined profiles available to all workspaces. |
| `bindings` | BindingsDefinition | (see §11.3) | Global keybinding configuration. |
| `scrollbackLines` | integer | `10000` | Default scrollback buffer size. |
| `restartPolicy` | RestartPolicy | `never` | Default restart policy. |
| `font` | FontConfig | (see §11.4) | Terminal font configuration. |
| `theme` | ThemeConfig | (see §11.5) | Color theme configuration. |
| `copyOnSelect` | boolean | `false` | If true, selecting text automatically copies to clipboard. |
| `confirmClose` | boolean | `true` | If true, closing a window with running sessions shows a confirmation. |
| `hostIdleShutdown` | integer or null | `null` | Seconds of idle time (no workspace instances) before host auto-shuts down. Null means never. |
| `logLevel` | string | `"info"` | Logging level: `trace`, `debug`, `info`, `warn`, `error`. |

### 11.3 Default keybindings

Default single-stroke bindings (overridable):

| Key | Action |
|-----|--------|
| `Ctrl+Shift+T` | `new-tab` |
| `Ctrl+Shift+W` | `close-pane` |
| `Ctrl+Shift+Space` | `toggle-command-palette` |
| `Ctrl+Shift+C` | `copy` |
| `Ctrl+Shift+V` | `paste` |
| `Ctrl+Tab` | `next-tab` |
| `Ctrl+Shift+Tab` | `prev-tab` |
| `Alt+Shift+D` | `split-right` |
| `Alt+Shift+Minus` | `split-down` |
| `F11` | `toggle-fullscreen` |

Default prefix: `Ctrl+B` with timeout 2000ms.

Default chords:

| Chord | Action |
|-------|--------|
| `%` | `split-right` |
| `"` | `split-down` |
| `o` | `focus-next-pane` |
| `c` | `new-tab` |
| `,` | `rename-pane` |
| `x` | `close-pane` |
| `z` | `zoom-pane` |
| `n` | `next-tab` |
| `p` | `prev-tab` |
| `d` | `close-workspace` |
| `Up` | `focus-pane-up` |
| `Down` | `focus-pane-down` |
| `Left` | `focus-pane-left` |
| `Right` | `focus-pane-right` |
| `[` | `enter-scrollback-mode` |

### 11.4 Font configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `family` | string | `"Cascadia Mono"` | Font family name. Falls back to `"Consolas"` if not found. |
| `size` | float | `12.0` | Font size in points. |
| `weight` | string | `"normal"` | Font weight: `light`, `normal`, `bold`. |

### 11.5 Theme configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | `"default"` | Theme name. |
| `foreground` | color | `"#CCCCCC"` | Default foreground color. |
| `background` | color | `"#0C0C0C"` | Default background color. |
| `cursorColor` | color | `"#FFFFFF"` | Cursor color. |
| `selectionBackground` | color | `"#FFFFFF"` | Selection highlight color. |
| `palette` | list\<color\> | (xterm-256color standard palette) | 16-color ANSI palette, indexed 0–15. |

Color values are 6-digit hex strings with `#` prefix.

### 11.6 Settings merge precedence

Settings are resolved in this order (later overrides earlier):

1. **Built-in defaults** (hardcoded in the binary).
2. **Global settings file** (`%APPDATA%\WinTermDriver\settings.yaml`).
3. **Workspace definition** (`bindings`, `defaults`, per-profile settings).
4. **Per-session overrides** (in session launch definitions).

For keybindings specifically:

- Workspace `bindings.chords` override global `bindings.chords` for the same chord key.
- Workspace `bindings.keys` override global `bindings.keys` for the same key spec.
- Workspace `bindings.prefix` overrides global `bindings.prefix`.
- Bindings not overridden in the workspace use the global value.

---

## 12. Workspace File Discovery

### 12.1 Search order

When `wtd open <name>` is invoked, the host searches for the workspace definition in the following order, using the first match:

1. **Explicit path:** If `--file <path>` is specified, use that path exactly. Error if not found.
2. **Current working directory:** Look for `.wtd/<name>.yaml`, `.wtd/<name>.yml`, or `.wtd/<name>.json` in the CWD of the `wtd` process.
3. **User workspace directory:** Look for `<name>.yaml`, `<name>.yml`, or `<name>.json` in `%APPDATA%\WinTermDriver\workspaces\`.

### 12.2 Save location

`wtd save <name>` writes to the user workspace directory (`%APPDATA%\WinTermDriver\workspaces\<name>.yaml`) by default. Use `--file <path>` to save to a specific location.

### 12.3 Directory creation

The host creates `%APPDATA%\WinTermDriver\` and `%APPDATA%\WinTermDriver\workspaces\` on first use if they do not exist.

### 12.4 Workspace listing

`wtd list workspaces` lists all workspace definitions found by scanning:

1. The current directory's `.wtd/` folder (marked as `[local]`).
2. The user workspace directory (marked as `[user]`).

If a workspace name appears in both locations, both are listed with their source labels.

---

## 13. IPC Architecture

### 13.1 Transport

Windows named pipes. The pipe name is `\\.\pipe\wtd-{SID}` where `{SID}` is the current user's Windows Security Identifier (SID). Using the SID rather than the username ensures uniqueness even for domain users and avoids encoding issues with usernames containing special characters.

### 13.2 Security

The named pipe is created with a `SECURITY_ATTRIBUTES` structure that grants access only to the creating user's SID. This is the primary access control mechanism. The host verifies the connecting client's token SID matches the pipe owner's SID using `GetNamedPipeClientProcessId` and token inspection.

### 13.3 Connection model

The named pipe server in `wtd-host` accepts multiple concurrent connections. Each connection is an independent bidirectional channel. A connecting client may be:

- A **UI client** — long-lived connection, receives output streams, sends input events.
- A **CLI client** — short-lived connection (request/response) or medium-lived (for `follow`).

The client identifies its role in the initial handshake message.

### 13.4 Framing

Messages are length-prefixed binary frames:

```
[4 bytes: payload length as u32 little-endian][payload bytes]
```

The payload is a UTF-8 JSON string representing a message envelope. The 4-byte length prefix enables efficient buffered reads without scanning for delimiters.

Maximum message size: 16 MiB. Messages exceeding this limit are rejected with an error.

### 13.5 Message envelope

Every message has the following envelope structure:

```json
{
  "id": "uuid-string",
  "type": "message-type-name",
  "payload": { ... }
}
```

- `id`: UUID generated by the sender. Used to correlate requests with responses.
- `type`: String discriminator for the message type.
- `payload`: Type-specific payload object.

### 13.6 Handshake

On connection, the client sends a `Handshake` message:

```json
{
  "id": "...",
  "type": "Handshake",
  "payload": {
    "clientType": "ui" | "cli",
    "clientVersion": "1.0.0",
    "protocolVersion": 1
  }
}
```

The host responds with:

```json
{
  "id": "...",
  "type": "HandshakeAck",
  "payload": {
    "hostVersion": "1.0.0",
    "protocolVersion": 1
  }
}
```

If protocol versions are incompatible, the host responds with an `Error` and closes the connection.

### 13.7 Request/Response pattern

CLI commands and UI actions follow request/response:

```json
// Request (client → host)
{
  "id": "req-1",
  "type": "ListPanes",
  "payload": { "workspace": "dev" }
}

// Response (host → client)
{
  "id": "req-1",
  "type": "ListPanesResult",
  "payload": {
    "panes": [
      { "name": "editor", "tab": "backend", "sessionState": "running" },
      { "name": "server", "tab": "backend", "sessionState": "running" }
    ]
  }
}
```

The response `id` matches the request `id`.

### 13.8 Error responses

```json
{
  "id": "req-1",
  "type": "Error",
  "payload": {
    "code": "target-not-found",
    "message": "No pane named 'editor' in workspace 'dev'",
    "candidates": ["dev/backend/editor", "dev/ops/prod-shell"]
  }
}
```

Error codes are defined strings (not integers) for readability. Required codes:

| Code | Description |
|------|-------------|
| `target-not-found` | The addressed target does not exist. |
| `target-ambiguous` | The address matches multiple targets. `candidates` field lists matches. |
| `workspace-not-found` | No workspace definition or instance with the given name. |
| `workspace-already-exists` | An instance already exists (when `open` requires a new instance). |
| `invalid-action` | The action name is not recognized. |
| `invalid-argument` | An argument to a command or action is malformed. |
| `session-failed` | A session operation failed (launch, restart, etc.). |
| `protocol-error` | Malformed message or protocol violation. |
| `internal-error` | Unexpected host error. |

### 13.9 Streaming pattern (UI output)

After a UI client attaches to a workspace, the host pushes output events without the client requesting each one:

```json
{
  "id": "evt-1",
  "type": "SessionOutput",
  "payload": {
    "sessionId": "...",
    "data": "<base64-encoded VT bytes>"
  }
}
```

Output data is base64-encoded because raw VT bytes are not valid UTF-8 in general. The UI decodes and feeds the bytes to its VT renderer.

### 13.10 Streaming pattern (CLI follow)

A `Follow` request creates a subscription:

```json
// Request
{ "id": "req-1", "type": "Follow", "payload": { "target": "dev/logs", "raw": false } }

// Stream of events (host → client, ongoing)
{ "id": "req-1", "type": "FollowData", "payload": { "text": "2024-01-15 New connection from..." } }
{ "id": "req-1", "type": "FollowData", "payload": { "text": "2024-01-15 Request processed..." } }

// Terminated by:
{ "id": "req-1", "type": "FollowEnd", "payload": { "reason": "session-exited", "exitCode": 0 } }
```

When `raw` is `false`, the host strips VT escape sequences and sends decoded text lines. When `raw` is `true`, the host sends base64-encoded raw VT bytes.

The client cancels a follow subscription by closing the connection or sending a `CancelFollow` message with the matching `id`.

### 13.11 UI client attachment

When a UI client connects and sends a handshake, it then sends an `AttachWorkspace` message:

```json
{
  "id": "req-1",
  "type": "AttachWorkspace",
  "payload": { "workspace": "dev" }
}
```

The host responds with the full workspace instance state (all windows, tabs, panes, session states, current titles) as an `AttachWorkspaceResult`. The host then begins pushing `SessionOutput` events for all sessions in the instance.

### 13.12 UI input forwarding

The UI sends input events to the host:

```json
// Keyboard input to a session
{
  "id": "...",
  "type": "SessionInput",
  "payload": {
    "sessionId": "...",
    "data": "<base64-encoded input bytes>"
  }
}

// Pane resize
{
  "id": "...",
  "type": "PaneResize",
  "payload": {
    "paneId": "...",
    "cols": 120,
    "rows": 30
  }
}

// Action invocation (from keybinding, chord, or palette)
{
  "id": "...",
  "type": "InvokeAction",
  "payload": {
    "action": "split-right",
    "targetPaneId": "...",
    "args": {}
  }
}
```

### 13.13 Host-to-UI notifications

The host pushes state change notifications to all attached UI clients:

```json
// Session state change
{
  "type": "SessionStateChanged",
  "payload": {
    "sessionId": "...",
    "newState": "exited",
    "exitCode": 0
  }
}

// Title change (from VT escape sequence)
{
  "type": "TitleChanged",
  "payload": {
    "sessionId": "...",
    "title": "vim - main.rs"
  }
}

// Layout change (pane added, removed, or resized by action)
{
  "type": "LayoutChanged",
  "payload": {
    "workspace": "dev",
    "window": "main",
    "tab": "backend",
    "layout": { ... } // full PaneRuntimeNode tree for the tab
  }
}
```

### 13.14 Complete message type catalog

**Client → Host (requests):**

| Type | Payload fields | Response type |
|------|---------------|---------------|
| `Handshake` | clientType, clientVersion, protocolVersion | `HandshakeAck` |
| `OpenWorkspace` | name, file (optional), recreate (bool) | `OpenWorkspaceResult` |
| `AttachWorkspace` | workspace | `AttachWorkspaceResult` |
| `CloseWorkspace` | workspace, kill (bool) | `Ok` |
| `RecreateWorkspace` | workspace | `RecreateWorkspaceResult` |
| `SaveWorkspace` | workspace, file (optional) | `Ok` |
| `ListWorkspaces` | (none) | `ListWorkspacesResult` |
| `ListInstances` | (none) | `ListInstancesResult` |
| `ListPanes` | workspace | `ListPanesResult` |
| `ListSessions` | workspace | `ListSessionsResult` |
| `Send` | target, text, newline (bool, default true) | `Ok` |
| `Keys` | target, keys (list of KeySpec) | `Ok` |
| `Capture` | target | `CaptureResult` |
| `Scrollback` | target, tail (int) | `ScrollbackResult` |
| `Follow` | target, raw (bool) | Stream of `FollowData`, terminated by `FollowEnd` |
| `CancelFollow` | id (of original Follow request) | `Ok` |
| `Inspect` | target | `InspectResult` |
| `InvokeAction` | action, targetPaneId (optional), args | `Ok` or action-specific result |
| `SessionInput` | sessionId, data (base64) | (no response — fire and forget for performance) |
| `PaneResize` | paneId, cols, rows | `Ok` |
| `FocusPane` | paneId | `Ok` |
| `RenamePane` | paneId, newName | `Ok` |

**Host → Client (responses and events):**

| Type | Context | Description |
|------|---------|-------------|
| `HandshakeAck` | Response to Handshake | Protocol accepted |
| `Ok` | Response to commands | Generic success |
| `Error` | Response to any request | Error with code, message, optional candidates |
| `OpenWorkspaceResult` | Response to OpenWorkspace | Instance ID, full state snapshot |
| `AttachWorkspaceResult` | Response to AttachWorkspace | Full workspace instance state |
| `RecreateWorkspaceResult` | Response to RecreateWorkspace | New instance ID, full state snapshot |
| `ListWorkspacesResult` | Response | List of workspace names with sources |
| `ListInstancesResult` | Response | List of running instances |
| `ListPanesResult` | Response | List of panes with metadata |
| `ListSessionsResult` | Response | List of sessions with metadata |
| `CaptureResult` | Response | Visible screen text as string |
| `ScrollbackResult` | Response | Scrollback lines as list of strings |
| `InspectResult` | Response | Full metadata for a pane/session |
| `FollowData` | Stream event | Output text or raw bytes |
| `FollowEnd` | Stream event | Follow terminated |
| `SessionOutput` | Push to UI | Raw VT output bytes (base64) |
| `SessionStateChanged` | Push to UI | Session state transition |
| `TitleChanged` | Push to UI | Session title update |
| `LayoutChanged` | Push to UI | Tab layout tree changed |
| `WorkspaceStateChanged` | Push to UI | Workspace instance state transition |

---

## 14. PTY and Terminal Emulation

### 14.1 ConPTY lifecycle

The host creates a ConPTY for each session using the following sequence:

1. Create pipes: `CreatePipe` for input (host writes, child reads) and output (child writes, host reads).
2. Create pseudo-console: `CreatePseudoConsole(size, inputRead, outputWrite, flags)` where `size` is the initial terminal dimensions (cols × rows) from the pane geometry.
3. Configure startup info: Initialize `STARTUPINFOEX` with the ConPTY handle via `UpdateProcThreadAttribute(PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, ...)`.
4. Create child process: `CreateProcess` with the resolved executable, arguments, working directory, and environment. The child inherits the ConPTY.
5. Close pipe ends: Close the child-side pipe ends (`inputRead`, `outputWrite`) in the host process after `CreateProcess` succeeds.
6. Start I/O loops: Spawn async tasks to read from the output pipe and write to the input pipe.

The host retains ownership of:
- The ConPTY handle (for resize and close).
- The `inputWrite` pipe handle (for sending input).
- The `outputRead` pipe handle (for reading output).
- The child process handle (for monitoring exit).

### 14.2 ConPTY resize

When a pane is resized in the UI:

1. The UI calculates the new pane dimensions in character cells (cols × rows) based on the pane's pixel dimensions, the font's cell size, and any padding.
2. The UI sends a `PaneResize` message to the host with the new cols and rows.
3. The host calls `ResizePseudoConsole(ptyHandle, newSize)`.
4. The host updates the session's screen buffer dimensions.
5. ConPTY delivers a `WINDOW_BUFFER_SIZE_EVENT` to the child process.

### 14.3 ConPTY signal model

ConPTY does not support POSIX signals. Control characters are delivered as input bytes:

| Key | Byte sent | Effect |
|-----|-----------|--------|
| `Ctrl+C` | `0x03` | Typically triggers SIGINT-like behavior in the child shell |
| `Ctrl+D` | `0x04` | EOF in many shells |
| `Ctrl+Z` | `0x1A` | Suspend in some shells (less meaningful on Windows) |
| `Ctrl+\` | `0x1C` | SIGQUIT equivalent (rarely used on Windows) |

The `wtd keys <target> Ctrl+C` command works by injecting `0x03` into the PTY input pipe.

### 14.4 Dual-path output model

PTY output follows two parallel paths:

**Path 1: Raw VT forwarding to UI.** The host reads raw bytes from the PTY output pipe and forwards them to all attached UI clients via `SessionOutput` IPC messages. The UI feeds these bytes to its own VT renderer for high-fidelity display. This path prioritizes rendering latency.

**Path 2: Host-side screen buffer.** The host also feeds the same raw bytes to a VT parser (`vte` crate) connected to a screen buffer state machine. The screen buffer maintains:

- An active screen grid: `cols × rows` cells, each cell containing a character, foreground color, background color, and attributes (bold, italic, underline, etc.).
- Alternate screen buffer support (for `\e[?1049h`/`\e[?1049l` — fullscreen TUI applications).
- A scrollback ring buffer of configurable depth.
- Cursor position.
- Current title (from `\e]0;title\a` or `\e]2;title\a` sequences).

This host-side screen buffer serves the `capture`, `scrollback`, `follow`, and `inspect` commands. It enables these commands to work even when no UI is attached.

### 14.5 VT compliance level

WinTermDriver shall support the following VT capabilities:

| Feature | Support level |
|---------|--------------|
| VT100/VT220 basic sequences | Full |
| xterm extended sequences | Common subset (colors, cursor, screen management) |
| 8-color ANSI (SGR 30–37, 40–47) | Full |
| 16-color (bright variants, SGR 90–97, 100–107) | Full |
| 256-color (SGR 38;5;n / 48;5;n) | Full |
| True color 24-bit RGB (SGR 38;2;r;g;b / 48;2;r;g;b) | Full |
| UTF-8 text | Full |
| Alternate screen buffer (`\e[?1049h`/`l`) | Full |
| Bracketed paste (`\e[?2004h`/`l`) | Full |
| Mouse reporting — SGR format (`\e[?1006h`) | Full |
| Mouse reporting — button events (`\e[?1000h`) | Full |
| Mouse reporting — any event (`\e[?1003h`) | Full |
| Window title (`\e]0;...\a`, `\e]2;...\a`) | Full |
| Cursor shape (`\e[n q`) | Full |
| Cursor visibility (`\e[?25h`/`l`) | Full |
| OSC hyperlinks (`\e]8;...;...\a`) | Deferred to post-v1 |

The `TERM` environment variable shall be set to `xterm-256color` for local sessions (PowerShell, cmd, WSL). SSH sessions inherit the remote's terminfo negotiation.

### 14.6 Process tree management

The host creates a Windows Job Object per workspace instance. All child processes created for that workspace instance are added to the job object. The job object is configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, ensuring that if the host process terminates unexpectedly, all child processes in the workspace are terminated.

---

## 15. Output Buffer Architecture

### 15.1 Screen buffer structure

Each session has one ScreenBuffer managed by the host, consisting of:

- **Active screen:** A grid of `cols × rows` cells representing the current terminal screen content. Updated by the VT parser as output arrives.
- **Alternate screen:** A second grid of `cols × rows` cells, activated by alternate screen buffer escape sequences. When active, the primary screen is preserved and restored when the alternate screen is deactivated.
- **Scrollback ring:** A ring buffer of rows that have scrolled off the top of the active screen. Configurable depth (default 10,000 lines, overridable per-profile, per-session, or in global settings). Oldest rows are discarded when the ring is full.
- **Cursor:** Current row, column, visibility, and shape.

### 15.2 Cell structure

Each cell contains:

| Field | Type | Description |
|-------|------|-------------|
| `character` | char (Unicode scalar) | The displayed character. Space for empty cells. |
| `fg` | Color | Foreground color (ANSI index or RGB). |
| `bg` | Color | Background color (ANSI index or RGB). |
| `attrs` | bitflags | Bold, dim, italic, underline, blink, inverse, strikethrough, hidden. |
| `wide` | bool | True if this cell is the left half of a wide (CJK) character. |
| `wideContinuation` | bool | True if this cell is the right half of a wide character (display only, no character). |

### 15.3 Scrollback sizing

| Setting level | Field | Default |
|---------------|-------|---------|
| Global settings | `scrollbackLines` | 10,000 |
| Workspace defaults | `defaults.scrollbackLines` | (inherits global) |
| Profile | `scrollbackLines` | (inherits workspace default) |
| Session override | (not directly overridable; uses effective profile value) | — |

Memory budget: at 120 columns, each row is approximately 120 × 16 bytes = ~2 KB. 10,000 rows ≈ 20 MB per session. For 20 concurrent sessions, this is ~400 MB. This is acceptable for the target workload.

### 15.4 Buffer lifecycle

- Created when a session is created.
- Updated continuously as the VT parser processes output bytes.
- Destroyed when the session is destroyed (on pane close or workspace teardown).
- Not persisted across workspace recreation.

---

## 16. Host Process Lifecycle

### 16.1 Startup

The host starts on-demand. The first `wtd` CLI command or `wtd-ui` launch checks for a running host:

1. Attempt to connect to the named pipe `\\.\pipe\wtd-{SID}`.
2. If the connection succeeds and the handshake completes, the host is running. Proceed.
3. If the connection fails (pipe does not exist or is not connectable), launch `wtd-host` as a detached background process:
   - Use `CreateProcess` with `DETACHED_PROCESS` flag (no console window).
   - The host writes its PID to `%APPDATA%\WinTermDriver\host.pid` on startup.
   - The host creates the named pipe and begins accepting connections.
4. Retry the connection with a brief polling loop (50ms intervals, up to 5 seconds).
5. If the host does not become available within the timeout, report a startup failure.

### 16.2 Auto-start behavior

The `wtd` CLI and `wtd-ui` both auto-start the host if it is not running. No manual host startup is required. Users never need to run `wtd-host` directly unless debugging.

### 16.3 Shutdown

The host shuts down when:

- `wtd host stop` is explicitly invoked. The host closes all workspace instances (terminating all sessions) and exits.
- The configurable idle shutdown timeout expires (`hostIdleShutdown` in global settings). Idle means no workspace instances exist and no clients are connected. Default: never (null).
- The host receives a termination signal (WM_CLOSE, CTRL_CLOSE_EVENT).

On shutdown:

1. The host sends `WorkspaceStateChanged { state: "closing" }` to all connected UI clients.
2. The host closes all sessions (closes ConPTY handles, which terminates child processes via the job object).
3. The host closes the named pipe.
4. The host removes the PID file.
5. The host exits.

### 16.4 Crash recovery

If `wtd-host` crashes:

- The Windows Job Object (configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) terminates all child processes.
- The named pipe handle is released by the OS.
- On next `wtd` or `wtd-ui` invocation, the auto-start logic detects the dead pipe, detects the stale PID file (PID no longer running), removes the stale PID file, and starts a fresh host.
- All previous workspace instances are lost. The user re-opens workspaces from definitions.
- No orphaned state accumulates.

### 16.5 Single-instance enforcement

The named pipe name (`\\.\pipe\wtd-{SID}`) serves as the single-instance mutex. If the pipe exists and is connectable, the host is running. A second `wtd-host` launch attempt detects the existing pipe and exits immediately with a message.

---

## 17. Session Model

### 17.1 Session creation

When a workspace instance is created, the host creates one session per pane in the layout tree:

1. Resolve the effective profile: session's `profile` → workspace `defaults.profile` → global `defaultProfile` → built-in `powershell`.
2. Resolve the executable and arguments from the profile type (see §25).
3. Resolve the working directory: session `cwd` → profile `cwd` → workspace `defaults.cwd` → host process CWD.
4. Resolve the environment: host process env + profile `env` overrides + workspace `defaults.env` overrides + session `env` overrides. Variables set to `null` are removed.
5. Expand environment variables in `cwd` (e.g., `%USERPROFILE%` → `C:\Users\alice`).
6. Create the ConPTY and child process (see §14.1).
7. Set the session state to `Running`.
8. If a `startupCommand` is defined, wait 100ms (to allow the shell to initialize), then send the startup command text followed by a newline (`\r\n`) to the PTY input pipe.

### 17.2 Session identity

Each session has:

- A `sessionId` (UUID) — runtime identifier, unique within the host's lifetime.
- A `name` (string) — typically the same as the pane name that created it. Used for display and debugging.

### 17.3 Session-to-pane cardinality (v1)

Strictly 1:1. One session is attached to at most one pane. One pane is attached to at most one session. When a pane is created, it creates a session. When a pane is closed, its session is terminated. There are no headless sessions.

### 17.4 Startup command delivery

The `startupCommand` field is a convenience mechanism. It is delivered by injecting text into the PTY input pipe as if the user typed it. This means:

- The command is visible in the session's terminal output (the shell echoes it).
- The command is subject to the shell's input processing (aliases, PATH, etc.).
- The startup command is not a hidden internal mechanism — it is literally simulated typing.

The 100ms delay before sending is a heuristic to allow the shell prompt to appear. If the shell takes longer to initialize (e.g., complex profile scripts), the startup command may arrive before the prompt. This is a known limitation; the spec does not require more sophisticated ready-detection in v1.

### 17.5 Session failure

If `CreateProcess` fails:

1. The session state is set to `Failed` with an error message (e.g., "Executable not found: pwsh.exe").
2. The pane state is set to `Detached` with the error information.
3. The UI displays the error in the pane area.
4. The user can invoke `restart-session` to retry.
5. Other sessions and panes in the workspace are not affected.

### 17.6 Session exit

When the child process exits:

1. The host detects the exit via the process handle becoming signaled (`WaitForSingleObject` or async equivalent).
2. The host reads the exit code via `GetExitCodeProcess`.
3. The host reads any remaining output from the PTY output pipe (drain to EOF).
4. The session state transitions to `Exited(exitCode)`.
5. The host evaluates the restart policy (see §9.1.3):
   - `never`: Pane enters `Detached` state showing exit code. No restart.
   - `on-failure` with non-zero exit: Schedule restart with backoff.
   - `always`: Schedule restart with backoff.
6. The host notifies attached UI clients via `SessionStateChanged`.

### 17.7 Session restart

When a restart is triggered (by policy or by user action):

1. The host creates a new ConPTY and child process using the same session definition.
2. The old session state (screen buffer, scrollback) is cleared.
3. The session state transitions back to `Running`.
4. The pane state transitions back to `Attached`.
5. The host notifies UI clients.

### 17.8 Restart backoff

Restart delay schedule:

| Restart count | Delay |
|---------------|-------|
| 1 | 500ms |
| 2 | 1,000ms |
| 3 | 2,000ms |
| 4 | 4,000ms |
| 5 | 8,000ms |
| 6 | 16,000ms |
| 7+ | 30,000ms (cap) |

The restart count resets to 0 if the session runs for more than 60 seconds without exiting. This prevents restart storms for processes that crash immediately but allows quick recovery for transient failures.

---

## 18. Layout Model

### 18.1 Layout tree structure

Each tab contains one pane layout tree. The tree is a strict binary tree:

- **Leaf nodes** are panes.
- **Internal nodes** are splits with exactly two children and an orientation (horizontal or vertical).

A single pane is a tree with one leaf node and no splits.

### 18.2 Split orientation semantics

| Orientation | Visual result | First child | Second child |
|-------------|---------------|-------------|--------------|
| `horizontal` | Children arranged left-to-right | Left pane/subtree | Right pane/subtree |
| `vertical` | Children arranged top-to-bottom | Top pane/subtree | Bottom pane/subtree |

### 18.3 Size allocation

Each split node has a `ratio` (float, 0.1–0.9) that determines the fraction of the split's total dimension allocated to the first child. The second child gets `1.0 - ratio`.

When a split's pixel dimensions change (window resize, parent split ratio change), the host recalculates child dimensions and sends resize events to affected panes.

### 18.4 Pane minimum size

Each pane has a minimum size of 2 columns × 1 row (character cells). If a split resize would make a pane smaller than this minimum, the resize is clamped.

### 18.5 Split operations

**Split right** (from a leaf pane):

1. Replace the leaf pane P with a new split node (orientation: horizontal, ratio: 0.5).
2. The first child of the split is the original pane P.
3. The second child is a new pane with a new session.

**Split down** (from a leaf pane):

1. Replace the leaf pane P with a new split node (orientation: vertical, ratio: 0.5).
2. The first child is the original pane P.
3. The second child is a new pane with a new session.

The new pane uses the default profile unless specified otherwise. The new pane receives an auto-generated name (e.g., `pane-1`, `pane-2`) unless renamed by the user.

### 18.6 Close pane operations

When a pane is closed:

1. The session attached to the pane is terminated.
2. The pane's leaf node is removed from the tree.
3. The parent split node is replaced by the sibling subtree (the split is collapsed).
4. If the closed pane was the last pane in the tab, the tab is closed.
5. If the closed tab was the last tab in the window, the window is closed.
6. If the closed window was the last window in the workspace, the workspace instance is closed.

### 18.7 Focus model

Each tab has exactly one focused pane at any time. Focus determines which pane receives keyboard input.

**Focus movement:**

| Action | Behavior |
|--------|----------|
| `focus-next-pane` | Move focus to the next pane in tree traversal order (depth-first, left-to-right). Wraps around. |
| `focus-prev-pane` | Move focus to the previous pane in tree traversal order. Wraps around. |
| `focus-pane-up` | Move focus to the nearest pane above the current pane. If no pane exists in that direction, no change. |
| `focus-pane-down` | Move focus to the nearest pane below. |
| `focus-pane-left` | Move focus to the nearest pane to the left. |
| `focus-pane-right` | Move focus to the nearest pane to the right. |
| `focus-pane` (by name) | Move focus to the pane with the given name. |

Directional focus uses the geometric center of each pane to determine "nearest in direction."

### 18.8 Zoom/maximize

The `zoom-pane` action toggles zoom for the focused pane:

- **Zoom in:** The focused pane expands to fill the entire tab content area. Other panes in the tab are hidden but not destroyed. Their sessions continue running.
- **Zoom out:** The tab returns to its normal split layout.

A visual indicator (e.g., `[Z]` in the tab title) shows when a pane is zoomed.

### 18.9 Pane resize by user

Users can resize panes by dragging the splitter bar between panes (mouse) or by using keyboard actions:

| Action | Behavior |
|--------|----------|
| `resize-pane-grow-right` | Increase the focused pane's width by moving the right splitter. |
| `resize-pane-grow-down` | Increase the focused pane's height by moving the bottom splitter. |
| `resize-pane-shrink-right` | Decrease width. |
| `resize-pane-shrink-down` | Decrease height. |

Resize actions adjust the `ratio` of the nearest ancestor split in the relevant orientation. The resize increment is a configurable number of character cells (default: 2).

---

## 19. Naming and Addressing

### 19.1 Naming rules

All user-assigned names (workspace, tab, pane, profile) follow these rules:

- Valid characters: `[a-zA-Z0-9_-]`.
- Maximum length: 64 characters.
- Must not be empty.
- The `/` character is reserved as a path separator and must not appear in names.
- Names are case-sensitive.

### 19.2 Uniqueness constraints

| Object | Uniqueness scope |
|--------|-----------------|
| Workspace name | Global (unique across all definitions) |
| Window name | Unique within workspace |
| Tab name | Unique within parent window |
| Pane name | Unique within entire workspace (not just within tab) |
| Profile name | Unique within workspace definition (workspace profiles shadow global profiles with the same name) |

Pane names are globally unique within a workspace to enable unambiguous short-path addressing (`dev/server` rather than `dev/backend/server`).

### 19.3 Addressing grammar

Targets are addressed using `/`-separated path segments:

```
<segment>[/<segment>[/<segment>[/<segment>]]]
```

### 19.4 Resolution algorithm

Given a target path and a context (active workspace, or specified workspace):

**1 segment** (e.g., `server`):

1. If exactly one workspace instance is active, search pane names within it.
2. If the segment matches a workspace name, resolve to the workspace.
3. If the segment matches a unique pane name within the active workspace, resolve to the pane.
4. If ambiguous, return error with candidates.

**2 segments** (e.g., `dev/server`):

1. First segment is the workspace name. Look up the workspace instance.
2. Second segment is the pane name within that workspace.
3. If the pane name is unique within the workspace, resolve.
4. If ambiguous (duplicate pane names — violates uniqueness constraint, but handle defensively), return error with candidates showing full paths.

**3 segments** (e.g., `dev/backend/server`):

1. First segment: workspace name.
2. Second segment: tab name within the workspace.
3. Third segment: pane name within that tab.

**4 segments** (e.g., `dev/main/backend/server`):

1. First segment: workspace name.
2. Second segment: window name.
3. Third segment: tab name within that window.
4. Fourth segment: pane name within that tab.

### 19.5 Implicit workspace

If no workspace is specified in a target path (1-segment path like `server`), and exactly one workspace instance is active, that workspace is used implicitly. If zero or multiple workspace instances are active, the command fails with an error instructing the user to specify the workspace.

### 19.6 Internal IDs

All runtime objects (workspace instances, sessions, panes, tabs, windows) also have UUID-based internal IDs. These can be used for unambiguous addressing in scripts:

```bash
wtd capture --id 550e8400-e29b-41d4-a716-446655440000
```

Internal IDs are shown in `wtd inspect` output and in `--verbose` mode of list commands.

### 19.7 Ambiguity error format

When a target is ambiguous, the error response includes candidate matches:

```
Error: target "server" is ambiguous. Candidates:
  dev/backend/server
  dev/ops/server
Use a longer path or --id to disambiguate.
```

---

## 20. Action System

### 20.1 Action identity

Each action has:

| Property | Description |
|----------|-------------|
| `name` | Stable string identifier (kebab-case). |
| `targetType` | What type of object the action operates on: `global`, `workspace`, `window`, `tab`, or `pane`. |
| `args` | Named arguments, each with a type and optionality. |
| `description` | Human-readable description for the command palette and help text. |
| `defaultBindings` | Default keybindings and chords (may be overridden). |

### 20.2 Action dispatch

All actions from all sources are dispatched through a single `ActionDispatcher` in the host:

1. **UI keybinding:** UI classifies keystroke as a host binding → UI sends `InvokeAction` to host → host dispatches.
2. **UI chord:** UI detects prefix → UI enters chord state → user presses chord key → UI sends `InvokeAction` to host → host dispatches.
3. **UI palette:** User selects action in palette → UI sends `InvokeAction` to host → host dispatches.
4. **CLI:** `wtd action <target> <action>` → CLI sends `InvokeAction` to host → host dispatches.
5. **CLI convenience commands:** `wtd send`, `wtd keys`, `wtd focus`, etc. are syntactic sugar that the CLI translates into the corresponding IPC messages. These bypass the action system where they map directly to IPC message types.

### 20.3 Complete v1 action catalog

#### Workspace lifecycle actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `open-workspace` | global | name: string, file?: string, recreate?: bool | Open or attach to a workspace |
| `close-workspace` | workspace | kill?: bool | Close workspace UI. If kill=true, destroy instance. |
| `recreate-workspace` | workspace | (none) | Tear down and recreate instance from definition |
| `save-workspace` | workspace | file?: string | Save current workspace state as definition |

#### Window actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `new-window` | workspace | (none) | Create a new window in the workspace |
| `close-window` | window | (none) | Close window and all its tabs/panes/sessions |

#### Tab actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `new-tab` | window | profile?: string | Create a new tab with a single pane |
| `close-tab` | tab | (none) | Close tab and all its panes/sessions |
| `next-tab` | window | (none) | Switch to the next tab |
| `prev-tab` | window | (none) | Switch to the previous tab |
| `goto-tab` | window | index?: int, name?: string | Switch to tab by index (0-based) or name |
| `rename-tab` | tab | name: string | Rename the tab |
| `move-tab-left` | tab | (none) | Move tab one position left in the tab strip |
| `move-tab-right` | tab | (none) | Move tab one position right |

#### Pane actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `split-right` | pane | profile?: string | Split focused pane horizontally, new pane on right |
| `split-down` | pane | profile?: string | Split focused pane vertically, new pane below |
| `close-pane` | pane | (none) | Close pane and kill its session |
| `focus-next-pane` | tab | (none) | Move focus to next pane |
| `focus-prev-pane` | tab | (none) | Move focus to previous pane |
| `focus-pane-up` | tab | (none) | Move focus up |
| `focus-pane-down` | tab | (none) | Move focus down |
| `focus-pane-left` | tab | (none) | Move focus left |
| `focus-pane-right` | tab | (none) | Move focus right |
| `focus-pane` | workspace | name: string | Move focus to named pane |
| `zoom-pane` | pane | (none) | Toggle pane zoom |
| `rename-pane` | pane | name: string | Rename pane |
| `resize-pane-grow-right` | pane | amount?: int | Grow pane to the right |
| `resize-pane-grow-down` | pane | amount?: int | Grow pane downward |
| `resize-pane-shrink-right` | pane | amount?: int | Shrink pane from the right |
| `resize-pane-shrink-down` | pane | amount?: int | Shrink pane from above |

#### Session actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `restart-session` | pane | (none) | Kill current session and launch a new one from the same definition |

#### Clipboard actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `copy` | pane | (none) | Copy selected text to clipboard |
| `paste` | pane | (none) | Paste clipboard content as input to the session |

#### UI actions

| Action | Target | Args | Description |
|--------|--------|------|-------------|
| `toggle-command-palette` | global | (none) | Open or close the command palette |
| `toggle-fullscreen` | window | (none) | Toggle window fullscreen |
| `enter-scrollback-mode` | pane | (none) | Enter scrollback navigation mode |

---

## 21. Input Model

### 21.1 Input classification

The UI classifies each keyboard event into one of four categories before processing:

1. **Prefix key match:** If the pressed key matches the configured prefix key and no prefix is currently active, enter prefix-active state. Consume the keystroke (do not forward to session).

2. **Chord key match:** If the prefix is active and the pressed key matches a chord binding, dispatch the bound action. Consume the keystroke and exit prefix state.

3. **Single-stroke binding match:** If no prefix is active and the pressed key matches a single-stroke host binding, dispatch the bound action. Consume the keystroke.

4. **Raw terminal input:** In all other cases, forward the keystroke to the focused pane's session as raw input bytes.

### 21.2 KeySpec format

A KeySpec is a string representation of a key combination:

```
[Modifier+[Modifier+[...]]]KeyName
```

Modifiers (combinable, order-insensitive): `Ctrl`, `Alt`, `Shift`.
Key names: `A`–`Z`, `0`–`9`, `F1`–`F12`, `Enter`, `Tab`, `Escape`, `Space`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`, `Plus`, `Minus`.
Punctuation keys: `%`, `"`, `,`, `.`, `/`, `\`, `[`, `]`, `;`, `'`, `` ` ``.

Examples: `Ctrl+B`, `Ctrl+Shift+T`, `Alt+Shift+Minus`, `F11`, `Escape`.

### 21.3 Prefix chord state machine

```
State: Idle
  Event: prefix key pressed → transition to PrefixActive, start timer

State: PrefixActive
  Event: chord key pressed → dispatch action, transition to Idle, cancel timer
  Event: prefix key pressed again → send prefix key to session as raw input, transition to Idle, cancel timer
  Event: Escape pressed → transition to Idle, cancel timer (cancel prefix)
  Event: unbound key pressed → send prefix key + this key to session as raw input, transition to Idle, cancel timer
  Event: timeout (default 2000ms) → transition to Idle (cancel prefix silently)
```

When the prefix is active, the UI displays a visual indicator (e.g., a highlighted status bar segment showing "PREFIX" or the prefix key name).

### 21.4 Binding conflict resolution

Precedence (highest to lowest):

1. **Workspace-local chord bindings** override global chord bindings for the same chord key.
2. **Workspace-local single-stroke bindings** override global single-stroke bindings for the same key spec.
3. **Global chord bindings** apply if not overridden by workspace.
4. **Global single-stroke bindings** apply if not overridden by workspace.
5. **Prefix chord sequences** take priority over single-stroke bindings for the chord key when the prefix is active.
6. **Unbound keys** pass through to the session.

If a key is configured as both a single-stroke binding and the prefix key, the prefix key wins. The configuration loader warns about this conflict on startup.

### 21.5 Concurrent input (UI + controller)

Both the UI (direct typing) and the controller (`wtd send`, `wtd keys`) may inject input into the same session simultaneously. The host serializes all input to the PTY input pipe in arrival order. There is no locking or exclusive acquisition in v1. Both sources write to the same pipe; interleaving is possible and expected.

### 21.6 Mouse input

The UI handles mouse events:

| Event | Behavior |
|-------|----------|
| Left click on pane | Focus that pane |
| Left click + drag in pane | Begin text selection |
| Left click + drag on splitter | Resize panes |
| Right click in pane | Paste clipboard (configurable) or context menu |
| Scroll wheel in pane | Scroll through scrollback buffer |
| Mouse events when mouse reporting is enabled | Forward to session as VT mouse escape sequences |

---

## 22. Controller Model (CLI)

### 22.1 CLI binary

The `wtd` binary is the controller CLI. It is a Rust binary using `clap` for argument parsing.

### 22.2 CLI grammar

```
wtd <command> [<subcommand>] [<target>] [<positional-args...>] [<flags>]
```

### 22.3 Command reference

#### Workspace commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `open` | `wtd open <name> [--file <path>] [--recreate]` | Open workspace from definition. Attaches if instance exists, unless `--recreate`. |
| `attach` | `wtd attach <name>` | Attach to existing instance. Error if no instance exists. |
| `recreate` | `wtd recreate <name>` | Tear down existing instance and recreate from definition. |
| `close` | `wtd close <name> [--kill]` | Close workspace UI. `--kill` also destroys the instance. |
| `save` | `wtd save <name> [--file <path>]` | Save workspace definition. |
| `list workspaces` | `wtd list workspaces` | List all available workspace definitions. |
| `list instances` | `wtd list instances` | List all running workspace instances. |

#### Pane and session commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `list panes` | `wtd list panes <workspace>` | List all panes in a workspace instance. |
| `list sessions` | `wtd list sessions <workspace>` | List all sessions in a workspace instance. |
| `focus` | `wtd focus <target>` | Focus a pane in the UI. |
| `rename` | `wtd rename <target> <new-name>` | Rename a pane. |
| `action` | `wtd action <target> <action-name> [<args...>]` | Invoke a named action on a target. |

#### Input commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `send` | `wtd send <target> <text> [--no-newline]` | Send text to a session. Appends `\r\n` unless `--no-newline`. |
| `keys` | `wtd keys <target> <key-spec>...` | Send key sequences. Each arg is a KeySpec. |

#### Inspection commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `capture` | `wtd capture <target>` | Capture the visible screen content as text. |
| `scrollback` | `wtd scrollback <target> --tail <n>` | Capture the last N lines of scrollback. |
| `follow` | `wtd follow <target> [--raw]` | Stream output. Runs until Ctrl+C or session exit. |
| `inspect` | `wtd inspect <target>` | Show full metadata for a pane/session. |

#### Host management commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `host status` | `wtd host status` | Show host process status (PID, uptime, instance count). |
| `host stop` | `wtd host stop` | Shut down the host process. |

### 22.4 Global flags

| Flag | Description |
|------|-------------|
| `--json` | Output in JSON format instead of human-readable text. |
| `--verbose` | Include internal IDs and additional metadata in output. |
| `--id <uuid>` | Address a target by internal ID instead of semantic path. |
| `--help` | Show help for the command. |
| `--version` | Show wtd version. |

### 22.5 Output format

By default, output is human-readable text formatted for terminal display. The `--json` flag switches to JSON output for scripting.

Example: `wtd list panes dev` (human-readable):

```
WORKSPACE: dev (instance abc123)

TAB       PANE        SESSION     STATE     PROFILE
backend   editor      editor      running   powershell
backend   server      server      running   powershell
backend   tests       tests       running   powershell
ops       prod-shell  prod-shell  running   ssh
ops       prod-logs   prod-logs   running   ssh
```

Example: `wtd list panes dev --json`:

```json
{
  "workspace": "dev",
  "instanceId": "abc123...",
  "panes": [
    {
      "tab": "backend",
      "name": "editor",
      "sessionName": "editor",
      "state": "running",
      "profile": "powershell",
      "paneId": "...",
      "sessionId": "..."
    }
  ]
}
```

### 22.6 `send` command text handling

The text argument to `wtd send` is sent verbatim to the PTY input pipe, with `\r\n` appended unless `--no-newline` is specified. Shell quoting rules apply normally (the OS shell parses the command line before `wtd` receives it).

Special characters in the text are sent as-is. No escape sequence processing is performed by `wtd send`. To send a literal tab character, use the shell's quoting mechanisms (e.g., `wtd send dev/server $'hello\tworld'` in bash/PowerShell).

### 22.7 `keys` command key handling

Each key spec argument is translated to the appropriate byte sequence or VT escape sequence:

| KeySpec | Bytes sent |
|---------|-----------|
| `Enter` | `\r` (0x0D) |
| `Tab` | `\t` (0x09) |
| `Escape` | `\e` (0x1B) |
| `Ctrl+C` | `0x03` |
| `Ctrl+D` | `0x04` |
| `Ctrl+Z` | `0x1A` |
| `Up` | `\e[A` |
| `Down` | `\e[B` |
| `Right` | `\e[C` |
| `Left` | `\e[D` |
| `Home` | `\e[H` |
| `End` | `\e[F` |
| `PageUp` | `\e[5~` |
| `PageDown` | `\e[6~` |
| `Delete` | `\e[3~` |
| `F1`–`F12` | Standard xterm function key sequences |
| `a`–`z`, `A`–`Z`, `0`–`9` | ASCII byte value |
| `Ctrl+A`–`Ctrl+Z` | `0x01`–`0x1A` |

### 22.8 `capture` output format

`capture` returns the current visible screen content as a plain text string, one line per row, with trailing whitespace trimmed from each row. VT formatting (colors, attributes) is stripped. The output is suitable for piping, grep, or programmatic parsing.

### 22.9 Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Target not found |
| 3 | Ambiguous target (candidates listed on stderr) |
| 4 | Host not running and auto-start failed |
| 5 | Workspace definition parse error |
| 6 | Connection error (host crashed during request) |
| 10 | Timeout (for commands with `--timeout` flag, future) |

---

## 23. Output and Inspection Model

### 23.1 Capture (visible viewport)

`wtd capture <target>` returns the current content of the session's active screen buffer as plain text. Each row is a string. Trailing whitespace on each row is trimmed. Empty trailing rows are omitted. If the alternate screen buffer is active (e.g., vim is running), the alternate screen content is captured.

### 23.2 Scrollback

`wtd scrollback <target> --tail <n>` returns the last N lines from the scrollback ring buffer as plain text. Lines are in chronological order (oldest first). If fewer than N lines exist in the scrollback, all available lines are returned. VT formatting is stripped.

### 23.3 Follow

`wtd follow <target>` opens a streaming connection that outputs new text as it arrives.

- Default mode (`--raw` not set): Output is VT-stripped plain text, line-buffered. New complete lines are printed as they appear.
- Raw mode (`--raw`): Output is raw VT bytes, unbuffered. Suitable for piping to another terminal or recording.

Follow continues until: the session exits (follow prints exit code on stderr and exits with code 0), the user presses Ctrl+C (exits with code 0), or the connection is lost (exits with code 6).

### 23.4 Inspect

`wtd inspect <target>` returns full metadata for a pane and its attached session:

```
Pane: editor
  Path: dev/backend/editor
  Pane ID: 550e8400-...
  Tab: backend
  Window: main
  State: attached (running)

Session: editor
  Session ID: 6ba7b810-...
  Profile: powershell
  Executable: pwsh.exe
  Args: (none)
  CWD: C:\src\app
  PID: 12345
  Title: pwsh - C:\src\app
  Cols: 120
  Rows: 30
  Scrollback: 3,420 lines
  Runtime: 2h 15m 30s
  Restart count: 0
  Restart policy: on-failure
```

### 23.5 Current working directory

The host does not actively track the session's current working directory (this would require OS-specific process inspection that is fragile and not always possible for remote SSH sessions). The `cwd` shown in inspect is the startup working directory as configured, not the current directory of the running process. The host does not falsely claim to know the current working directory.

---

## 24. UI Architecture

### 24.1 Rendering technology

See §7.9 for the rendering technology evaluation. This section specifies the UI behavior requirements regardless of rendering technology.

### 24.2 Window structure

Each `wtd-ui` window contains:

- **Title bar:** Standard Windows title bar. Shows workspace name and active tab name.
- **Tab strip:** Horizontal strip of named tabs. Active tab is visually highlighted. Tabs can be clicked to switch, dragged to reorder, and have close buttons.
- **Content area:** The pane layout tree rendered as split panes with draggable splitter bars between them.
- **Status bar:** Bottom strip showing: active workspace name, focused pane path, prefix-active indicator, session state indicator.

### 24.3 Tab strip behavior

| Feature | Behavior |
|---------|----------|
| Tab display | Shows tab name. Optionally shows session count or status indicator. |
| Tab switching | Click or keyboard (`Ctrl+Tab`, `Ctrl+Shift+Tab`, `Ctrl+B,n`, `Ctrl+B,p`). |
| Tab creation | `Ctrl+Shift+T` or `Ctrl+B,c`. New tab with single pane using default profile. |
| Tab close | Close button on tab, or `Ctrl+B,x` when only one pane in the tab (close the pane, which closes the tab). |
| Tab reorder | Drag and drop within the tab strip. |
| Active tab | Visually distinct (e.g., highlighted background, bold text). |
| Overflow | If more tabs than the tab strip can display, show scroll arrows or a dropdown. |

### 24.4 Pane rendering

Each pane viewport renders:

- **Terminal content:** Character grid from the session's VT output stream.
- **Cursor:** Block, underline, or bar cursor as configured or set by VT sequences.
- **Selection:** Highlighted text region when the user is selecting with the mouse.
- **Pane title:** Optional small overlay or border label showing the pane's semantic name.
- **Pane border/indicator:** Focused pane has a visually distinct border (e.g., colored accent).

### 24.5 Failed pane display

When a pane is in `Detached` state (session failed or exited):

- The pane area shows a centered message: "Session exited (code N)" or "Session failed: error message".
- Below the message: "Press Enter to restart" or "Use Ctrl+B,r to restart".
- The pane remains in the layout. It does not collapse or disappear.

### 24.6 Command palette

The command palette is a modal overlay triggered by `Ctrl+Shift+Space` or `Ctrl+B,:`.

Features:

- **Search field:** Fuzzy-match filter over action names and descriptions.
- **Action list:** Scrollable list of matching actions with their keybindings shown.
- **Target selection:** If an action requires a target (e.g., `focus-pane`), the palette shows a secondary list of valid targets after selecting the action.
- **Escape to dismiss:** Pressing Escape or clicking outside the palette closes it.

### 24.7 Clipboard

| Operation | Trigger | Behavior |
|-----------|---------|----------|
| Copy | `Ctrl+Shift+C` or `Ctrl+B,y` or select + right-click | Copy selected text to Windows clipboard. VT formatting is stripped. |
| Paste | `Ctrl+Shift+V` or `Ctrl+B,]` or right-click (configurable) | Paste clipboard text as input to the focused session. If bracketed paste is enabled, wraps with `\e[200~`...`\e[201~`. |
| Copy-on-select | Configurable (`copyOnSelect: true`) | Completing a selection automatically copies. |

### 24.8 Settings-driven configuration

All UI behavior is configurable via the global settings file (§11). The UI reads the settings file on startup and applies the configuration. Hot-reload of settings is a post-v1 feature.

---

## 25. Profile System

### 25.1 Profile resolution

When a session is launched, the profile is resolved as follows:

1. Look up the profile name in the workspace's `profiles` section.
2. If not found, look up in the global settings' `profiles` section.
3. If not found and the name matches a built-in profile type (`powershell`, `cmd`, `wsl`, `ssh`, `custom`), use the built-in defaults.
4. If not found at all, error.

### 25.2 Built-in profile defaults

#### PowerShell

| Field | Default value |
|-------|---------------|
| `executable` | `pwsh.exe` (PowerShell 7+). If not found on PATH, falls back to `powershell.exe` (Windows PowerShell 5.1). |
| `args` | `["-NoLogo"]` |
| `cwd` | `%USERPROFILE%` |
| `env` | `{}` |

#### cmd

| Field | Default value |
|-------|---------------|
| `executable` | `cmd.exe` |
| `args` | `[]` |
| `cwd` | `%USERPROFILE%` |

#### WSL

| Field | Default value |
|-------|---------------|
| `executable` | `wsl.exe` |
| `args` | `["-d", "{distribution}"]` where `{distribution}` is the required `distribution` field. If `distribution` is not set, `wsl.exe` runs with no `-d` flag (uses the default WSL distribution). |
| `cwd` | WSL home directory (determined by WSL). |

#### SSH

| Field | Default value |
|-------|---------------|
| `executable` | `ssh.exe` |
| `args` | Built from profile fields: `["{user}@{host}", "-p", "{port}"]`. If `identityFile` is set, prepend `["-i", "{identityFile}"]`. If `useAgent` is false, prepend `["-o", "IdentitiesOnly=yes"]`. If `remoteCommand` is set, append it as the final argument. |
| `cwd` | `%USERPROFILE%` |

#### Custom

| Field | Default value |
|-------|---------------|
| `executable` | (must be specified) |
| `args` | `[]` |
| `cwd` | `%USERPROFILE%` |

### 25.3 Environment variable merge order

1. Start with the host process (`wtd-host`) environment.
2. Apply global settings `defaultProfile` env overrides (if the resolved profile was defined globally).
3. Apply workspace `defaults.env` overrides.
4. Apply profile `env` overrides.
5. Apply session `env` overrides.
6. Set `TERM=xterm-256color` (for local sessions; not set for SSH sessions where the remote negotiates its own TERM).

At each layer, keys present in the override map replace or add to the accumulated environment. Keys with a `null` value remove the variable from the environment.

---

## 26. Workspace Lifecycle Operations

### 26.1 Open

`wtd open <name>`:

1. Search for the workspace definition file (§12.1).
2. Parse and validate the definition (§10.3).
3. Check if a workspace instance with this name already exists in the host:
   - If yes and `--recreate` is not set: attach to the existing instance (go to step 7).
   - If yes and `--recreate` is set: close the existing instance (step 4 of §26.4), then proceed.
4. Create a new WorkspaceInstance with a new UUID.
5. Create a Windows Job Object for the instance.
6. For each pane in the layout tree (depth-first traversal): create a session (§17.1), attach it to the pane.
7. Send the full instance state to the requesting UI client (or print summary to CLI).
8. The UI creates windows, tabs, and pane viewports, and begins receiving output streams.

### 26.2 Attach

`wtd attach <name>`:

1. Look up the workspace instance by name.
2. If no instance exists, error: "No running instance for workspace '<name>'. Use 'wtd open <name>' to create one."
3. Send the full instance state to the requesting client.

### 26.3 Close

`wtd close <name> [--kill]`:

- Without `--kill`: Notify all attached UI clients to close their windows for this workspace. The workspace instance remains running in the host. Sessions continue. The user can later `attach`.
- With `--kill`: Close all UI clients, terminate all sessions, destroy the workspace instance, remove the job object.

### 26.4 Recreate

`wtd recreate <name>`:

1. Look up the existing workspace instance.
2. If it exists: terminate all sessions, close all UI client attachments, destroy the instance.
3. Search for and parse the workspace definition (same as open).
4. Create a new instance from the definition (same as open steps 4–8).

### 26.5 Save

`wtd save <name> [--file <path>]`:

Save creates a new workspace definition file from a running workspace instance. This captures:

- The current pane names and layout tree structure.
- The profiles used by each session.
- The startup commands and working directories from the original session definitions.

It does not capture: current screen content, current process state, runtime titles set by VT sequences, or runtime pane resizes (ratios are saved as currently configured in the layout tree).

---

## 27. State Machines

### 27.1 Session state machine

```
                   ┌────────────────────────┐
                   │                        │
                   ▼                        │
Creating ──► Running ──► Exited(code) ──► Restarting
                │              │
                │              ▼
                │         (if restart policy
                │          says don't restart)
                │              │
                │              ▼
                │          Exited (final)
                │
                ▼
            Failed(error) ──► Restarting
                  │
                  ▼
             (if restart policy
              says don't restart)
                  │
                  ▼
              Failed (final)
```

| State | Description |
|-------|-------------|
| `Creating` | ConPTY and child process are being set up. |
| `Running` | Child process is alive. PTY I/O is active. |
| `Exited(code)` | Child process has exited with the given exit code. PTY is drained and closed. |
| `Failed(error)` | Session creation failed (e.g., executable not found). No PTY exists. |
| `Restarting` | Waiting for restart backoff delay before creating a new session. |

### 27.2 Workspace instance state machine

```
Creating ──► Active ──► Closing ──► Closed
               │
               ▼
          Recreating ──► Creating
```

| State | Description |
|-------|-------------|
| `Creating` | Sessions are being launched. |
| `Active` | All initial sessions have been attempted. Workspace is operational. |
| `Closing` | Sessions are being terminated and resources released. |
| `Closed` | Instance is destroyed and removed from registry. |
| `Recreating` | Existing instance is being torn down before re-creation. |

### 27.3 Pane state machine

```
Attached(sessionId) ──► Detached(exitInfo)
      │                       │
      │                       ▼
      │                 (user restarts)
      │                       │
      ▼                       ▼
Zoomed(sessionId) ──► Attached(sessionId)
      │
      ▼
 (unzoom)
      │
      ▼
Attached(sessionId)
```

| State | Description |
|-------|-------------|
| `Attached(sessionId)` | Pane is displaying a live session. |
| `Detached(exitInfo)` | Session has exited or failed. Pane shows status message. |
| `Zoomed(sessionId)` | Pane is zoomed to fill the tab area. Session is still attached. |

### 27.4 Prefix chord state machine

```
Idle ──► PrefixActive(timestamp)
            │         │         │         │
            ▼         ▼         ▼         ▼
       ChordMatch  Timeout  EscCancel  UnboundKey
            │         │         │         │
            ▼         ▼         ▼         ▼
           Idle      Idle      Idle      Idle
```

| State | Entry action | Exit action |
|-------|-------------|-------------|
| `Idle` | Clear prefix indicator | — |
| `PrefixActive` | Show prefix indicator, start timeout timer | Cancel timer |

| Transition | Condition | Side effect |
|------------|-----------|-------------|
| Idle → PrefixActive | Prefix key pressed | Consume keystroke |
| PrefixActive → Idle (ChordMatch) | Recognized chord key pressed | Dispatch bound action, consume keystroke |
| PrefixActive → Idle (Timeout) | Timer expires | No action |
| PrefixActive → Idle (EscCancel) | Escape pressed | Consume keystroke |
| PrefixActive → Idle (UnboundKey) | Unrecognized key pressed | Forward prefix key + this key to session |
| PrefixActive → Idle (PrefixAgain) | Prefix key pressed again | Send one prefix key to session as literal input |

---

## 28. Security Model

### 28.1 Access boundary

Only the Windows user account that started `wtd-host` may:

- Connect to the named pipe.
- List workspace instances.
- Inject input to sessions.
- Capture output from sessions.
- Invoke actions.
- Attach UI clients.

### 28.2 Named pipe ACL

The named pipe is created with a `SECURITY_DESCRIPTOR` that contains a DACL granting `FILE_GENERIC_READ | FILE_GENERIC_WRITE` only to the creating user's SID. The `SECURITY_ATTRIBUTES` are passed to `CreateNamedPipe`.

### 28.3 Client verification

On each new connection, the host obtains the client's process ID via `GetNamedPipeClientProcessId`, opens the client's process token, and verifies the token's user SID matches the pipe owner's SID. If it does not match, the connection is closed immediately.

### 28.4 Secrets and sensitive data

The system does not classify, detect, or redact secrets in terminal output. Any client with controller access can read any text visible in any session's screen buffer or scrollback. This is documented and users are expected to understand that controller access implies full output visibility.

### 28.5 Startup command visibility

Startup commands are sent as visible input to the terminal. They appear in the session's output (the shell echoes them). They are not treated as secrets by the system. Users should not put credentials in startup commands; SSH identity files and agent-based authentication are the recommended patterns for remote access.

### 28.6 No remote access

The named pipe is local only. There is no TCP listener, HTTP API, or other network-accessible interface. Remote control is explicitly out of scope.

---

## 29. Error Handling

### 29.1 Principle

Errors are explicit, localizable, and non-destructive. The system prefers partial success over total failure.

### 29.2 Session launch failure

If a session fails to launch during workspace creation:

1. The session state is set to `Failed` with a descriptive error message.
2. The pane state is set to `Detached` showing the error.
3. Other sessions in the workspace continue launching.
4. The workspace opens successfully with some panes in error state.
5. The user can restart failed sessions individually.

### 29.3 Partial workspace recovery

If N out of M sessions fail during workspace creation, the M-N successful sessions are fully usable. The UI shows which panes failed and why. The workspace is not torn down.

### 29.4 Ambiguous target

When a target path matches multiple objects:

1. The command fails (exit code 3 for CLI, `target-ambiguous` error code for IPC).
2. The error message lists all candidate matches with their full paths.
3. The user refines the path or uses `--id`.

### 29.5 Missing workspace definition

When `wtd open <name>` finds no definition file:

1. The command fails with exit code 5.
2. The error message lists the search paths that were checked.
3. No partial state is created.

### 29.6 Host connection failure

When `wtd` cannot connect to the host:

1. Attempt auto-start (§16.1).
2. If auto-start fails, report "Failed to start or connect to wtd-host" with diagnostic information (PID file status, pipe name attempted).
3. Exit code 4.

### 29.7 Workspace definition validation errors

When a workspace definition file has validation errors:

1. The parse error is reported with the file path, the field path (e.g., `windows[0].tabs[1].layout.children[0].name`), and a human-readable message.
2. No workspace instance is created.
3. Exit code 5.

### 29.8 Runtime action errors

If an action fails at runtime (e.g., `split-right` when the pane is too small to split):

1. The error is returned as an `Error` IPC response.
2. In the UI, the error is shown as a transient notification.
3. No state change occurs.

---

## 30. Performance Requirements

### 30.1 Latency targets

| Metric | Target | Description |
|--------|--------|-------------|
| Keystroke-to-echo | < 50ms | From key press in UI to character appearing on screen via host round-trip |
| Terminal output rendering | < 16ms | From host pushing output bytes to UI to pixels updated (60fps frame budget) |
| `capture` command response | < 100ms | From CLI sending Capture to receiving CaptureResult |
| `send` command response | < 50ms | From CLI sending Send to host acknowledging |
| Workspace open (5 sessions) | < 2s | From CLI sending Open to all sessions in Running state |

### 30.2 Throughput targets

| Metric | Target | Description |
|--------|--------|-------------|
| Output rendering | 100 MB/s per session | Sustained output from a fast producer (e.g., `cat` large file) without UI freeze |
| Concurrent sessions | 20+ without degradation | Common developer workload across multiple tabs and windows |

### 30.3 UI responsiveness

The UI render thread must never block on:

- Host IPC communication (use async channels).
- Session output processing (use buffered async reads).
- Scrollback buffer access (use the host-side buffer; the UI only renders the current viewport).

### 30.4 Host robustness

- One failed session must not crash the host process or affect other sessions.
- One crashed UI connection must not destroy workspace instances.
- One malformed CLI request must not destabilize host state.
- Output from one high-throughput session must not starve other sessions of processing time (use fair scheduling or per-session output budgets per event loop tick).

---

## 31. Logging and Diagnostics

### 31.1 Log infrastructure

All three processes use the `tracing` crate for structured logging. Log output goes to:

- **Host:** Log file at `%APPDATA%\WinTermDriver\logs\wtd-host.log`. Rotated by size (10 MB per file, keep 5 files).
- **UI:** stderr (visible if launched from a terminal) and optionally to a log file.
- **CLI:** stderr only.

### 31.2 Log levels

| Level | Usage |
|-------|-------|
| `error` | Unrecoverable failures, session crashes, IPC protocol violations. |
| `warn` | Recoverable issues: session restart, binding conflict, startup command sent before prompt. |
| `info` | Lifecycle events: session start/stop, workspace open/close, client connect/disconnect. |
| `debug` | Action dispatch, IPC messages (summarized), configuration resolution. |
| `trace` | Raw PTY I/O, VT parser state transitions, full IPC message payloads. |

Default level: `info`. Configurable via global settings (`logLevel`) and `WTD_LOG` environment variable.

### 31.3 Host diagnostics

The host tracks and exposes:

| Diagnostic | Access method |
|------------|---------------|
| Why a session failed to launch | `wtd inspect <target>` shows error message |
| What workspace definition was used | `wtd inspect <target>` shows definition path |
| What command/profile launched a session | `wtd inspect <target>` shows resolved executable, args, cwd |
| Runtime pane-to-session mapping | `wtd list panes <workspace> --verbose` |
| IPC connection count | `wtd host status` |
| Uptime | `wtd host status` |

### 31.4 User-facing diagnostic commands

| Command | Output |
|---------|--------|
| `wtd host status` | Host PID, uptime, active instance count, connected clients count, log file path |
| `wtd inspect <target>` | Full metadata for pane/session including error information |
| `wtd list instances --verbose` | Instance IDs, definition paths, session counts, UI attachment counts |

---

## 32. Testability Requirements

### 32.1 Component isolation

The following components shall be testable in isolation without launching PTY processes, UI windows, or IPC connections:

| Component | Test strategy |
|-----------|--------------|
| Workspace definition parser | Unit tests: parse YAML strings → validate WorkspaceDefinition structs. Test valid files, invalid files, edge cases (missing fields, invalid names, duplicate names). |
| Naming resolution algorithm | Unit tests: build in-memory WorkspaceInstance graphs → resolve target paths → assert correct results or correct error types. |
| Layout tree operations | Unit tests: split, close, resize, focus traversal on in-memory PaneRuntimeNode trees. Assert tree structure, ratio values, focus targets. |
| Action dispatch | Unit tests: register actions → dispatch by name with arguments → assert effects on mock state. |
| VT screen buffer | Unit tests: feed byte sequences to the screen buffer → assert cell contents, cursor position, scrollback state, title. |
| Profile resolution | Unit tests: given global settings, workspace definition, and session definition → assert resolved executable, args, env, cwd. |
| Environment merge | Unit tests: given layered env maps with null removals → assert final environment. |
| IPC message serialization | Unit tests: serialize/deserialize every message type → round-trip equality. |
| Prefix chord state machine | Unit tests: feed key events → assert state transitions, dispatched actions. |
| Restart backoff | Unit tests: simulate exit events → assert delay values and restart count behavior. |
| Binding conflict detection | Unit tests: given conflicting binding configurations → assert warnings. |

### 32.2 Integration test requirements

| Test scenario | Description |
|---------------|-------------|
| End-to-end workspace lifecycle | Open → list panes → send command → capture output → close. Uses real ConPTY but headless (no UI). |
| Session failure and restart | Launch a session with a nonexistent executable → assert Failed state → restart → assert Running. |
| Multi-session workspace | Open a workspace with 4+ sessions → assert all sessions reach Running state → close → assert all sessions terminate. |
| CLI command coverage | Each `wtd` subcommand is tested against a running host with a test workspace. |
| Concurrent input | Send input via CLI and simulate UI input simultaneously → assert no crash or hang. |

### 32.3 Test infrastructure

- A `TestHost` harness that starts `wtd-host` in-process (or as a subprocess) with a test-specific named pipe name (to avoid conflicting with a user's real host).
- A `TestClient` that connects to the test pipe and sends/receives IPC messages.
- Test workspace definition fixtures in YAML.

---

## 33. Versioning and Migration

### 33.1 Workspace definition versioning

The `version` field in workspace definition files is an integer. Currently `1`. The version number increments when a breaking schema change is made (field renamed, field removed, semantic change).

### 33.2 Forward compatibility

Unknown fields in workspace definition files are ignored with a warning logged at `warn` level. This allows newer workspace files (from a newer version of the tool) to be partially usable by older tool versions, as long as the `version` number has not incremented.

### 33.3 Version mismatch behavior

If the `version` field in a workspace file is higher than the tool supports:

1. The tool refuses to load the file.
2. The error message states: "Workspace file version N is not supported. This version of wtd supports version M. Please upgrade wtd."
3. No partial loading or best-effort interpretation is attempted for major version mismatches.

### 33.4 Settings file versioning

The global settings file does not have a `version` field in v1. Unknown fields are ignored. Future versions may add a version field if breaking changes are needed.

### 33.5 IPC protocol versioning

The handshake message includes `protocolVersion: 1`. If the client and host protocol versions differ, the host returns an error and closes the connection. This allows rolling upgrades: upgrade the host first, then upgrade clients.

---

## 34. WT Codebase Reuse Map

### 34.1 Directly referenced for algorithm/correctness

| WT Component | WT source location | Usage |
|--------------|-------------------|-------|
| ConPTY connection | `src/cascadia/TerminalConnection/ConPtyConnection.*` | Reference for CreatePseudoConsole lifecycle, resize handling, process creation |
| VT parser state machine | `src/terminal/parser/` | Correctness reference for VT sequence interpretation |
| Settings schema design | `src/cascadia/TerminalSettingsModel/` | Design patterns for profiles, keybindings, defaults |
| Pane layout tree | `src/cascadia/TerminalApp/Pane.*` | Splitting, closing, resize redistribution, focus traversal algorithms |
| Tab management | `src/cascadia/TerminalApp/Tab*.*` | Tab ordering, activation, close-last-tab behavior |
| Selection model | `src/cascadia/TerminalCore/Terminal.cpp` (selection methods) | Word/line/block selection, selection coordinate mapping |

### 34.2 Behavioral reference (no code reuse)

| Feature | WT behavior referenced |
|---------|----------------------|
| Clipboard | Copy strips VT formatting, paste supports bracketed paste mode |
| Scrollback | Ring buffer, scroll-to-bottom on new output, scrollback mark on scroll |
| Resize | Reflow text on terminal resize (ConPTY handles this) |
| TUI rendering | How WT handles alternate screen buffer, mouse reporting, cursor shapes |
| Font fallback | WT's DirectWrite font fallback chain behavior |

### 34.3 Not used

| WT Component | Reason |
|--------------|--------|
| XAML UI framework | WinTermDriver uses Rust, not WinUI/XAML |
| WinUI tab strip | Custom implementation needed |
| WT's JSON settings parser | WinTermDriver uses serde_yaml |
| WT's command palette implementation | Custom implementation in Rust/chosen UI framework |
| `wt.exe` CLI | Different CLI grammar; no compatibility attempted |
| WT's extension/fragment system | Not relevant to workspace model |

---

## 35. Required Invariants

The implementation shall preserve the following invariants at all times:

1. **Definition durability.** A Workspace Definition file can always be used to recreate a workspace from scratch. The file is never modified by the runtime unless the user explicitly saves.

2. **Instance transience.** A Workspace Instance is transient. Its loss (host crash, explicit close) is recoverable by re-opening the workspace from its definition.

3. **Session ≠ Pane.** A Session is an internal host object (PTY + process + buffer). A Pane is a UI viewport. They are separate objects with separate lifecycles, connected by attachment.

4. **1:1 attachment (v1).** A Pane is attached to at most one Session. A Session is attached to at most one Pane.

5. **Session independence from UI.** A Session exists and runs within a workspace instance whether or not a UI is currently attached. Output accumulates in the host-side buffer.

6. **Non-exclusive input.** Both UI typing and controller commands may inject input to the same session concurrently. Neither side steals exclusive ownership.

7. **Action unification.** All host-level actions have identical semantics regardless of whether invoked by keybinding, chord, palette, or CLI.

8. **Semantic naming primacy.** Pane names are the primary addressing mechanism. Internal IDs are secondary.

9. **Per-user security.** The IPC channel is accessible only to the owning Windows user. This is enforced by named pipe ACLs.

10. **Failure isolation.** One session's failure does not affect other sessions. One UI's crash does not affect the host or other UIs. One CLI request's failure does not affect host state.

---

## 36. Acceptance Criteria

The system is conformant to this specification when all of the following are demonstrably true:

### 36.1 Workspace lifecycle

A user can: define a workspace YAML file → `wtd open <name>` → interact with panes → close the UI → `wtd attach <name>` (sessions still running) → `wtd recreate <name>` → get fresh sessions from the definition.

### 36.2 Mixed session support

A single workspace contains at least one local PowerShell pane, one WSL pane, and one SSH pane. All are usable interactively and via the controller.

### 36.3 Manual interaction

Each pane supports: typing, cursor movement, pasting, text selection, scrollback navigation, and running TUI applications (vim, htop, or similar) without artifacts.

### 36.4 Controller interaction

The controller can: `list panes`, `send` text, `keys` to send key sequences, `capture` visible screen, `scrollback --tail N`, `follow` output stream, `inspect` metadata, `action` to invoke actions.

### 36.5 Semantic naming

`wtd send dev/server "test"` works. `wtd capture dev/logs` works. Ambiguous targets produce clear error messages with candidates.

### 36.6 Prefix chords

`Ctrl+B,%` splits right. `Ctrl+B,"` splits down. `Ctrl+B,o` cycles focus. The prefix indicator is visible. Timeout cancels the prefix.

### 36.7 Partial failure tolerance

A workspace with 4 sessions where 1 session's executable does not exist: the other 3 sessions start normally, the failed pane shows an error, and the user can restart the failed session.

### 36.8 Local security

Connecting to the named pipe from a different user account fails. Output is not accessible across user boundaries.

### 36.9 Workspace-as-code

A `.wtd/dev.yaml` file in a project directory is found by `wtd open dev` when invoked from that directory.

### 36.10 Recreation determinism

Opening the same workspace definition twice produces the same logical structure: same pane names, same tab names, same profiles, same layout tree shape.

---

## 37. Bead-Ready Work Breakdown

This section structures the implementation for bead-based execution. It identifies end-to-end slices, decomposes the design into capability worksets, lists candidate beads per workset, defines dependencies, and surfaces risks that must be resolved before bead generation.

This is a preparation artifact. It does not contain generated beads. Bead generation follows from this structure.

### 37.1 Slices

Slices are ordered end-to-end proofs that cut vertically through multiple worksets. Each slice proves that a part of the design works from input to output. Slices determine which beads from which worksets to execute first.

#### Slice 1: Headless round-trip

Parse a workspace YAML file, resolve a profile to a concrete launch spec, create a ConPTY session, send input to the session, maintain a VT screen buffer from output, and capture the visible screen state — all without a UI.

This is the first thin slice. It proves the core pipeline from workspace definition to live terminal I/O.

Draws from worksets: W1 (parse, resolve), W2 (ConPTY, screen buffer), W3 (session manager, IPC server, workspace instance manager), W4 (IPC message types only).

#### Slice 2: CLI-driven workspace

Open a named workspace via `wtd open`, list panes by semantic name, send text to a named pane, capture output, inspect session metadata, and close the workspace — all driven by the `wtd` CLI.

Proves the workspace model, semantic naming, action dispatch, and full controller round-trip.

Draws from worksets: W1 (workspace discovery, global settings), W2 (layout tree, naming resolution), W3 (action dispatcher, host lifecycle), W4 (CLI parsing, CLI IPC client, end-to-end tests).

#### Slice 3: Visual terminal

Render terminal content in a window with tabs and split panes, receiving VT bytes from the host via IPC. A user can see terminal output, see the tab strip, and see pane borders with focus indicators.

Proves the UI rendering pipeline end-to-end. Gated on the rendering technology decision (W5).

Draws from worksets: W5 (rendering spike), W6 (window/tab chrome, pane layout rendering, terminal content rendering, UI-host IPC client).

#### Slice 4: Interactive workspace

Type into a pane, use single-stroke keybindings, execute prefix chord sequences, click to focus panes, drag splitters, select and copy text, paste from clipboard, and invoke actions from the command palette.

Proves the full interaction model. The application is usable as a real terminal workspace.

Draws from worksets: W6 (status bar, failed pane display), W7 (keyboard pipeline, prefix chords, mouse, clipboard, command palette).

### 37.2 Worksets

Each workset is a major capability area. Worksets are larger than beads but smaller than the full design. Each one answers: "what meaningful capability becomes real when this is done?"

Candidate beads are listed per workset as likely executable units. They are phrased as reviewable outcomes. Final bead generation will refine scope and add completion evidence.

#### W1: Workspace definition and configuration

**Capability outcome:** A YAML workspace file can be loaded, validated, and its profiles resolved to concrete launch specs. Global settings can be loaded and merged. Workspace files can be discovered from the CWD or user directory.

**Candidate beads:**

- Parse a YAML workspace definition into a validated WorkspaceDefinition model (§9.1, §10). Unit tests cover valid files, invalid files, edge cases (missing fields, invalid names, duplicate pane names, mutually exclusive `windows`/`tabs`).
- Resolve profiles from workspace definition + global settings into concrete launch specs — executable, args, env, cwd (§25). Unit tests cover all profile types, fallback chains, environment merge with null removal.
- Load and validate the global settings file with built-in defaults and merge precedence (§11). Unit tests cover missing file, partial overrides, font/theme defaults.
- Discover workspace definition files from CWD `.wtd/` directory and user workspace directory (§12). Unit tests cover search order, explicit `--file` path, listing from both sources.

#### W2: Terminal core

**Capability outcome:** ConPTY sessions can be created, receive input, and produce VT output. Output is parsed into a queryable screen buffer with scrollback. A binary pane layout tree supports split, close, resize, and focus traversal.

**Candidate beads:**

- Create, resize, and close ConPTY pseudo-consoles and child processes (§14.1–14.3). Integration test: launch pwsh, send "echo hello", read output bytes, resize, close cleanly.
- Parse VT byte stream into queryable screen state with scrollback ring buffer (§14.4, §15). Feed bytes via `vte` parser, assert cell contents, cursor position, colors, attributes, alternate screen, scrollback depth, and title extraction. Unit tests.
- Split, close, resize, and traverse focus in a binary pane layout tree (§18). Pure data structure with unit tests covering split operations, close with sibling promotion, ratio-based resize, directional focus movement, and minimum size clamping.

#### W3: Host process and IPC

**Capability outcome:** A singleton per-user host process manages workspace instances, session lifecycles, and an IPC server. Actions from any source dispatch through a unified action system. The host is auto-startable, crash-recoverable, and cleanly shutdownable.

**Candidate beads:**

- Define all IPC message types as Rust structs with serde serialization (§13.4–13.14). Round-trip serialization tests for every message type. Length-prefixed framing implementation.
- Create, monitor, and restart sessions using the ConPTY wrapper with configurable backoff policy (§17). Integration tests cover session creation, exit detection, restart on failure, backoff delay progression, backoff reset after stable run.
- Create workspace instances from definitions with pane-session attachments and lifecycle state transitions (§26, §27.2). Integration tests cover open, attach, recreate, close, partial failure tolerance (§29.2–29.3).
- Accept per-user named pipe IPC connections, perform handshake, and route request/response and streaming messages to workspace and session managers (§13). Security: named pipe ACL restricts to owning user SID (§28). Integration tests cover connection, handshake, message routing, concurrent clients.
- Register, look up, and validate host actions by name with typed arguments (§20). Dispatch actions from any source through a unified action system wired to workspace and session managers.
- Enforce single-instance host with auto-start from CLI/UI, PID file, Windows Job Objects for child process cleanup, and clean shutdown on signal or idle timeout (§16).

#### W4: CLI controller

**Capability outcome:** The `wtd` CLI can drive the host for all specified commands — open, send, keys, capture, scrollback, follow, list, inspect, action — with human-readable and JSON output, structured exit codes, and semantic target addressing.

**Candidate beads:**

- Parse and validate all `wtd` CLI commands with structured subcommands, typed arguments, and global flags (§22). Unit tests cover all commands, argument validation, and help text.
- Resolve semantic target paths (e.g., `dev/server`, `dev/backend/server`) to runtime workspace objects (§19). Unit tests cover 1–4 segment paths, implicit workspace, ambiguity detection with candidate reporting.
- Connect to host via named pipe, send commands, receive responses, and format human-readable or JSON output with structured exit codes (§22.4–22.9). Integration tests cover connection, auto-start trigger, all output formats.
- Verify all `wtd` commands end-to-end against a running host with test workspace fixtures (§32.2). Each subcommand is tested. JSON output is validated. Exit codes are asserted.

#### W5: Rendering technology evaluation

**Capability outcome:** A rendering technology is chosen for the terminal UI with benchmarks and a written decision document. A minimal prototype demonstrates VT output rendering in a window.

This workset is a time-boxed spike (§7.9). It can proceed in parallel with Slices 1–2.

**Candidate beads:**

- Evaluate candidate renderers — wezterm components, Win32+DirectWrite, WebView2+xterm.js — against latency, memory, build complexity, and embeddability criteria. Produce a written decision document with benchmarks and a recommendation.
- Build a minimal prototype with the chosen renderer that displays VT output bytes in a window. This prototype is the foundation for W6.

#### W6: UI rendering pipeline

**Capability outcome:** Terminal content renders in panes within windows with tabs. The UI connects to the host via IPC, receives VT output streams, and displays them. Tab strip, pane splitters, status bar, and failed pane states are all rendered.

Gated on W5 (rendering technology decision).

**Candidate beads:**

- Create native windows with a tab strip that supports tab switching, tab creation, tab close, and tab reorder (§24.2–24.3).
- Render pane layout areas with draggable splitter bars, pane borders, and focus indicators that reflect the host's layout tree (§24.4).
- Render terminal content in pane viewports — character grid with cursor, colors, attributes, and alternate screen support (§24.4, §14.5 VT compliance).
- Connect the UI to the host via IPC: attach to workspace, receive SessionOutput streams, send SessionInput and PaneResize messages, receive state-change notifications (§13.9–13.13).
- Display failed/exited pane states with error message, exit code, and restart prompt (§24.5).
- Render the status bar showing workspace name, focused pane path, prefix-active indicator, and session state (§24.8).

#### W7: UI interaction model

**Capability outcome:** The terminal workspace is fully interactive. Keyboard input works (raw typing, single-stroke bindings, prefix chord sequences). Mouse interaction works (click focus, selection, splitter drag, scroll). Clipboard and command palette are functional.

**Candidate beads:**

- Classify keyboard input and route appropriately: raw terminal input to session, single-stroke bindings to action dispatch, prefix key to chord state (§21.1).
- Implement the prefix chord state machine with timeout, visual indicator, escape cancellation, and double-prefix passthrough (§21.3, §27.4).
- Handle mouse input: left-click pane focus, text selection with drag, splitter bar resize, scroll wheel through scrollback, and mouse reporting forwarding when enabled (§21.6).
- Implement clipboard operations: copy strips VT formatting, paste supports bracketed paste mode, copy-on-select is configurable (§24.7).
- Build the command palette with fuzzy search, action listing with keybinding hints, and target selection for targeted actions (§24.6).

### 37.3 Dependency structure

#### Workset dependencies

```
W1 (definition/config) ──┐
                          ├──► W3 (host/IPC) ──► W4 (CLI) ──► Slice 2 complete
W2 (terminal core) ──────┘         │
                                   └──► Slice 1 complete (with minimal W4)

W5 (rendering spike) ──► W6 (UI rendering) ──► W7 (UI interaction)
                                   │                    │
                                   └──► Slice 3         └──► Slice 4
```

W5 (rendering spike) has no dependency on W1–W4 and should start early, running in parallel with Slices 1–2.

#### Critical path

W1 → W3 (session manager, workspace instance manager) → W3 (IPC server) → W4 (CLI client) → Slice 1 → Slice 2.

The UI path is: W5 → W6 → W7 → Slice 3 → Slice 4.

#### Parallel paths

These can proceed concurrently:

- W1 (workspace definition parsing) and W2 (ConPTY wrapper, VT screen buffer, layout tree) have no mutual dependency.
- W5 (rendering spike) can run in parallel with all of Slices 1–2 work.
- Within W3, the IPC message type definitions can proceed in parallel with the session manager.
- Within W4, CLI argument parsing can proceed in parallel with the IPC client implementation.

#### Key bead-level dependencies

| Bead (outcome) | Depends on |
|----------------|------------|
| Profile resolution | Workspace definition parsing |
| Session manager | ConPTY wrapper, VT screen buffer |
| Workspace instance manager | Workspace definition parsing, profile resolution, layout tree, session manager |
| Named pipe IPC server | IPC message types |
| Action dispatcher | Action registry, workspace instance manager |
| Host lifecycle (single-instance, auto-start) | IPC server |
| CLI IPC client | IPC message types, IPC server running |
| Target path resolution | Layout tree, workspace instance model |
| CLI end-to-end tests | CLI parsing, CLI IPC client, host lifecycle |
| Window and tab chrome | Rendering spike decision |
| Pane layout rendering | Window and tab chrome |
| Terminal content rendering | Pane layout rendering, chosen renderer |
| UI-host IPC client | IPC message types, IPC server |
| Keyboard input pipeline | Terminal content rendering |
| Prefix chord state machine | Keyboard input pipeline |
| Mouse input | Terminal content rendering |
| Clipboard | Terminal content rendering (for selection) |
| Command palette | Window and tab chrome, action registry |

### 37.4 Risks and ambiguities

These must be resolved or accepted before bead generation to avoid vague or speculative beads.

**Rendering technology choice (§7.9).** The biggest open question. Gates all UI work (W6, W7). The rendering spike (W5) must complete before UI beads can be fully scoped. Bead generation for W6–W7 should use outcome-oriented phrasing that is renderer-agnostic, but implementation-level detail will depend on the chosen technology. Start the spike early.

**Wezterm component extraction feasibility.** The top candidate renderer (wezterm) requires extracting and embedding rendering components in a custom window/tab/pane framework. If extraction proves infeasible, the fallback is Win32+DirectWrite (high implementation effort) or WebView2+xterm.js (added runtime dependency). The spike must produce a clear go/no-go for each candidate.

**ConPTY edge cases.** ConPTY behavior varies across Windows versions (§6.1–6.2). Resize handling, VT passthrough completeness, and process tree termination via Job Objects may reveal issues on older builds. The ConPTY wrapper bead should include testing on the minimum supported version (build 17763).

**VT screen buffer correctness.** Full VT compliance (§14.5) is complex. The `vte` crate handles parsing, but the screen buffer state machine (cursor movement, scrolling regions, alternate screen, wide characters) is custom. Expect discovered work during implementation. Beads should be scoped to core correctness first, with edge cases tracked as follow-up beads.

**Throughput target validation.** The 100 MB/s per-session output target (§30.2) depends on the renderer's ability to keep up. This cannot be validated until Slice 3 is working. Performance validation beads belong after UI rendering is functional.

**Startup command timing.** The 100ms delay heuristic for startup commands (§17.4) is acknowledged as fragile. This is a known limitation, not a blocker for bead generation, but beads that exercise startup commands should note this.

**Global settings vs. workspace settings interaction.** The merge precedence rules (§11.6) for keybindings, profiles, and defaults have several layers. The W1 beads for settings and profile resolution should include explicit test cases for override chains.

### 37.5 Milestones

Milestones are mapped to slice completions. Each milestone has a concrete definition of done.

| Milestone | Slice | Definition of done |
|-----------|-------|-------------------|
| **M1: Headless round-trip** | Slice 1 | A workspace YAML is parsed, a ConPTY session is launched, input is sent via IPC, the screen buffer is populated, and `capture` returns the expected output. No UI required. |
| **M2: CLI-driven workspace** | Slice 2 | `wtd open dev` creates a workspace instance. `wtd list panes dev` shows all panes. `wtd send dev/server "echo hello"` delivers input. `wtd capture dev/server` returns output. `wtd inspect dev/server` shows metadata. `wtd close dev --kill` tears down cleanly. JSON output and exit codes are correct. |
| **M3: Rendering spike complete** | W5 | Decision document written with benchmarks. A minimal prototype renders VT output bytes in a window using the chosen renderer. |
| **M4: Visual terminal** | Slice 3 | A window displays tabs and split panes with live terminal content from the host. Tab switching works. Pane focus indicators are visible. The status bar shows workspace and pane information. |
| **M5: Interactive workspace** | Slice 4 | Typing works in panes. Single-stroke keybindings dispatch actions. Prefix chords work (`Ctrl+B,%` splits right). Mouse click changes pane focus. Text selection and copy/paste work. The command palette opens, searches, and dispatches actions. |
| **M6: Validated release** | Post-Slice 4 | All acceptance criteria (§36) pass. Performance targets (§30) are met. Logging is operational. Error messages are clear and complete. |

### 37.6 Recommended bead generation order

Generate beads in slice order. For each slice, generate beads from the contributing worksets in dependency order.

**First bead set (Slice 1):** Generate beads for W1 (parse, resolve), W2 (ConPTY wrapper, screen buffer), and the W3/W4 beads needed to close the headless round-trip (IPC message types, session manager, workspace instance manager, IPC server). This is the recommended first parent bead with 8–12 child beads.

**Second bead set (Slice 2):** Generate remaining W1 beads (settings, discovery), remaining W2 beads (layout tree, naming resolution), remaining W3 beads (action dispatcher, host lifecycle), and all W4 beads (CLI parsing, CLI client, end-to-end tests).

**Third bead set (parallel):** Generate W5 beads (rendering spike). These can be generated and executed at any time, independent of Slices 1–2.

**Fourth bead set (Slice 3):** Generate W6 beads after W5 completes and the rendering technology is decided.

**Fifth bead set (Slice 4):** Generate W7 beads after W6 beads are substantially complete.

**Validation beads:** Generate after Slice 4 for end-to-end acceptance testing, performance validation, and error message review. These are cross-cutting and draw from §30, §32, and §36.

---

*End of specification.*