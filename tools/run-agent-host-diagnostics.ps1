param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    if (-not $SkipBuild) {
        cargo build -p wtd-probe --bin wtd-probe
    }

    $commands = @(
        'cargo test -p wtd-host --test gate_probe_harness -- --nocapture',
        'cargo test -p wtd-host --test gate_keyboard_probe_acceptance -- --nocapture',
        'cargo test -p wtd-host --test gate_pi_acceptance -- --nocapture',
        'cargo test -p wtd-host --test gate_tui_fidelity -- --nocapture',
        'cargo test -p wtd-host --test gate_osc8_hyperlinks -- --nocapture',
        'cargo test -p wtd-host --test gate_inline_images -- --nocapture',
        'cargo test -p wtd-host --test gate_capability_negotiation -- --nocapture',
        'cargo test -p wtd-ui --test gate_non_us_keyboard -- --nocapture',
        'cargo test -p wtd-host --test gate_non_us_keyboard_probe -- --nocapture'
    )

    foreach ($command in $commands) {
        Write-Host "==> $command" -ForegroundColor Cyan
        Invoke-Expression $command
    }
}
finally {
    Pop-Location
}
