param(
    [switch]$IncludeBash
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$psShim = Join-Path $root "wtd-tmux.ps1"
$bashShim = Join-Path $root "wtd-tmux.sh"
$bashExe = "C:\Program Files\Git\bin\bash.exe"

$cases = @(
    @{
        Name = "split horizontal"
        Args = @("split-window", "-h", "-t", "agents/main/worker")
        Expected = "wtd action agents/main/worker split-right"
    },
    @{
        Name = "split vertical"
        Args = @("split-window", "-v", "-t", "agents/main/worker")
        Expected = "wtd action agents/main/worker split-down"
    },
    @{
        Name = "focus"
        Args = @("select-pane", "-t", "agents/main/reviewer")
        Expected = "wtd focus agents/main/reviewer"
    },
    @{
        Name = "send prompt"
        Args = @("send-keys", "-t", "agents/main/worker", "run tests", "C-m")
        Expected = "wtd prompt agents/main/worker run tests"
    },
    @{
        Name = "list panes"
        Args = @("list-panes", "-t", "agents")
        Expected = "wtd list panes agents"
    },
    @{
        Name = "capture tail"
        Args = @("capture-pane", "-p", "-t", "agents/main/worker", "-S", "-80")
        Expected = "wtd capture agents/main/worker --lines 80"
    }
)

function Assert-Equal {
    param(
        [string]$Name,
        [string]$Expected,
        [string]$Actual
    )
    if ($Actual.Trim() -ne $Expected) {
        throw "$Name failed. Expected '$Expected', got '$Actual'"
    }
}

foreach ($case in $cases) {
    $output = & powershell -NoProfile -ExecutionPolicy Bypass -File $psShim -WhatIf @($case.Args)
    Assert-Equal -Name "PowerShell $($case.Name)" -Expected $case.Expected -Actual ($output -join "`n")
}

$failed = $false
try {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $psShim -WhatIf kill-session *> $null
} catch {
    $failed = $true
}
if (-not $failed) {
    throw "PowerShell unsupported command should fail"
}

if ($IncludeBash) {
    if (-not (Test-Path $bashExe)) {
        throw "Git Bash not found at $bashExe"
    }
    foreach ($case in $cases) {
        $output = & $bashExe $bashShim --what-if @($case.Args)
        $actual = ($output -join "`n").Replace("\ ", " ")
        Assert-Equal -Name "Bash $($case.Name)" -Expected $case.Expected -Actual $actual
    }
    $bashFailed = $false
    try {
        & $bashExe $bashShim --what-if kill-session *> $null
    } catch {
        $bashFailed = $true
    }
    if (-not $bashFailed) {
        throw "Bash unsupported command should fail"
    }
}

Write-Output "wtd-tmux shim tests passed"
