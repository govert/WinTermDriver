# Pi WTD Extension Pattern

WTD's Pi integration path is a Pi-side extension or wrapper that publishes
structured state into WTD while the normal Pi TUI continues to run in a WTD pane.
This keeps WTD out of Pi core and avoids screen scraping for coordination.

## Components

- WTD hosts Pi with the built-in `pi` driver profile.
- The Pi extension keeps a configurable WTD target such as
  `workspace/main/pi`.
- Pi lifecycle, queue, input, completion, and error callbacks invoke the bridge
  scripts in `tools/agent-hooks/pi/`.
- The bridge maps Pi events to `wtd status`, `wtd notify`, and
  `wtd clear-attention` using source `pi`.
- Supervisors consume the published state with `wtd inspect`, pane metadata UI,
  or `wtd wait`.

## Installation Shape

Copy or reference the bridge directory from a Pi extension package:

```text
tools/agent-hooks/pi/
  README.md
  wtd-pi-event.ps1
  wtd-pi-event.sh
```

Set the WTD target in the extension configuration. Prefer a full pane path when
known:

```text
WTD_TARGET=workspace/main/pi
```

The extension should call the platform-native bridge from Pi callbacks. On
Windows, use `wtd-pi-event.ps1`; under Git Bash, WSL, or POSIX shells, use
`wtd-pi-event.sh`.

## Pi Event Mapping

| Pi callback/event | WTD result |
|-------------------|------------|
| turn or agent starts | `wtd status <target> --phase working --source pi` |
| queue update, pending > 0 | `wtd status <target> --phase queued --queue-pending <n> --source pi` |
| queue update, pending = 0 | `wtd status <target> --phase idle --queue-pending 0 --source pi`, then `wtd clear-attention` |
| user input or approval needed | `wtd notify <target> --state needs-attention --source pi` |
| turn or task completes | `wtd status <target> --phase done --completion <marker> --source pi`, then `wtd notify --state done` |
| turn, tool, or agent error | `wtd status <target> --phase error --source pi`, then `wtd notify --state error` |

## Extension Pseudocode

```text
on_turn_start:
  exec("wtd-pi-event --target $WTD_TARGET --pi-event turn_start")

on_queue_update(pending):
  exec("wtd-pi-event --target $WTD_TARGET --pi-event queue_update --queue-pending <pending>")

on_input_requested(message):
  exec("wtd-pi-event --target $WTD_TARGET --pi-event input_requested --message <message>")

on_turn_end(summary):
  exec("wtd-pi-event --target $WTD_TARGET --pi-event turn_end --completion turn --message <summary>")

on_error(message):
  exec("wtd-pi-event --target $WTD_TARGET --pi-event turn_error --message <message>")
```

This is a reference pattern, not a Pi core requirement. If Pi exposes richer
event names, the extension should normalize them to the supported bridge events
listed in `tools/agent-hooks/pi/README.md`.

## Supervisor Workflow

Once Pi publishes state, another pane can coordinate without parsing Pi screen
text:

```bash
wtd wait workspace/main/pi --for done --timeout 60
wtd wait workspace/main/pi --for needs-attention --recent-lines 80
wtd wait workspace/main/pi --for queue-empty --timeout 30
```

Timeouts return attention state, pane metadata, and recent output. That gives a
supervisor enough context to retry, prompt the pane, or focus it for a user.

## Validation

The bridge supports `--what-if`, which prints the exact `wtd` commands it would
run. A representative hosted Pi workflow is:

```bash
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event turn_start --message "planning" --what-if
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event queue_update --queue-pending 2 --what-if
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event input_requested --message "approval needed" --what-if
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event turn_end --completion turn --message "done" --what-if
```

The expected output is a sequence of `wtd status`, `wtd notify`, and
`wtd clear-attention` commands with `--source pi`.
