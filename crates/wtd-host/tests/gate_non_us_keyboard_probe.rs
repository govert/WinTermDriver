#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;

#[test]
fn probe_round_trips_altgr_and_composed_utf8_text() {
    let mut harness = ProbeHarness::open(&[]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));

    harness.send_input("@\r{\ré\r".as_bytes());

    assert!(
        harness.wait_for_text("input hex=40 0D", Duration::from_secs(5)),
        "expected @ round-trip; screen was:\n{}",
        harness.capture_text()
    );
    assert!(
        harness.wait_for_text("input hex=7B 0D", Duration::from_secs(5)),
        "expected {{ round-trip; screen was:\n{}",
        harness.capture_text()
    );
    assert!(
        harness.wait_for_text("input hex=C3 A9 0D", Duration::from_secs(5)),
        "expected é round-trip; screen was:\n{}",
        harness.capture_text()
    );
}
