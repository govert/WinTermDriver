# Beads Working Method

This note describes the working method we want when turning an engineering design into executable bead-based work.

It is intentionally about the bead method itself, not about `ntm`, Agent Mail, or multi-agent orchestration.

## Purpose

The goal of the beads approach is to convert a design into a sequence of real, reviewable units of progress.

It is not meant to produce:
- a giant abstract roadmap,
- a flat dump of tickets,
- or a backlog full of tiny pseudo-tasks that create bookkeeping without moving the project.

The core idea is:

1. understand the design,
2. identify the first thin slice,
3. reduce the work into meaningful outcomes,
4. express those outcomes as dependency-aware beads,
5. execute one ready bead at a time.

## The Layers

Design should move through these layers:

1. Engineering spec
2. Worksets
3. Beads
4. Execution

Each layer should reduce ambiguity.

### 1. Engineering Spec

This is the broad design material:
- scope,
- purpose,
- constraints,
- architecture,
- interfaces,
- risks,
- and non-goals.

At this stage, the design is usually too broad to execute directly.

The spec answers:
- what are we building?
- why are we building it?
- what matters most?
- what must be true when it works?

### 2. Worksets

A workset is a major capability slice or milestone-sized area of progress.

Worksets are larger than beads, but smaller than the full design.

A workset should describe a meaningful engineering outcome such as:
- load project data into the app,
- render workspace/module navigation,
- round-trip edits through the project model,
- expose host policy configuration.

Good worksets:
- describe capabilities,
- have clear boundaries,
- can be decomposed into several beads,
- and often correspond to one thin slice or one milestone area.

Bad worksets:
- are vague themes like "improve architecture",
- or are huge umbrellas like "build the IDE".

### 3. Beads

A bead is the unit of executable progress.

A bead should be:
- concrete,
- reviewable,
- assignable,
- and small enough that someone can reasonably complete or clearly advance it in one focused session.

Beads are not meant to be:
- design paragraphs,
- broad aspirations,
- or one-line micro-steps for every trivial action.

## The Design Reduction Process

When converting a spec into beads, use this sequence.

### Step 1: Identify the Thin Slice

Find the first end-to-end slice that proves the design direction.

A thin slice should:
- exercise the core architecture,
- produce something concrete,
- and avoid premature expansion into every future feature.

Examples:
- open a project, display modules, open one module, save changes,
- load a config, render it, edit one value, persist it,
- compile one basic unit and show diagnostics.

The thin slice is the backbone for the first workset.

### Step 2: Extract Worksets

Reduce the spec into a small number of capability outcomes.

A good target is usually:
- 3 to 7 worksets for a phase,
- not 20 to 40.

Each workset should answer:
- what meaningful capability becomes real when this is done?

Examples:
- project loading,
- workspace navigation,
- editor binding,
- save round-trip,
- diagnostics integration.

### Step 3: Break Worksets into Beads

For each workset, define the beads that create real forward motion.

A bead should describe an outcome, not just an activity.

Prefer:
- "Load an OxVba project into OxIde"
- "Show project modules in the workspace UI"
- "Save module edits back to the project model"

Avoid:
- "Think about project loading"
- "Work on module list"
- "Do more UI"

### Step 4: Add Dependency Structure

Dependencies are what turn beads from a list into an execution graph.

Ask:
- what must happen first?
- what becomes possible only after another bead?
- what can proceed independently?

Examples:
- module navigation depends on project loading
- editor binding depends on module navigation
- save round-trip depends on editor binding

Dependencies should reflect real engineering ordering, not imagined process neatness.

### Step 5: Check the Ready Path

After creating the beads and dependencies, the graph should produce a sensible ready queue.

The key test is:
- does `br ready` surface the actual next bead you would want to execute?

If not, the dependency structure is wrong or the beads are still too vague.

## Bead Size and Scope

The intended bead size is:
- one real unit of progress,
- usually doable in one focused session,
- or at least small enough to review clearly at the end of a session.

### Too Large

These are too large:
- "Build project system"
- "Implement language services"
- "Create the IDE"

They contain too many unknowns and too many internal milestones.

### Too Small

These are too small:
- "Create struct"
- "Add one menu item"
- "Rename variable"

These are implementation steps, not work units.

### About Right

These are about right:
- "Load an OxVba project into OxIde"
- "Show project modules in the workspace UI"
- "Open one module in the editor surface"
- "Save module edits back to the project model"

Each one:
- changes the state of the system,
- can be tested or reviewed,
- and leaves the next step clearer.

## Unit of Work Philosophy

A bead should map to a unit of work that can be:
- understood before starting,
- executed without broad ambiguity,
- and judged afterward as complete, incomplete, blocked, or split.

A bead is successful when a reviewer can say:
- yes, this capability now exists,
- or no, this needs to be split or clarified.

That means a bead should produce one of:
- a concrete capability,
- a concrete design decision,
- a concrete integration,
- or a concrete testable result.

## Dependency Philosophy

Dependencies should express engineering truth, not paperwork.

Use dependencies when:
- one bead truly cannot proceed until another exists,
- one capability unlocks another,
- the architecture requires sequencing,
- or the project model demands an order.

Do not add dependencies just to make the graph look structured.

Good dependency use:
- project load before module list
- module list before open editor
- open editor before save back to model

Bad dependency use:
- every bead depends on the previous one just because it was written first
- broad parent beads used as blockers when they should be summaries

## Execution Philosophy

The beads method embodies a specific execution style.

### 1. Work from Ready Beads

Do not choose work from memory or from emotional momentum.

Use the ready set:
- what is unblocked,
- what is real,
- what is the next sensible move.

### 2. One Bead at a Time

The normal mode is:
- pick one bead,
- work it,
- review it,
- close it or split it,
- then choose the next ready bead.

This keeps scope clear and review frequent.

### 3. Close or Split, Do Not Smear

If a bead turns out to be bigger than expected:
- do not keep silently expanding it,
- split follow-up work into new beads.

The point is to keep the bead graph honest.

### 4. Let Discovery Create New Beads

Design is never perfectly complete.

If new work is discovered:
- create a new bead,
- link it to the parent or source bead,
- and keep going.

Discovery is normal. Hidden work is the problem.

### 5. Review by Outcome

At the end of bead work, ask:
- what now exists that did not exist before?
- what was verified?
- what remains?

This keeps the method anchored in engineering outcomes rather than task theater.

## Parent Beads and Child Beads

Use parent beads when they help summarize a milestone or thin slice.

A good parent bead:
- represents a larger outcome,
- is blocked on child beads,
- and becomes done when the children are done.

Example:
- parent: first executable project/workspace editor slice
- children:
  - load project
  - show modules
  - open module
  - save round-trip

The parent is useful as a milestone summary.
The children are the actual execution path.

## What the Beads Method Is Trying to Prevent

This method exists partly to avoid common planning failures:

- giant specs with no executable path
- flat backlogs with no dependency truth
- huge tasks that never feel done
- microscopic tasks that create noise
- work chosen by intuition instead of readiness
- hidden follow-up work that never gets tracked

## Practical `br` Loop

The minimal execution loop is:

1. inspect ready work
2. choose one bead
3. mark it in progress
4. do the work
5. close it or split follow-up work

Core commands:

```bash
br ready
br show <id>
br update <id> --status in_progress
br close <id> --reason "Completed"
```

Create new beads when discovery happens:

```bash
br create "New bead title" -t task -p 2
```

## Prompt Shape for Spec-to-Beads Preparation

When asking an agent to prepare a spec for bead generation, do not ask it to immediately explode the spec into tickets.

Instead, ask it to produce:

1. project intent
2. in-scope and out-of-scope
3. first thin slice
4. 3-7 worksets
5. candidate beads for each workset
6. dependency structure
7. ambiguities that must be clarified before bead generation

This keeps the bead breakdown tied to architecture and execution reality.

## Standard for a Good Bead Breakdown

A good bead breakdown should have these properties:

- the first ready bead is obvious
- the dependency order feels technically correct
- each bead has clear reviewable scope
- the thin slice is visible in the graph
- future work is present but not over-specified
- discovered work can be added naturally later

If those are true, the bead graph is in good shape.
