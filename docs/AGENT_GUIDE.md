# WinTermDriver Agent Guide

How to use WinTermDriver programmatically from an AI agent, script, or any automation tool.

## Architecture

WinTermDriver runs as three processes:

| Process | Binary | Role |
|---------|--------|------|
| **Host** | `wtd-host.exe` | Per-user background daemon. Owns ConPTY sessions, screen buffers, workspace instances, and the IPC server. Auto-starts on first connection. |
| **CLI** | `wtd.exe` | Short-lived controller. Every `wtd` command connects to the host, sends one request, prints the result, and exits. This is the primary interface for agents. |
| **UI** | `wtd-ui.exe` | Graphical window with tabs and split panes. Optional — agents typically don't need it. |

All three communicate over a single Windows named pipe (`\\.\pipe\wtd-{SID}`),
restricted to the current user's SID. The host also verifies the connecting
process SID before accepting a protocol handshake and reports
`same-user-local` in `HandshakeAck.accessPolicy`. Same-user CLI/UI automation
does not need tokens; remote, relay, or cross-user access is not enabled unless
a future protocol capability explicitly adds it. The host auto-starts when the
CLI or UI first connects, so agents never need to launch it manually.

## Getting started

### Prerequisites

- Windows 10 version 1809+ (ConPTY support)
- `wtd.exe` and `wtd-host.exe` on your PATH (or in the same directory)

### Verify the toolchain

```bash
wtd --version
wtd host status
```

If the host isn't running, any `wtd` command will start it automatically.

## Workspace YAML for automation

A workspace definition is a YAML file that declares named panes, each running a shell session. The key principle for agent-friendly workspaces: **give every pane a semantic name** that describes its role.

### Minimal single-pane workspace

```yaml
version: 1
name: agent-task
tabs:
  - name: main
    layout:
      type: pane
      name: shell
      session:
        profile: cmd
```

### Multi-pane workspace for a typical agent workflow

```yaml
version: 1
name: build-and-test
description: "Run server in one pane, test in another"
tabs:
  - name: dev
    layout:
      type: split
      orientation: horizontal
      ratio: 0.5
      children:
        - type: pane
          name: server
          session:
            profile: powershell
            cwd: "C:\\src\\myapp"
            startupCommand: "dotnet watch run"
        - type: pane
          name: tests
          session:
            profile: powershell
            cwd: "C:\\src\\myapp"
    focus: tests
```

### Deeper splits

Splits nest to any depth. Each split has exactly two children and a `ratio` (default 0.5, range 0.1-0.9).

```yaml
version: 1
name: ops
tabs:
  - name: monitoring
    layout:
      type: split
      orientation: horizontal
      children:
        - type: pane
          name: logs
          session:
            profile: powershell
            startupCommand: "Get-Content C:\\logs\\app.log -Tail 50 -Wait"
        - type: split
          orientation: vertical
          children:
            - type: pane
              name: metrics
              session:
                profile: powershell
            - type: pane
              name: control
              session:
                profile: powershell
```

### File placement

Workspace definitions are found in this order:

1. Explicit `--file <path>` argument
2. `.wtd/<name>.yaml` in the current working directory (project-local)
3. `%APPDATA%\WinTermDriver\workspaces\<name>.yaml` (user-global)

Extensions `.yaml`, `.yml`, and `.json` are searched in that order.

For automation, use `--file` for full control:

```bash
wtd open agent-task --file C:\automation\workspaces\agent-task.yaml
```

### Profile types

| Type | Description | Key fields |
|------|-------------|------------|
| `powershell` | PowerShell (pwsh.exe or powershell.exe) | `executable`, `args` |
| `cmd` | Command Prompt | `executable`, `args` |
| `wsl` | Windows Subsystem for Linux | `distribution` |
| `ssh` | Remote SSH session | `host`, `user`, `port`, `identityFile` |
| `custom` | Arbitrary executable | `executable` (required), `args` |

If no profile is specified, the global default (`powershell`) is used.

## Core CLI commands for agents

### Opening and closing workspaces

```bash
# Open and attach the UI in one step
wtd start build-and-test

# Open from project-local or user-global definition
wtd open build-and-test

# Open from explicit file path
wtd open build-and-test --file ./workspace.yaml

# Tear down and recreate from the same definition
wtd open build-and-test --recreate

# Persist the running layout and pane/session definitions
wtd save build-and-test --file ./saved-workspace.yaml

# Close and destroy the instance
wtd close build-and-test --kill
```

`wtd start <name>` is the ergonomic entry point for interactive use. `wtd open` remains the lower-level command for headless automation or when you want to launch `wtd-ui` separately.
The UI command palette action `save-workspace` uses the same host save path as
`wtd save`, so saving from the UI or CLI writes a YAML definition that can be
opened again with the same tab layout, pane names, and session definitions.

### Sending commands

```bash
# Send raw text followed by a trailing carriage return
wtd send build-and-test/tests "cargo test --lib"

# Send text without a trailing newline
wtd send build-and-test/tests "partial input" --no-newline

# Send a pane-aware prompt
wtd prompt build-and-test/tests "Summarize the failing tests"

# Send key sequences (Ctrl+C, Enter, function keys, etc.)
wtd keys build-and-test/server Ctrl+C
```

Use `wtd prompt` for interactive agent CLIs such as Codex, Claude Code, Gemini CLI, and Copilot CLI. It uses the pane's driver profile to prepare the composer, expand multiline input, and submit safely. Keep `wtd send` for low-level shell input and literal text injection.

The shortest agent-safe workflow to remember is:

1. `wtd prompt <pane> "<prompt text>"` to write
2. `wtd capture <pane>` to read what is on screen now
3. `wtd configure-pane <pane> ...` only when you need to override the inferred driver

If you are driving a coding agent, prefer `prompt` and `capture`. Treat `send` as the low-level shell/text primitive.

Agent panes launched directly as `pi`, `codex`, `claude`, `gemini`, or `copilot` are auto-detected, so the common case is just `prompt` and `capture`.

For launch profiles, `wtd-ui` now keeps the path simple: creating a new tab or split opens a profile selector, and the command palette exposes `change-profile` to relaunch the focused pane with a different launch profile.

For one-shot shortcut bypass, use the `pass-through-next-key` action. In the default `windows-terminal` preset it is bound to `Alt+Shift+K`, which arms the focused pane so the next keypress goes to the app instead of being handled as a WTD shortcut.

WTD-launched sessions also advertise a Windows Terminal-compatible terminal identity (`TERM_PROGRAM=Windows_Terminal`, `WT_SESSION`, `WT_PROFILE_ID`, `COLORTERM=truecolor`) and expose `WTD_WORKSPACE`, `WTD_PANE`, and `WTD_SESSION_ID` for WTD-specific detection.

For agent-aware panes, WTD also exports a capability-oriented contract:
- `WTD_AGENT_HOST=1` — session is hosted by WTD in agent-aware mode
- `WTD_AGENT_DRIVER` — resolved built-in driver profile such as `plain`, `pi`, `codex`, `claude-code`, `gemini-cli`, or `copilot-cli`
- `WTD_AGENT_MULTILINE_MODE` — prompt multiline strategy (`reject`, `soft-break-key`, or `literal-paste`)
- `WTD_AGENT_PASTE_MODE` — prompt paste strategy (`plain` or `bracketed-if-enabled`)
- `WTD_AGENT_SUBMIT_KEY` — key used to submit the composed prompt
- `WTD_AGENT_SOFT_BREAK_KEY` — optional soft-break key when multiline entry is supported without submission
- `WTD_AGENT_HYPERLINKS` — hyperlink capability identifier, currently `osc8`
- `WTD_AGENT_IMAGES` — inline image capability identifier, currently `kitty-placeholder`

For full probe-driven compatibility diagnostics, see `docs/AGENT_HOST_COMPATIBILITY.md` and `tools/run-agent-host-diagnostics.ps1`.

Hosted agents can publish pane attention and completion state for the UI and
inspection tools:

```bash
wtd notify build-and-test/tests --source pi "input requested"
wtd notify build-and-test/tests --state done --source codex "tests passed"
wtd notify build-and-test/tests --state error --source codex "tests failed"
wtd clear-attention build-and-test/tests
wtd status build-and-test/tests --phase working --source codex --queue-pending 1 "running tests"
```

Attention states are `active`, `needs-attention`, `done`, and `error` in the
CLI. The protocol uses snake_case (`needs_attention`). Terminal applications can
also raise attention with OSC 9 or OSC 777 `notify` sequences; WTD records those
as `needs_attention` from source `osc`.

Use `wtd status` for durable pane metadata that should appear in inspect and
attach snapshots: phase, status text, queue count, completion marker, and source.
WTD also includes runtime metadata such as driver profile, cwd, and terminal
progress when available.

Coordinators can wait on those states without screen polling:

```bash
wtd wait build-and-test/tests --for done --timeout 60
wtd wait build-and-test/tests --for needs-attention --recent-lines 80
wtd wait build-and-test/tests --for queue-empty --timeout 30
```

`wtd wait` returns the matched condition and current metadata on success. On
timeout it exits with the timeout code and still prints a snapshot with attention
state, metadata, and recent output so the coordinator can decide whether to
prompt, retry, or surface the pane to a user.

In the UI, use the command palette actions `toggle-pane-metadata-list`,
`filter-pane-list-attention`, `filter-pane-list-status`,
`filter-pane-list-driver`, `filter-pane-list-cwd`, and
`filter-pane-list-branch` to surface pane metadata in the status area. The list
includes pane path, attention state, published phase/status/queue/source, and
runtime driver profile, cwd, or branch when those fields are available in the
attach snapshot. Filtered lists are sorted by the selected metadata field.

For reusable Pi, Codex, Claude Code, Gemini CLI, and Copilot CLI hook patterns,
see `docs/operations/AGENT_NOTIFICATION_HOOKS.md` and the helper scripts in
`tools/agent-hooks/`. For Pi-specific extension wiring, see
`docs/operations/PI_WTD_EXTENSION_PATTERN.md` and
`tools/agent-hooks/pi/`.

Built-in prompt driver profiles:

| Profile | Submit key | Multiline strategy | Notes |
|---------|------------|--------------------|-------|
| `plain` | `Enter` | rejected | Default shell-like behavior |
| `codex` | `Enter` | terminal-style multiline paste, then submit | Replaces the current draft first and matches the working `Ctrl+Shift+V` path in `wtd-ui` |
| `pi` | `Enter` | `Shift+Enter` soft breaks | First-class pi host behavior |
| `claude-code` | `Enter` | `Shift+Enter` soft breaks | Multiline supported |
| `gemini-cli` | `Enter` | `Shift+Enter` soft breaks | Multiline supported |
| `copilot-cli` | `Enter` | `Shift+Enter` soft breaks | Multiline supported |

### Capturing output

```bash
# Capture the visible screen content (what you'd see in the terminal)
wtd capture build-and-test/tests

# Capture with JSON envelope for machine parsing
wtd capture build-and-test/tests --json

# Get the last N lines from scrollback history
wtd scrollback build-and-test/tests --tail 200
```

### Listing and inspecting

```bash
# List available workspace definitions (on disk + running)
wtd list workspaces

# List running workspace instances
wtd list instances

# List all panes in a workspace
wtd list panes build-and-test

# List all sessions with their states
wtd list sessions build-and-test

# Full metadata for a specific pane
wtd inspect build-and-test/tests
```

### Invoking actions

```bash
# Split the focused pane
wtd action build-and-test/server split-right

# Split the focused pane using an explicit launch profile
wtd action build-and-test/server split-right profile=cmd

# Close a specific pane
wtd action build-and-test/tests close-pane

# Relaunch a pane with a different launch profile
wtd action build-and-test/server change-profile profile=wsl

# Resize a pane
wtd action build-and-test/server resize-pane-right cells=5
```

### Target paths

Panes are addressed by semantic names, not positional IDs:

| Path form | Example | When to use |
|-----------|---------|-------------|
| `pane` | `server` | Only one workspace is running |
| `workspace/pane` | `build-and-test/server` | Pane name is unique in the workspace |
| `workspace/tab/pane` | `build-and-test/dev/server` | Pane name appears in multiple tabs |

For automation, always use at least `workspace/pane` to avoid ambiguity.

## JSON output for machine consumption

Every command supports `--json` for structured output:

```bash
wtd capture build-and-test/tests --json
```

```json
{
  "text": "C:\\src\\myapp> cargo test --lib\n  Compiling myapp v0.1.0\n    Running unittests\ntest result: ok. 42 passed; 0 failed\n\nC:\\src\\myapp> "
}
```

```bash
wtd list panes build-and-test --json
```

```json
{
  "panes": [
    { "name": "server", "tab": "dev", "sessionState": "running" },
    { "name": "tests", "tab": "dev", "sessionState": "running" }
  ]
}
```

Error responses in JSON mode:

```json
{
  "code": "target-not-found",
  "message": "pane 'typo' not found in workspace 'build-and-test'",
  "candidates": ["build-and-test/dev/server", "build-and-test/dev/tests"]
}
```

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Target or workspace not found |
| 3 | Ambiguous target (multiple matches) |
| 4 | Host failed to start |
| 5 | Workspace definition error (invalid YAML, validation failure) |
| 6 | Connection error (pipe unavailable) |
| 10 | Request timeout |

Check `$?` (bash) or `$LASTEXITCODE` (PowerShell) after each command.

## Timing and polling patterns

ConPTY sessions are real processes. Output arrives asynchronously. An agent must **poll** for expected results rather than assuming output is immediately available after sending a command.

### Output fencing with markers

The most reliable pattern: send a command that produces a known marker, then poll `capture` until the marker appears.

```bash
# Send a command with a unique fence marker
wtd send myws/shell "echo DONE_12345"

# Poll until the marker appears on screen
while true; do
    output=$(wtd capture myws/shell)
    if echo "$output" | grep -q "DONE_12345"; then
        break
    fi
    sleep 0.2
done
```

A tighter loop with timeout:

```bash
poll_for() {
    local target="$1" marker="$2" timeout="${3:-30}" interval="${4:-0.2}"
    local elapsed=0
    while (( $(echo "$elapsed < $timeout" | bc -l) )); do
        if wtd capture "$target" | grep -q "$marker"; then
            return 0
        fi
        sleep "$interval"
        elapsed=$(echo "$elapsed + $interval" | bc -l)
    done
    return 1  # timed out
}

wtd send myws/shell "cargo build 2>&1 && echo BUILD_OK || echo BUILD_FAIL"
if poll_for myws/shell "BUILD_OK" 120; then
    echo "Build succeeded"
elif poll_for myws/shell "BUILD_FAIL" 1; then
    echo "Build failed"
fi
```

### Recommended polling intervals

| Scenario | Interval | Timeout |
|----------|----------|---------|
| Fast command (echo, cd, ls) | 100-200ms | 5s |
| Build or compile | 200-500ms | 120s+ |
| Server startup | 500ms-1s | 30-60s |
| SSH connection | 1s | 30s |

### Using scrollback for long output

`capture` returns only the visible screen (typically 24-80 lines). For commands that produce more output, use `scrollback`:

```bash
wtd send myws/shell "cargo test 2>&1"
# Wait for completion marker
poll_for myws/shell "test result:"
# Then grab full output from scrollback
wtd scrollback myws/shell --tail 500
```

## Orchestrating multi-session workflows

### Example: start server, wait for ready, run tests, capture results

```bash
#!/usr/bin/env bash
set -euo pipefail

WORKSPACE="ci-run"
YAML="$(dirname "$0")/ci-workspace.yaml"

# Helper: poll capture for a marker
poll_for() {
    local target="$1" marker="$2" timeout="${3:-30}"
    local elapsed=0
    while (( $(echo "$elapsed < $timeout" | bc -l) )); do
        if wtd capture "$target" 2>/dev/null | grep -q "$marker"; then
            return 0
        fi
        sleep 0.3
        elapsed=$(echo "$elapsed + 0.3" | bc -l)
    done
    echo "Timed out waiting for '$marker' in $target" >&2
    return 1
}

# 1. Open the workspace
wtd open "$WORKSPACE" --file "$YAML"

# 2. Wait for both panes to be ready (shell prompt appears)
poll_for "$WORKSPACE/server" ">" 10
poll_for "$WORKSPACE/tests" ">" 10

# 3. Start the server
wtd send "$WORKSPACE/server" "cd C:\\src\\myapp && dotnet run"
poll_for "$WORKSPACE/server" "Now listening on" 30

# 4. Run tests against the running server
wtd send "$WORKSPACE/tests" "cd C:\\src\\myapp && dotnet test 2>&1 && echo TESTS_DONE"
poll_for "$WORKSPACE/tests" "TESTS_DONE" 120

# 5. Capture test results
RESULTS=$(wtd scrollback "$WORKSPACE/tests" --tail 200)
echo "$RESULTS"

# 6. Stop the server
wtd keys "$WORKSPACE/server" Ctrl+C
sleep 1

# 7. Clean up
wtd close "$WORKSPACE" --kill

# 8. Parse results
if echo "$RESULTS" | grep -q "Failed:  0"; then
    echo "All tests passed"
    exit 0
else
    echo "Some tests failed"
    exit 1
fi
```

The workspace YAML for this script:

```yaml
# ci-workspace.yaml
version: 1
name: ci-run
tabs:
  - name: main
    layout:
      type: split
      orientation: horizontal
      children:
        - type: pane
          name: server
          session:
            profile: cmd
        - type: pane
          name: tests
          session:
            profile: cmd
```

### Example: parallel builds across multiple panes

```bash
#!/usr/bin/env bash
set -euo pipefail

# Open workspace with 3 panes
wtd open parallel-build --file ./parallel-build.yaml

# Kick off builds concurrently
wtd send parallel-build/frontend "cd C:\\src\\frontend && npm run build 2>&1 && echo FRONT_OK || echo FRONT_FAIL"
wtd send parallel-build/backend  "cd C:\\src\\backend && cargo build --release 2>&1 && echo BACK_OK || echo BACK_FAIL"
wtd send parallel-build/docs     "cd C:\\src\\docs && mkdocs build 2>&1 && echo DOCS_OK || echo DOCS_FAIL"

# Wait for all three
FAILED=0
for pane in frontend backend docs; do
    MARKER="${pane^^}_OK"   # e.g. FRONTEND_OK
    FAIL_MARKER="${pane^^}_FAIL"
    if poll_for "parallel-build/$pane" "$MARKER" 300; then
        echo "$pane: passed"
    else
        echo "$pane: failed or timed out"
        FAILED=1
    fi
done

# Grab logs from any failures
if [ "$FAILED" -eq 1 ]; then
    for pane in frontend backend docs; do
        wtd scrollback "parallel-build/$pane" --tail 100 > "${pane}-build.log"
    done
fi

wtd close parallel-build --kill
exit $FAILED
```

## Error handling

### Session failures and restarts

Sessions can exit unexpectedly. Check pane state before sending commands:

```bash
# Check if sessions are healthy
STATE=$(wtd list sessions myws --json)
echo "$STATE" | jq '.sessions[] | select(.state != "running")'

# Inspect a specific pane for details
wtd inspect myws/server --json | jq '.state'
```

Restart policies are configured per workspace:

| Policy | Behaviour |
|--------|-----------|
| `never` | Session stays exited. Agent must detect and handle. |
| `on-failure` | Auto-restarts on non-zero exit. Exponential backoff (500ms to 30s). |
| `always` | Auto-restarts on any exit. |

For agent workflows, `never` is usually best — it gives the agent full control over when to retry.

### Handling common errors

```bash
# Target not found (exit code 2) — typo or pane was closed
wtd send myws/typo "hello"
if [ $? -eq 2 ]; then
    echo "Pane not found. Available panes:"
    wtd list panes myws
fi

# Workspace not found (exit code 2)
wtd open nonexistent
if [ $? -eq 2 ]; then
    echo "Workspace definition not found. Check .wtd/ or --file path."
fi

# Ambiguous target (exit code 3) — name exists in multiple tabs
wtd send myws/shell "hello"
if [ $? -eq 3 ]; then
    echo "Ambiguous. Use full path: workspace/tab/pane"
    wtd list panes myws
fi

# Timeout (exit code 10)
wtd capture myws/shell --timeout 5
if [ $? -eq 10 ]; then
    echo "Host not responding. Check: wtd host status"
fi
```

### Recovering from stuck sessions

```bash
# Send Ctrl+C to interrupt a hung process
wtd keys myws/shell Ctrl+C
sleep 1

# If that doesn't work, restart the session via action
wtd action myws/shell restart-session

# Nuclear option: recreate the entire workspace
wtd open myws --recreate
```

## Dynamic layout manipulation

Agents can modify the pane layout at runtime:

```bash
# Split a pane to create a new one
wtd action myws/server split-right
# The new pane gets an auto-generated name (e.g. "pane-3")

# List panes to find the new one
wtd list panes myws

# Rename it for clarity
wtd rename myws/pane-3 debugger

# Use the new pane
wtd send myws/debugger "gdb ./myapp"

# Close it when done
wtd action myws/debugger close-pane
```

Available layout actions:

| Action | Description |
|--------|-------------|
| `split-right` | Split target pane horizontally |
| `split-down` | Split target pane vertically |
| `close-pane` | Close target pane and its session |
| `zoom-pane` | Toggle zoom (pane fills entire tab area) |
| `focus-next-pane` | Focus the next pane in tab order |
| `focus-prev-pane` | Focus the previous pane |
| `focus-pane-up` | Focus the pane above |
| `focus-pane-down` | Focus the pane below |
| `focus-pane-left` | Focus the pane to the left |
| `focus-pane-right` | Focus the pane to the right |
| `swap-pane-up` | Swap the focused pane with the nearest pane above |
| `swap-pane-down` | Swap the focused pane with the nearest pane below |
| `swap-pane-left` | Swap the focused pane with the nearest pane on the left |
| `swap-pane-right` | Swap the focused pane with the nearest pane on the right |
| `toggle-split-orientation` | Toggle the nearest ancestor split orientation |
| `equalize-pane-split` | Reset the nearest ancestor split to an even ratio |
| `equalize-tab` | Reset all tab splits to even ratios |
| `retile-even-horizontal` | Retile the tab into an even left-to-right layout |
| `retile-even-vertical` | Retile the tab into an even top-to-bottom layout |
| `retile-grid` | Retile the tab into a near-square grid |
| `retile-main-left` | Retile the tab with the focused pane as the main left pane |
| `retile-main-right` | Retile the tab with the focused pane as the main right pane |
| `retile-main-top` | Retile the tab with the focused pane as the main top pane |
| `retile-main-bottom` | Retile the tab with the focused pane as the main bottom pane |
| `resize-pane-right` | Move the nearest vertical splitter right (`cells=N`) |
| `resize-pane-left` | Move the nearest vertical splitter left (`cells=N`) |
| `resize-pane-down` | Move the nearest horizontal splitter down (`cells=N`) |
| `resize-pane-up` | Move the nearest horizontal splitter up (`cells=N`) |
| `resize-pane-grow-right` | Grow the focused pane rightward (`cells=N`) |
| `resize-pane-grow-down` | Grow the focused pane downward (`cells=N`) |
| `resize-pane-shrink-right` | Shrink the focused pane from the right (`cells=N`) |
| `resize-pane-shrink-down` | Shrink the focused pane from the bottom (`cells=N`) |
| `rename-pane` | Rename a pane (`name=new-name`) |
| `restart-session` | Restart the session in a pane |

The initial rearrangement and retile rollout is **command-palette first**. No new default single-stroke bindings are assigned for swap/equalize/retile actions yet; users can bind them explicitly in workspace or global bindings if desired.

## Best practices

1. **Always use `--json` for machine parsing.** Text output is for humans and may change format between versions.

2. **Use unique fence markers** when polling for command completion. Include a random or sequential suffix (e.g., `DONE_$(date +%s)`) to avoid matching stale output from a previous command.

3. **Poll `capture`, not `follow`.** `capture` is a point-in-time snapshot (stateless). `follow` is a streaming connection that requires lifecycle management. `capture` with polling is simpler and more robust for most agent patterns.

4. **Give every pane a semantic name** in your workspace YAML. Positional addressing is fragile. `myws/server` is self-documenting; `myws/pane-2` is not.

5. **Use `workspace/pane` paths** (not bare pane names) in automation. Bare names require exactly one running workspace instance, which is fragile.

6. **Check exit codes after every command.** Non-zero means the action did not complete. Don't assume success.

7. **Send `Ctrl+C` before reusing a pane** for a new task, to make sure the previous command has exited.

8. **Use `scrollback --tail N`** for long output. `capture` only returns the visible screen (~24-80 lines). Scrollback preserves up to 10,000 lines by default (configurable via `scrollbackLines`).

9. **Clean up with `wtd close <name> --kill`** when done. This terminates all sessions and releases resources. Leaked host instances consume memory.

10. **Set `--timeout` appropriately** for slow operations. The default is 30 seconds. A long-running `open` with many sessions may need more.

11. **Use `restartPolicy: never`** in agent workspaces. Automatic restarts can mask failures. Let the agent decide when and whether to retry.

12. **Avoid sending raw VT escapes** via `wtd send`. Use `wtd keys` for control characters and special keys. `send` is for low-level text injection; `prompt` is for pane-aware agent prompting.

## Complete command reference

```
wtd start <name> [--file <path>] [--recreate]
wtd open <name> [--file <path>] [--recreate]
wtd close <name> [--kill]
wtd attach <name>
wtd recreate <name>
wtd save <name> [--file <path>]
wtd list workspaces
wtd list instances
wtd list panes <workspace>
wtd list sessions <workspace>
wtd send <target> <text> [--no-newline]
wtd prompt <target> <text>
wtd keys <target> <key>...
wtd capture <target>
wtd scrollback <target> --tail <n>
wtd follow <target> [--raw]
wtd inspect <target>
wtd configure-pane <target> [--driver-profile <profile>] [--submit-key <key>] [--soft-break-key <key>] [--clear-soft-break] [--clear-driver]
wtd focus <target>
wtd rename <target> <new-name>
wtd action <target> <action> [key=value]...
wtd host status
wtd host stop
```

Global flags (work with any command): `--json`, `--verbose`, `--timeout <seconds>`
