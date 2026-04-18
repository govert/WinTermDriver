#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::thread;
use std::time::{Duration, Instant};

use probe_harness::ProbeHarness;
use wtd_core::workspace::{PaneDriverDefinition, PaneDriverProfile, SessionLaunchDefinition};
use wtd_host::prompt_driver::{build_prompt_input_plan, resolve_pane_driver};
use wtd_host::terminal_input::encode_key_spec;
use wtd_pty::screen::{KeyboardProtocolMode, ScreenBuffer};

#[test]
fn pi_prompt_plan_uses_shift_enter_and_bracketed_lines() {
    let driver = resolve_pane_driver(
        Some(&SessionLaunchDefinition {
            driver: Some(PaneDriverDefinition {
                profile: Some(PaneDriverProfile::Pi),
                submit_key: None,
                soft_break_key: None,
                disable_soft_break: false,
            }),
            ..Default::default()
        }),
        None,
    );

    let plan = build_prompt_input_plan("alpha\nbeta", &driver, true).unwrap();
    assert_eq!(driver.profile, "pi");
    assert_eq!(
        plan.body,
        b"\x1b[200~alpha\x1b[201~\x1b[13;2u\x1b[200~beta\x1b[201~"
    );
    assert_eq!(plan.submit, b"\r");
}

#[test]
fn pi_enter_variants_encode_as_expected() {
    assert_eq!(encode_key_spec("Shift+Enter").unwrap(), b"\x1b[13;2u");
    assert_eq!(encode_key_spec("Alt+Enter").unwrap(), b"\x1b[13;3u");
    assert_eq!(encode_key_spec("Ctrl+Enter").unwrap(), b"\x1b[13;5u");
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn pi_probe_requests_expected_terminal_modes() {
    let mut harness = ProbeHarness::open(&[
        "--keyboard-mode",
        "csi-u",
        "--enable-bracketed-paste",
        "--alt-screen",
    ]);

    assert!(harness.wait_for_text("[wtd-probe] ready", Duration::from_secs(5)));
    assert_eq!(harness.keyboard_protocol(), KeyboardProtocolMode::CsiU);
    assert!(harness.bracketed_paste());
    assert!(harness.on_alternate());
}

#[test]
fn pi_title_sequences_update_screen_state() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b]2;pi acceptance\x07");
    assert_eq!(screen.title, "pi acceptance");
}
