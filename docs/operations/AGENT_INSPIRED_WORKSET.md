# Agent-Inspired Workset

## Purpose

This document captures the broader WTD improvement workset for agent-hosted
terminal workflows. It started from specific external reviews, including cmux,
but it now reflects a more general perspective on what WTD should become as an
agent-aware Windows terminal host.

- durable host-owned sessions
- explicit workspace/tab/pane structure
- strong CLI and automation story
- Windows-first terminal hosting
- multi-agent coding workflows

This workset covers both:

- making WTD a first-class host for agentic coding
- making WTD a genuinely excellent general-purpose Windows terminal

The current `workspace -> tab -> pane` model remains in scope and is retained.

## What We Learn From Agent Hosts

The strongest ideas from modern agent-host products are not about terminal
rendering. They are about attention management, metadata visibility, and
orchestration.

Key takeaways:

1. Attention is first-class.
   Operators should be able to tell which pane or workspace needs them without
   scanning every pane manually.

2. Metadata reduces context switching.
   A pane list that shows branch, cwd, driver, status, and recent notification
   text is materially better than a bare title list.

3. CLI, UI, and automation should share one control plane.
   If an action matters, it should be available consistently from both the UI
   and the command/API layer.

4. Agent ecosystems matter.
   A terminal host for coding agents should actively meet existing tools where
   they are, including tmux-oriented workflows and agent-specific notification
   hooks.

5. Project-local workflow memory is high leverage.
   Reusable workspace recipes and command-palette commands reduce operator
   memory load and make good practice easier to repeat.

6. WTD should keep its own advantage.
   WTD's split host/runtime architecture is better than a pure UI-owned session
   model for persistence and recovery. That should be kept and strengthened, not
   collapsed.

7. Agent-specific semantics matter.
   Generic terminal hosting is not enough for modern coding agents. For example,
   Pi exposes queue semantics, extension-driven status and notifications, and
   emerging async-task / cross-session workflows that should map into WTD as
   structured state rather than only screen text.

8. Local stack health matters.
   An agent host is more useful when the surrounding local project processes are
   visible and actionable. Crashed workers, dead dev servers, and restart loops
   should not be invisible background failures.

## Product Direction

WTD should become:

- an excellent Windows terminal for everyday shell work
- the best Windows host for multiple coding agents
- easy to drive manually and easy to drive programmatically
- reliable across restore, restart, reattach, minimize, split, and resize paths

WTD should not currently take on:

- an embedded browser pane
- a remote helper daemon
- remote network/browser proxying
- distributed multi-host WTD orchestration

Those may be revisited later, but they are explicitly deferred for now.

Pi-specific note:

- WTD should treat Pi as a first-class hosted agent, not just another TUI.
- Interactive Pi panes remain the default host path.
- Supervisor-style orchestration should rely on explicit waitable pane state and
  agent-published status, not only prompt + capture loops.

## Workset Tracks

## Track A: Agent Attention System

### Outcome

Operators can see which pane or workspace needs attention and jump to it
quickly.

### Scope

- pane/workspace attention state
- notification ingestion from OSC and explicit commands
- agent-published attention and completion signals
- focus-aware notification suppression when the relevant pane is already visible
- unread tracking
- next/previous attention navigation
- notification list / popover / panel
- status-bar and tab/sidebar indicators
- APIs for publishing and clearing attention

### Why It Matters

This is the single most important idea for multi-agent workflows.

## Track B: Structured Pane Metadata and Status

### Outcome

Each pane exposes useful structured state beyond raw screen text.

### Scope

- driver profile
- cwd
- git branch
- current command / session type where available
- ports or service hints where detectable
- short status line
- progress value
- latest notification summary
- last activity timestamp
- activity phase / waitable completion state
- queue summary where the hosted agent exposes one

### Why It Matters

Operators should not need to open six panes just to understand the state of the
workspace.

## Track C: Stable Automation and Driver Model

### Outcome

WTD is easy to control safely from other tools and other agents.

### Scope

- stable documented JSON control protocol
- explicit authentication / local access model
- parity between CLI and protocol surface
- stronger pane-driver capabilities model
- `prompt`/`capture`/`inspect` as the standard agent workflow
- per-driver capabilities and status publication
- first-class waiting primitive for cross-pane coordination

### Why It Matters

This is what makes WTD a host platform, not just a terminal UI.

## Track D: Agent Ecosystem Compatibility

### Outcome

Existing agent tooling can use WTD with minimal adaptation.

### Scope

- tmux-compat shim for pane/workspace operations
- agent hook kits for Pi, Codex, Claude Code, Gemini CLI, Copilot CLI
- notification helper scripts and examples
- compatibility notes and test fixtures

### Why It Matters

Compatibility is leverage. It reduces bespoke glue code and increases adoption.

## Track E: Project Recipes and Workflow Memory

### Outcome

Projects can define reusable WTD commands, layouts, prompts, and workspace
recipes.

### Scope

- project-local command definitions
- command-palette integration
- workspace command macros
- prompt templates
- wait / capture / prompt coordination recipes
- trust confirmation when checked-in workflow files change before auto-run
- driver-aware pane targeting
- shared conventions for common multi-agent workflows

### Why It Matters

This turns WTD from an ad hoc terminal host into a repeatable team tool.

## Track F: Lifecycle and Persistence Reliability

### Outcome

Opening, saving, restarting, reattaching, restoring, and resizing work
predictably.

### Scope

- command lifecycle cleanup (`start/open/attach/stop/restart`)
- reliable save-workspace behavior from UI and CLI
- restore/restart correctness
- full buffer and history rehydration
- minimize/restore and resize robustness
- split/new-pane profile correctness
- startup and restart race cleanup

### Why It Matters

A terminal host for long-running agent sessions must be boringly reliable.

## Track G: General-Purpose Terminal Polish

### Outcome

WTD is a strong everyday Windows terminal, not only an agent shell host.

### Scope

- copy/paste correctness
- keybinding quality and pass-through behavior
- search and scrollback consistency
- selection and link behavior
- profile management and profile switching
- local/WSL/SSH profile polish
- rendering and sizing robustness
- mouse behavior and discoverability

### Why It Matters

General terminal quality compounds everything else. If the basics feel weak, the
agent features do not save the product.

## Track H: Managed Process Health and Recovery

### Outcome

WTD workspaces make the health of the local project stack legible and
actionable, not just the health of interactive agent panes.

### Scope

- visible process health states such as running, exited, restarting, and
  crash-looping
- rate-limited restart behavior surfaced clearly in UI and metadata
- lightweight per-process resource visibility where practical
- restart controls exposed to users and agents
- stack-health context available through inspect / metadata / wait surfaces

### Why It Matters

Coding agents do better work when the surrounding local stack is healthy and
that health is obvious.

## Priority Order

The recommended execution order is:

1. Track A: Agent Attention System
2. Track B: Structured Pane Metadata and Status
3. Track C: Stable Automation and Driver Model
4. Track F: Lifecycle and Persistence Reliability
5. Track H: Managed Process Health and Recovery
6. Track G: General-Purpose Terminal Polish
7. Track E: Project Recipes and Workflow Memory
8. Track D: Agent Ecosystem Compatibility

This order balances product leverage with reliability. It prioritizes the core
agent-host product gains, then hardens WTD's platform quality, then adds
workflow convenience and ecosystem compatibility.

## Explicit Deferrals

The following are intentionally out of this workset for now:

- embedded browser pane
- browser automation inside WTD
- remote helper daemon
- remote CLI relay
- remote notification relay over SSH
- remote network proxying and remote `localhost` browser routing
- distributed multi-host WTD orchestration

These are interesting, but they materially expand the product surface. They are
not required to make WTD the best Windows terminal host for agentic coding.

## Bead Mapping

This workset maps to the following bead epics:

- `Agent Attention System`
- `Structured Pane Metadata`
- `Automation API Stabilization`
- `Lifecycle and Command Cleanup`
- `Workspace Persistence Reliability`
- `Project Recipes and Palette Commands`
- `tmux Compatibility Layer`
- `Agent CLI Notification Hooks`
- `Managed Process Health and Recovery`
- `General-Purpose Terminal Polish`

Each epic should have concrete child tasks with clear acceptance criteria and
test coverage expectations.

Pi-specific follow-on work should include:

- a `wtd wait` primitive for pane coordination
- agent-published status and attention updates
- a Pi integration package or extension pattern that feeds WTD state directly

Solo-inspired follow-on work should include:

- managed process health and restart visibility
- focus-aware notification suppression
- trust confirmation for changed checked-in workflow files before auto-run

## Success Criteria

We should consider this workset successful when:

1. An operator can manage several coding agents without scanning every pane.
2. Another agent can drive WTD using a small, stable set of commands.
3. Workspaces reopen and restore predictably enough for daily long-running use.
4. WTD feels competitive as a normal Windows terminal even outside agent use.
5. Common project workflows can be encoded once and reused by others.
