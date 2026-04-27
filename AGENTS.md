# WinTermDriver Agent Guide

## GPT-5.5 / Codex Working Guidance

Use this repository guidance as stable context for GPT-5.5 and Codex-style
agents. Bead details, user requests, and command output are dynamic context and
should be read after these rules.

### Outcome First

- Treat the user's request or current bead as the goal, with the bead's
  expected outcome and completion evidence as the success criteria.
- Choose the implementation path from the codebase context. Do not turn the
  documentation into a rigid step-by-step script unless a step is a true safety
  or correctness requirement.
- Ask only when missing information materially changes the outcome or creates
  meaningful risk. Otherwise, make a reasonable assumption, proceed, and record
  it in the final summary or `tools/MEMORY.md` when future beads need it.

### Context Gathering

- Start with `bv --robot-triage` or the assigned bead, then use focused `br`,
  `rg`, and targeted file reads.
- Stop gathering context once the relevant files, commands, and acceptance
  checks are clear. Search again only when validation fails or new uncertainty
  appears.
- Keep stable project knowledge in docs and `tools/MEMORY.md`; keep bead-specific
  discoveries in the bead, commit, and closure notes.

### Validation And Stop Rules

- Before finishing code changes, run the most relevant validation available:
  targeted tests for changed behavior, build/type checks for touched crates, or
  a smoke test when full validation is too expensive.
- If validation cannot be run, state why and name the next best check.
- A bead is done only when its stated outcome is implemented, completion
  evidence is satisfied, follow-up work is either unnecessary or tracked in new
  beads, and changes are committed.

### GPT-5.5 API Notes For WTD Work

- If WTD later hosts GPT-5.5 through the Responses API, prefer
  `previous_response_id` for continuing multi-turn state.
- If WTD manually replays assistant items for long-running or tool-heavy
  workflows, preserve assistant `phase` values exactly so commentary/preambles
  and final answers remain distinct.
- Put durable tool behavior in tool descriptions or WTD protocol docs. Keep
  prompts focused on outcomes, constraints, validation, and stop rules.

<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`/`bd`) for issue tracking. Issues are stored in the project beads workspace and tracked in git.

### Essential Commands

Most subcommands accept multiple issue IDs in a single invocation.

```bash
# View ready issues (open, unblocked, not deferred)
br ready              # or: bd ready

# List and search
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br show <id1> <id2>   # Show multiple issues at once
br search "keyword"   # Full-text search

# Create and update
br create --title="..." --description="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once
br reopen <id1> <id2> # Reopen multiple issues at once
br delete <id1> <id2> # Delete multiple issues at once

# Sync with git
br sync --flush-only  # Export DB to JSONL
br sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Always run `br sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only open, unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads changes to JSONL
git commit -m "..."     # Commit everything
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always sync before ending session

<!-- end-br-agent-instructions -->

<!-- bv-agent-instructions-v2 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking and [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) (`bv`) for graph-aware triage. Issues are stored in `.beads/` and tracked in git.

### Using bv as an AI sidecar

bv is a graph-aware triage engine for Beads projects (.beads/beads.jsonl). Instead of parsing JSONL or hallucinating graph traversal, use robot flags for deterministic, dependency-aware outputs with precomputed metrics (PageRank, betweenness, critical path, cycles, HITS, eigenvector, k-core).

**Scope boundary:** bv handles *what to work on* (triage, priority, planning). `br` handles creating, modifying, and closing beads.

**CRITICAL: Use ONLY --robot-* flags. Bare bv launches an interactive TUI that blocks your session.**

#### The Workflow: Start With Triage

**`bv --robot-triage` is your single entry point.** It returns everything you need in one call:
- `quick_ref`: at-a-glance counts + top 3 picks
- `recommendations`: ranked actionable items with scores, reasons, unblock info
- `quick_wins`: low-effort high-impact items
- `blockers_to_clear`: items that unblock the most downstream work
- `project_health`: status/type/priority distributions, graph metrics
- `commands`: copy-paste shell commands for next steps

```bash
bv --robot-triage        # THE MEGA-COMMAND: start here
bv --robot-next          # Minimal: just the single top pick + claim command

# Token-optimized output (TOON) for lower LLM context usage:
bv --robot-triage --format toon
```

#### Other bv Commands

| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with unblocks lists |
| `--robot-priority` | Priority misalignment detection with confidence |
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions, cycle breaks |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |

#### Scoping & Filtering

```bash
bv --robot-plan --label backend              # Scope to label's subgraph
bv --robot-insights --as-of HEAD~30          # Historical point-in-time
bv --recipe actionable --robot-plan          # Pre-filter: ready to work (no blockers)
bv --recipe high-impact --robot-triage       # Pre-filter: top PageRank scores
```

### br Commands for Issue Management

```bash
br ready              # Show issues ready to work (no blockers)
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br create --title="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once
br sync --flush-only  # Export DB to JSONL
```

### Workflow Pattern

1. **Triage**: Run `bv --robot-triage` to find the highest-impact actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Always run `br sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads changes to JSONL
git commit -m "..."     # Commit everything
git push                # Push to remote
```

<!-- end-bv-agent-instructions -->

---

## Bead Runner

The bead runner (`tools/bead-runner.sh`) automates sequential bead execution. It picks the next ready bead, invokes `claude -p` with full context (instructions, cross-bead memory, bead details), and repeats until done.

### Running from PowerShell

```powershell
$env:MAX_BEADS="10"; $env:CLAUDE_FLAGS="--dangerously-skip-permissions"; & "C:\Program Files\Git\bin\bash.exe" ./tools/bead-runner.sh
```

### Running from Git Bash

```bash
MAX_BEADS=10 CLAUDE_FLAGS="--dangerously-skip-permissions" ./tools/bead-runner.sh
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_BEADS` | 0 (unlimited) | Stop after N beads |
| `CLAUDE_MODEL` | (unset) | Model override (e.g., `sonnet`, `opus`) |
| `CLAUDE_FLAGS` | (unset) | Extra flags for `claude` CLI |

### How It Works

1. Picks the first ready task bead via `br ready --json --limit 1 --type task`
2. Marks it `in_progress`
3. Builds a prompt from `tools/bead-instructions.md` + `tools/MEMORY.md` + bead details
4. Runs `claude -p --output-format stream-json` with live formatted display
5. Checks if the bead was closed by the agent
6. Syncs bead state and commits
7. After the loop, closes any epics whose children are all done (`br epic close-eligible`)

### Prompt Assembly Guidance

For Codex/GPT-5.5-compatible runners, keep stable instructions first and
dynamic context last:

1. `tools/bead-instructions.md`
2. relevant durable excerpts from `tools/MEMORY.md`
3. current bead details and command output

This layout improves prompt-cache reuse and keeps the model focused on the
current outcome without duplicating stable tool behavior in every bead.

### Key Files

| File | Purpose |
|------|---------|
| `tools/bead-runner.sh` | Main loop script |
| `tools/bead-instructions.md` | Agent instructions included in every bead prompt |
| `tools/MEMORY.md` | Cross-bead persistent memory (agents read and append) |
| `logs/bead-runs/*.jsonl` | Full stream-json log per bead run |

### Safety

- **Nested invocation guard**: The runner sets `BEAD_RUNNER_PID` and aborts if it detects it is already set, preventing accidental recursive runner loops.
- **Stops on failure**: If a bead agent exits without closing the bead, the runner stops and prints recovery instructions.
- Agents are instructed to use `br close`, never `bead-runner.sh`.
