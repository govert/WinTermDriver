#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  wtd-pi-event.sh --target <workspace/pane> --pi-event <event> [options]

Options:
  --message <text>
  --queue-pending <count>
  --completion <text>
  --what-if

Supported Pi events:
  agent_start agent_end turn_start turn_end tool_execution_start tool_execution_end
  queue_update input_requested approval_requested ui_request task_completed
  turn_error tool_execution_error agent_error idle
USAGE
}

target=""
pi_event=""
message=""
queue_pending=""
completion=""
what_if=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --pi-event) pi_event="${2:-}"; shift 2 ;;
    --message) message="${2:-}"; shift 2 ;;
    --queue-pending) queue_pending="${2:-}"; shift 2 ;;
    --completion) completion="${2:-}"; shift 2 ;;
    --what-if) what_if=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$target" ]]; then
  echo "--target is required" >&2
  exit 2
fi

case "$pi_event" in
  agent_start|agent_end|turn_start|turn_end|tool_execution_start|tool_execution_end|queue_update|input_requested|approval_requested|ui_request|task_completed|turn_error|tool_execution_error|agent_error|idle) ;;
  *) echo "--pi-event is required and must be supported" >&2; usage >&2; exit 2 ;;
esac

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
generic_hook="$script_dir/../wtd-agent-event.sh"

run_generic() {
  local event="$1"
  local event_message="${2:-}"
  local pending="${3:-}"
  local completion_text="${4:-}"
  local args=(--target "$target" --agent pi --event "$event")
  [[ -n "$event_message" ]] && args+=(--message "$event_message")
  [[ -n "$pending" ]] && args+=(--queue-pending "$pending")
  [[ -n "$completion_text" ]] && args+=(--completion "$completion_text")
  [[ "$what_if" -eq 1 ]] && args+=(--what-if)
  "$generic_hook" "${args[@]}"
}

case "$pi_event" in
  agent_start|turn_start|tool_execution_start)
    run_generic working "${message:-$pi_event}"
    ;;
  queue_update)
    if [[ -n "$queue_pending" && "$queue_pending" -gt 0 ]]; then
      run_generic queued "$message" "$queue_pending"
    else
      run_generic idle "$message" "0"
    fi
    ;;
  input_requested|approval_requested|ui_request)
    run_generic input-needed "${message:-input requested}"
    ;;
  agent_end|turn_end|tool_execution_end|task_completed)
    run_generic completed "${message:-completed}" "" "${completion:-$pi_event}"
    ;;
  turn_error|tool_execution_error|agent_error)
    run_generic error "${message:-$pi_event}"
    ;;
  idle)
    run_generic idle "$message" "${queue_pending:-0}"
    ;;
esac
