# WTD tmux Shim

`wtd-tmux` is an opt-in compatibility shim for tmux-oriented agent tooling. It
translates a small orchestration subset into native WTD commands. It does not
start tmux and does not create nested tmux panes.

## Supported Subset

| tmux command | WTD translation |
|--------------|-----------------|
| `split-window -h -t <target>` | `wtd action <target> split-right` |
| `split-window -v -t <target>` | `wtd action <target> split-down` |
| `select-pane -t <target>` | `wtd focus <target>` |
| `send-keys -t <target> <text...> C-m` | `wtd prompt <target> <text>` |
| `send-keys -t <target> <text...>` | `wtd send <target> <text>` |
| `list-panes -t <workspace>` | `wtd list panes <workspace>` |
| `capture-pane -p -t <target> [-S -N]` | `wtd capture <target> [--lines N]` |

Unsupported commands fail with exit code `2` and usage text.

## Examples

```bash
tools/tmux-shim/wtd-tmux.sh split-window -h -t agents/main/worker
tools/tmux-shim/wtd-tmux.sh send-keys -t agents/main/worker "run tests" C-m
tools/tmux-shim/wtd-tmux.sh capture-pane -p -t agents/main/worker -S -80
```

PowerShell:

```powershell
tools/tmux-shim/wtd-tmux.ps1 split-window -h -t agents/main/worker
tools/tmux-shim/wtd-tmux.ps1 send-keys -t agents/main/worker "run tests" C-m
tools/tmux-shim/wtd-tmux.ps1 capture-pane -p -t agents/main/worker -S -80
```

Use `--what-if` or `-WhatIf` to validate translations without contacting a WTD
host.

## Representative Agent Workflow

```bash
tools/tmux-shim/wtd-tmux.sh split-window -h -t agents/main/coordinator
tools/tmux-shim/wtd-tmux.sh send-keys -t agents/main/worker "Run tests and publish WTD status" C-m
wtd wait agents/main/worker --for done --timeout 120
tools/tmux-shim/wtd-tmux.sh capture-pane -p -t agents/main/worker -S -120
tools/tmux-shim/wtd-tmux.sh select-pane -t agents/main/reviewer
```

This keeps pane creation, input, capture, focus, and wait coordination in WTD.
