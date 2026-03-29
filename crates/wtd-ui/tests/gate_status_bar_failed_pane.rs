//! Gate integration test: Status bar and failed pane display (S24.5, S24.8).
//!
//! Verifies:
//! 1. Status bar shows workspace name, pane path, and session state
//! 2. Status bar renders all session status variants (running, exited, failed, etc.)
//! 3. Status bar renders prefix-active indicator
//! 4. Failed pane overlay shows error message with restart hint
//! 5. Exited pane overlay shows exit code with restart hint
//! 6. Full composited frame: tab strip + normal pane + failed pane + status bar

#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use wtd_core::layout::LayoutTree;
use wtd_pty::ScreenBuffer;
use wtd_ui::pane_layout::PaneLayout;
use wtd_ui::renderer::{
    exited_pane_message, failed_pane_message, RendererConfig, TerminalRenderer, RESTART_HINT,
};
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::TabStrip;

// ── Unique class names ──────────────────────────────────────────────────

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(9000);

// ── Test window helpers ─────────────────────────────────────────────────

unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn create_test_window(label: &str) -> HWND {
    let n = CLASS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let class_name_str = format!("WtdGateStatusBar_{label}_{n}\0");
    let class_name_wide: Vec<u16> = class_name_str.encode_utf16().collect();

    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let hinstance: HINSTANCE = instance.into();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name_wide.as_ptr()),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name_wide.as_ptr()),
            w!("Gate Status Bar Test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            800,
            600,
            None,
            None,
            Some(&hinstance),
            None,
        )
        .unwrap()
    }
}

fn destroy_test_window(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

// ── Test 1: Status bar shows workspace name, pane path, running state ───

#[test]
fn status_bar_shows_workspace_and_pane_info() {
    let hwnd = create_test_window("ws_pane_info");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut bar = StatusBar::new(&dw).unwrap();
    bar.set_workspace_name("dev-workspace".to_string());
    bar.set_pane_path("dev-workspace/main/server".to_string());
    bar.set_session_status(SessionStatus::Running);
    bar.layout(800.0);

    // Verify state was stored
    assert_eq!(bar.workspace_name(), "dev-workspace");
    assert_eq!(bar.pane_path(), "dev-workspace/main/server");
    assert_eq!(bar.session_status(), &SessionStatus::Running);
    assert!(!bar.is_prefix_active());

    // Paint the status bar
    renderer.begin_draw();
    renderer.clear_background();
    let y = 600.0 - bar.height();
    bar.paint(renderer.render_target(), y)
        .expect("status bar paint with workspace and pane info must succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 2: Status bar renders all session status variants ──────────────

#[test]
fn status_bar_renders_all_session_states() {
    let hwnd = create_test_window("all_states");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut bar = StatusBar::new(&dw).unwrap();
    bar.set_workspace_name("test".to_string());
    bar.layout(800.0);

    let statuses = vec![
        SessionStatus::Creating,
        SessionStatus::Running,
        SessionStatus::Exited { exit_code: 0 },
        SessionStatus::Exited { exit_code: 1 },
        SessionStatus::Failed {
            error: "spawn failed".to_string(),
        },
        SessionStatus::Restarting { attempt: 3 },
    ];

    for status in &statuses {
        bar.set_session_status(status.clone());

        // Verify label is non-empty
        let label = status.label();
        assert!(!label.is_empty(), "status label must not be empty for {status:?}");

        renderer.begin_draw();
        renderer.clear_background();
        bar.paint(renderer.render_target(), 600.0 - bar.height())
            .unwrap_or_else(|e| panic!("status bar paint must succeed for {status:?}: {e}"));
        renderer.end_draw().unwrap();
    }

    destroy_test_window(hwnd);
}

// ── Test 3: Status bar renders prefix-active indicator ──────────────────

#[test]
fn status_bar_renders_prefix_indicator() {
    let hwnd = create_test_window("prefix");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut bar = StatusBar::new(&dw).unwrap();
    bar.set_workspace_name("dev".to_string());
    bar.set_pane_path("dev/main/shell".to_string());
    bar.set_session_status(SessionStatus::Running);
    bar.layout(800.0);

    // Render without prefix
    bar.set_prefix_active(false);
    renderer.begin_draw();
    renderer.clear_background();
    bar.paint(renderer.render_target(), 600.0 - bar.height())
        .expect("status bar without prefix must succeed");
    renderer.end_draw().unwrap();

    // Render with prefix active
    bar.set_prefix_active(true);
    bar.set_prefix_label("Ctrl+B".to_string());
    assert!(bar.is_prefix_active());

    renderer.begin_draw();
    renderer.clear_background();
    bar.paint(renderer.render_target(), 600.0 - bar.height())
        .expect("status bar with prefix active must succeed");
    renderer.end_draw().unwrap();

    // Deactivate prefix and render again
    bar.set_prefix_active(false);
    assert!(!bar.is_prefix_active());

    renderer.begin_draw();
    renderer.clear_background();
    bar.paint(renderer.render_target(), 600.0 - bar.height())
        .expect("status bar after prefix deactivated must succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 4: Failed pane overlay shows error with restart hint ────────────

#[test]
fn failed_pane_shows_error_with_restart_prompt() {
    let hwnd = create_test_window("failed_pane");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();

    let error = "CreateProcess failed: executable not found";
    let message = failed_pane_message(error);
    assert!(
        message.contains("Session failed"),
        "message must contain 'Session failed'"
    );
    assert!(
        message.contains(error),
        "message must contain the error details"
    );

    // Verify restart hint constant is meaningful
    assert!(
        RESTART_HINT.contains("restart"),
        "restart hint must mention restart"
    );
    assert!(
        RESTART_HINT.contains("Ctrl+B"),
        "restart hint must mention the chord key"
    );

    // Paint the failed pane overlay
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&message, 0.0, 32.0, 800.0, 544.0)
        .expect("paint_failed_pane must render error message without error");
    renderer.end_draw().unwrap();

    // Paint with a short error message
    let short_msg = failed_pane_message("profile not found");
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&short_msg, 0.0, 32.0, 400.0, 300.0)
        .expect("paint_failed_pane must render short error in smaller viewport");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 5: Exited pane overlay shows exit code with restart hint ────────

#[test]
fn exited_pane_shows_exit_code_with_restart_prompt() {
    let hwnd = create_test_window("exited_pane");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();

    // Exit code 0 (normal exit)
    let msg0 = exited_pane_message(0);
    assert!(msg0.contains("exited"), "message must mention exit");
    assert!(msg0.contains("0"), "message must contain exit code 0");

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&msg0, 0.0, 32.0, 800.0, 544.0)
        .expect("paint_failed_pane for exit code 0 must succeed");
    renderer.end_draw().unwrap();

    // Exit code 1 (error exit)
    let msg1 = exited_pane_message(1);
    assert!(msg1.contains("1"), "message must contain exit code 1");

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&msg1, 100.0, 50.0, 600.0, 400.0)
        .expect("paint_failed_pane for exit code 1 must succeed");
    renderer.end_draw().unwrap();

    // Exit code 255
    let msg255 = exited_pane_message(255);
    assert!(msg255.contains("255"), "message must contain exit code 255");

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&msg255, 0.0, 32.0, 800.0, 544.0)
        .expect("paint_failed_pane for exit code 255 must succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 6: Full composited frame — tabs + live pane + failed pane + status bar

#[test]
fn full_composited_frame_with_failed_pane_and_status_bar() {
    let hwnd = create_test_window("full_composite");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();
    let (cell_w, cell_h) = renderer.cell_size();

    // ── Tab strip ────────────────────────────────────────────────────
    let mut tab_strip = TabStrip::new(&dw).unwrap();
    tab_strip.add_tab("dev".to_string());
    tab_strip.layout(800.0);

    // ── Layout: two panes (left = live, right = failed) ─────────────
    let mut tree = LayoutTree::new();
    let pane1 = tree.focus();
    let pane2 = tree.split_right(pane1.clone()).unwrap();
    assert_eq!(tree.pane_count(), 2);

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    let tab_h = tab_strip.height();
    let status_h = 24.0; // STATUS_BAR_HEIGHT
    let content_rows = ((600.0 - tab_h - status_h) / cell_h) as u16;
    let content_cols = (800.0 / cell_w) as u16;
    pane_layout.update(&tree, 0.0, tab_h, content_cols, content_rows);

    let pr1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let pr2 = pane_layout.pane_pixel_rect(&pane2).unwrap();

    // ── Screen buffer for live pane ─────────────────────────────────
    let mut screen = ScreenBuffer::new(40, content_rows, 100);
    screen.advance(b"\x1b[32mserver running on :8080\x1b[0m\r\n$ ");

    // ── Status bar ──────────────────────────────────────────────────
    let mut status_bar = StatusBar::new(&dw).unwrap();
    status_bar.set_workspace_name("dev".to_string());
    status_bar.set_pane_path("dev/main/server".to_string());
    status_bar.set_session_status(SessionStatus::Running);
    status_bar.layout(800.0);

    // ── Composited render: live pane + failed pane overlay ──────────
    let failed_msg = failed_pane_message("executable not found");

    renderer.begin_draw();
    renderer.clear_background();

    // Tab strip
    tab_strip
        .paint(renderer.render_target())
        .expect("tab strip in composited frame");

    // Live pane (pane1)
    renderer
        .paint_pane_viewport(&screen, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .expect("live pane viewport");

    // Failed pane overlay (pane2)
    renderer
        .paint_failed_pane(&failed_msg, pr2.x, pr2.y, pr2.width, pr2.height)
        .expect("failed pane overlay");

    // Pane borders and focus indicator
    pane_layout
        .paint(renderer.render_target(), &pane1)
        .expect("pane layout borders");

    // Status bar at bottom
    status_bar
        .paint(renderer.render_target(), 600.0 - status_bar.height())
        .expect("status bar in composited frame");

    renderer.end_draw().unwrap();

    // ── Now switch focus to the failed pane and update status bar ────
    tree.set_focus(pane2.clone()).unwrap();
    status_bar.set_pane_path("dev/main/build".to_string());
    status_bar.set_session_status(SessionStatus::Failed {
        error: "executable not found".to_string(),
    });

    renderer.begin_draw();
    renderer.clear_background();

    tab_strip.paint(renderer.render_target()).unwrap();
    renderer
        .paint_pane_viewport(&screen, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .unwrap();
    renderer
        .paint_failed_pane(&failed_msg, pr2.x, pr2.y, pr2.width, pr2.height)
        .unwrap();
    pane_layout
        .paint(renderer.render_target(), &pane2)
        .expect("pane layout with focus on failed pane");
    status_bar
        .paint(renderer.render_target(), 600.0 - status_bar.height())
        .expect("status bar reflecting failed session");

    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 7: Exited pane in composited frame with status bar update ───────

#[test]
fn composited_frame_with_exited_pane_and_status_bar() {
    let hwnd = create_test_window("exited_composite");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();
    let (cell_w, cell_h) = renderer.cell_size();

    // Single pane layout
    let tree = LayoutTree::new();
    let pane = tree.focus();

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    let tab_h = 32.0;
    let status_h = 24.0;
    let content_rows = ((600.0 - tab_h - status_h) / cell_h) as u16;
    let content_cols = (800.0 / cell_w) as u16;
    pane_layout.update(&tree, 0.0, tab_h, content_cols, content_rows);

    let pr = pane_layout.pane_pixel_rect(&pane).unwrap();

    // Status bar showing exited state
    let mut status_bar = StatusBar::new(&dw).unwrap();
    status_bar.set_workspace_name("build".to_string());
    status_bar.set_pane_path("build/main/compiler".to_string());
    status_bar.set_session_status(SessionStatus::Exited { exit_code: 1 });
    status_bar.layout(800.0);

    // Render exited pane with status bar
    let exited_msg = exited_pane_message(1);

    renderer.begin_draw();
    renderer.clear_background();

    renderer
        .paint_failed_pane(&exited_msg, pr.x, pr.y, pr.width, pr.height)
        .expect("exited pane overlay in composited frame");

    pane_layout
        .paint(renderer.render_target(), &pane)
        .expect("pane layout for exited pane");

    status_bar
        .paint(renderer.render_target(), 600.0 - status_bar.height())
        .expect("status bar showing exited state");

    renderer.end_draw().unwrap();

    // Verify status bar reflects the exited state
    assert_eq!(
        status_bar.session_status(),
        &SessionStatus::Exited { exit_code: 1 }
    );
    assert_eq!(status_bar.workspace_name(), "build");
    assert_eq!(status_bar.pane_path(), "build/main/compiler");

    destroy_test_window(hwnd);
}

// ── Test 8: Status bar state transitions (running → exited → restarting) ─

#[test]
fn status_bar_state_transitions() {
    let hwnd = create_test_window("state_transitions");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut bar = StatusBar::new(&dw).unwrap();
    bar.set_workspace_name("monitor".to_string());
    bar.set_pane_path("monitor/main/process".to_string());
    bar.layout(800.0);

    let transitions = vec![
        SessionStatus::Creating,
        SessionStatus::Running,
        SessionStatus::Exited { exit_code: 0 },
        SessionStatus::Restarting { attempt: 1 },
        SessionStatus::Running,
        SessionStatus::Failed {
            error: "connection refused".to_string(),
        },
        SessionStatus::Restarting { attempt: 2 },
        SessionStatus::Running,
    ];

    for (i, status) in transitions.iter().enumerate() {
        bar.set_session_status(status.clone());
        assert_eq!(
            bar.session_status(),
            status,
            "status must match after transition {i}"
        );

        renderer.begin_draw();
        renderer.clear_background();
        bar.paint(renderer.render_target(), 600.0 - bar.height())
            .unwrap_or_else(|e| panic!("paint must succeed at transition {i}: {e}"));
        renderer.end_draw().unwrap();
    }

    destroy_test_window(hwnd);
}
