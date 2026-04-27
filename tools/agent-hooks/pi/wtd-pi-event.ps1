param(
    [Parameter(Mandatory = $true)]
    [string]$Target,

    [Parameter(Mandatory = $true)]
    [ValidateSet(
        "agent_start",
        "agent_end",
        "turn_start",
        "turn_end",
        "tool_execution_start",
        "tool_execution_end",
        "queue_update",
        "input_requested",
        "approval_requested",
        "ui_request",
        "task_completed",
        "turn_error",
        "tool_execution_error",
        "agent_error",
        "idle"
    )]
    [string]$PiEvent,

    [string]$Message = "",
    [int]$QueuePending = -1,
    [string]$Completion = "",
    [switch]$WhatIf
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$genericHook = Join-Path (Split-Path -Parent $scriptDir) "wtd-agent-event.ps1"

function Invoke-GenericHook {
    param(
        [string]$Event,
        [string]$EventMessage = "",
        [int]$Pending = -1,
        [string]$CompletionText = ""
    )

    $hookParams = @{
        Target = $Target
        Agent = "pi"
        Event = $Event
    }
    if ($EventMessage) { $hookParams["Message"] = $EventMessage }
    if ($Pending -ge 0) { $hookParams["QueuePending"] = $Pending }
    if ($CompletionText) { $hookParams["Completion"] = $CompletionText }
    if ($WhatIf) { $hookParams["WhatIf"] = $true }
    & $genericHook @hookParams
}

switch ($PiEvent) {
    { $_ -in @("agent_start", "turn_start", "tool_execution_start") } {
        Invoke-GenericHook -Event "working" -EventMessage $(if ($Message) { $Message } else { $PiEvent })
    }
    "queue_update" {
        if ($QueuePending -gt 0) {
            Invoke-GenericHook -Event "queued" -EventMessage $Message -Pending $QueuePending
        } else {
            Invoke-GenericHook -Event "idle" -EventMessage $Message -Pending 0
        }
    }
    { $_ -in @("input_requested", "approval_requested", "ui_request") } {
        Invoke-GenericHook -Event "input-needed" -EventMessage $(if ($Message) { $Message } else { "input requested" })
    }
    { $_ -in @("agent_end", "turn_end", "tool_execution_end", "task_completed") } {
        $done = if ($Completion) { $Completion } else { $PiEvent }
        Invoke-GenericHook -Event "completed" -EventMessage $(if ($Message) { $Message } else { "completed" }) -CompletionText $done
    }
    { $_ -in @("turn_error", "tool_execution_error", "agent_error") } {
        Invoke-GenericHook -Event "error" -EventMessage $(if ($Message) { $Message } else { $PiEvent })
    }
    "idle" {
        Invoke-GenericHook -Event "idle" -EventMessage $Message -Pending $(if ($QueuePending -ge 0) { $QueuePending } else { 0 })
    }
}
