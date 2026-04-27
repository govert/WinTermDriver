#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  wtd-tmux.sh [--what-if] <tmux-command> [args]

Supported:
  split-window [-h|-v] -t <target>
  select-pane -t <target>
  send-keys -t <target> <text...> [C-m]
  list-panes -t <workspace>
  capture-pane -p -t <target> [-S -<lines>]

Unsupported commands fail with exit code 2.
USAGE
}

what_if=0
if [[ "${1:-}" == "--what-if" ]]; then
  what_if=1
  shift
fi

cmd="${1:-}"
[[ -n "$cmd" ]] || { usage >&2; exit 2; }
shift

run_wtd() {
  if [[ "$what_if" -eq 1 ]]; then
    printf 'wtd'
    printf ' %q' "$@"
    printf '\n'
  else
    wtd "$@"
  fi
}

need_target() {
  local target="$1"
  if [[ -z "$target" ]]; then
    echo "wtd-tmux: command requires -t <target>" >&2
    exit 2
  fi
}

case "$cmd" in
  split-window)
    orientation="vertical"
    target=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -h) orientation="horizontal"; shift ;;
        -v) orientation="vertical"; shift ;;
        -t) target="${2:-}"; shift 2 ;;
        *) echo "wtd-tmux: unsupported split-window argument: $1" >&2; exit 2 ;;
      esac
    done
    need_target "$target"
    if [[ "$orientation" == "horizontal" ]]; then
      run_wtd action "$target" split-right
    else
      run_wtd action "$target" split-down
    fi
    ;;
  select-pane)
    target=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -t) target="${2:-}"; shift 2 ;;
        *) echo "wtd-tmux: unsupported select-pane argument: $1" >&2; exit 2 ;;
      esac
    done
    need_target "$target"
    run_wtd focus "$target"
    ;;
  send-keys)
    target=""
    keys=()
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -t) target="${2:-}"; shift 2 ;;
        --) shift; while [[ $# -gt 0 ]]; do keys+=("$1"); shift; done ;;
        *) keys+=("$1"); shift ;;
      esac
    done
    need_target "$target"
    submit=0
    if [[ "${keys[-1]:-}" == "C-m" ]]; then
      submit=1
      unset 'keys[-1]'
    fi
    text="${keys[*]}"
    if [[ "$submit" -eq 1 ]]; then
      run_wtd prompt "$target" "$text"
    else
      run_wtd send "$target" "$text"
    fi
    ;;
  list-panes)
    target=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -t) target="${2:-}"; shift 2 ;;
        -F) shift 2 ;;
        *) echo "wtd-tmux: unsupported list-panes argument: $1" >&2; exit 2 ;;
      esac
    done
    need_target "$target"
    run_wtd list panes "$target"
    ;;
  capture-pane)
    target=""
    lines=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -p) shift ;;
        -t) target="${2:-}"; shift 2 ;;
        -S)
          value="${2:-}"
          lines="${value#-}"
          shift 2
          ;;
        *) echo "wtd-tmux: unsupported capture-pane argument: $1" >&2; exit 2 ;;
      esac
    done
    need_target "$target"
    if [[ -n "$lines" ]]; then
      run_wtd capture "$target" --lines "$lines"
    else
      run_wtd capture "$target"
    fi
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    echo "wtd-tmux: unsupported tmux command: $cmd" >&2
    usage >&2
    exit 2
    ;;
esac
