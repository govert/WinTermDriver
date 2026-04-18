#![cfg(windows)]

#[path = "support/probe_harness.rs"]
mod probe_harness;

use std::thread;
use std::time::{Duration, Instant};

use probe_harness::ProbeHarness;
use wtd_pty::{CursorShape, MouseMode, ScreenBuffer};

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
fn probe_drives_alt_screen_cursor_and_mouse_modes() {
    let mut harness = ProbeHarness::open(&[
        "--alt-screen",
        "--cursor-hidden",
        "--cursor-style",
        "5",
        "--mouse-mode",
    ]);

    let ok = wait_until(Duration::from_secs(5), || {
        for session in harness.instance.sessions_mut().values_mut() {
            session.process_pending_output();
        }
        let screen = harness.instance.sessions().values().next().unwrap().screen();
        screen.on_alternate()
            && !screen.cursor().visible
            && screen.cursor().shape == CursorShape::Bar
            && screen.mouse_mode() == MouseMode::ButtonEvent
            && screen.sgr_mouse()
    });
    let screen = harness.instance.sessions().values().next().unwrap().screen();
    assert!(
        ok,
        "probe should drive alternate-screen cursor/mouse modes; alt={} visible={} shape={:?} mouse={:?} sgr={} text={}",
        screen.on_alternate(),
        screen.cursor().visible,
        screen.cursor().shape,
        screen.mouse_mode(),
        screen.sgr_mouse(),
        screen.visible_text()
    );
    assert!(screen.on_alternate());
    assert!(!screen.cursor().visible);
    assert_eq!(screen.cursor().shape, CursorShape::Bar);
    assert_eq!(screen.mouse_mode(), MouseMode::ButtonEvent);
    assert!(screen.sgr_mouse());
}

#[test]
fn screen_restores_shell_state_after_tui_exit_sequences() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[?1049h\x1b[?25l\x1b[5 q\x1b[?1002h\x1b[?1006h");
    assert!(screen.on_alternate());
    assert!(!screen.cursor().visible);
    assert_eq!(screen.cursor().shape, CursorShape::Bar);
    assert_eq!(screen.mouse_mode(), MouseMode::ButtonEvent);
    assert!(screen.sgr_mouse());

    screen.advance(b"\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[2 q\x1b[?1049l");
    assert!(!screen.on_alternate());
    assert!(screen.cursor().visible);
    assert_eq!(screen.cursor().shape, CursorShape::Block);
    assert_eq!(screen.mouse_mode(), MouseMode::None);
    assert!(!screen.sgr_mouse());
}
