param(
    [Parameter(Mandatory = $true)]
    [string]$Target,

    [Parameter(Mandatory = $true)]
    [ValidateSet("pi", "codex", "claude-code", "gemini-cli", "copilot-cli")]
    [string]$Agent,

    [Parameter(Mandatory = $true)]
    [ValidateSet("working", "input-needed", "queued", "completed", "error", "idle")]
    [string]$Event,

    [string]$Message = "",
    [int]$QueuePending = -1,
    [string]$Completion = "",
    [switch]$WhatIf
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-Wtd {
    param([string[]]$CommandArgs)
    if ($WhatIf) {
        Write-Output ("wtd " + ($CommandArgs -join " "))
        return
    }
    & wtd @CommandArgs
}

switch ($Event) {
    "working" {
        $cmdArgs = @("status", $Target, "--phase", "working", "--source", $Agent)
        if ($Message) { $cmdArgs += $Message }
        Invoke-Wtd $cmdArgs
    }
    "input-needed" {
        $cmdArgs = @("notify", $Target, "--state", "needs-attention", "--source", $Agent)
        if ($Message) { $cmdArgs += $Message } else { $cmdArgs += "input requested" }
        Invoke-Wtd $cmdArgs
    }
    "queued" {
        $cmdArgs = @("status", $Target, "--phase", "queued", "--source", $Agent)
        if ($QueuePending -ge 0) { $cmdArgs += @("--queue-pending", "$QueuePending") }
        if ($Message) { $cmdArgs += $Message }
        Invoke-Wtd $cmdArgs
    }
    "completed" {
        $statusArgs = @("status", $Target, "--phase", "done", "--source", $Agent)
        if ($Completion) { $statusArgs += @("--completion", $Completion) }
        if ($Message) { $statusArgs += $Message }
        Invoke-Wtd $statusArgs

        $notifyArgs = @("notify", $Target, "--state", "done", "--source", $Agent)
        if ($Message) { $notifyArgs += $Message } else { $notifyArgs += "completed" }
        Invoke-Wtd $notifyArgs
    }
    "error" {
        $statusArgs = @("status", $Target, "--phase", "error", "--source", $Agent)
        if ($Message) { $statusArgs += $Message }
        Invoke-Wtd $statusArgs

        $notifyArgs = @("notify", $Target, "--state", "error", "--source", $Agent)
        if ($Message) { $notifyArgs += $Message } else { $notifyArgs += "error" }
        Invoke-Wtd $notifyArgs
    }
    "idle" {
        $cmdArgs = @("status", $Target, "--phase", "idle", "--source", $Agent)
        if ($QueuePending -ge 0) { $cmdArgs += @("--queue-pending", "$QueuePending") }
        if ($Message) { $cmdArgs += $Message }
        Invoke-Wtd $cmdArgs
        Invoke-Wtd @("clear-attention", $Target)
    }
}
