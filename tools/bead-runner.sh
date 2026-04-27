#!/usr/bin/env bash
# bead-runner.sh — Legacy Claude sequential bead execution loop
#
# Picks the next ready bead, constructs context, invokes `claude -p`,
# logs the full run, and repeats until no ready beads remain.
#
# This runner is intentionally disabled by default. WinTermDriver's Codex
# workflow should use a manual br-driven loop in the current agent session:
# triage, claim one bead, implement, validate, close, sync, commit, repeat.
# Set WTD_ALLOW_LEGACY_CLAUDE_BEAD_RUNNER=1 only when you explicitly want the
# old Claude subprocess runner.
#
# Environment variables:
#   MAX_BEADS      Stop after N beads (default: 0 = unlimited)
#   CLAUDE_MODEL   Model override (default: unset, uses claude default)
#   CLAUDE_FLAGS   Extra flags for claude CLI (see below)
#
# For unattended use, you must bypass permission prompts:
#   CLAUDE_FLAGS="--dangerously-skip-permissions" ./tools/bead-runner.sh
#
# Examples:
#   ./tools/bead-runner.sh                          # interactive (prompts for permissions)
#   MAX_BEADS=1 ./tools/bead-runner.sh              # run one bead only
#   CLAUDE_FLAGS="--dangerously-skip-permissions" \
#     MAX_BEADS=3 ./tools/bead-runner.sh            # three beads, unattended

set -euo pipefail

if [[ "${WTD_ALLOW_LEGACY_CLAUDE_BEAD_RUNNER:-}" != "1" ]]; then
  cat >&2 <<'EOF'
[runner] ERROR: tools/bead-runner.sh is the legacy Claude subprocess runner.
[runner] It is disabled by default for Codex/GPT-5.5 work.
[runner]
[runner] Use the manual bead loop instead:
[runner]   bv --robot-triage
[runner]   br update <bead-id> --status in_progress
[runner]   # implement, validate, br close, br sync --flush-only, git commit
[runner]
[runner] To intentionally run the legacy Claude runner, set:
[runner]   WTD_ALLOW_LEGACY_CLAUDE_BEAD_RUNNER=1
EOF
  exit 2
fi

# ── Guard against nested invocation ──────────────────────────────
# A bead agent once ran `bash bead-runner.sh close ...` (confusing it
# with `br close`), which spawned a nested runner loop.  Prevent this.
if [[ -n "${BEAD_RUNNER_PID:-}" ]]; then
  echo "[runner] ERROR: Nested invocation detected (parent PID=$BEAD_RUNNER_PID). Aborting." >&2
  echo "[runner] Did you mean:  br close <bead-id> --reason \"...\"" >&2
  exit 1
fi
export BEAD_RUNNER_PID=$$

# Ensure common tool locations are in PATH (Git Bash from PowerShell may not have them)
USERDIR="${HOME:-/c/Users/${USERNAME:-${USER}}}"
export PATH="${USERDIR}/.local/bin:${USERDIR}/go/bin:${USERDIR}/.dotnet/tools:${USERDIR}/AppData/Local/Microsoft/WinGet/Links:${PATH}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MEMORY_FILE="$SCRIPT_DIR/MEMORY.md"
INSTRUCTIONS="$SCRIPT_DIR/bead-instructions.md"
LOG_DIR="$PROJECT_DIR/logs/bead-runs"
MAX_BEADS="${MAX_BEADS:-0}"
CLAUDE_MODEL="${CLAUDE_MODEL:-}"
CLAUDE_FLAGS="${CLAUDE_FLAGS:-}"

cd "$PROJECT_DIR"
mkdir -p "$LOG_DIR"

# ── Preflight checks ──────────────────────────────────────────────

for cmd in claude br python3 git; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "[runner] ERROR: $cmd not found in PATH" >&2
    exit 1
  fi
done

for f in "$MEMORY_FILE" "$INSTRUCTIONS"; do
  if [[ ! -f "$f" ]]; then
    echo "[runner] ERROR: missing $f" >&2
    exit 1
  fi
done

# ── Live display formatter ────────────────────────────────────────
# Reads stream-json from stdin, writes formatted progress to stderr,
# passes all input through to stdout unchanged (for the log file).
make_formatter() {
  python3 -u -c '
import json, sys, time

def dim(s):    return f"\033[2m{s}\033[0m"
def bold(s):   return f"\033[1m{s}\033[0m"
def green(s):  return f"\033[32m{s}\033[0m"
def yellow(s): return f"\033[33m{s}\033[0m"
def cyan(s):   return f"\033[36m{s}\033[0m"

tool_count = 0
start = time.time()

for raw in sys.stdin:
    # Pass through to stdout (log file) unchanged
    sys.stdout.write(raw)
    sys.stdout.flush()

    line = raw.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue

    t = msg.get("type", "")

    if t == "assistant":
        for block in msg.get("message", {}).get("content", []):
            kind = block.get("type", "")
            if kind == "tool_use":
                tool_count += 1
                name = block.get("name", "?")
                inp = block.get("input", {})
                detail = ""
                if name == "Bash":
                    cmd = inp.get("command", "")
                    detail = cmd[:80].replace("\n", " ")
                elif name == "Edit":
                    detail = inp.get("file_path", "")[-60:]
                elif name == "Write":
                    detail = inp.get("file_path", "")[-60:]
                elif name == "Read":
                    detail = inp.get("file_path", "")[-60:]
                elif name == "Glob":
                    detail = inp.get("pattern", "")
                elif name == "Grep":
                    detail = inp.get("pattern", "")[:40]
                elapsed = int(time.time() - start)
                prefix = dim(f"[{elapsed:>4d}s #{tool_count:>3d}]")
                print(f"  {prefix} {cyan(name):>20s}  {dim(detail)}", file=sys.stderr, flush=True)
            elif kind == "text":
                text = block.get("text", "")
                if len(text) > 120:
                    text = text[:117] + "..."
                elapsed = int(time.time() - start)
                prefix = dim(f"[{elapsed:>4d}s]")
                print(f"  {prefix} {green(text)}", file=sys.stderr, flush=True)

    elif t == "result":
        elapsed = int(time.time() - start)
        sub = msg.get("subtype", "?")
        cost = msg.get("total_cost_usd", 0)
        turns = msg.get("num_turns", 0)
        result_text = msg.get("result", "")
        prefix = dim(f"[{elapsed:>4d}s]")
        if sub == "success":
            label = bold(green("DONE"))
        else:
            label = bold(yellow("EXIT: " + sub))
        print(f"  {prefix} {label} ({turns} turns, ${cost:.2f})", file=sys.stderr, flush=True)
        if result_text:
            # Print first 3 lines of result
            for rline in result_text.strip().split("\n")[:3]:
                print(f"         {dim(rline[:100])}", file=sys.stderr, flush=True)
'
}

# ── Banner ─────────────────────────────────────────────────────────

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  WinTermDriver Bead Runner                                  ║"
echo "║  Project : $PROJECT_DIR"
echo "║  Logs    : $LOG_DIR"
[[ "$MAX_BEADS" -gt 0 ]] && \
echo "║  Limit   : $MAX_BEADS bead(s)"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# ── Main loop ──────────────────────────────────────────────────────

bead_count=0
total_cost=0

while true; do
  # Get first ready task bead (skip epics)
  BEAD_ID=$(br ready --json --limit 1 --type task 2>/dev/null \
    | python3 -c "
import json, sys
d = json.load(sys.stdin)
# br ready --json returns a plain array, br list --json returns {issues:[...]}
items = d if isinstance(d, list) else d.get('issues', [])
print(items[0]['id'] if items else '')
" 2>/dev/null) || BEAD_ID=""

  if [[ -z "$BEAD_ID" ]]; then
    echo "[runner] No ready task beads. Done."
    break
  fi

  bead_count=$((bead_count + 1))
  if [[ "$MAX_BEADS" -gt 0 && "$bead_count" -gt "$MAX_BEADS" ]]; then
    echo "[runner] Reached MAX_BEADS=$MAX_BEADS. Stopping."
    break
  fi

  TIMESTAMP=$(date +%Y%m%d-%H%M%S)
  LOG_FILE="$LOG_DIR/${TIMESTAMP}_${BEAD_ID}.jsonl"

  # ── Bead details ───────────────────────────────────────────────
  BEAD_SHOW=$(br show "$BEAD_ID" 2>&1)
  BEAD_TITLE=$(echo "$BEAD_SHOW" | head -1)

  echo "┌──────────────────────────────────────────────────────────────"
  echo "│ Bead #$bead_count: $BEAD_ID"
  echo "│ $BEAD_TITLE"
  echo "│ Log: $LOG_FILE"
  echo "└──────────────────────────────────────────────────────────────"

  # Mark bead in-progress
  br update "$BEAD_ID" --status in_progress --quiet

  # ── Build prompt ───────────────────────────────────────────────
  PROMPT_FILE=$(mktemp)
  {
    cat "$INSTRUCTIONS"
    echo ""
    echo "---"
    echo ""
    echo "## Cross-Bead Memory"
    echo ""
    cat "$MEMORY_FILE"
    echo ""
    echo "---"
    echo ""
    echo "## Current Bead"
    echo ""
    echo '```'
    echo "$BEAD_SHOW"
    echo '```'
    echo ""
    echo "Implement this bead now. Follow the instructions above."
  } > "$PROMPT_FILE"

  # ── Build claude command ───────────────────────────────────────
  CLAUDE_ARGS=(-p --output-format stream-json)
  [[ -n "$CLAUDE_MODEL" ]] && CLAUDE_ARGS+=(--model "$CLAUDE_MODEL")
  # shellcheck disable=SC2206
  [[ -n "$CLAUDE_FLAGS" ]] && CLAUDE_ARGS+=($CLAUDE_FLAGS)

  # ── Run claude with live display ───────────────────────────────
  echo "[runner] Starting claude..."
  SECONDS=0

  set +e
  claude "${CLAUDE_ARGS[@]}" < "$PROMPT_FILE" 2>&1 | make_formatter > "$LOG_FILE"
  CLAUDE_EXIT=${PIPESTATUS[0]}
  set -e

  ELAPSED=$SECONDS
  rm -f "$PROMPT_FILE"

  echo "[runner] Claude exited ($CLAUDE_EXIT) after ${ELAPSED}s"

  # ── Extract cost from log ──────────────────────────────────────
  BEAD_COST=$(tail -1 "$LOG_FILE" 2>/dev/null \
    | python3 -c "
import json, sys
line = sys.stdin.read().strip()
if line:
    msg = json.loads(line)
    if msg.get('type') == 'result':
        print(f\"{msg.get('total_cost_usd', 0):.2f}\")
    else:
        print('0')
else:
    print('0')
" 2>/dev/null) || BEAD_COST="0"
  total_cost=$(python3 -c "print(f'{$total_cost + $BEAD_COST:.2f}')" 2>/dev/null) || true

  # ── Check bead status ──────────────────────────────────────────
  BEAD_STATUS=$(br show "$BEAD_ID" --json 2>/dev/null \
    | python3 -c "
import json, sys
d = json.load(sys.stdin)
# br show --json returns a list with one element
item = d[0] if isinstance(d, list) else d
print(item.get('status', 'unknown'))
" 2>/dev/null) || BEAD_STATUS="unknown"

  echo "[runner] Bead status: $BEAD_STATUS"

  if [[ "$BEAD_STATUS" == "closed" ]]; then
    # Sync bead state to JSONL
    br sync --flush-only --quiet 2>/dev/null || true

    # Commit bead state changes (code was already committed by the agent)
    git add .beads/issues.jsonl tools/MEMORY.md 2>/dev/null || true
    if ! git diff --cached --quiet 2>/dev/null; then
      git commit -m "bead-runner: close $BEAD_ID" --quiet 2>/dev/null || true
    fi

    echo "[runner] Done: $BEAD_ID closed and committed."

    # Show what's newly unblocked
    echo ""
    NEWLY_READY=$(br ready --json --limit 5 --type task 2>/dev/null \
      | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d if isinstance(d, list) else d.get('issues', [])
for i in items:
    print(f\"  -> {i['id']}: {i['title']}\")
" 2>/dev/null) || NEWLY_READY=""
    if [[ -n "$NEWLY_READY" ]]; then
      echo "[runner] Ready beads:"
      echo "$NEWLY_READY"
    fi
  else
    echo "[runner] ERROR: Bead not closed (status=$BEAD_STATUS). Stopping."
    echo "[runner] Review the log: $LOG_FILE"
    echo "[runner] To retry: br update $BEAD_ID --status open"
    break
  fi

  echo ""
done

# ── Close eligible epics ──────────────────────────────────────────

if [[ "$bead_count" -gt 0 ]]; then
  CLOSED_EPICS=$(br epic close-eligible 2>&1) || true
  if [[ -n "$CLOSED_EPICS" && "$CLOSED_EPICS" != *"No eligible"* ]]; then
    echo "[runner] Epics closed:"
    echo "$CLOSED_EPICS" | sed 's/^/  /'
    br sync --flush-only --quiet 2>/dev/null || true
    git add .beads/issues.jsonl 2>/dev/null || true
    if ! git diff --cached --quiet 2>/dev/null; then
      git commit -m "bead-runner: close eligible epics" --quiet 2>/dev/null || true
    fi
  fi
fi

# ── Final summary ────────────────────────────────────────────────

echo ""
echo "════════════════════════════════════════════════════════════════"
echo " Bead runner finished after $bead_count bead(s). Total cost: \$$total_cost"
echo " Logs: $LOG_DIR"
echo "════════════════════════════════════════════════════════════════"
