# WinTermDriver

A Windows-native terminal workspace manager for defining, launching, viewing, and controlling collections of console sessions arranged into windows, tabs, and panes.

WinTermDriver combines three ideas:

1. A real terminal UI with tabs and split panes that feel close to Windows Terminal.
2. A persistent workspace model that can save and recreate named layouts and session launch definitions.
3. A controller plane that can drive any pane or session programmatically without breaking ordinary interactive use.

## Status

Initial implementation complete (all 68 beads closed). The codebase has 745 tests across 7 crates. Integration and end-to-end validation are in progress.

See [WINTERMDRIVER_SPEC.md](WINTERMDRIVER_SPEC.md) for the full engineering specification.

## Key Concepts

- **Workspace definitions** are human-editable YAML files that describe windows, tabs, panes, profiles, and keybindings. They are version-controllable and deterministically recreatable.
- **Semantic naming** lets you address panes by role (`dev/server`, `ops/prod-logs`) rather than positional IDs.
- **Controller CLI** (`wtd`) can send text, send keys, capture output, and invoke actions on any named pane — without interrupting interactive use.
- **Prefix chords** provide tmux-like keyboard navigation (`Ctrl+B,%` to split, `Ctrl+B,o` to cycle focus).

## Architecture

Three processes with clear boundaries:

| Process | Role |
|---------|------|
| `wtd-host` | Per-user background process. Owns ConPTY sessions, screen buffers, workspace instances, and the IPC server. |
| `wtd-ui` | Graphical terminal window. Renders tabs, panes, and terminal content via Direct2D/DirectWrite. Connects to the host via named pipe. |
| `wtd` | CLI controller. Short-lived commands that drive the host: open, send, capture, list, inspect. |

## Crate Structure

```
crates/
  wtd-core/         Shared types: workspace definitions, layout tree, profile resolver, global settings
  wtd-ipc/          IPC message types and named pipe framing (4-byte LE + JSON)
  wtd-pty/          ConPTY wrapper, VT screen buffer with scrollback
  wtd-host/         Host process: session manager, workspace instances, IPC server, action dispatcher
  wtd-ui/           UI process: window/tab/pane rendering, input handling, command palette
  wtd-cli/          CLI controller (produces the `wtd` binary)
  eval-renderer/    Renderer evaluation benchmarks (ADR-001)
```

## Prerequisites

- Windows 10 1809+ (build 17763+)
- [Rust](https://rustup.rs/) stable toolchain, MSVC target
- `rust-toolchain.toml` pins the target automatically

## Build

```bash
cargo build --workspace
```

This produces three binaries in `target/debug/`:
- `wtd-host.exe` — the background host process
- `wtd-ui.exe` — the graphical UI
- `wtd.exe` — the CLI controller

## Test

```bash
cargo test --workspace
```

745 tests across all crates. Some integration tests in `wtd-pty` spawn real ConPTY sessions (requires Windows).

## Run

Not yet runnable as a complete application. Individual components can be exercised through their tests and the CLI:

```bash
# Build and run the CLI (shows usage)
cargo run --bin wtd -- --help

# Run host tests (includes ConPTY integration tests)
cargo test -p wtd-host

# Run UI tests
cargo test -p wtd-ui
```

## Project Management

This project uses the [beads working method](docs/operations/BEADS_WORKING_METHOD.md) for task tracking. See [AGENTS.md](AGENTS.md) for AI agent workflow instructions and the bead runner documentation.

## License

[MIT](LICENSE.md)
