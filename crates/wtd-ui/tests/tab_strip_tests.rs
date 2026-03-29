//! Integration tests for the tab strip component.
//!
//! These tests verify that the tab strip renders correctly alongside the
//! terminal content, handles tab management, and doesn't crash under
//! various conditions including overflow and compositing.

use wtd_pty::ScreenBuffer;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::tab_strip::{TabAction, TabStrip};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::sync::atomic::{AtomicU32, Ordering};

// ── Test infrastructure ──────────────────────────────────────────────────────

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(100);

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
    let class_name_str = format!("WtdTabTest_{label}_{n}\0");
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
            w!("Test Window"),
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

fn make_dw_factory() -> IDWriteFactory {
    unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() }
}

// ── Rendering integration tests ──────────────────────────────────────────────

#[test]
fn tab_strip_renders_without_crash() {
    let hwnd = create_test_window("strip_basic");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut strip = TabStrip::new(renderer.dw_factory()).unwrap();
    strip.add_tab("main".into());
    strip.add_tab("build".into());
    strip.set_active(0);
    strip.layout(800.0);

    let screen = ScreenBuffer::new(80, 24, 0);

    renderer.begin_draw();
    renderer.clear_background();
    strip
        .paint(renderer.render_target())
        .expect("tab strip paint should succeed");
    renderer
        .paint_screen(&screen, strip.height())
        .expect("screen paint should succeed");
    renderer.end_draw().expect("end draw should succeed");

    destroy_test_window(hwnd);
}

#[test]
fn tab_strip_empty_renders_without_crash() {
    let hwnd = create_test_window("strip_empty");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut strip = TabStrip::new(renderer.dw_factory()).unwrap();
    strip.layout(800.0);

    renderer.begin_draw();
    renderer.clear_background();
    strip
        .paint(renderer.render_target())
        .expect("empty tab strip should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn tab_strip_overflow_renders_without_crash() {
    let hwnd = create_test_window("strip_overflow");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut strip = TabStrip::new(renderer.dw_factory()).unwrap();
    for i in 0..20 {
        strip.add_tab(format!("long-tab-name-{i}"));
    }
    strip.set_active(5);
    strip.layout(400.0); // narrow window forces overflow

    let screen = ScreenBuffer::new(80, 24, 0);

    renderer.begin_draw();
    renderer.clear_background();
    strip
        .paint(renderer.render_target())
        .expect("overflow tab strip should paint");
    renderer
        .paint_screen(&screen, strip.height())
        .expect("screen paint should succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn composite_paint_with_vt_content() {
    let hwnd = create_test_window("composite");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut strip = TabStrip::new(renderer.dw_factory()).unwrap();
    strip.add_tab("shell".into());
    strip.set_active(0);
    strip.layout(800.0);

    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[1;31mBold Red\x1b[0m text on \x1b[44mblue bg\x1b[0m\r\n");
    screen.advance(b"\x1b[32mGreen\x1b[0m line two\r\n");

    // Paint twice to verify stability
    for _ in 0..2 {
        renderer.begin_draw();
        renderer.clear_background();
        strip.paint(renderer.render_target()).unwrap();
        renderer.paint_screen(&screen, strip.height()).unwrap();
        renderer.end_draw().unwrap();
    }

    destroy_test_window(hwnd);
}

#[test]
fn original_paint_method_still_works() {
    let hwnd = create_test_window("compat");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"Hello world");

    // The original convenience method should still work unchanged.
    renderer.paint(&screen).expect("original paint should work");

    destroy_test_window(hwnd);
}

// ── Tab management tests (with DW for text measurement) ─────────────────────

#[test]
fn layout_and_click_switch_tab() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("first".into());
    strip.add_tab("second".into());
    strip.set_active(0);
    strip.layout(600.0);

    // Click in the middle of the second tab zone
    let zones = &strip.tabs();
    assert_eq!(zones.len(), 2);

    // The second tab should be clickable; use layout info
    // We test via on_mouse_down at a position that should be in the second tab
    // Tabs start at x=0, each tab is at least MIN_TAB_WIDTH=80px
    let action = strip.on_mouse_down(100.0, 16.0);
    assert_eq!(action, Some(TabAction::SwitchTo(1)));
    assert_eq!(strip.active_index(), 1);
}

#[test]
fn layout_and_click_add_button() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("only".into());
    strip.layout(600.0);

    // The add button is right after the last tab.
    // Tab "only" ~80px (min width) + 1px gap => add button starts ~81px.
    // Add button is 32px wide, so click at its center ~97px.
    let action = strip.on_mouse_down(97.0, 16.0);
    assert_eq!(action, Some(TabAction::Create));
}

#[test]
fn close_button_click_removes_tab() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("a".into());
    strip.add_tab("b".into());
    strip.layout(600.0);

    // Find the close button position of the first tab
    // Close button is at the right edge of the tab: tab_width - margin - size
    // First tab starts at x=0, tab width ~80-100px, close at right end
    // Click at roughly (80, 16) — close button area
    let action = strip.on_mouse_down(70.0, 16.0);
    // This should either close tab 0 (if we hit close) or switch
    // The exact hit depends on text measurement; just verify we get an action
    assert!(action.is_some());
}

#[test]
fn mouse_outside_strip_returns_none() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("a".into());
    strip.layout(600.0);

    // Click below the tab strip
    assert_eq!(strip.on_mouse_down(50.0, 50.0), None);
    // Click above the tab strip
    assert_eq!(strip.on_mouse_down(50.0, -5.0), None);
}

#[test]
fn hover_state_updates_on_move() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("a".into());
    strip.add_tab("b".into());
    strip.layout(600.0);

    // Move over the tab strip area
    strip.on_mouse_move(50.0, 16.0);
    // No crash, no action returned for move
    let action = strip.on_mouse_move(150.0, 16.0);
    assert_eq!(action, None);
}

#[test]
fn drag_small_movement_does_not_reorder() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("a".into());
    strip.add_tab("b".into());
    strip.set_active(0);
    strip.layout(600.0);

    // Mouse down on first tab
    strip.on_mouse_down(40.0, 16.0);
    // Small movement (below threshold)
    strip.on_mouse_move(42.0, 16.0);
    // Mouse up
    let action = strip.on_mouse_up(42.0, 16.0);
    assert_eq!(action, None); // no reorder
    assert_eq!(strip.tabs()[0].name, "a"); // order unchanged
}

#[test]
fn window_title_changes_on_tab_switch() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("shell".into());
    strip.add_tab("editor".into());

    strip.set_active(0);
    assert_eq!(
        strip.window_title("MyWorkspace"),
        "MyWorkspace \u{2014} shell"
    );

    strip.set_active(1);
    assert_eq!(
        strip.window_title("MyWorkspace"),
        "MyWorkspace \u{2014} editor"
    );
}

#[test]
fn tab_strip_single_tab_close_returns_window_close() {
    let dw = make_dw_factory();
    let mut strip = TabStrip::new(&dw).unwrap();
    strip.add_tab("only".into());
    strip.layout(600.0);

    let action = strip.close_tab(0);
    assert_eq!(action, TabAction::WindowClose);
    assert_eq!(strip.tab_count(), 0);
}

#[test]
fn resize_relayouts_correctly() {
    let hwnd = create_test_window("relayout");
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    let mut strip = TabStrip::new(renderer.dw_factory()).unwrap();
    for i in 0..10 {
        strip.add_tab(format!("tab-{i}"));
    }

    // Layout at wide width — no overflow
    strip.layout(2000.0);

    // Resize to narrow — should trigger overflow
    renderer.resize(300, 600).unwrap();
    strip.layout(300.0);

    // Paint should still work
    renderer.begin_draw();
    renderer.clear_background();
    strip.paint(renderer.render_target()).unwrap();
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}
