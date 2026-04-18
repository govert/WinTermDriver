#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;
use wtd_pty::screen::KeyboardProtocolMode;

#[test]
fn probe_harness_launches_probe_and_captures_readiness() {
    let mut harness = ProbeHarness::open(&["--enable-bracketed-paste", "--alt-screen"]);

    assert!(
        harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)),
        "probe should announce readiness"
    );

    let env = harness.session_env();
    assert_eq!(env.get("WTD_AGENT_HOST").map(String::as_str), Some("1"));
    assert_eq!(
        env.get("WTD_AGENT_DRIVER").map(String::as_str),
        Some("plain")
    );
}

#[test]
fn probe_harness_tracks_negotiated_keyboard_mode() {
    let mut harness = ProbeHarness::open(&["--keyboard-mode", "kitty"]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));
    assert_eq!(harness.keyboard_protocol(), KeyboardProtocolMode::Kitty);
}

#[test]
fn probe_harness_round_trips_exact_input_bytes() {
    let mut harness = ProbeHarness::open(&[]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));

    harness.send_input(b"A\r");

    assert!(
        harness.wait_for_text("input hex=41 0D", Duration::from_secs(5)),
        "probe should log exact input bytes; screen was:\n{}",
        harness.capture_text()
    );
}
