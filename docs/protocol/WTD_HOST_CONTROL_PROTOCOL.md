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
    "protocolVersion": 1
  }
}
```

`protocolVersion` is the current capability discovery boundary. Version `1`
means the message table below is available. If versions differ, the host returns
`Error { code: "protocol-error" }` and closes the connection.

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
| `wtd configure-pane <target>` | `ConfigurePane` |
| `wtd action <target> <action>` | `InvokeAction` |

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
| `Capture` | `target` plus optional capture flags | Visible, scrollback, anchors, VT |
| `Scrollback` | `target`, `tail` | Tail scrollback |
| `Follow` | `target`, `raw` | Streaming output |
| `CancelFollow` | `id` | Cancels a follow request |
| `Inspect` | `target` | Full pane/session metadata |
| `ConfigurePane` | `target` plus optional driver fields | Prompt-driver metadata |
| `InvokeAction` | `action`, optional `targetPaneId`, `args` | Split/focus/resize/etc. |
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
| `CaptureResult` | Captured text/VT metadata |
| `ScrollbackResult` | Scrollback lines |
| `InspectResult` | Full metadata JSON |
| `InvokeActionResult` | Action outcome |
| `FollowData` | Follow stream chunk |
| `FollowEnd` | Follow stream end |
| `SessionOutput` | UI VT output bytes |
| `SessionStateChanged` | UI session state event |
| `TitleChanged` | UI title event |
| `ProgressChanged` | UI OSC progress event |
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

### CLI Timeout

Request timeout is enforced by the CLI client, not by the host protocol. The
global flag is `--timeout <seconds>`. A timeout exits with code `10` and prints
the request timeout error. Future `wait-v1` will add host-level timeout
snapshots for pane coordination.

## Planned Agent Coordination Messages

The following messages are reserved for upcoming beads and are not accepted by
the current `wtd-ipc` dispatcher yet:

| Planned type | CLI shape | Purpose |
|--------------|-----------|---------|
| `Notify` | `wtd notify <target> ...` | Publish an attention notification |
| `SetPaneStatus` | `wtd status <target> ...` | Publish status text/progress/phase |
| `WaitPane` | `wtd wait <target> --for <condition>` | Wait for idle/done/error/attention/queue-empty |

The target response shape for `WaitPane` success and timeout is state-rich:

```json
{
  "matched": false,
  "condition": "done",
  "timedOut": true,
  "pane": {
    "target": "dev/server",
    "phase": "working",
    "attention": "active",
    "driverProfile": "pi",
    "statusText": "running tests",
    "queueSummary": {
      "pending": 1
    }
  },
  "recentOutput": {
    "lines": ["running test crate::integration ..."]
  }
}
```

## Example Fixtures

`docs/protocol/examples/v1-current-envelopes.json` contains representative
currently implemented envelopes. The `wtd-ipc` test suite parses that fixture
with the real `parse_envelope` dispatcher so examples stay aligned with code.
