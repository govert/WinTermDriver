param(
    [string]$WorkspaceName = "wtd-crossterm-probe",
    [int]$Cols = 128,
    [int]$Rows = 48,
    [int]$TickMs = 100,
    [switch]$NoAlt,
    [switch]$NoMouse,
    [switch]$NoBuild,
    [switch]$NoUi,
    [switch]$AutoResize,
    [switch]$HoldOpen
)

$ErrorActionPreference = "Stop"

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptRoot
$workspaceFile = Join-Path $repoRoot ".wtd\$WorkspaceName.yaml"
$outputRoot = Join-Path $repoRoot ("logs\wtd-crossterm-probe\" + (Get-Date -Format "yyyyMMdd-HHmmss"))
$capturesRoot = Join-Path $outputRoot "captures"
$probeLogPath = Join-Path $outputRoot "probe.log"
$summaryPath = Join-Path $outputRoot "summary.txt"
$wtdExe = Join-Path $repoRoot "target\debug\wtd.exe"
$wtdUiExe = Join-Path $repoRoot "target\debug\wtd-ui.exe"
$script:CaptureSummaries = New-Object 'System.Collections.Generic.List[string]'

function Require-Path {
    param([string]$PathValue, [string]$Label)
    if (-not (Test-Path -LiteralPath $PathValue)) {
        throw "$Label not found at '$PathValue'."
    }
}

function Wait-ForMarker {
    param(
        [string]$Target,
        [string]$Marker,
        [int]$TimeoutMs = 15000,
        [int]$PollMs = 300
    )

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    while ($stopwatch.ElapsedMilliseconds -lt $TimeoutMs) {
        $text = & $wtdExe capture $Target 2>$null
        if ($LASTEXITCODE -eq 0 -and $text -match [regex]::Escape($Marker)) {
            return $text
        }

        Start-Sleep -Milliseconds $PollMs
    }

    throw "Timed out waiting for marker '$Marker' in target '$Target'."
}

function Save-Capture {
    param([string]$Label)

    $target = "probe"
    $capturePath = Join-Path $capturesRoot ($Label + ".visible.txt")
    $captureAllPath = Join-Path $capturesRoot ($Label + ".all.txt")
    $text = Wait-ForMarker -Target $target -Marker "WTD-CROSSTERM-PROBE"
    $textAll = & $wtdExe capture $target --all
    $firstLine = (($text | Out-String) -split "\r?\n")[0]
    $script:CaptureSummaries.Add("$Label :: $firstLine")
    $text | Set-Content -LiteralPath $capturePath -Encoding UTF8
    $textAll | Set-Content -LiteralPath $captureAllPath -Encoding UTF8
    return $capturePath
}

function Ensure-WindowApi {
    if ("WtdWindowApi" -as [type]) {
        return
    }

    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class WtdWindowApi
{
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint flags);

    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }
}
"@
}

function Resize-UiWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [int]$Width,
        [int]$Height
    )

    Ensure-WindowApi

    $handle = [IntPtr]::Zero
    for ($i = 0; $i -lt 50; $i++) {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne 0) {
            $handle = [IntPtr]$Process.MainWindowHandle
            break
        }

        Start-Sleep -Milliseconds 200
    }

    if ($handle -eq [IntPtr]::Zero) {
        throw "Could not resolve wtd-ui main window handle."
    }

    $rect = New-Object WtdWindowApi+RECT
    [void][WtdWindowApi]::GetWindowRect($handle, [ref]$rect)
    [void][WtdWindowApi]::SetWindowPos($handle, [IntPtr]::Zero, $rect.Left, $rect.Top, $Width, $Height, 0)
}

Require-Path $wtdExe "wtd.exe"
Require-Path $wtdUiExe "wtd-ui.exe"

if (-not $NoBuild) {
    cargo build -p wtd-ui --example crossterm_probe --release
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $workspaceFile) | Out-Null
New-Item -ItemType Directory -Force -Path $capturesRoot | Out-Null

$yamlLines = @(
    "version: 1",
    "name: $WorkspaceName",
    "defaults:",
    "  terminalSize:",
    "    cols: $Cols",
    "    rows: $Rows",
    "profiles:",
    "  probe:",
    "    type: custom",
    "    executable: 'cargo'",
    "    cwd: '$repoRoot'",
    "    args:",
    "      - run",
    "      - -p",
    "      - wtd-ui",
    "      - --example",
    "      - crossterm_probe",
    "      - --release",
    "      - --",
    "      - --tick-ms",
    "      - '$TickMs'",
    "      - --log",
    "      - '$probeLogPath'"
)

if ($NoAlt) {
    $yamlLines += "      - --no-alt"
}
if ($NoMouse) {
    $yamlLines += "      - --no-mouse"
}

$yamlLines += @(
    "tabs:",
    "  - name: main",
    "    layout:",
    "      type: pane",
    "      name: probe",
    "      session:",
    "        profile: probe"
)

Set-Content -LiteralPath $workspaceFile -Value $yamlLines -Encoding ASCII

$uiProcess = $null

try {
    & $wtdExe open $WorkspaceName --file $workspaceFile --recreate

    Save-Capture -Label "initial" | Out-Null

    if (-not $NoUi) {
        $uiProcess = Start-Process -FilePath $wtdUiExe -ArgumentList @("--workspace", $WorkspaceName) -PassThru
        Start-Sleep -Seconds 2
        Save-Capture -Label "after-ui-launch" | Out-Null

        if ($AutoResize) {
            $steps = @(
                @{ Label = "resize-1200x800"; Width = 1200; Height = 800 },
                @{ Label = "resize-1500x950"; Width = 1500; Height = 950 },
                @{ Label = "resize-980x720"; Width = 980; Height = 720 }
            )

            foreach ($step in $steps) {
                Resize-UiWindow -Process $uiProcess -Width $step.Width -Height $step.Height
                Start-Sleep -Seconds 2
                Save-Capture -Label $step.Label | Out-Null
            }
        }
    }

    $finalCapture = Save-Capture -Label "final"
    Set-Content -LiteralPath $summaryPath -Value $script:CaptureSummaries -Encoding UTF8
    Write-Host "Workspace file: $workspaceFile"
    Write-Host "Captures:       $capturesRoot"
    Write-Host "Summary:        $summaryPath"
    Write-Host "Probe log:      $probeLogPath"
    Write-Host "Final capture:  $finalCapture"

    if ($HoldOpen) {
        Write-Host "Workspace left running. Press Enter to close."
        [void][Console]::ReadLine()
    }
}
finally {
    try {
        & $wtdExe close $WorkspaceName --kill | Out-Null
    }
    catch {
    }
}
