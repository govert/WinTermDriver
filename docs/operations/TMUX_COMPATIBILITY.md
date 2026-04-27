# tmux Compatibility Shim

WTD provides a minimal tmux-oriented shim in `tools/tmux-shim/` for agent tools
that expect a few tmux pane orchestration commands. The shim translates commands
to native WTD operations and fails clearly for unsupported tmux features.

See `tools/tmux-shim/README.md` for the supported subset and examples.
Run `tools/tmux-shim/test-wtd-tmux.ps1` to validate the repeatable shim
translation harness.

The first supported subset is intentionally narrow:

- pane split creation
- pane focus
- send prompt/input
- pane listing
- pane capture

Unsupported tmux concepts such as sessions, layouts, options, copy mode,
renumbering, hooks, and status-line configuration are not emulated by this shim.
Use native WTD workspaces, recipes, actions, and `wtd wait` for those workflows.
