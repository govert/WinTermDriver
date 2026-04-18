#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;

#[test]
fn probe_round_trips_altgr_and_composed_utf8_text() {
    let mut harness = ProbeHarness::open(&[]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));

    harness.send_input("@".as_bytes());
    assert!(
        harness.wait_for_text("input hex=40 text=@", Duration::from_secs(5)),
        "expected @ round-trip; screen was:\n{}",
        harness.capture_text()
    );

    harness.send_input("{".as_bytes());
    assert!(
        harness.wait_for_text("input hex=7B text={", Duration::from_secs(5)),
        "expected {{ round-trip; screen was:\n{}",
        harness.capture_text()
    );

    harness.send_input("é".as_bytes());
    assert!(
        harness.wait_for_text("input hex=C3 A9 text=\\xc3\\xa9", Duration::from_secs(5)),
        "expected é round-trip; screen was:\n{}",
        harness.capture_text()
    );
}
