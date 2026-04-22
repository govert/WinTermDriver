# Pi Agent Integration Review

## Purpose

This document reviews the current Pi coding agent surfaces that matter for
WinTermDriver integration and updates the WTD product direction accordingly.

The goal is not to embed Pi into WTD. The goal is to make WTD an excellent host
for Pi sessions and a good control surface for supervisors coordinating
multiple Pi panes.

## Current Pi Surface Area

Pi currently exposes four distinct integration modes:

- interactive TUI
- print / JSON mode
- RPC mode over stdin/stdout
- SDK embedding

For WTD, all four are relevant, but not equally.

### 1. Interactive TUI

This is the mode WTD already hosts in panes today. It matters because Pi is not
just a line-oriented shell application:

- it has a multiline editor
- it supports queued messages while busy
- it can show status and widgets
- it supports extension-provided UI

This means "prompt" support alone is not enough. WTD must understand that Pi
has meaningful session states beyond "screen changed recently".

### 2. JSON / RPC

Pi's RPC mode provides a clean machine surface:

- streamed lifecycle events such as `agent_start`, `agent_end`, `turn_start`,
  `turn_end`, `tool_execution_*`, and `queue_update`
- explicit prompt queueing semantics for busy agents
- extension UI requests and fire-and-forget UI messages

This is important even if WTD does not initially host Pi through RPC. RPC tells
us what Pi itself considers meaningful state.

### 3. Extension API

Pi's extension API is the strongest fit for WTD integration.

Extensions can:

- react to session lifecycle events
- publish notifications
- set footer status text
- set widgets
- set terminal title
- prefill or paste into the editor
- override or intercept shell execution
- execute external programs via `pi.exec()`

That means a Pi-to-WTD integration package can be built without waiting for Pi
core changes.

### 4. Emerging async and coordination patterns

Recent Pi work shows the ecosystem is already moving toward multi-agent and
background-task workflows:

- the async-tasks extension example demonstrates background Pi subprocesses,
  persistent task tracking, and TUI status integration
- the event-bus RFC demonstrates cross-session coordination, notification
  routing, and session registration via an external CLI

These are strong signals that WTD should not model agent coordination as only
"send prompt, then poll screen".

## What This Means For WTD

## 1. The correct WTD integration path is hybrid

For Pi, WTD should support two complementary paths:

- **host mode**: run the full Pi TUI in a normal pane
- **coordination mode**: let Pi or a Pi extension publish structured status,
  attention, and completion signals into WTD

The first path preserves Pi's full UX.
The second path gives supervisors a real control plane.

WTD should not rely on screen parsing alone for Pi orchestration.

## 2. WTD should model Pi queue semantics explicitly

Pi distinguishes at least these states:

- actively working
- queued steering message pending
- queued follow-up pending
- idle / awaiting user

WTD's current attention and metadata roadmap is compatible with this, but too
generic. Pi integration should explicitly account for:

- busy state
- queued message counts or queue summary
- waiting-for-user / needs-input state
- completed / error state

## 3. WTD should treat agent-published status as first-class

Pi extensions can already publish status, widgets, and notifications. WTD
should provide matching host primitives so a Pi extension can call something as
simple as:

- `wtd notify ...`
- `wtd set-status ...`
- `wtd set-progress ...`
- `wtd complete ...` or equivalent completion marker

This is better than trying to infer "done" from terminal silence.

## 4. WTD needs a real waiting primitive

The most important missing primitive for supervisor workflows is:

`wtd wait <pane>`

This should not mean "sleep and then capture". It should mean:

- wait on structured pane state
- return early when the pane reaches a target condition
- return timeout context when the condition is not reached

## Recommended `wtd wait` Shape

### Primary command

```text
wtd wait <pane> [--for <condition>] [--timeout <duration>] [--recent-lines <n>] [--json]
```

### Suggested conditions

- `idle`
- `done`
- `needs-attention`
- `error`
- `queue-empty`
- `state-change`

### Suggested behavior

Success:

- exits `0`
- returns the matched condition, current pane metadata, and recent output

Timeout:

- exits with a distinct timeout code
- returns:
  - current pane state
  - attention state
  - driver profile
  - recent notification/status text
  - recent visible output and/or recent captured lines

### Why this is needed

For Pi specifically, a coordinator often wants:

- "wait until this pane is idle, then prompt it again"
- "wait until this pane needs attention, then focus it"
- "wait until the task is done, else give me the latest state and output"

That is a first-class workflow primitive, not a convenience wrapper.

## How Pi Can Feed `wtd wait`

The most practical first implementation is:

1. WTD provides the waitable state model and the `wtd wait` CLI.
2. Pi integration publishes state into WTD using explicit commands from a Pi
   extension or hook package.
3. WTD falls back to generic pane heuristics when no explicit agent state is
   available.

That lets WTD work well with Pi without forcing a deep RPC embedding on day one.

## Recommended Pi-Specific Roadmap Adjustments

## Agent Attention System

Expand the scope to explicitly include agent-published attention and completion
signals, not only OSC notifications and manual commands.

Pi-specific goal:

- a Pi extension can mark a pane as `needs_attention`, `done`, `error`, or
  `working`

## Structured Pane Metadata

Expand the metadata model to include:

- activity phase
- queue summary
- last structured notification
- waitable completion state

Pi-specific goal:

- map Pi's working/queued/idle semantics into stable pane metadata

## Automation API Stabilization

Add `wait` as a first-class automation primitive.

Pi-specific goal:

- a planning/supervisor agent can reliably coordinate multiple Pi panes without
  screen scraping loops

## Agent CLI Notification Hooks

This track should explicitly include Pi. The current wording only names Codex,
Claude Code, Gemini CLI, and Copilot CLI.

Pi-specific goal:

- provide a reference Pi package or extension that publishes WTD status,
  attention, and completion signals

## Project Recipes and Palette Commands

Pi coordination benefits strongly from project-local recipes.

Examples:

- wait for `worker-1` to finish, then prompt `reviewer`
- broadcast a status request to all Pi panes
- focus the next pane needing attention

## Recommended Execution Order For Pi Readiness

1. Define waitable pane state model in WTD
2. Add `wtd wait`
3. Add agent-published status/attention primitives
4. Add a Pi notification/status integration package
5. Add supervisor recipes that combine `prompt`, `wait`, and `capture`

## Recommendation Summary

WTD is already well positioned to host Pi interactively, but not yet well
positioned to coordinate Pi-rich workflows at scale.

The key product move is:

- stop treating agent coordination as only prompt + capture
- add explicit waitable state and agent-published status

For Pi, that is the difference between "terminal that can run Pi" and "best
Windows host for multiple Pi agents".
