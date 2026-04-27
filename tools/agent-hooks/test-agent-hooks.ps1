param(
    [switch]$IncludeBash
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$psHook = Join-Path $root "wtd-agent-event.ps1"
$bashHook = Join-Path $root "wtd-agent-event.sh"
$bashExe = "C:\Program Files\Git\bin\bash.exe"

$cases = @(
    @{
        Name = "codex working"
        PsArgs = @("-Target", "agents/codex", "-Agent", "codex", "-Event", "working", "-Message", "running")
        BashArgs = @("--target", "agents/codex", "--agent", "codex", "--event", "working", "--message", "running")
        Expected = @("wtd status agents/codex --phase working --source codex running")
    },
    @{
        Name = "pi queued"
        PsArgs = @("-Target", "agents/pi", "-Agent", "pi", "-Event", "queued", "-QueuePending", "2")
        BashArgs = @("--target", "agents/pi", "--agent", "pi", "--event", "queued", "--queue-pending", "2")
        Expected = @("wtd status agents/pi --phase queued --source pi --queue-pending 2")
    },
    @{
        Name = "claude input"
        PsArgs = @("-Target", "agents/claude", "-Agent", "claude-code", "-Event", "input-needed", "-Message", "approval")
        BashArgs = @("--target", "agents/claude", "--agent", "claude-code", "--event", "input-needed", "--message", "approval")
        Expected = @("wtd notify agents/claude --state needs-attention --source claude-code approval")
    },
    @{
        Name = "gemini completed"
        PsArgs = @("-Target", "agents/gemini", "-Agent", "gemini-cli", "-Event", "completed", "-Completion", "turn", "-Message", "done")
        BashArgs = @("--target", "agents/gemini", "--agent", "gemini-cli", "--event", "completed", "--completion", "turn", "--message", "done")
        Expected = @(
            "wtd status agents/gemini --phase done --source gemini-cli --completion turn done",
            "wtd notify agents/gemini --state done --source gemini-cli done"
        )
    },
    @{
        Name = "copilot error"
        PsArgs = @("-Target", "agents/copilot", "-Agent", "copilot-cli", "-Event", "error", "-Message", "failed")
        BashArgs = @("--target", "agents/copilot", "--agent", "copilot-cli", "--event", "error", "--message", "failed")
        Expected = @(
            "wtd status agents/copilot --phase error --source copilot-cli failed",
            "wtd notify agents/copilot --state error --source copilot-cli failed"
        )
    }
)

function Normalize-BashOutput {
    param([string[]]$Lines)
    $Lines | ForEach-Object { $_.Replace("\ ", " ") }
}

function Assert-Lines {
    param(
        [string]$Name,
        [string[]]$Expected,
        [string[]]$Actual
    )
    $actualLines = @($Actual | Where-Object { $_ -ne "" })
    if ($actualLines.Count -ne $Expected.Count) {
        throw "$Name failed. Expected $($Expected.Count) lines, got $($actualLines.Count): $($actualLines -join ' | ')"
    }
    for ($i = 0; $i -lt $Expected.Count; $i++) {
        if ($actualLines[$i].Trim() -ne $Expected[$i]) {
            throw "$Name failed at line $i. Expected '$($Expected[$i])', got '$($actualLines[$i])'"
        }
    }
}

foreach ($case in $cases) {
    $output = & powershell -NoProfile -ExecutionPolicy Bypass -File $psHook @($case.PsArgs) -WhatIf
    Assert-Lines -Name "PowerShell $($case.Name)" -Expected $case.Expected -Actual $output
}

if ($IncludeBash) {
    if (-not (Test-Path $bashExe)) {
        throw "Git Bash not found at $bashExe"
    }
    foreach ($case in $cases) {
        $output = & $bashExe $bashHook @($case.BashArgs) --what-if
        Assert-Lines -Name "Bash $($case.Name)" -Expected $case.Expected -Actual (Normalize-BashOutput $output)
    }
}

Write-Output "agent hook tests passed"
