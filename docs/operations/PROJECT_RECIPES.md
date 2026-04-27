# Project Recipes

Project recipes let a repository define reusable WTD workflows in a checked-in
manifest. The first supported file names are:

- `.wtd/recipes.yaml`
- `wtd-recipes.yaml`

`wtd recipe` searches upward from the current directory unless `--file` is
provided.

## Schema

```yaml
version: 1
commands:
  - name: test-and-review
    description: Run tests, wait for completion, then capture output
    cwd: .
    env:
      RUST_LOG: info
    target:
      workspace: build-and-test
      tab: main
      pane: tests
    palette: true
    steps:
      - type: prompt
        text: cargo test
      - type: wait
        condition: done
        timeout: 60
        recentLines: 80
      - type: capture
        lines: 80
      - type: action
        target: build-and-test/main/reviewer
        action: focus-pane
```

Command fields:

- `name`: stable command id.
- `description`: human-readable summary for list and palette surfaces.
- `cwd`: intended working directory for future auto-run trust checks.
- `env`: intended environment overlay for future auto-run trust checks.
- `target`: default workspace/tab/pane selector used by steps without an
  explicit target.
- `palette`: whether UI palette surfaces should expose the recipe.
- `steps`: sequential operations.

Step types:

- `prompt`: sends driver-aware prompt text to a pane.
- `capture`: captures recent output from a pane.
- `wait`: waits for `idle`, `done`, `needs-attention`, `error`,
  `queue-empty`, or `state-change`.
- `action`: invokes a WTD workspace/pane action.

## CLI

```bash
wtd recipe list
wtd recipe show test-and-review
wtd recipe run test-and-review --dry-run
wtd recipe run test-and-review
```

`--dry-run` prints the WTD operations without connecting to the host. Normal
execution sends each step to the host in order and stops on the first non-zero
result.

## Trust Boundary

The manifest records `cwd`, `env`, and all executable workflow steps in one
checked-in file. WTD does not auto-run changed checked-in recipe files in this
bead; that is deliberately left for the follow-up trust-check bead. Until then,
recipes run only after an explicit `wtd recipe run` command or a user-selected
palette entry.

## Multi-Pane Agent Example

```yaml
version: 1
commands:
  - name: pi-test-review
    description: Run tests in one Pi pane and hand results to a reviewer pane
    target:
      workspace: agents
      tab: main
      pane: worker
    palette: true
    steps:
      - type: prompt
        text: Run the focused tests and publish WTD status when complete.
      - type: wait
        condition: done
        timeout: 120
        recentLines: 120
      - type: capture
        lines: 120
      - type: prompt
        target: agents/main/reviewer
        text: Review the worker pane's latest test result and summarize risks.
      - type: wait
        target: agents/main/reviewer
        condition: needs-attention
        timeout: 120
        recentLines: 80
```

This composes with the Pi bridge in `tools/agent-hooks/pi/`: Pi publishes
`working`, queue, completion, and attention state; the recipe waits on those
states instead of scraping screen text.
