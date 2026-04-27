# Agent Notification Compatibility

WTD supports a common notification/status vocabulary for hosted agent CLIs:

- `working`
- `queued`
- `input-needed`
- `completed`
- `error`
- `idle`

The hook scripts in `tools/agent-hooks/` map that vocabulary to `wtd status`,
`wtd notify`, and `wtd clear-attention`. Run
`tools/agent-hooks/test-agent-hooks.ps1 -IncludeBash` to validate the reference
translations.

## Expected Behavior

| Agent | Working | Queue | Input needed | Completion | Error |
|-------|---------|-------|--------------|------------|-------|
| Pi | publish `phase=working` from lifecycle hooks | publish `phase=queued` with pending count | notify `needs-attention` for input or approval requests | publish `phase=done` and notify `done` | publish `phase=error` and notify `error` |
| Codex | wrapper emits before long-running task | wrapper may publish queued orchestration work | supervisor wrapper emits when human review is needed | exit code `0` maps to `done` | non-zero exit maps to `error` |
| Claude Code | wrapper emits before invocation | wrapper-specific | approval/tool prompts map to `needs-attention` | successful invocation maps to `done` | failed invocation maps to `error` |
| Gemini CLI | wrapper emits before invocation | queued prompts map to pending count when known | confirmation prompts map to `needs-attention` | successful invocation maps to `done` | failed invocation maps to `error` |
| Copilot CLI | wrapper emits before generation or execution | wrapper-specific | command review/confirmation maps to `needs-attention` | successful command maps to `done` | failed generation/execution maps to `error` |

## Manual Verification

```powershell
tools/agent-hooks/wtd-agent-event.ps1 -Target agents/codex -Agent codex -Event working -Message running -WhatIf
tools/agent-hooks/wtd-agent-event.ps1 -Target agents/pi -Agent pi -Event queued -QueuePending 2 -WhatIf
tools/agent-hooks/wtd-agent-event.ps1 -Target agents/claude -Agent claude-code -Event input-needed -Message approval -WhatIf
tools/agent-hooks/wtd-agent-event.ps1 -Target agents/gemini -Agent gemini-cli -Event completed -Completion turn -Message done -WhatIf
tools/agent-hooks/wtd-agent-event.ps1 -Target agents/copilot -Agent copilot-cli -Event error -Message failed -WhatIf
```

The same cases are covered by the harness. The Pi-specific event bridge in
`tools/agent-hooks/pi/` builds on the same vocabulary.
