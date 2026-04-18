# Agent-host compatibility diagnostics

WinTermDriver now exposes a probe-driven compatibility path for pi and similar agent TUIs.

## Supported host features

- split text-input and control-key input paths
- bracketed paste tracking per pane
- built-in agent driver profiles, including `pi`
- protocol-aware keyboard encoding:
  - legacy/xterm-compatible fallback
  - CSI-u negotiation
  - Kitty-compatible modified printable keys
  - Kitty-gated key release encoding
- OSC 8 hyperlink tracking
- minimal Kitty inline-image placeholder support
- alternate-screen, cursor-shape, cursor-visibility, and mouse-mode fidelity tests

## Capability discovery contract

WTD-launched agent panes expose:

- `WTD_AGENT_HOST=1`
- `WTD_AGENT_DRIVER`
- `WTD_AGENT_MULTILINE_MODE`
- `WTD_AGENT_PASTE_MODE`
- `WTD_AGENT_SUBMIT_KEY`
- `WTD_AGENT_SOFT_BREAK_KEY` when applicable
- `WTD_AGENT_HYPERLINKS=osc8`
- `WTD_AGENT_IMAGES=kitty-placeholder`

## Recommended diagnostics flow

1. Build the probe
   - `cargo build -p wtd-probe --bin wtd-probe`
2. Run probe-backed acceptance gates
   - `cargo test -p wtd-host --test gate_probe_harness -- --nocapture`
   - `cargo test -p wtd-host --test gate_keyboard_probe_acceptance -- --nocapture`
   - `cargo test -p wtd-host --test gate_pi_acceptance -- --nocapture`
3. Run terminal-capability fidelity checks
   - `cargo test -p wtd-host --test gate_tui_fidelity -- --nocapture`
   - `cargo test -p wtd-host --test gate_osc8_hyperlinks -- --nocapture`
   - `cargo test -p wtd-host --test gate_inline_images -- --nocapture`
4. Run layout/international keyboard regressions
   - `cargo test -p wtd-ui --test gate_non_us_keyboard -- --nocapture`
   - `cargo test -p wtd-host --test gate_non_us_keyboard_probe -- --nocapture`

## Failure triage hints

- **Shift+Enter / Alt+Enter / Ctrl+Enter wrong**
  - run `gate_keyboard_protocol_matrix`
  - run `gate_keyboard_probe_acceptance`
- **pi multiline prompt submits too early**
  - run `gate_pi_acceptance`
  - inspect `WTD_AGENT_*` env vars inside the pane
- **paste wraps incorrectly**
  - verify `bracketedPaste` state in capture metadata
  - rerun prompt-driver and clipboard gates
- **links are not clickable or tracked**
  - run `gate_osc8_hyperlinks`
  - verify `WTD_AGENT_HYPERLINKS=osc8`
- **images do not show**
  - current implementation is placeholder-oriented (`kitty-placeholder`)
  - run `gate_inline_images`
- **TUI visual state leaks back to shell**
  - run `gate_tui_fidelity`

## Probe commands

The probe accepts these flags:

- `--keyboard-mode csi-u|kitty`
- `--enable-bracketed-paste`
- `--disable-bracketed-paste`
- `--alt-screen`
- `--title TEXT`
- `--cursor-hidden`
- `--cursor-style N`
- `--mouse-mode`
- `--hyperlink URL TEXT`
- `--request-image-probe`

Use the probe through the WTD test harnesses unless you are actively debugging the ConPTY path.
