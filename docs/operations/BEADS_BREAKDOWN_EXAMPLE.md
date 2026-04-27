# Beads Breakdown Example

This example shows a breakdown style intended for high-autonomy bead execution.

It is stricter than the general guide.

The assumption here is:
- the worksets are already detailed,
- the beads are expected to be directly executable,
- and the bead runner should move forward with minimal additional review.

This style is useful when:
- the engineering direction is already clear,
- the interfaces are mostly known,
- the work can be expressed as concrete outcomes,
- and you want the runner to keep going bead by bead.

## Example Scenario

Suppose the project goal is:

"Build a small console configuration editor that can load a JSON config file, display sections, edit values, validate the document, and save changes back to disk."

Assume the design is already fairly specific:
- Rust application
- TUI shell already chosen
- config format already fixed
- validation rules already known
- save behavior already known

This is a good candidate for a more auto-run-friendly bead breakdown.

## Step 1: Detailed Worksets

In this style, worksets are already close to execution.

Instead of broad worksets like:
- "configuration support"
- "editing UI"

we use more detailed worksets like:

1. Load config file into in-memory model
2. Display sections and keys in the UI
3. Open one selected key for editing
4. Validate edited values against schema rules
5. Save validated changes back to disk

These worksets are still larger than beads, but they are already narrow enough that bead generation is straightforward.

## Step 2: Executable Bead Breakdown

From those detailed worksets, we create beads that are intended to run with little ambiguity.

### Parent Bead

`bd-500`
Console config editor first executable slice

Meaning:
- the app can load one config file,
- display its structure,
- edit one value,
- validate it,
- and save it back successfully.

This parent bead is blocked on the child beads below.

### Child Beads

`bd-501`
Load a JSON config file into the editor's in-memory config model

Expected outcome:
- the application can open a real config file,
- parse it into the internal model,
- and expose the loaded structure to the UI layer.

Completion evidence:
- loading succeeds for a valid fixture file,
- failure is surfaced cleanly for invalid JSON,
- a test or demo path proves the model is populated.

`bd-502`
Render config sections and keys in the navigation pane from the loaded model

Depends on:
- `bd-501`

Expected outcome:
- the UI shows sections and keys from the loaded config,
- and a user can move selection across them.

Completion evidence:
- a loaded fixture config appears in the UI,
- navigation changes the selected item,
- the rendered list stays in sync with the model.

`bd-503`
Open the selected config key in an editable value editor

Depends on:
- `bd-502`

Expected outcome:
- selecting a key opens its current value in the editing surface,
- and the edit buffer is initialized from the model.

Completion evidence:
- a selected key's value appears in the editor,
- editing changes the in-memory value draft,
- switching keys updates the editor correctly.

`bd-504`
Validate edited config values against the known schema rules

Depends on:
- `bd-503`

Expected outcome:
- edited values are checked before save,
- valid edits are accepted,
- invalid edits surface a clear validation error.

Completion evidence:
- at least one valid case passes,
- at least one invalid case fails with the expected validation message,
- validation result is visible to the caller or UI.

`bd-505`
Save validated config edits back to the source file

Depends on:
- `bd-504`

Expected outcome:
- validated edits persist to disk,
- reloading the file reflects the saved value,
- invalid edits do not write corrupted output.

Completion evidence:
- a fixture file can be edited and saved,
- re-open proves the new value is persisted,
- invalid state does not write output.

## Why This Breakdown Is Auto-Run Friendly

These beads are suitable for a runner because:
- each bead has one clear outcome,
- each bead has explicit dependency order,
- each bead has visible completion evidence,
- and each bead is framed as a capability rather than an open-ended investigation.

There is little room for the runner to ask:
- "what exactly do you mean?"
- "what counts as done?"
- "should I keep going?"

That is the point.

## The Completion Rule

For auto-run, completion must be strict.

When a bead is worked, one of two things must happen:

1. the bead is fully completed and can be closed
2. the bead cannot be fully completed, and a new bead must be created for the blocking or follow-up work

What should not happen:
- the runner silently leaves the bead half-done
- the runner smears extra work into the bead without tracking it
- the runner declares success when the stated outcome is not actually achieved

## The Blocking / Follow-Up Rule

If the runner encounters a real blocker, it must:

1. explain the blocker clearly
2. create a new bead for the blocker or follow-up work
3. link it to the current bead as discovered work
4. leave the current bead in the appropriate non-closed state unless the original bead can still be completed

Examples:

- if saving requires a serializer fix not covered by the current bead:
  - create a new bead for serializer repair
  - do not pretend the save bead is complete

- if validation reveals a missing schema rule implementation:
  - create a new bead for that rule support
  - keep the current bead honest

The important principle is:
- every uncovered piece of necessary work must either be completed now or be tracked as a bead

Nothing should disappear into narrative text.

## GPT-5.5-Friendly Auto-Run Prompt

Use a stricter runner prompt for this style of bead graph, but keep it
outcome-first. The runner should know what success means and when to stop
without being forced through an unnecessary implementation script.

```text
Work exactly one bead at a time.

You are operating in auto-run-friendly bead mode.

Goal:
- Complete the assigned bead's stated outcome.
- Verify the bead's completion evidence.
- Keep the bead graph honest.

Context gathering:
- Read the bead, cross-bead memory, and only the spec/code sections needed for this bead.
- Stop gathering context once the files, interfaces, and validation checks are clear.
- Search again only if implementation or validation reveals a new unknown.

Stop rules:
- If the bead outcome is fully achieved, close the bead.
- If the bead cannot be fully completed, create and link follow-up beads for blocking or newly discovered required work.
- Do not start the next bead until the current bead has either been completed or had its blocking follow-up beads created.

A. If the bead outcome is fully achieved:
- close the bead
- summarize what changed
- state what was verified
- recommend the next ready bead

B. If the bead cannot be fully completed:
- do not close it as complete
- create one or more new beads for the blocking or newly discovered required work
- explain the blocker clearly
- state whether the current bead should remain in progress, be deferred, or be split
- recommend the next sensible bead

Do not leave necessary follow-up work untracked.
Do not silently broaden the bead beyond its intended scope unless that work is still necessary to complete the bead outcome directly.
```

## What Makes This Different from a Looser Breakdown

Compared with a looser bead style, this example:
- uses narrower worksets,
- defines tighter bead outcomes,
- gives explicit completion evidence,
- and enforces a strict "complete or create follow-up beads" rule.

That is what makes it suitable for higher-autonomy execution.

## Standard for This Style

Use this stricter style when:
- the design is already specific,
- you want a runner to keep moving,
- and you want bead state to remain trustworthy without constant reinterpretation.

Do not use it when:
- the design is still exploratory,
- the interfaces are not stable,
- or the real work is still architectural discovery rather than execution.
