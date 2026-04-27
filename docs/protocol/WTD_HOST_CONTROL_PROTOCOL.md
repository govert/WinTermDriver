# WTD Host Control Protocol

## Scope

This document defines the local host-control protocol used by `wtd`, `wtd-ui`,
and automation clients to control a per-user `wtd-host` process.

The current protocol is a single forward schema for this product push. Old
human-facing command names, including `wtd up`, are not part of this schema.
The UI-launching lifecycle command is `wtd start`; the underlying host message
remains `OpenWorkspace`.

## Transport

Clients connect to a Windows named pipe:

```text
\\.\pipe\wtd-{user-SID}
```

The pipe is local-only and scoped to the creating user's SID. Remote relay,
network transport, and cross-user attach are out of scope for this protocol.

Each message is framed as:

```text
u32 little-endian byte length
UTF-8 JSON envelope
```

The maximum frame size is 16 MiB.

## Envelope

Every request, response, and pushed event uses this envelope:

```json
{
  "id": "req-1",
  "type": "MessageType",
  "payload": {}
}
```

`id` is a client-generated correlation string. Responses use the request `id`.
Host-pushed UI events use host-generated ids.

## Handshake And Version

The first client message must be `Handshake`:

```json
{
  "id": "hello-1",
  "type": "Handshake",
  "payload": {
    "clientType": "cli",
    "clientVersion": "0.1.0",
    "protocolVersion": 1
  }
}
```

The host replies with:

```json
{
  "id": "hello-1",
  "type": "HandshakeAck",
  "payload": {
    "hostVersion": "0.1.0",
    "protocolVersion": 1,
    "accessPolicy": {
      "transport": "windows-named-pipe",
      "scope": "same-user-local",
      "identity": "current-user-sid",
      "remoteAccess": false
    }
  }
}
```

`protocolVersion` is the current capability discovery boundary. Version `1`
means the message table below is available. If versions differ, the host returns
`Error { code: "protocol-error" }` and closes the connection.

## Access Policy

Protocol v1 is local automation only. The host creates
`\\.\pipe\wtd-{user-SID}` with a DACL for the current user and verifies each
connecting process resolves to the same user SID before accepting the protocol
handshake. `HandshakeAck.accessPolicy` reports this enforced model:

| Field | Value | Meaning |
|-------|-------|---------|
| `transport` | `windows-named-pipe` | Local Windows named-pipe IPC |
| `scope` | `same-user-local` | Only processes running as the host user may connect |
| `identity` | `current-user-sid` | Client identity is the Windows user SID |
| `remoteAccess` | `false` | Network, relay, and cross-user access are not enabled |

CLI and UI clients do not need tokens for same-user local automation. Broader
access, such as cross-user attach, remote relay, or network transport, requires
a future protocol capability and explicit configuration; it is not implied by
the v1 pipe name or handshake.

Dynamic capability listing is not implemented yet. Planned capability names for
the forward push are reserved as:

| Capability | Purpose |
|------------|---------|
| `attention-v1` | Agent-published attention and notification state |
| `pane-metadata-v1` | Structured pane metadata/status publication |
| `wait-v1` | Waitable pane coordination and timeout snapshots |
| `recipes-v1` | Project-local workflow recipes |
| `tmux-shim-v1` | Focused tmux-compatible command subset |

## CLI Mapping

Human-facing commands map to protocol messages as follows:

| CLI | Host message |
|-----|--------------|
| `wtd start <name>` | `OpenWorkspace`, then launch `wtd-ui --workspace <name>` |
| `wtd open <name>` | `OpenWorkspace` |
| `wtd attach <name>` | `AttachWorkspace` |
| `wtd close <name>` | `CloseWorkspace` |
| `wtd recreate <name>` | `RecreateWorkspace` |
| `wtd save <name>` | `SaveWorkspace` |
| `wtd list workspaces` | `ListWorkspaces` |
| `wtd list instances` | `ListInstances` |
| `wtd list panes <workspace>` | `ListPanes` |
| `wtd list sessions <workspace>` | `ListSessions` |
| `wtd send <target> <text>` | `Send` |
| `wtd prompt <target> <text>` | `Prompt` |
| `wtd keys <target> <key>...` | `Keys` |
| `wtd capture <target>` | `Capture` |
| `wtd scrollback <target> --tail <n>` | `Scrollback` |
| `wtd follow <target>` | `Follow` |
| `wtd inspect <target>` | `Inspect` |
| `wtd wait <target> --for <condition>` | `WaitPane` |
| `wtd configure-pane <target>` | `ConfigurePane` |
| `wtd action <target> <action>` | `InvokeAction` |
| `wtd notify <target> [--state <state>] [--source <source>] [message]` | `Notify` |
| `wtd clear-attention <target>` | `ClearAttention` |
| `wtd status <target> ...` | `SetPaneStatus` |

## Current Messages

Client-to-host:

| Type | Required payload fields | Notes |
|------|-------------------------|-------|
| `Handshake` | `clientType`, `clientVersion`, `protocolVersion` | First message |
| `OpenWorkspace` | optional `name`, optional `file`, `recreate`, optional `profile` | Starts or reuses an instance |
| `AttachWorkspace` | `workspace` | UI attach snapshot |
| `CloseWorkspace` | `workspace`, `kill` | `kill` destroys sessions |
| `RecreateWorkspace` | `workspace` | Recreate from definition |
| `SaveWorkspace` | `workspace`, optional `file` | Persist definition |
| `ListWorkspaces` | none | Lists definitions |
| `ListInstances` | none | Lists running instances |
| `ListPanes` | `workspace` | Pane summaries |
| `ListSessions` | `workspace` | Session summaries |
| `Send` | `target`, `text`, `newline` | Low-level text input |
| `Prompt` | `target`, `text` | Driver-aware prompt input |
| `Keys` | `target`, `keys` | Semantic key specs |
| `Mouse` | `target`, `kind`, `col`, `row` | Semantic mouse input |
| `PaneInput` | `target`, `data` | Base64 raw bytes |
| `Capture` | `target` plus optional capture flags | Visible, scrollback, anchors, VT; results include `processHealth` for managed panes |
| `Scrollback` | `target`, `tail` | Tail scrollback |
| `Follow` | `target`, `raw` | Streaming output |
| `CancelFollow` | `id` | Cancels a follow request |
| `Inspect` | `target` | Full pane/session metadata |
| `WaitPane` | `target`, `condition`, optional `timeoutMs`, optional `pollMs`, optional `recentLines` | Waitable pane coordination; snapshots include attention, metadata, recent output, and managed process health |
| `ConfigurePane` | `target` plus optional driver fields | Prompt-driver metadata |
| `Notify` | `target`, `state`, optional `message`, optional `source` | Set pane attention/status |
| `ClearAttention` | `target` | Reset pane attention to `active` |
| `SetPaneStatus` | `target` plus optional metadata fields | Publish pane phase/status/progress metadata |
| `InvokeAction` | `action`, optional `targetPaneId`, `args` | Split/focus/resize/restart/clear/etc.; `restart-session` restarts a managed pane session, `clear-buffer` clears visible text plus scrollback, and `clear-scrollback` preserves visible text |
| `SessionInput` | `workspace`, `sessionId`, `data` | UI raw input |
| `PaneResize` | `paneId`, `cols`, `rows` | UI resize |
| `FocusPane` | `paneId` | UI focus |
| `RenamePane` | `paneId`, `newName` | Rename pane |

Host-to-client:

| Type | Purpose |
|------|---------|
| `HandshakeAck` | Protocol accepted |
| `Ok` | Generic success |
| `Error` | Structured failure |
| `OpenWorkspaceResult` | Instance id and state snapshot |
| `AttachWorkspaceResult` | Full attach snapshot |
| `RecreateWorkspaceResult` | Instance id and state snapshot |
| `ListWorkspacesResult` | Workspace definition summaries |
| `ListInstancesResult` | Running instance summaries |
| `ListPanesResult` | Pane summaries |
| `ListSessionsResult` | Session summaries |
| `CaptureResult` | Captured text/VT metadata plus optional `processHealth` |
| `ScrollbackResult` | Scrollback lines |
| `InspectResult` | Full metadata JSON |
| `WaitPaneResult` | Matched condition or timeout snapshot, including optional `processHealth` |
| `InvokeActionResult` | Action outcome |
| `FollowData` | Follow stream chunk |
| `FollowEnd` | Follow stream end |
| `SessionOutput` | UI VT output bytes |
| `SessionStateChanged` | UI session state event |
| `TitleChanged` | UI title event |
| `ProgressChanged` | UI OSC progress event |
| `AttentionChanged` | UI attention/status event |
| `LayoutChanged` | UI layout event |
| `WorkspaceStateChanged` | UI workspace state event |

## Representative Flows

### Prompt, Capture, Inspect

```json
{
  "id": "prompt-1",
  "type": "Prompt",
  "payload": {
    "target": "dev/server",
    "text": "Run the focused tests and summarize failures."
  }
}
```

### Agent Recovery

Managed panes expose the same `processHealth` object in `InspectResult`,
`CaptureResult`, and `WaitPaneResult.data`. Agents can inspect the state before
prompting, include health context while polling with `wait`, and restart a
failed managed pane through the action surface:

```json
{
  "id": "restart-1",
  "type": "InvokeAction",
  "payload": {
    "action": "restart-session",
    "targetPaneId": "dev/server",
    "args": {}
  }
}
```

```json
{
  "id": "capture-1",
  "type": "Capture",
  "payload": {
    "target": "dev/server",
    "lines": 80,
    "maxLines": 120
  }
}
```

```json
{
  "id": "inspect-1",
  "type": "Inspect",
  "payload": {
    "target": "dev/server"
  }
}
```

### Split

The CLI command `wtd action dev/server split-right` sends:

```json
{
  "id": "action-1",
  "type": "InvokeAction",
  "payload": {
    "action": "split-right",
    "targetPaneId": "dev/server",
    "args": {}
  }
}
```

Successful pane creation returns:

```json
{
  "id": "action-1",
  "type": "InvokeActionResult",
  "payload": {
    "result": "pane-created",
    "paneId": "2"
  }
}
```

### Errors

```json
{
  "id": "capture-1",
  "type": "Error",
  "payload": {
    "code": "target-not-found",
    "message": "No pane named 'server' in workspace 'dev'",
    "candidates": ["dev/backend/api", "dev/tests/runner"]
  }
}
```

### Attention And Completion

Hosted agents can publish pane attention state directly:

```json
{
  "id": "notify-1",
  "type": "Notify",
  "payload": {
    "target": "dev/server",
    "state": "needs_attention",
    "message": "input requested",
    "source": "pi"
  }
}
```

Accepted states are `active`, `needs_attention`, `done`, and `error`.
`ClearAttention` is equivalent to setting `active` with no message. The host
also ingests terminal notifications from OSC 9 and OSC 777 `notify` sequences as
`needs_attention` with source `osc`.

UI clients apply a focus-aware policy for external notification surfaces:
`needs_attention` targeting the visible focused pane is downgraded to focused
status instead of unread attention. `error` is not suppressed.

Structured pane metadata is published with `SetPaneStatus`:

```json
{
  "id": "status-1",
  "type": "SetPaneStatus",
  "payload": {
    "target": "dev/server",
    "phase": "working",
    "statusText": "running tests",
    "queuePending": 1,
    "source": "codex"
  }
}
```

`InspectResult.data.metadata` includes published fields plus host-derived
runtime fields where available, including `driverProfile`, `cwd`, and terminal
`progress`.

### CLI Timeout

Request timeout is enforced by the CLI client, not by the host protocol. The
global flag is `--timeout <seconds>`. A timeout exits with code `10` and prints
the request timeout error.

### Waitable Agent Coordination

`wait-v1` adds host-level pane waits for coordinator workflows. The CLI command
is:

```bash
wtd wait dev/server --for done --timeout 60 --recent-lines 80
```

Accepted conditions are `idle`, `done`, `needs-attention`, `error`,
`queue-empty`, and `state-change`. `--timeout` is in seconds, `--poll-ms`
controls host polling, and `--recent-lines` controls how much recent output is
included in the returned snapshot.

On success the command exits `0` and returns the matched condition plus current
metadata. On timeout it exits `10` and still returns a state-rich snapshot:

```json
{
  "matched": false,
  "condition": "done",
  "target": "dev/server",
  "data": {
    "paneName": "server",
    "paneId": "1",
    "workspace": "dev",
    "attention": {
      "state": "active"
    },
    "metadata": {
      "phase": "working",
      "statusText": "running tests",
      "queuePending": 1,
      "source": "pi"
    },
    "recentOutput": ["running test crate::integration ..."],
    "stateChanged": false
  }
}
```

## Example Fixtures

`docs/protocol/examples/v1-current-envelopes.json` contains representative
currently implemented envelopes. The `wtd-ipc` test suite parses that fixture
with the real `parse_envelope` dispatcher so examples stay aligned with code.
