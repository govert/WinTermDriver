# Planned Additions Overview

## Scope

This review covers the planning work added after the last code-bearing commit,
`231aede Add one-shot pass-through-next-key action`.

Planning commits reviewed:

- `b0822d0 Add cmux-inspired WTD workset roadmap`
- `85eee0e Update roadmap for Pi agent integration`
- `669978b Rename workset doc to agent-inspired roadmap`
- `5cc3bed Incorporate SoloTerm ideas into roadmap`

Primary planning artifacts:

- `docs/operations/AGENT_INSPIRED_WORKSET.md`
- `docs/operations/PI_AGENT_INTEGRATION_REVIEW.md`
- `docs/operations/SOLOTERM_REVIEW.md`
- `.beads/issues.jsonl`

The intent is one forward product push. There is no release-phase plan here and
no backward-compatibility requirement for legacy command names or partial legacy
protocol behavior.

## Coherent Feature Set

The planned additions are coherent around one product shape:

WTD remains a durable Windows terminal host, but gains a structured control
plane for multi-agent local development.

The additions fit together as one system:

1. Agent attention and notifications make panes/workspaces actionable without
   manual scanning.
2. Structured pane metadata gives attention, UI lists, `inspect`, `wait`, hooks,
   and recipes a common state model.
3. Stable automation protocol and local access rules make the CLI/API safe and
   predictable for other tools and agents.
4. Lifecycle and persistence reliability keep long-running workspaces useful
   across restart, reattach, save, minimize, resize, and restore paths.
5. Managed process health extends the workspace model from only panes to the
   local project stack those panes depend on.
6. Project-local recipes turn repeated prompt/capture/wait workflows into
   reusable team commands.
7. Agent ecosystem adapters, including Pi hooks and a focused tmux shim, publish
   real agent state into WTD instead of relying only on screen scraping.
8. General terminal polish keeps the non-agent baseline strong enough that the
   agent features sit on a credible everyday terminal.

The important dependency is conceptual as well as technical: hooks, recipes,
and Pi integration should publish into the same metadata and attention model
that `inspect`, `wait`, and the UI consume.

## Implementation Ordering

This is not a version or phase plan. It is the preferred implementation order
inside the single forward push, chosen to reduce rework and keep contracts
stable as the feature set lands.

1. Clean up lifecycle naming and persistence foundations:
   `wintermdriver-qlej`, `wintermdriver-ca61`, `wintermdriver-1gwl`, then
   `wintermdriver-hhx8`.

2. Lock the automation contract and local access model:
   `wintermdriver-g9kn`, then `wintermdriver-gei6`.

3. Build the shared state foundations:
   `wintermdriver-rvev` for attention state and notification ingestion, and
   `wintermdriver-0fsd` for structured pane metadata.

4. Add first-class coordination:
   `wintermdriver-smew` for `wtd wait` after protocol, access, metadata, and
   attention state exist.

5. Surface the state in the operator UI:
   `wintermdriver-0o1b`, `wintermdriver-ld7t`, and `wintermdriver-h1iu`.

6. Add managed process health:
   `wintermdriver-2vg4`, then `wintermdriver-c80q`.

7. Add project workflow memory:
   `wintermdriver-mz9o`, then `wintermdriver-36ko` and
   `wintermdriver-w8gs`.

8. Add agent ecosystem adapters:
   `wintermdriver-ul9o`, then `wintermdriver-verr` and
   `wintermdriver-glhe`.

9. Add tmux-oriented compatibility:
   `wintermdriver-egid`, then `wintermdriver-vwb5`.

10. Keep general terminal polish moving from explicit audit output:
    `wintermdriver-00sz` and `wintermdriver-gosq`.

## Priority Review

The P1 set should represent structural commitments that other work builds on:

- lifecycle command cleanup and persistence regression coverage
- attention state and structured metadata
- protocol documentation, access model, and `wtd wait`
- first operator surfaces for metadata and attention
- agent notification hooks needed by the first-class Pi path

The P2 set is still in the one-push scope, but it depends on those foundations:

- managed process health
- project recipes and trust-on-change behavior
- tmux compatibility
- broader terminal polish

The bead graph has been adjusted so the important prerequisites are encoded as
dependencies rather than only described in prose.

## Bead Detail Review

The current bead set is detailed enough to start implementation. The strongest
beads already have expected outcomes and completion evidence. A few beads were
tightened during this review:

- `wintermdriver-g9kn` now states that this is a single forward schema, with no
  long-term legacy dual-stack requirement.
- `wintermdriver-mz9o` now calls out schema fields, selectors, palette
  visibility, and the need to compose cleanly with the trust check.
- `wintermdriver-egid` now requires an explicit supported tmux subset and clear
  unsupported-command failures.
- `wintermdriver-2vg4` now bounds process health as terminal-first metadata, not
  a dashboard-first product pivot.

Implementation should still make these details concrete before broad coding:

- pane state enum and transition rules
- metadata JSON shape for `inspect`, protocol responses, and agent-published
  updates
- `wtd wait` timeout exit code and timeout snapshot shape
- local automation access policy and where trust material is stored
- recipe manifest filename, schema version field, and changed-file detection
- tmux command subset chosen from actual target agent workflows
- process resource hints that are practical and reliable on Windows

No additional beads are required before implementation starts; these details can
be resolved inside the existing beads unless discovery shows a larger split is
needed.

## GPT-5.5/Codex Planning Implications

The GPT-5.5 guidance reinforces the existing WTD direction, but it sharpens a
few design requirements:

- Prompts and bead instructions should be outcome-first: goal, success criteria,
  constraints, validation, and stop rules. Avoid long implementation scripts
  unless the sequence is truly required.
- Agent workflows should use concise preambles or progress updates for
  tool-heavy work so users can see what is happening without reading internal
  reasoning.
- The `wtd wait`, `inspect`, metadata, and attention work should produce
  state-rich snapshots that let agents stop polling once they have enough
  evidence to act.
- If WTD implements GPT-5.5-backed orchestration through the Responses API, it
  should prefer `previous_response_id` for state continuity. If WTD manually
  replays assistant items, it must preserve assistant `phase` values exactly so
  intermediate commentary and final answers remain distinct.
- Tool behavior should live in stable WTD protocol/tool descriptions where
  possible. Project recipes and prompts should describe outcomes and constraints
  rather than duplicating low-level tool instructions.
- Validation should be explicit in beads and recipes: targeted tests, build
  checks, smoke tests, timeout behavior, and what to do when validation cannot
  run.

Reference docs:

- `https://developers.openai.com/api/docs/guides/latest-model`
- `https://developers.openai.com/api/docs/guides/prompt-guidance`

## Current Risk Notes

- There is one stale unrelated in-progress bead,
  `wintermdriver-3f2 Add upstream frankentui showcase capture driver`, last
  updated before this planning set. It should be resolved or parked separately
  so it does not confuse the roadmap.
- The docs intentionally defer embedded browser panes, remote helper daemons,
  remote relay/proxy work, and distributed multi-host orchestration. Those
  deferrals are consistent with the current feature set.
- The phrase "version" in protocol planning should mean a schema/capability
  marker, not a commitment to supporting old protocol versions in this push.
