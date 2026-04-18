#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::time::Duration;

use probe_harness::ProbeHarness;

#[test]
fn launched_probe_session_advertises_hyperlink_and_image_capabilities() {
    let mut harness = ProbeHarness::open(&[]);
    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));

    let env = harness.session_env();
    assert_eq!(
        env.get("WTD_AGENT_HYPERLINKS").map(String::as_str),
        Some("osc8")
    );
    assert_eq!(
        env.get("WTD_AGENT_IMAGES").map(String::as_str),
        Some("kitty-placeholder")
    );
}
