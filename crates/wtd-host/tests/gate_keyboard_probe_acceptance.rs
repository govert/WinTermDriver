#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;
use wtd_host::terminal_input::encode_key_spec_with_protocol;
use wtd_pty::screen::KeyboardProtocolMode;

fn send_and_expect(harness: &mut ProbeHarness, bytes: &[u8], expected_hex: &str) {
    harness.send_input(bytes);
    assert!(
        harness.wait_for_text(expected_hex, Duration::from_secs(5)),
        "expected transcript {expected_hex}; screen was:\n{}",
        harness.capture_text()
    );
}

#[test]
fn csi_u_probe_transcript_matches_pi_style_enter_and_arrow_expectations() {
    let mut harness = ProbeHarness::open(&["--keyboard-mode", "csi-u"]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));
    assert_eq!(harness.keyboard_protocol(), KeyboardProtocolMode::CsiU);

    let cases = [
        ("Enter", "input hex=0D"),
        ("Shift+Enter", "input hex=1B 5B 31 33 3B 32 75"),
        ("Alt+Enter", "input hex=1B 5B 31 33 3B 33 75"),
        ("Ctrl+Enter", "input hex=1B 5B 31 33 3B 35 75"),
        ("Shift+Up", "input hex=1B 5B 31 3B 32 41"),
        ("Alt+Left", "input hex=1B 1B 5B 31 3B 33 44"),
        ("Alt+X", "input hex=1B 78"),
    ];

    for (spec, expected_hex) in cases {
        let bytes = encode_key_spec_with_protocol(spec, KeyboardProtocolMode::CsiU).unwrap();
        send_and_expect(&mut harness, &bytes, expected_hex);
    }
}

#[test]
fn kitty_probe_transcript_matches_modified_printable_expectations() {
    let mut harness = ProbeHarness::open(&["--keyboard-mode", "kitty"]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));
    assert_eq!(harness.keyboard_protocol(), KeyboardProtocolMode::Kitty);

    let cases = [
        ("Shift+Enter", "input hex=1B 5B 31 33 3B 32 75"),
        ("Alt+X", "input hex=1B 5B 31 32 30 3B 33 75"),
        ("Ctrl+C", "input hex=1B 5B 39 39 3B 35 75"),
        ("Shift+A", "input hex=1B 5B 36 35 3B 32 75"),
    ];

    for (spec, expected_hex) in cases {
        let bytes = encode_key_spec_with_protocol(spec, KeyboardProtocolMode::Kitty).unwrap();
        send_and_expect(&mut harness, &bytes, expected_hex);
    }
}

#[test]
fn legacy_probe_transcript_preserves_fallback_behavior() {
    let mut harness = ProbeHarness::open(&[]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));
    assert_eq!(harness.keyboard_protocol(), KeyboardProtocolMode::Legacy);

    let cases = [
        ("Shift+Enter", "input hex=0D"),
        ("Shift+Up", "input hex=1B 5B 31 3B 32 41"),
        ("Ctrl+F5", "input hex=1B 5B 31 35 3B 35 7E"),
        ("Alt+X", "input hex=1B 78"),
    ];

    for (spec, expected_hex) in cases {
        let bytes = encode_key_spec_with_protocol(spec, KeyboardProtocolMode::Legacy).unwrap();
        send_and_expect(&mut harness, &bytes, expected_hex);
    }
}
