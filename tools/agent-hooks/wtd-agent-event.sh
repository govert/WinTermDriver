#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  wtd-agent-event.sh --target <workspace/pane> --agent <pi|codex|claude-code|gemini-cli|copilot-cli> --event <working|input-needed|queued|completed|error|idle> [options]

Options:
  --message <text>
  --queue-pending <count>
  --completion <text>
  --what-if
USAGE
}

target=""
agent=""
event=""
message=""
queue_pending=""
completion=""
what_if=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --agent) agent="${2:-}"; shift 2 ;;
    --event) event="${2:-}"; shift 2 ;;
    --message) message="${2:-}"; shift 2 ;;
    --queue-pending) queue_pending="${2:-}"; shift 2 ;;
    --completion) completion="${2:-}"; shift 2 ;;
    --what-if) what_if=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$agent" in
  pi|codex|claude-code|gemini-cli|copilot-cli) ;;
  *) echo "--agent is required and must be supported" >&2; exit 2 ;;
esac

case "$event" in
  working|input-needed|queued|completed|error|idle) ;;
  *) echo "--event is required and must be supported" >&2; exit 2 ;;
esac

if [[ -z "$target" ]]; then
  echo "--target is required" >&2
  exit 2
fi

run_wtd() {
  if [[ "$what_if" -eq 1 ]]; then
    printf 'wtd'
    printf ' %q' "$@"
    printf '\n'
  else
    wtd "$@"
  fi
}

case "$event" in
  working)
    args=(status "$target" --phase working --source "$agent")
    [[ -n "$message" ]] && args+=("$message")
    run_wtd "${args[@]}"
    ;;
  input-needed)
    args=(notify "$target" --state needs-attention --source "$agent")
    if [[ -n "$message" ]]; then args+=("$message"); else args+=("input requested"); fi
    run_wtd "${args[@]}"
    ;;
  queued)
    args=(status "$target" --phase queued --source "$agent")
    [[ -n "$queue_pending" ]] && args+=(--queue-pending "$queue_pending")
    [[ -n "$message" ]] && args+=("$message")
    run_wtd "${args[@]}"
    ;;
  completed)
    status_args=(status "$target" --phase done --source "$agent")
    [[ -n "$completion" ]] && status_args+=(--completion "$completion")
    [[ -n "$message" ]] && status_args+=("$message")
    run_wtd "${status_args[@]}"
    notify_args=(notify "$target" --state done --source "$agent")
    if [[ -n "$message" ]]; then notify_args+=("$message"); else notify_args+=("completed"); fi
    run_wtd "${notify_args[@]}"
    ;;
  error)
    status_args=(status "$target" --phase error --source "$agent")
    [[ -n "$message" ]] && status_args+=("$message")
    run_wtd "${status_args[@]}"
    notify_args=(notify "$target" --state error --source "$agent")
    if [[ -n "$message" ]]; then notify_args+=("$message"); else notify_args+=("error"); fi
    run_wtd "${notify_args[@]}"
    ;;
  idle)
    args=(status "$target" --phase idle --source "$agent")
    [[ -n "$queue_pending" ]] && args+=(--queue-pending "$queue_pending")
    [[ -n "$message" ]] && args+=("$message")
    run_wtd "${args[@]}"
    run_wtd clear-attention "$target"
    ;;
esac

