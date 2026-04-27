# Pi WTD Event Bridge

This directory contains a reference Pi-to-WTD bridge. It is intentionally small:
Pi extension hooks or wrappers call `wtd-pi-event`, and the bridge translates Pi
lifecycle/queue events into the stable WTD primitives:

- `wtd status`
- `wtd notify`
- `wtd clear-attention`
- `wtd wait` for supervisors consuming the state

The bridge does not require Pi core changes. It assumes a Pi extension can run an
external command from lifecycle, queue, input, completion, and error hooks.

## PowerShell

```powershell
tools/agent-hooks/pi/wtd-pi-event.ps1 -Target workspace/main/pi -PiEvent turn_start -Message "turn started"
tools/agent-hooks/pi/wtd-pi-event.ps1 -Target workspace/main/pi -PiEvent queue_update -QueuePending 2
tools/agent-hooks/pi/wtd-pi-event.ps1 -Target workspace/main/pi -PiEvent input_requested -Message "approval requested"
tools/agent-hooks/pi/wtd-pi-event.ps1 -Target workspace/main/pi -PiEvent turn_end -Completion turn -Message "turn completed"
```

## Bash

```bash
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event turn_start --message "turn started"
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event queue_update --queue-pending 2
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event input_requested --message "approval requested"
tools/agent-hooks/pi/wtd-pi-event.sh --target workspace/main/pi --pi-event turn_end --completion turn --message "turn completed"
```

## Event Mapping

| Pi event | WTD event |
|----------|-----------|
| `agent_start`, `turn_start`, `tool_execution_start` | `working` |
| `queue_update` with pending count greater than zero | `queued` |
| `queue_update` with zero or missing pending count | `idle` |
| `input_requested`, `approval_requested`, `ui_request` | `input-needed` |
| `agent_end`, `turn_end`, `tool_execution_end`, `task_completed` | `completed` |
| `turn_error`, `tool_execution_error`, `agent_error` | `error` |
| `idle` | `idle` |

## Extension Pattern

A Pi extension package should keep the WTD target configurable and call the
bridge from Pi event callbacks:

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

Supervisors can then coordinate Pi panes with:

```bash
wtd wait workspace/main/pi --for done --timeout 60
wtd wait workspace/main/pi --for needs-attention --recent-lines 80
wtd wait workspace/main/pi --for queue-empty --timeout 30
```
