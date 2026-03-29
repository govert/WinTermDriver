# Bead Runner — Agent Instructions

You are executing a single bead from the WinTermDriver project as part of an automated bead runner. Each bead runs in a fresh Claude session. Your job is to implement the bead, verify it works, commit, and close it.

## Project Context

**WinTermDriver** is a Windows-native terminal workspace manager written in Rust. Three processes — `wtd-host` (background, owns ConPTY sessions), `wtd-ui` (graphical window), `wtd` (CLI controller) — communicate via Windows named pipes.

- Engineering spec: `WINTERMDRIVER_SPEC.md` (read relevant sections as needed, do not load the whole file)
- Platform: Windows 10 1809+, Rust stable, MSVC target
- Dependencies: `windows-rs` for Win32/ConPTY, `serde`/`serde_yaml` for config, `crossterm`/renderer TBD for UI

## How to Work

1. **Read the bead details** below. Note the expected outcome, completion evidence, and spec references.
2. **Read the cross-bead memory** for decisions and context from previous beads.
3. **Read spec sections** referenced by the bead (use offset/limit to read specific sections of `WINTERMDRIVER_SPEC.md`).
4. **Examine existing code**. Previous beads may have created crates, types, or patterns you should follow. Always check what exists before creating new files.
5. **Implement the bead**:
   - Write clean, idiomatic Rust.
   - Follow existing project structure and naming conventions.
   - If no Cargo workspace exists yet, create one (workspace `Cargo.toml` at repo root, member crates in subdirectories).
   - Write tests that demonstrate the completion evidence described in the bead.
6. **Run tests**: `cargo test --workspace` must pass.
7. **Commit** your code changes with a message like: `bead <id>: <brief description>`

## Closing the Bead

After implementation and tests pass, close the bead using the `br` CLI tool:

```bash
br close <bead-id> --reason "Completed: <one-line summary of what was done>"
```

**IMPORTANT:** The command is `br close`, NOT `bash bead-runner.sh close` or any
variation involving the runner script. The runner script does not accept subcommands.
`br` is the issue tracker CLI. `bead-runner.sh` is the outer loop that invoked you —
never call it.

## Updating Cross-Bead Memory

If you made decisions that future beads need to know, **append** to `tools/MEMORY.md`. Examples of things worth recording:

- Crate names and their purpose (e.g., "wtd-core contains shared types")
- Key type names and where they live
- Architecture decisions (e.g., "using vte crate for VT parsing")
- API patterns established (e.g., "all IPC messages implement BeadMessage trait")
- Gotchas discovered (e.g., "ConPTY requires specific pipe flags on Windows")

Keep entries concise. Use this format:

```markdown
## <bead-id>: <topic>
<what future beads need to know>
```

## Handling Partial Completion

If part of the bead's work is blocked or turns out to be too large for one session:

1. **Complete what you can** — implement and test the parts that work.
2. **Create a follow-up bead** for the remaining work:
   ```bash
   br create "Follow-up: <what remains>" \
     --type task \
     --priority <same as this bead> \
     --labels <same workset label> \
     --description "Split from <this-bead-id>. Remaining work: <details>"
   ```
3. **Wire dependencies**: anything that depended on the blocked part should depend on the follow-up:
   ```bash
   br dep add <follow-up-id> <this-bead-id>
   br dep add <downstream-id> <follow-up-id>
   ```
4. **Close this bead** with a note about what was split:
   ```bash
   br close <bead-id> --reason "Partial: <done>. Follow-up <follow-up-id> for <remaining>."
   ```

## Constraints

- **Only work on YOUR bead.** Do not modify or close other beads.
- **Do not push** to the remote repository — the runner or user handles that.
- **Do not run `br sync`** — the runner handles JSONL export.
- **Do not run `bead-runner.sh`** — it is the outer loop. You are inside it already.
- **Keep commits focused** on this bead's changes.
- If existing code (from other beads) has test failures, **note it in MEMORY.md** but do not fix other beads' code.
- Do not add features beyond the bead's scope. If you discover needed work, create a follow-up bead.
