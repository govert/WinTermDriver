# Restore Regression Suite

This suite guards the reopen, restart, reattach, and UI rehydration paths that
preserve long-running workspaces.

Run the focused checks with:

```bash
cargo test -p wtd-host --test test_file_path_open open_split_workspace_with_file_path
cargo test -p wtd-host --test test_attach_snapshot attach_includes_session_screen_snapshots
cargo test -p wtd-host --test test_attach_snapshot attach_includes_session_scrollback_history
cargo test -p wtd-host backoff_delay_progression
cargo test -p wtd-host --lib save_reconstructs_definition
cargo test -p wtd-ui --lib rebuild_snapshot_restores_multi_pane_layout_identity_and_buffers
```

Coverage map:

| Flow | Check |
|------|-------|
| Reopen | `open_split_workspace_with_file_path` saves through the UI-style `InvokeAction`, closes, reopens the saved YAML, and verifies both pane identities survive. |
| Saved focus | `save_reconstructs_definition` verifies saved YAML preserves the focused pane for each tab. |
| Restart | `backoff_delay_progression` verifies restart backoff progression without depending on ConPTY spawn availability. `session_test::restart_on_failure_with_backoff` covers the full relaunch path when the local test environment can spawn a PTY. |
| Reattach | `attach_includes_session_screen_snapshots` and `attach_includes_session_scrollback_history` verify attach snapshots contain replayable visible buffers and retained history. |
| Rehydrate | `rebuild_snapshot_restores_multi_pane_layout_identity_and_buffers` verifies the UI rebuilds multi-pane layout, focus, visible buffers, and scrollback from an attach snapshot. |
