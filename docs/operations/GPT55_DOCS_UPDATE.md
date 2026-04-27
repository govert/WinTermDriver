# GPT-5.5 Documentation Update

## What Changed

This repo's agent and operations docs were updated to work better with GPT-5.5
and Codex-style coding agents while keeping the existing worksets and beads
method.

The main change is a shift toward outcome-first instructions:

- define the goal and success criteria clearly
- keep constraints and stop rules explicit
- let the model choose the efficient implementation path from repo context
- avoid long procedural scripts unless a sequence is truly required
- require validation before closing work

## Files Updated

- `AGENTS.md` now includes GPT-5.5/Codex working guidance for outcome-first
  execution, bounded context gathering, validation, and future Responses API
  `phase` handling.
- `tools/bead-instructions.md` now frames each bead around success criteria,
  focused context gathering, targeted validation, and a clear completion
  contract.
- `docs/operations/BEADS_WORKING_METHOD.md` now describes how to write
  GPT-5.5-friendly beads: expected outcome, completion evidence, validation,
  failure behavior, and stop rules.
- `docs/operations/BEADS_BREAKDOWN_EXAMPLE.md` now uses an outcome-first
  auto-run prompt instead of a rigid step-by-step script.
- `docs/operations/AGENT_INSPIRED_WORKSET.md` and
  `docs/operations/PLANNED_ADDITIONS_OVERVIEW.md` now call out GPT-5.5
  implications for WTD's automation protocol, recipes, state continuity, and
  validation surfaces.

## Skill Update

The local `openai-docs` skill was stale. Its bundled fallback references still
described GPT-5.4 as the latest model. I refreshed the local system skill from
`openai/skills`, and its resolver now returns:

- model: `gpt-5.5`
- migration guide: `https://developers.openai.com/api/docs/guides/upgrading-to-gpt-5p5.md`
- prompting guide: `https://developers.openai.com/api/docs/guides/prompt-guidance.md`

Future Codex sessions may need a restart to load the refreshed skill body, but
the local skill files have been updated.

## Why This Helps

GPT-5.5 guidance emphasizes shorter prompts that specify outcomes, constraints,
validation, and stop conditions. The repo docs now align the bead workflow with
that style:

- less unnecessary process prescription
- clearer bead completion criteria
- more explicit validation expectations
- better handling of long-running tool-heavy work
- cleaner separation between durable project guidance and dynamic bead context

This should make Codex/GPT-5.5 agents more decisive without weakening the bead
graph, workset planning, or WTD product intent.

## Sources

- `https://developers.openai.com/api/docs/guides/latest-model`
- `https://developers.openai.com/api/docs/guides/prompt-guidance`
