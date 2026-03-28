#!/usr/bin/env bash
# bead-runner.sh — Sequential bead execution loop
#
# Picks the next ready bead, constructs context, invokes `claude -p`,
# logs the full run, and repeats until no ready beads remain.
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

while true; do
  # Get first ready task bead (skip epics)
  BEAD_ID=$(br ready --json --limit 1 --type task 2>/dev/null \
    | python3 -c "
import json, sys
d = json.load(sys.stdin)
print(d['issues'][0]['id'] if d.get('issues') else '')
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

  # ── Run claude ─────────────────────────────────────────────────
  echo "[runner] Starting claude..."
  SECONDS=0

  set +e
  claude "${CLAUDE_ARGS[@]}" < "$PROMPT_FILE" > "$LOG_FILE" 2>&1
  CLAUDE_EXIT=$?
  set -e

  ELAPSED=$SECONDS
  rm -f "$PROMPT_FILE"

  echo "[runner] Claude exited ($CLAUDE_EXIT) after ${ELAPSED}s"

  # ── Extract summary from log ───────────────────────────────────
  python3 -c "
import json, sys, os
last_text = ''
for line in open(os.environ['LOG_FILE']):
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
        if msg.get('type') == 'assistant':
            for block in msg.get('message', {}).get('content', []):
                if block.get('type') == 'text':
                    last_text = block['text']
        elif msg.get('type') == 'result':
            if msg.get('result', ''):
                last_text = msg['result']
    except (json.JSONDecodeError, KeyError):
        pass
if last_text:
    # Show first 500 chars of the final output
    preview = last_text[:500]
    if len(last_text) > 500:
        preview += '...'
    print(f'[runner] Summary: {preview}')
else:
    print('[runner] (no summary extracted from log)')
" 2>/dev/null || echo "[runner] (could not parse log)"

  # ── Check bead status ──────────────────────────────────────────
  BEAD_STATUS=$(br show "$BEAD_ID" --json 2>/dev/null \
    | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" \
    2>/dev/null) || BEAD_STATUS="unknown"

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
for i in d.get('issues', []):
    print(f\"  → {i['id']}: {i['title']}\")
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

# ── Final summary ────────────────────────────────────────────────

echo ""
echo "════════════════════════════════════════════════════════════════"
echo " Bead runner finished after $bead_count bead(s)."
echo " Logs: $LOG_DIR"
echo "════════════════════════════════════════════════════════════════"
