#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;

#[test]
fn probe_publishes_osc8_hyperlink_targets_into_screen_state() {
    let mut harness = ProbeHarness::open(&["--hyperlink", "https://pi.ai", "pi"]);

    assert!(harness.wait_for_text("pi", Duration::from_secs(5)));

    let screen = harness
        .instance
        .sessions()
        .values()
        .next()
        .unwrap()
        .screen();
    assert_eq!(screen.hyperlink_at(0, 0), Some("https://pi.ai"));
    assert_eq!(screen.hyperlink_at(0, 1), Some("https://pi.ai"));
    assert_eq!(screen.hyperlink_at(0, 2), None);
}
