# Profile and Keybinding Discovery

This note summarizes the UI and YAML paths operators should use when they need
to discover or customize launch profiles and shortcuts.

## Profile Flows

- `new-tab`, `split-right`, and `split-down` open a profile selector in `wtd-ui`
  when run without a `profile=` argument.
- `change-profile` relaunches the focused pane with a selected profile.
- CLI automation can bypass the selector with explicit action arguments:

```bash
wtd action dev/server split-right profile=cmd
wtd action dev/server split-down profile=wsl
wtd action dev/server change-profile profile=powershell
```

Workspace-local profiles override global profiles with the same name. Built-in
profile types are `powershell`, `cmd`, `wsl`, `ssh`, and `custom`.

## Keybinding Presets

The default preset is `windows-terminal`. It is single-stroke only and includes
common Windows Terminal-compatible shortcuts:

- `Ctrl+Shift+P` opens the command palette.
- `Ctrl+Shift+F` starts focused-pane search; `F3` moves to the next match.
- `Ctrl+Shift+A` selects the focused pane viewport.
- `Ctrl+Shift+M` enters keyboard mark mode.
- `Ctrl+Shift+Up/Down/PageUp/PageDown/Home/End` navigates scrollback.
- `Alt+Shift+K` passes the next keypress directly to the app.

The `tmux` preset keeps the `Ctrl+B` prefix workflow. It includes prefix chords
for split/focus/tab actions plus search, selection, and scrollback navigation.

## Customization

Use global settings for user-wide defaults and workspace YAML for project-local
overrides:

```yaml
bindings:
  preset: windows-terminal
  keys:
    Ctrl+Shift+Space: toggle-command-palette
    Ctrl+Alt+F: find
  chords:
    f: find
```

Use `pass-through-next-key` when an app needs a key that WTD normally captures.
With the default preset, press `Alt+Shift+K`, then press the app key.
