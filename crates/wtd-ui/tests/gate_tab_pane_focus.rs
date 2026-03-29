//! Gate integration test: Window shows tab strip and split panes with focus
//! indicators (§24.2–24.4).
//!
//! Verifies the complete UI compositing pipeline:
//! 1. Tab strip renders with multiple tabs
//! 2. Split panes layout and render in a window
//! 3. Tab switching updates active state
//! 4. Focus indicator visible on active pane
//! 5. Focus cycling moves indicator between panes

#![cfg(windows)]

use std::collections::HashMap;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::sync::atomic::{AtomicU32, Ordering};

use wtd_core::ids::PaneId;
use wtd_core::layout::{LayoutTree, Rect};
use wtd_pty::ScreenBuffer;
use wtd_ui::pane_layout::PaneLayout;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::tab_strip::{TabAction, TabStrip};

// ── Unique class names ──────────────────────────────────────────────────

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(7000);

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
    let class_name_str = format!("WtdGateTabPane_{label}_{n}\0");
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
            w!("Gate Tab Pane Test"),
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

// ── Test 1: Tab strip renders with tabs, switching works ────────────────

#[test]
fn tab_strip_renders_with_multiple_tabs_and_switching() {
    let hwnd = create_test_window("tab_strip");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut tab_strip = TabStrip::new(&dw).unwrap();
    let tab1_id = tab_strip.add_tab("dev".to_string());
    let tab2_id = tab_strip.add_tab("logs".to_string());

    assert_eq!(tab_strip.tab_count(), 2);
    assert_eq!(tab_strip.active_index(), 0);
    assert_eq!(tab_strip.active_tab().unwrap().name, "dev");

    // Layout and render with two tabs
    tab_strip.layout(800.0);
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip
        .paint(renderer.render_target())
        .expect("tab strip paint must succeed with two tabs");
    renderer.end_draw().unwrap();

    // Switch to second tab
    tab_strip.set_active(1);
    assert_eq!(tab_strip.active_index(), 1);
    assert_eq!(tab_strip.active_tab().unwrap().name, "logs");
    assert_eq!(tab_strip.active_tab().unwrap().id, tab2_id);

    // Render again after switch
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip
        .paint(renderer.render_target())
        .expect("tab strip paint must succeed after tab switch");
    renderer.end_draw().unwrap();

    // Switch back to first tab
    tab_strip.set_active(0);
    assert_eq!(tab_strip.active_index(), 0);
    assert_eq!(tab_strip.active_tab().unwrap().id, tab1_id);

    // Window title reflects workspace + tab
    let title = tab_strip.window_title("my-workspace");
    assert!(title.contains("my-workspace"), "title must contain workspace name");
    assert!(title.contains("dev"), "title must contain active tab name");

    destroy_test_window(hwnd);
}

// ── Test 2: Split panes render with layout and focus indicator ──────────

#[test]
fn split_panes_render_with_focus_indicator() {
    let hwnd = create_test_window("split_panes");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let (cell_w, cell_h) = renderer.cell_size();

    // Two panes via vertical split
    let mut tree = LayoutTree::new();
    let pane1 = tree.focus();
    let pane2 = tree.split_right(pane1.clone()).unwrap();

    assert_eq!(tree.pane_count(), 2);
    assert_eq!(tree.focus(), pane1, "pane1 should remain focused after split");

    // Compute character-cell rects
    let total = Rect::new(0, 0, 80, 24);
    let rects = tree.compute_rects(total);
    assert_eq!(rects.len(), 2);
    assert!(rects.contains_key(&pane1));
    assert!(rects.contains_key(&pane2));

    // Pixel-space pane layout
    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&tree, 0.0, 32.0, 80, 24);

    let pr1 = pane_layout.pane_pixel_rect(&pane1).expect("pane1 must have pixel rect");
    let pr2 = pane_layout.pane_pixel_rect(&pane2).expect("pane2 must have pixel rect");
    assert!(pr1.x < pr2.x, "pane1 should be left of pane2 (split_right)");
    assert!(pane_layout.splitter_count() > 0, "vertical split must produce a splitter");

    // Screen buffers with content
    let mut screen1 = ScreenBuffer::new(40, 24, 100);
    let mut screen2 = ScreenBuffer::new(40, 24, 100);
    screen1.advance(b"Left pane content\r\n");
    screen2.advance(b"Right pane content\r\n");

    // Render with focus on pane1
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen1, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .expect("paint pane1 viewport");
    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .expect("paint pane2 viewport");
    pane_layout
        .paint(renderer.render_target(), &pane1)
        .expect("pane layout with focus on pane1");
    renderer.end_draw().unwrap();

    // Switch focus to pane2 and re-render
    tree.set_focus(pane2.clone()).unwrap();
    assert_eq!(tree.focus(), pane2);

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen1, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .unwrap();
    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .unwrap();
    pane_layout
        .paint(renderer.render_target(), &pane2)
        .expect("pane layout with focus on pane2");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 3: Full composited frame — tab strip + split panes + focus ─────

#[test]
fn full_composited_frame_tab_strip_and_split_panes() {
    let hwnd = create_test_window("full_composite");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();
    let (cell_w, cell_h) = renderer.cell_size();

    // Tab strip with two tabs
    let mut tab_strip = TabStrip::new(&dw).unwrap();
    tab_strip.add_tab("dev".to_string());
    tab_strip.add_tab("build".to_string());
    tab_strip.layout(800.0);

    // Layout tree: two panes (vertical split) for "dev" tab
    let mut tree = LayoutTree::new();
    let pane1 = tree.focus();
    let pane2 = tree.split_right(pane1.clone()).unwrap();

    // Pane layout in pixel space
    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&tree, 0.0, tab_strip.height(), 80, 24);

    // Screen buffers with VT-colored content
    let mut screen1 = ScreenBuffer::new(40, 24, 100);
    let mut screen2 = ScreenBuffer::new(40, 24, 100);
    screen1.advance(b"\x1b[32meditor content\x1b[0m\r\n");
    screen2.advance(b"\x1b[33mterminal output\x1b[0m\r\n");

    let pr1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let pr2 = pane_layout.pane_pixel_rect(&pane2).unwrap();

    // Composited render: tab strip + pane viewports + pane borders/focus
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip
        .paint(renderer.render_target())
        .expect("tab strip in composited frame");
    renderer
        .paint_pane_viewport(&screen1, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .expect("pane1 viewport in composited frame");
    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .expect("pane2 viewport in composited frame");
    pane_layout
        .paint(renderer.render_target(), &pane1)
        .expect("pane layout in composited frame");
    renderer.end_draw().unwrap();

    // Switch to second tab and re-render
    tab_strip.set_active(1);
    assert_eq!(tab_strip.active_tab().unwrap().name, "build");

    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer
        .paint_pane_viewport(&screen1, pr1.x, pr1.y, pr1.width, pr1.height, None)
        .unwrap();
    renderer
        .paint_pane_viewport(&screen2, pr2.x, pr2.y, pr2.width, pr2.height, None)
        .unwrap();
    pane_layout
        .paint(renderer.render_target(), &pane1)
        .unwrap();
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 4: Focus cycling through three panes ───────────────────────────

#[test]
fn focus_cycles_through_split_panes() {
    let hwnd = create_test_window("focus_cycle");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let (cell_w, cell_h) = renderer.cell_size();

    // Three panes: split right, then split the right pane down
    let mut tree = LayoutTree::new();
    let pane1 = tree.focus();
    let pane2 = tree.split_right(pane1.clone()).unwrap();
    let pane3 = tree.split_down(pane2.clone()).unwrap();

    assert_eq!(tree.pane_count(), 3);
    assert_eq!(tree.focus(), pane1);

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&tree, 0.0, 32.0, 80, 24);

    assert!(pane_layout.pane_pixel_rect(&pane1).is_some());
    assert!(pane_layout.pane_pixel_rect(&pane2).is_some());
    assert!(pane_layout.pane_pixel_rect(&pane3).is_some());

    // Screen buffers
    let mut screens: HashMap<PaneId, ScreenBuffer> = HashMap::new();
    screens.insert(pane1.clone(), ScreenBuffer::new(40, 24, 100));
    screens.insert(pane2.clone(), ScreenBuffer::new(40, 12, 100));
    screens.insert(pane3.clone(), ScreenBuffer::new(40, 12, 100));

    let paint_all = |renderer: &TerminalRenderer,
                     pane_layout: &PaneLayout,
                     screens: &HashMap<PaneId, ScreenBuffer>,
                     focused: &PaneId| {
        renderer.begin_draw();
        renderer.clear_background();
        for (pid, screen) in screens {
            let r = pane_layout.pane_pixel_rect(pid).unwrap();
            renderer
                .paint_pane_viewport(screen, r.x, r.y, r.width, r.height, None)
                .unwrap();
        }
        pane_layout
            .paint(renderer.render_target(), focused)
            .unwrap();
        renderer.end_draw().unwrap();
    };

    // Focus pane1 (initial)
    paint_all(&renderer, &pane_layout, &screens, &tree.focus());

    // Cycle: pane1 → pane2
    tree.focus_next();
    assert_eq!(tree.focus(), pane2);
    paint_all(&renderer, &pane_layout, &screens, &tree.focus());

    // Cycle: pane2 → pane3
    tree.focus_next();
    assert_eq!(tree.focus(), pane3);
    paint_all(&renderer, &pane_layout, &screens, &tree.focus());

    // Cycle: pane3 → wraps to pane1
    tree.focus_next();
    assert_eq!(tree.focus(), pane1);
    paint_all(&renderer, &pane_layout, &screens, &tree.focus());

    // Reverse: pane1 → pane3
    tree.focus_prev();
    assert_eq!(tree.focus(), pane3);
    paint_all(&renderer, &pane_layout, &screens, &tree.focus());

    destroy_test_window(hwnd);
}

// ── Test 5: Tab close and creation ──────────────────────────────────────

#[test]
fn tab_close_and_create_with_rendering() {
    let hwnd = create_test_window("tab_close_create");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut tab_strip = TabStrip::new(&dw).unwrap();
    tab_strip.add_tab("first".to_string());
    tab_strip.add_tab("second".to_string());
    tab_strip.add_tab("third".to_string());
    tab_strip.layout(800.0);
    assert_eq!(tab_strip.tab_count(), 3);

    // Render with three tabs
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer.end_draw().unwrap();

    // Close middle tab
    let action = tab_strip.close_tab(1);
    assert!(matches!(action, TabAction::Close(1)));
    assert_eq!(tab_strip.tab_count(), 2);
    tab_strip.layout(800.0);

    // Render after close
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer.end_draw().unwrap();

    // Add a new tab
    let _new_id = tab_strip.add_tab("new-tab".to_string());
    assert_eq!(tab_strip.tab_count(), 3);
    tab_strip.layout(800.0);

    // Render after adding
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer.end_draw().unwrap();

    // Close all — last close returns WindowClose
    tab_strip.close_tab(0);
    tab_strip.close_tab(0);
    let last_action = tab_strip.close_tab(0);
    assert_eq!(last_action, TabAction::WindowClose);

    destroy_test_window(hwnd);
}

// ── Test 6: Horizontal split (split_down) with focus ────────────────────

#[test]
fn horizontal_split_renders_with_focus() {
    let hwnd = create_test_window("hsplit");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let (cell_w, cell_h) = renderer.cell_size();

    let mut tree = LayoutTree::new();
    let top = tree.focus();
    let bottom = tree.split_down(top.clone()).unwrap();
    assert_eq!(tree.pane_count(), 2);

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&tree, 0.0, 32.0, 80, 24);

    let pr_top = pane_layout.pane_pixel_rect(&top).unwrap();
    let pr_bot = pane_layout.pane_pixel_rect(&bottom).unwrap();
    assert!(pr_top.y < pr_bot.y, "top pane should be above bottom pane");

    let mut screen_top = ScreenBuffer::new(80, 12, 100);
    let mut screen_bot = ScreenBuffer::new(80, 12, 100);
    screen_top.advance(b"top pane\r\n");
    screen_bot.advance(b"bottom pane\r\n");

    // Render with focus on top
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen_top, pr_top.x, pr_top.y, pr_top.width, pr_top.height, None)
        .unwrap();
    renderer
        .paint_pane_viewport(&screen_bot, pr_bot.x, pr_bot.y, pr_bot.width, pr_bot.height, None)
        .unwrap();
    pane_layout
        .paint(renderer.render_target(), &top)
        .expect("focus on top pane");
    renderer.end_draw().unwrap();

    // Switch focus to bottom
    tree.set_focus(bottom.clone()).unwrap();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen_top, pr_top.x, pr_top.y, pr_top.width, pr_top.height, None)
        .unwrap();
    renderer
        .paint_pane_viewport(&screen_bot, pr_bot.x, pr_bot.y, pr_bot.width, pr_bot.height, None)
        .unwrap();
    pane_layout
        .paint(renderer.render_target(), &bottom)
        .expect("focus on bottom pane");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── Test 7: Tab reorder ─────────────────────────────────────────────────

#[test]
fn tab_reorder_renders_correctly() {
    let hwnd = create_test_window("tab_reorder");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let mut tab_strip = TabStrip::new(&dw).unwrap();
    let id_a = tab_strip.add_tab("alpha".to_string());
    let _id_b = tab_strip.add_tab("beta".to_string());
    let id_c = tab_strip.add_tab("gamma".to_string());
    tab_strip.layout(800.0);

    // Move gamma to position 0: [alpha, beta, gamma] → [gamma, alpha, beta]
    tab_strip.reorder(2, 0);
    assert_eq!(tab_strip.tabs()[0].id, id_c, "gamma should be first");
    assert_eq!(tab_strip.tabs()[1].id, id_a, "alpha should be second");

    tab_strip.layout(800.0);

    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(renderer.render_target()).unwrap();
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}
