# SoloTerm Review

## Purpose

This note extracts only the SoloTerm ideas that are useful for WTD's broader
agent-inspired roadmap.

Solo is not the target shape for WTD. Solo is primarily a process-management
layer with built-in terminals, while WTD is a terminal host with durable
workspace/session ownership. The useful question is not "how do we copy Solo?"
The useful question is "what does Solo expose as a real gap in the current WTD
roadmap?"

## What Solo Gets Right

The strongest relevant ideas are:

1. **Process health is visible**
   Solo keeps agents, dev servers, queues, workers, and shells in one dashboard
   and makes health obvious at a glance.

2. **Crash recovery is part of the product**
   Auto-restart with rate limiting is a first-class part of the workflow, not a
   hidden implementation detail.

3. **Shared project command configuration is central**
   A checked-in `solo.yml` makes common project commands and startup behavior
   team-shareable.

4. **Notifications are process-aware**
   Solo's notifications are about things that actually matter in agentic local
   development: crashed services, stuck workers, and commands that need
   attention. It also avoids yelling at you when you are already looking at the
   relevant process.

5. **Agents are better when the local stack is legible**
   Solo's MCP angle is not magic. It simply gives agents visibility into the
   health of the project around them.

## What This Means For WTD

Solo highlights one real gap in the current WTD roadmap:

WTD has strong terminal-host and agent-coordination direction, but it does not
yet explicitly treat **managed local process health** as a first-class part of
the workspace.

That matters because agent panes do not exist in isolation. They are usually
working against:

- a dev server
- a watcher
- a queue worker
- a database proxy
- tests running in the background

If those fail silently, the agent host loses credibility.

## Recommended Additions To The Roadmap

## 1. Managed Process Health and Recovery

WTD should gain an explicit roadmap area for managed process health:

- crash / exited / restarting / crash-loop states
- rate-limited restart behavior that is visible in the UI
- lightweight per-process health metadata
- CPU / memory visibility where practical
- restart controls exposed to both users and agents

Important nuance:

WTD already has restart policy support at the host/session layer. The missing
work is productizing it:

- expose the state clearly
- surface crash loops
- tie it into attention and metadata
- make it easy for agents and users to act on it

## 2. Focus-aware notification suppression

This is small but worthwhile.

If the user is already looking at the pane/process that emitted the event, WTD
should suppress or downgrade the external notification. This fits naturally into
the existing attention roadmap.

## 3. Shared command safety for checked-in workflow files

WTD is already moving toward project-local recipes and shared workspace command
definitions. Solo's trust-on-change behavior is a good addition:

- if a checked-in workflow/recipe/workspace file changed after a pull, WTD
  should confirm before auto-running newly changed commands

This is especially relevant once WTD grows more project-local recipes and
managed services.

## 4. Better process health visibility for agents

This does not require turning WTD into Solo or building a full process
dashboard. It means:

- agents can inspect process health cleanly
- status metadata includes process health where relevant
- waiting/attention flows can include stack-health context

That complements the new `wtd wait` direction.

## What We Should Not Pull From Solo

These are not strong additions for WTD right now:

- Solo's overall "dashboard first, terminal second" center of gravity
- full app-level process dashboard as the primary UX
- Tauri-style product direction as such
- pricing / commercial packaging ideas
- Raycast-specific integration

WTD should remain terminal-first.

## Recommended WTD Changes From This Review

1. Add a roadmap area for managed process health and recovery.
2. Add focus-aware notification suppression to the attention work.
3. Add trust confirmation for changed checked-in workflow files before auto-run.
4. Ensure metadata/inspect/wait surfaces can include process health and restart
   state.

## Summary

The good Solo idea is not "be more like Solo."

The good Solo idea is:

**an agent host should not only manage panes; it should make the health of the
local project stack legible and actionable.**
