# WinTermDriver

A Windows-native terminal workspace manager for defining, launching, viewing, and controlling collections of console sessions arranged into windows, tabs, and panes.

WinTermDriver combines three ideas:

1. A real terminal UI with tabs and split panes that feel close to Windows Terminal.
2. A persistent workspace model that can save and recreate named layouts and session launch definitions.
3. A controller plane that can drive any pane or session programmatically without breaking ordinary interactive use.

## Status

Engineering specification phase. See [WINTERMDRIVER_SPEC.md](WINTERMDRIVER_SPEC.md) for the full design.

## Key concepts

- **Workspace definitions** are human-editable YAML files that describe windows, tabs, panes, profiles, and keybindings. They are version-controllable and deterministically recreatable.
- **Semantic naming** lets you address panes by role (`dev/server`, `ops/prod-logs`) rather than positional IDs.
- **Controller CLI** (`wtd`) can send text, send keys, capture output, and invoke actions on any named pane — without interrupting interactive use.
- **Prefix chords** provide tmux-like keyboard navigation (`Ctrl+B,%` to split, `Ctrl+B,o` to cycle focus).

## Architecture

Three processes with clear boundaries:

| Process | Role |
|---------|------|
| `wtd-host` | Per-user background process. Owns ConPTY sessions, screen buffers, workspace instances, and the IPC server. |
| `wtd-ui` | Graphical terminal window. Renders tabs, panes, and terminal content. Connects to the host via named pipe. |
| `wtd` | CLI controller. Short-lived commands that drive the host: open, send, capture, list, inspect. |

## Platform

- Windows 10 1809+ (build 17763+)
- Rust (stable toolchain, MSVC target)
- ConPTY backend via `windows-rs`

## License

[MIT](LICENSE.md)
