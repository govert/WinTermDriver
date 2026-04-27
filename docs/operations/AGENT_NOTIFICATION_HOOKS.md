# Agent Notification Hooks

WTD exposes two stable primitives for agent panes:

- `wtd notify <target>` for attention states: `needs-attention`, `done`, and `error`.
- `wtd status <target>` for structured metadata: phase, source, queue count,
  completion marker, and status text.
- `wtd wait <target>` for supervisors that need to block until one of those
  states is reached.

The helper scripts in `tools/agent-hooks/` provide a common event vocabulary for
agent CLIs and wrappers:

```powershell
tools/agent-hooks/wtd-agent-event.ps1 -Target build/tests -Agent codex -Event working -Message "running tests"
tools/agent-hooks/wtd-agent-event.ps1 -Target build/tests -Agent codex -Event input-needed -Message "review requested"
tools/agent-hooks/wtd-agent-event.ps1 -Target build/tests -Agent codex -Event completed -Completion tests -Message "tests passed"
```

```bash
tools/agent-hooks/wtd-agent-event.sh --target build/tests --agent codex --event working --message "running tests"
tools/agent-hooks/wtd-agent-event.sh --target build/tests --agent codex --event input-needed --message "review requested"
tools/agent-hooks/wtd-agent-event.sh --target build/tests --agent codex --event completed --completion tests --message "tests passed"
```

Supported `--agent` values are `pi`, `codex`, `claude-code`, `gemini-cli`, and
`copilot-cli`. Supported events are `working`, `input-needed`, `queued`,
`completed`, `error`, and `idle`.

## Event Mapping

| Event | WTD surface |
|-------|-------------|
| `working` | `wtd status --phase working --source <agent>` |
| `input-needed` | `wtd notify --state needs-attention --source <agent>` |
| `queued` | `wtd status --phase queued --queue-pending <n> --source <agent>` |
| `completed` | `wtd status --phase done`, then `wtd notify --state done` |
| `error` | `wtd status --phase error`, then `wtd notify --state error` |
| `idle` | `wtd status --phase idle`, then `wtd clear-attention` |

## Waiting From Coordinators

Supervisor panes can wait on agent-published state instead of polling terminal
text:

```bash
wtd wait build/tests --for done --timeout 60
wtd wait build/tests --for needs-attention --recent-lines 80
wtd wait build/tests --for queue-empty --timeout 30
```

On success, `wtd wait` returns the matched condition plus current pane metadata.
On timeout, it exits with the timeout code but still includes the attention
state, metadata, and recent output needed to decide whether to retry or involve
a user.

## Pi

Pi is the preferred first-class integration target because Pi extensions can
publish status and notification events without screen parsing. A Pi extension or
wrapper should call the helper with:

- `--event working` when a task starts or a queued item begins running.
- `--event queued --queue-pending <n>` when Pi has pending queued work.
- `--event input-needed` when Pi needs user or supervisor input.
- `--event completed` when a task finishes successfully.
- `--event error` when Pi reports a failed task or extension error.

Pi extension packages should keep the WTD target configurable. Use the pane path
when known, for example `workspace/tab/pane`; otherwise pass a workspace/pane
target assigned by the operator.

## Codex

For Codex panes, use wrapper scripts around long-running commands or task
launchers. Emit `working` before starting the operation, `completed` on exit
code `0`, and `error` on non-zero exit. Emit `input-needed` from any supervising
script that detects a prompt requiring human review.

## Claude Code

Claude Code wrappers should use the same exit-code pattern as Codex. When a
project-specific hook can detect approval prompts or blocked tool use, map that
to `input-needed` so the WTD UI can jump to the pane.

## Gemini CLI

Gemini CLI wrappers can publish `working`, `completed`, and `error` around each
invocation. If the wrapper queues follow-up prompts while Gemini is busy, publish
`queued` with `--queue-pending`.

## Copilot CLI

Copilot CLI wrappers should publish `working` before command generation or
execution, `completed` after success, and `error` when the CLI exits non-zero.
Use `input-needed` for confirmation prompts or cases where the generated command
requires operator review.

## Wrapper Pattern

PowerShell:

```powershell
$target = "build/tests"
tools/agent-hooks/wtd-agent-event.ps1 -Target $target -Agent codex -Event working -Message "cargo test"
cargo test
if ($LASTEXITCODE -eq 0) {
    tools/agent-hooks/wtd-agent-event.ps1 -Target $target -Agent codex -Event completed -Completion "cargo test" -Message "tests passed"
} else {
    tools/agent-hooks/wtd-agent-event.ps1 -Target $target -Agent codex -Event error -Message "tests failed"
    exit $LASTEXITCODE
}
```

Bash:

```bash
target="build/tests"
tools/agent-hooks/wtd-agent-event.sh --target "$target" --agent codex --event working --message "cargo test"
if cargo test; then
  tools/agent-hooks/wtd-agent-event.sh --target "$target" --agent codex --event completed --completion "cargo test" --message "tests passed"
else
  code=$?
  tools/agent-hooks/wtd-agent-event.sh --target "$target" --agent codex --event error --message "tests failed"
  exit "$code"
fi
```
