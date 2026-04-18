Set-Location C:\Work\WinTermDriver

$EPIC = br create --silent `
  --title "Make WinTermDriver a first-class host for pi and other agent TUIs" `
  --type epic `
  --priority 0 `
  --labels "agent-host,pi,terminal,tui" `
  --description @"
Outcome:
- WTD becomes a high-fidelity Windows host for pi and comparable agent CLIs.
- Text input is correct across keyboard layouts and IME flows.
- Modified-key delivery, multiline prompt handling, paste semantics, and terminal capabilities are modern and reliable.
- Automated verification proves compatibility for pi and other built-in agent profiles.
"@

$WS1 = br create --silent `
  --title "Rebuild WTD terminal input around proper text input + control key separation" `
  --type epic `
  --priority 0 `
  --parent $EPIC `
  --labels "agent-host,input,keyboard,pi" `
  --description @"
Outcome:
- Printable text is delivered via a proper text path.
- Control/navigation keys are delivered via a key path.
- AltGr, dead keys, IME, and non-US layouts behave correctly.
"@

$WS2 = br create --silent `
  --title "Make prompt, paste, and driver behavior agent-correct" `
  --type epic `
  --priority 0 `
  --parent $EPIC `
  --labels "agent-host,prompt,paste,drivers,pi" `
  --description @"
Outcome:
- UI paste respects per-pane bracketed paste state.
- pi is a first-class pane driver profile.
- Prompt-driver behavior is correct for pi and other supported agents.
"@

$WS3 = br create --silent `
  --title "Add modern keyboard protocol support beyond ad-hoc CSI-u" `
  --type epic `
  --priority 1 `
  --parent $EPIC `
  --labels "agent-host,keyboard,protocol,kitty" `
  --description @"
Outcome:
- WTD supports negotiated modern keyboard protocols.
- Modified-key coverage is expanded.
- Kitty keyboard protocol is supported.
- Deterministic compatibility probes verify behavior.
"@

$WS4 = br create --silent `
  --title "Complete modern terminal capabilities needed by pi and comparable TUIs" `
  --type epic `
  --priority 1 `
  --parent $EPIC `
  --labels "agent-host,terminal,osc8,images" `
  --description @"
Outcome:
- OSC 8 hyperlinks are supported.
- Inline image protocol support is added.
- Alternate-screen and cursor-state fidelity are verified against agent/TUI workloads.
"@

$WS5 = br create --silent `
  --title "Build deterministic agent-host verification harnesses and acceptance gates" `
  --type epic `
  --priority 0 `
  --parent $EPIC `
  --labels "agent-host,testing,acceptance,probe" `
  --description @"
Outcome:
- A reusable probe app and WTD↔probe harness exist.
- pi-focused and cross-agent acceptance suites become the source of truth for host behavior.
"@

$AH101 = br create --silent `
  --title "Introduce a two-lane input architecture: text events vs control-key events" `
  --type task `
  --priority 0 `
  --parent $WS1 `
  --labels "input,keyboard,architecture" `
  --description @"
Expected outcome:
- Refactor input plumbing so printable text and control keys are distinct internal event types.
- WM_CHAR / WM_SYSCHAR / committed text input feed the text lane.
- WM_KEYDOWN / WM_SYSKEYDOWN feed the control-key lane.

Completion evidence:
- wtd-ui no longer depends on ToUnicode-derived printable chars for ordinary text entry.
- Keybindings and raw terminal control bytes still work.
- Tests prove routing split.
"@

$AH102 = br create --silent `
  --title "Remove printable-character synthesis from WM_KEYDOWN path" `
  --type task `
  --priority 0 `
  --parent $WS1 `
  --labels "input,keyboard,regression" `
  --description @"
Expected outcome:
- Keydown no longer invents printable text via ToUnicode except where strictly required for non-text fallback behavior.
- Printable text comes from the text lane.
- Control lane handles arrows, Enter, Tab, Escape, function keys, and bindings.

Completion evidence:
- Existing raw-byte behavior for control keys is preserved.
- Printable text does not depend on KeyEvent.character in the normal path.
"@

$AH103 = br create --silent `
  --title "Implement correct AltGr handling for terminal text input" `
  --type task `
  --priority 0 `
  --parent $WS1 `
  --labels "input,keyboard,altgr,international" `
  --description @"
Expected outcome:
- AltGr-generated characters are delivered as plain text, not ESC-prefixed meta sequences.

Completion evidence:
- Representative AltGr characters such as @, {, }, [, ], \\, ~, and | are sent as UTF-8 text.
- Regression tests prove no accidental ESC prefixing.
"@

$AH104 = br create --silent `
  --title "Support dead-key and composed-character committed text input" `
  --type task `
  --priority 0 `
  --parent $WS1 `
  --labels "input,keyboard,dead-keys,composition" `
  --description @"
Expected outcome:
- Composed characters from Windows text input are delivered correctly to the PTY.
- No duplicated or partial characters appear.

Completion evidence:
- Message-level and probe-level tests cover composed text such as é, ü, and ñ.
"@

$AH105 = br create --silent `
  --title "Add IME-aware committed text handling for CJK input" `
  --type task `
  --priority 1 `
  --parent $WS1 `
  --labels "input,ime,cjk,international" `
  --description @"
Expected outcome:
- IME committed text is delivered correctly to terminal sessions.
- Text composition and key navigation do not interfere with each other.

Completion evidence:
- Deterministic tests and/or acceptance probes verify committed multibyte text behavior.
"@

$AH106 = br create --silent `
  --title "Reconcile binding resolution with the new text/control event model" `
  --type task `
  --priority 0 `
  --parent $WS1 `
  --labels "input,bindings,prefix-chords" `
  --description @"
Expected outcome:
- Bindings, prefix chords, and raw text entry all still work under the split architecture.

Completion evidence:
- Ctrl+B prefix mode still works.
- Single-stroke bindings still dispatch.
- Plain text reaches ConPTY.
- Text-entry keys are not spuriously consumed.
"@

br dep add $AH102 $AH101
br dep add $AH103 $AH101
br dep add $AH104 $AH101
br dep add $AH105 $AH101
br dep add $AH105 $AH104
br dep add $AH106 $AH101
br dep add $AH106 $AH102

$AH201 = br create --silent `
  --title "Fix UI paste to honor per-pane bracketed paste state" `
  --type task `
  --priority 0 `
  --parent $WS2 `
  --labels "paste,bracketed-paste,ui,pi" `
  --description @"
Expected outcome:
- UI paste uses the focused pane's actual screen.bracketed_paste() state instead of hardcoded false.

Completion evidence:
- Paste wraps with ESC[200~ ... ESC[201~ when DECSET 2004 is active.
- Plain paste remains plain when not active.
- UI action path and focused-pane path are covered by tests.
"@

$AH202 = br create --silent `
  --title "Add built-in pi pane driver profile" `
  --type task `
  --priority 0 `
  --parent $WS2 `
  --labels "drivers,pi,prompt" `
  --description @"
Expected outcome:
- WTD understands pi as a first-class prompt driver.

Proposed behavior:
- profile: pi
- submit key: Enter
- soft break: Shift+Enter
- multiline mode: soft-break
- paste mode: bracketed-if-enabled

Completion evidence:
- resolve_pane_driver supports pi.
- configure-pane --driver-profile pi works.
- Prompt-driver tests lock behavior in place.
"@

$AH203 = br create --silent `
  --title "Infer pi driver profile from program names and startup commands" `
  --type task `
  --priority 0 `
  --parent $WS2 `
  --labels "drivers,pi,inference" `
  --description @"
Expected outcome:
- Sessions launched as pi / pi.exe are auto-detected as the pi pane driver profile.

Completion evidence:
- Startup command and executable-path inference return PaneDriverProfile::Pi.
- Tests cover quoted and unquoted executable names.
"@

$AH204 = br create --silent `
  --title "Add agent-hosted multiline prompt acceptance tests for pi, Claude, Gemini, Copilot, and Codex" `
  --type task `
  --priority 0 `
  --parent $WS2 `
  --labels "prompt,drivers,acceptance,agents" `
  --description @"
Expected outcome:
- Prompt-driver behavior is verified across all built-in agent profiles.

Completion evidence:
- pi, claude-code, gemini-cli, copilot-cli, codex, and plain all have explicit multiline behavior tests.
- Submit and soft-break semantics are locked down by acceptance tests.
"@

$AH205 = br create --silent `
  --title "Add agent terminal identity mode for pi and other agent panes" `
  --type task `
  --priority 1 `
  --parent $WS2 `
  --labels "drivers,environment,capabilities,agents" `
  --description @"
Expected outcome:
- WTD exposes consistent capability-oriented environment identity for agent tools.

Completion evidence:
- WTD advertises agent-host capability variables such as WTD_AGENT_HOST and protocol/capability indicators.
- Documentation defines the contract.
"@

br dep add $AH203 $AH202
br dep add $AH204 $AH202
br dep add $AH204 $AH203
br dep add $AH205 $AH202

$AH301 = br create --silent `
  --title "Define WTD keyboard protocol negotiation model" `
  --type task `
  --priority 1 `
  --parent $WS3 `
  --labels "keyboard,protocol,architecture" `
  --description @"
Expected outcome:
- Host-side keyboard protocol negotiation becomes explicit and testable.

Scope:
- legacy/xterm-compatible mode
- current CSI-u coverage
- app-requested enhanced keyboard mode
- future Kitty keyboard support

Completion evidence:
- Protocol state is explicit and covered by negotiation tests.
"@

$AH302 = br create --silent `
  --title "Expand modified-key encoding coverage under negotiated keyboard modes" `
  --type task `
  --priority 1 `
  --parent $WS3 `
  --labels "keyboard,encoding,protocol" `
  --description @"
Expected outcome:
- More modified keys are encoded consistently, not just modified Enter.

Completion evidence:
- A key-encoding coverage matrix exists.
- Modern TUIs receive expected sequences for representative modified keys.
"@

$AH303 = br create --silent `
  --title "Implement Kitty keyboard protocol support" `
  --type task `
  --priority 1 `
  --parent $WS3 `
  --labels "keyboard,kitty,protocol,pi" `
  --description @"
Expected outcome:
- When an application negotiates Kitty keyboard protocol, WTD emits Kitty-compatible key events.

Completion evidence:
- Enable/disable negotiation is recognized.
- Negotiated key events emit Kitty-formatted sequences.
- Fallback to legacy behavior remains correct.
"@

$AH304 = br create --silent `
  --title "Support key release events where negotiated keyboard protocol requires them" `
  --type task `
  --priority 2 `
  --parent $WS3 `
  --labels "keyboard,kitty,key-release,protocol" `
  --description @"
Expected outcome:
- WTD can surface key release events to apps that request them.

Completion evidence:
- Key release generation is protocol-gated.
- Legacy mode behavior is unchanged.
"@

$AH305 = br create --silent `
  --title "Add keyboard-compat acceptance probe targeting pi-like TUI expectations" `
  --type task `
  --priority 1 `
  --parent $WS3 `
  --labels "keyboard,probe,acceptance,pi" `
  --description @"
Expected outcome:
- A deterministic probe verifies the exact key sequences a pi-like TUI would see.

Coverage:
- Enter / Shift+Enter / Alt+Enter / Ctrl+Enter
- arrows + modifiers
- Escape/meta cases
- Kitty mode on/off

Completion evidence:
- Probe transcript tests become a CI gate.
"@

br dep add $AH301 $AH101
br dep add $AH302 $AH301
br dep add $AH303 $AH301
br dep add $AH303 $AH302
br dep add $AH304 $AH303
br dep add $AH305 $AH302
br dep add $AH305 $AH303

$AH401 = br create --silent `
  --title "Implement OSC 8 hyperlink parsing, state tracking, and rendering" `
  --type task `
  --priority 1 `
  --parent $WS4 `
  --labels "terminal,osc8,hyperlinks,renderer" `
  --description @"
Expected outcome:
- WTD supports terminal hyperlinks end-to-end.

Completion evidence:
- Screen buffer parses OSC 8 open/close.
- Renderer preserves clickable link regions or equivalent actionable mapping.
- Copy behavior remains safe and plain-text.
"@

$AH402 = br create --silent `
  --title "Implement inline image protocol support for modern agent TUIs" `
  --type task `
  --priority 2 `
  --parent $WS4 `
  --labels "terminal,images,kitty,renderer" `
  --description @"
Expected outcome:
- WTD can display terminal inline images in a practical modern protocol, preferably Kitty image protocol first.

Completion evidence:
- Screen/render pipeline can receive, cache, lay out, and paint inline images.
- Unsupported modes degrade safely.
"@

$AH403 = br create --silent `
  --title "Expose host capability negotiation for hyperlinks and images" `
  --type task `
  --priority 2 `
  --parent $WS4 `
  --labels "terminal,capabilities,environment" `
  --description @"
Expected outcome:
- Applications can discover that WTD supports hyperlinks and images.

Completion evidence:
- Capability environment variables or negotiated identifiers are documented and tested.
"@

$AH404 = br create --silent `
  --title "Validate alternate-screen and cursor-state fidelity against pi-like TUI workloads" `
  --type task `
  --priority 1 `
  --parent $WS4 `
  --labels "terminal,alt-screen,cursor,acceptance,pi" `
  --description @"
Expected outcome:
- WTD proves robust behavior for alternate screen, cursor shape, cursor visibility, title changes, and mouse mode changes during agent/TUI workflows.

Completion evidence:
- Probe workloads transition cleanly between shell and TUI states.
- Snapshots and rendering tests prove fidelity.
"@

br dep add $AH403 $AH401
br dep add $AH403 $AH402

$AH501 = br create --silent `
  --title "Create a minimal agent-host probe fixture application for keyboard, paste, and terminal-capability testing" `
  --type task `
  --priority 0 `
  --parent $WS5 `
  --labels "testing,probe,fixtures,acceptance" `
  --description @"
Expected outcome:
- A deterministic test app exists that can request keyboard modes, enable/disable bracketed paste, switch alt screen, set OSC 8 links, request image display, and log exact input bytes received.

Completion evidence:
- Probe app is checked into the repo and self-tested.
- Later acceptance tests use the probe instead of ad-hoc inspection.
"@

$AH502 = br create --silent `
  --title "Add an automated WTD↔probe round-trip harness for ConPTY acceptance testing" `
  --type task `
  --priority 0 `
  --parent $WS5 `
  --labels "testing,probe,conpty,harness" `
  --description @"
Expected outcome:
- Integration tests can launch the probe in a real WTD pane and assert exact behavior.

Completion evidence:
- A reusable helper opens a workspace, attaches the probe, sends keys, and captures results.
"@

$AH503 = br create --silent `
  --title "Add a pi-focused acceptance suite driven by the probe and WTD driver profiles" `
  --type task `
  --priority 0 `
  --parent $WS5 `
  --labels "testing,acceptance,pi,drivers" `
  --description @"
Expected outcome:
- A dedicated acceptance suite captures the behaviors that make WTD a first-class pi host.

Assertions:
- Shift+Enter newline behavior
- Alt+Enter follow-up-compatible behavior
- multiline prompt plan for pi
- bracketed paste correctness
- alternate-screen fidelity
- title updates
- hyperlink rendering where supported
"@

$AH504 = br create --silent `
  --title "Add non-US keyboard layout regression suite" `
  --type task `
  --priority 1 `
  --parent $WS5 `
  --labels "testing,international,keyboard,regression" `
  --description @"
Expected outcome:
- WTD has durable regression coverage for international layouts.

Coverage:
- US International
- German-like AltGr cases
- one dead-key scenario

Completion evidence:
- Deterministic translation-layer tests plus probe-driven representative integration tests.
"@

$AH505 = br create --silent `
  --title "Add docs and operator diagnostics for agent-host compatibility" `
  --type task `
  --priority 1 `
  --parent $WS5 `
  --labels "docs,diagnostics,agent-host,pi" `
  --description @"
Expected outcome:
- Docs clearly state what WTD supports for pi and other agents, how capability detection works, how to run compatibility probes, and how to diagnose failures.

Completion evidence:
- Documentation and scripted diagnostic flows are checked and verified.
"@

br dep add $AH502 $AH501
br dep add $AH503 $AH202
br dep add $AH503 $AH502
br dep add $AH504 $AH103
br dep add $AH504 $AH502
br dep add $AH505 $AH202
br dep add $AH505 $AH303
br dep add $AH505 $AH401
br dep add $AH505 $AH402

br dep add $AH305 $AH502
br dep add $AH404 $AH502
br dep add $AH204 $AH502

Write-Host "Top epic: $EPIC"
Write-Host "WS1: $WS1"
Write-Host "WS2: $WS2"
Write-Host "WS3: $WS3"
Write-Host "WS4: $WS4"
Write-Host "WS5: $WS5"

br sync --flush-only
