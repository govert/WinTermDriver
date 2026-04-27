param(
    [switch]$WhatIf,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Args
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Show-Usage {
    @"
Usage:
  wtd-tmux.ps1 [-WhatIf] <tmux-command> [args]

Supported:
  split-window [-h|-v] -t <target>
  select-pane -t <target>
  send-keys -t <target> <text...> [C-m]
  list-panes -t <workspace>
  capture-pane -p -t <target> [-S -<lines>]
"@
}

function Invoke-Wtd {
    param([string[]]$CommandArgs)
    if ($WhatIf) {
        Write-Output ("wtd " + ($CommandArgs -join " "))
    } else {
        & wtd @CommandArgs
    }
}

function Require-Target {
    param([string]$Target)
    if (-not $Target) {
        throw "wtd-tmux: command requires -t <target>"
    }
}

if ($Args.Count -eq 0) {
    Show-Usage
    exit 2
}

$cmd = $Args[0]
$rest = @()
if ($Args.Count -gt 1) { $rest = $Args[1..($Args.Count - 1)] }

switch ($cmd) {
    "split-window" {
        $orientation = "vertical"
        $target = ""
        for ($i = 0; $i -lt $rest.Count; $i++) {
            switch ($rest[$i]) {
                "-h" { $orientation = "horizontal" }
                "-v" { $orientation = "vertical" }
                "-t" { $i++; $target = $rest[$i] }
                default { throw "wtd-tmux: unsupported split-window argument: $($rest[$i])" }
            }
        }
        Require-Target $target
        if ($orientation -eq "horizontal") {
            Invoke-Wtd @("action", $target, "split-right")
        } else {
            Invoke-Wtd @("action", $target, "split-down")
        }
    }
    "select-pane" {
        $target = ""
        for ($i = 0; $i -lt $rest.Count; $i++) {
            switch ($rest[$i]) {
                "-t" { $i++; $target = $rest[$i] }
                default { throw "wtd-tmux: unsupported select-pane argument: $($rest[$i])" }
            }
        }
        Require-Target $target
        Invoke-Wtd @("focus", $target)
    }
    "send-keys" {
        $target = ""
        $keys = New-Object System.Collections.Generic.List[string]
        for ($i = 0; $i -lt $rest.Count; $i++) {
            if ($rest[$i] -eq "-t") {
                $i++; $target = $rest[$i]
            } else {
                $keys.Add($rest[$i])
            }
        }
        Require-Target $target
        $submit = $false
        if ($keys.Count -gt 0 -and $keys[$keys.Count - 1] -eq "C-m") {
            $submit = $true
            $keys.RemoveAt($keys.Count - 1)
        }
        $text = ($keys -join " ")
        if ($submit) {
            Invoke-Wtd @("prompt", $target, $text)
        } else {
            Invoke-Wtd @("send", $target, $text)
        }
    }
    "list-panes" {
        $target = ""
        for ($i = 0; $i -lt $rest.Count; $i++) {
            switch ($rest[$i]) {
                "-t" { $i++; $target = $rest[$i] }
                "-F" { $i++ }
                default { throw "wtd-tmux: unsupported list-panes argument: $($rest[$i])" }
            }
        }
        Require-Target $target
        Invoke-Wtd @("list", "panes", $target)
    }
    "capture-pane" {
        $target = ""
        $lines = ""
        for ($i = 0; $i -lt $rest.Count; $i++) {
            switch ($rest[$i]) {
                "-p" {}
                "-t" { $i++; $target = $rest[$i] }
                "-S" { $i++; $lines = $rest[$i].TrimStart("-") }
                default {
                    if (-not $target) {
                        $target = $rest[$i]
                    } else {
                        throw "wtd-tmux: unsupported capture-pane argument: $($rest[$i])"
                    }
                }
            }
        }
        Require-Target $target
        if ($lines) {
            Invoke-Wtd @("capture", $target, "--lines", $lines)
        } else {
            Invoke-Wtd @("capture", $target)
        }
    }
    { $_ -in @("help", "-h", "--help") } {
        Show-Usage
    }
    default {
        Write-Error "wtd-tmux: unsupported tmux command: $cmd"
        Show-Usage
        exit 2
    }
}
