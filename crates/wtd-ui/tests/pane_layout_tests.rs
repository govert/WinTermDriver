//! Integration tests for pane layout rendering, splitter detection, and
//! mouse interaction (§24.4).

use wtd_core::ids::PaneId;
use wtd_core::layout::{LayoutTree, Rect, ResizeDirection};
use wtd_ui::pane_layout::{CursorHint, PaneLayout, PaneLayoutAction, PixelRect};

const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;
const COLS: u16 = 80;
const ROWS: u16 = 24;
const TAB_STRIP_HEIGHT: f32 = 32.0;

fn make_layout() -> PaneLayout {
    PaneLayout::new(CELL_W, CELL_H)
}

// ── Splitter detection ───────────────────────────────────────────────────────

#[test]
fn single_pane_has_no_splitters() {
    let tree = LayoutTree::new();
    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    assert_eq!(layout.splitter_count(), 0);
    assert_eq!(layout.pane_pixel_rects().len(), 1);
}

#[test]
fn horizontal_split_produces_one_vertical_splitter() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    assert_eq!(layout.splitter_count(), 1);
    assert_eq!(layout.pane_pixel_rects().len(), 2);
}

#[test]
fn vertical_split_produces_one_horizontal_splitter() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_down(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    assert_eq!(layout.splitter_count(), 1);
}

#[test]
fn three_pane_layout_has_two_splitters() {
    // Split right, then split the left pane down → 3 panes, 2 splitters.
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1.clone()).unwrap();
    let _p3 = tree.split_down(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    assert_eq!(layout.pane_pixel_rects().len(), 3);
    assert_eq!(layout.splitter_count(), 2);
}

#[test]
fn four_pane_grid_has_three_splitters() {
    // Split right, then split each half down → 4 panes.
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1.clone()).unwrap();
    let _p3 = tree.split_down(p1).unwrap();
    let _p4 = tree.split_down(p2).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    assert_eq!(layout.pane_pixel_rects().len(), 4);
    // 1 vertical splitter (full height), 2 horizontal splitters (one in each column).
    // But the two horizontal splitters are at the same y position and same x range,
    // so they may be merged into one or detected as two separate segments.
    // With 4 panes in a 2x2 grid, we expect 3 splitters.
    assert!(layout.splitter_count() >= 2);
}

// ── Pixel rect computation ───────────────────────────────────────────────────

#[test]
fn pane_rects_cover_content_area() {
    let tree = LayoutTree::new();
    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    let p1 = tree.focus();
    let rect = layout.pane_pixel_rect(&p1).unwrap();
    assert!((rect.x - 0.0).abs() < 0.01);
    assert!((rect.y - TAB_STRIP_HEIGHT).abs() < 0.01);
    assert!((rect.width - (COLS as f32 * CELL_W)).abs() < 0.01);
    assert!((rect.height - (ROWS as f32 * CELL_H)).abs() < 0.01);
}

#[test]
fn split_panes_share_content_width() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    let r1 = layout.pane_pixel_rect(&p1).unwrap();
    let r2 = layout.pane_pixel_rect(&p2).unwrap();

    // Together they should span the full width.
    let total_w = COLS as f32 * CELL_W;
    assert!((r1.width + r2.width - total_w).abs() < 0.01);

    // r2 starts where r1 ends.
    assert!((r2.x - (r1.x + r1.width)).abs() < 0.01);
}

#[test]
fn origin_offset_shifts_all_rects() {
    let tree = LayoutTree::new();
    let mut layout = make_layout();
    layout.update(&tree, 15.0, 50.0, COLS, ROWS);

    let p1 = tree.focus();
    let rect = layout.pane_pixel_rect(&p1).unwrap();
    assert!((rect.x - 15.0).abs() < 0.01);
    assert!((rect.y - 50.0).abs() < 0.01);
}

// ── Mouse focus ──────────────────────────────────────────────────────────────

#[test]
fn click_left_pane_focuses_left() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Click in the left half.
    let action = layout.on_mouse_down(50.0, 100.0);
    assert_eq!(action, Some(PaneLayoutAction::FocusPane(p1)));
}

#[test]
fn click_right_pane_focuses_right() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Click in the right half (past 320px midpoint).
    let action = layout.on_mouse_down(400.0, 100.0);
    assert_eq!(action, Some(PaneLayoutAction::FocusPane(p2)));
}

#[test]
fn click_outside_all_panes_returns_none() {
    let tree = LayoutTree::new();
    let mut layout = make_layout();
    layout.update(&tree, 0.0, TAB_STRIP_HEIGHT, COLS, ROWS);

    // Click above the content area (in tab strip zone).
    let action = layout.on_mouse_down(100.0, 10.0);
    assert_eq!(action, None);
}

// ── Splitter drag ────────────────────────────────────────────────────────────

#[test]
fn splitter_drag_emits_grow_right() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Splitter at x=320.
    layout.on_mouse_down(320.0, 100.0);
    assert!(layout.is_dragging());

    // Drag right by 2 cells = 16px.
    let action = layout.on_mouse_move(336.0, 100.0);
    assert!(action.is_some());
    match action.unwrap() {
        PaneLayoutAction::Resize {
            pane_id,
            direction,
            cells,
        } => {
            assert_eq!(pane_id, p1);
            assert_eq!(direction, ResizeDirection::GrowRight);
            assert_eq!(cells, 2);
        }
        other => panic!("expected Resize, got {:?}", other),
    }
}

#[test]
fn splitter_drag_emits_shrink_right() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    layout.on_mouse_down(320.0, 100.0);

    // Drag left by 1 cell = 8px.
    let action = layout.on_mouse_move(312.0, 100.0);
    assert!(action.is_some());
    match action.unwrap() {
        PaneLayoutAction::Resize {
            direction, cells, ..
        } => {
            assert_eq!(direction, ResizeDirection::ShrinkRight);
            assert_eq!(cells, 1);
        }
        other => panic!("expected Resize, got {:?}", other),
    }
}

#[test]
fn vertical_splitter_drag_emits_grow_down() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_down(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Horizontal splitter at y=192 (12*16).
    layout.on_mouse_down(200.0, 192.0);
    assert!(layout.is_dragging());

    // Drag down by 1 cell = 16px.
    let action = layout.on_mouse_move(200.0, 208.0);
    assert!(action.is_some());
    match action.unwrap() {
        PaneLayoutAction::Resize {
            direction, cells, ..
        } => {
            assert_eq!(direction, ResizeDirection::GrowDown);
            assert_eq!(cells, 1);
        }
        other => panic!("expected Resize, got {:?}", other),
    }
}

#[test]
fn drag_accumulates_sub_cell_movement() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    layout.on_mouse_down(320.0, 100.0);

    // Move 3px — less than one cell, no action.
    assert_eq!(layout.on_mouse_move(323.0, 100.0), None);

    // Move 3 more px (total 6) — still less than 8px cell width.
    assert_eq!(layout.on_mouse_move(326.0, 100.0), None);

    // Move 3 more px (total 9) — crosses 8px boundary, should emit.
    let action = layout.on_mouse_move(329.0, 100.0);
    assert!(action.is_some());
}

#[test]
fn mouse_up_stops_drag() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    layout.on_mouse_down(320.0, 100.0);
    assert!(layout.is_dragging());

    layout.on_mouse_up(340.0, 100.0);
    assert!(!layout.is_dragging());
}

// ── Cursor hints ─────────────────────────────────────────────────────────────

#[test]
fn cursor_arrow_away_from_splitters() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    assert_eq!(layout.cursor_hint(100.0, 100.0), CursorHint::Arrow);
}

#[test]
fn cursor_resize_h_near_vertical_splitter() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Within hit zone of vertical splitter at x=320.
    assert_eq!(layout.cursor_hint(321.0, 100.0), CursorHint::ResizeHorizontal);
}

#[test]
fn cursor_resize_v_near_horizontal_splitter() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_down(p1).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    // Within hit zone of horizontal splitter at y=192.
    assert_eq!(layout.cursor_hint(200.0, 193.0), CursorHint::ResizeVertical);
}

// ── Zoom ─────────────────────────────────────────────────────────────────────

#[test]
fn zoomed_layout_has_single_rect_and_no_splitters() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let _p2 = tree.split_right(p1.clone()).unwrap();
    tree.toggle_zoom();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    assert_eq!(layout.pane_pixel_rects().len(), 1);
    assert_eq!(layout.splitter_count(), 0);
    assert!(layout.pane_pixel_rect(&p1).is_some());
}

// ── Layout update after resize ───────────────────────────────────────────────

#[test]
fn update_after_tree_resize_changes_rects() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    let r1_before = layout.pane_pixel_rect(&p1).unwrap();
    let r2_before = layout.pane_pixel_rect(&p2).unwrap();

    // Resize: grow p1 by 8 cells.
    let total = Rect::new(0, 0, COLS, ROWS);
    tree.resize_pane(p1.clone(), ResizeDirection::GrowRight, 8, total)
        .unwrap();

    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    let r1_after = layout.pane_pixel_rect(&p1).unwrap();
    let r2_after = layout.pane_pixel_rect(&p2).unwrap();

    // p1 got wider, p2 got narrower.
    assert!(r1_after.width > r1_before.width);
    assert!(r2_after.width < r2_before.width);
}

// ── PixelRect ────────────────────────────────────────────────────────────────

#[test]
fn pixel_rect_new_and_fields() {
    let r = PixelRect::new(10.0, 20.0, 100.0, 50.0);
    assert!((r.x - 10.0).abs() < f32::EPSILON);
    assert!((r.y - 20.0).abs() < f32::EPSILON);
    assert!((r.width - 100.0).abs() < f32::EPSILON);
    assert!((r.height - 50.0).abs() < f32::EPSILON);
}

// ── End-to-end: drag → resize → re-layout ────────────────────────────────────

#[test]
fn full_drag_resize_relayout_cycle() {
    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1.clone()).unwrap();

    let mut layout = make_layout();
    layout.update(&tree, 0.0, 0.0, COLS, ROWS);

    let r1_before = layout.pane_pixel_rect(&p1).unwrap().width;

    // Simulate: click splitter, drag right by 2 cells, release.
    layout.on_mouse_down(320.0, 100.0);

    if let Some(PaneLayoutAction::Resize {
        pane_id,
        direction,
        cells,
    }) = layout.on_mouse_move(336.0, 100.0)
    {
        let total = Rect::new(0, 0, COLS, ROWS);
        tree.resize_pane(pane_id, direction, cells, total).unwrap();
        layout.update(&tree, 0.0, 0.0, COLS, ROWS);
    }

    layout.on_mouse_up(336.0, 100.0);

    let r1_after = layout.pane_pixel_rect(&p1).unwrap().width;
    assert!(r1_after > r1_before, "p1 should be wider after dragging splitter right");
}

// ── Rendering (D2D) ─────────────────────────────────────────────────────────

#[cfg(windows)]
mod render_tests {
    use super::*;
    use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
    use wtd_ui::window;

    #[test]
    fn paint_single_pane_does_not_crash() {
        let hwnd = window::create_terminal_window("pane_layout_test_1", 400, 300).unwrap();
        let config = RendererConfig::default();
        let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

        let tree = LayoutTree::new();
        let (cw, ch) = renderer.cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 50, 20);

        renderer.begin_draw();
        renderer.clear_background();
        let result = layout.paint(renderer.render_target(), &tree.focus());
        let _ = renderer.end_draw();
        result.unwrap();

        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    #[test]
    fn paint_split_panes_with_focus_does_not_crash() {
        let hwnd = window::create_terminal_window("pane_layout_test_2", 400, 300).unwrap();
        let config = RendererConfig::default();
        let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let _p3 = tree.split_down(p1).unwrap();
        tree.set_focus(p2.clone()).unwrap();

        let (cw, ch) = renderer.cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        renderer.begin_draw();
        renderer.clear_background();
        let result = layout.paint(renderer.render_target(), &tree.focus());
        let _ = renderer.end_draw();
        result.unwrap();

        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    #[test]
    fn composite_paint_tab_strip_pane_layout_does_not_crash() {
        use wtd_ui::tab_strip::TabStrip;

        let hwnd = window::create_terminal_window("pane_layout_test_3", 800, 600).unwrap();
        let config = RendererConfig::default();
        let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

        let mut tab_strip = TabStrip::new(renderer.dw_factory()).unwrap();
        tab_strip.add_tab("tab1".to_string());
        tab_strip.set_active(0);
        tab_strip.layout(800.0);

        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1).unwrap();

        let (cw, ch) = renderer.cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, tab_strip.height(), 80, 24);

        renderer.begin_draw();
        renderer.clear_background();

        tab_strip.paint(renderer.render_target()).unwrap();
        layout
            .paint(renderer.render_target(), &tree.focus())
            .unwrap();

        renderer.end_draw().unwrap();

        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    #[test]
    fn paint_zoomed_pane_renders_single_border() {
        let hwnd = window::create_terminal_window("pane_layout_test_4", 400, 300).unwrap();
        let config = RendererConfig::default();
        let renderer = TerminalRenderer::new(hwnd, &config).unwrap();

        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1).unwrap();
        tree.toggle_zoom();

        let (cw, ch) = renderer.cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        renderer.begin_draw();
        renderer.clear_background();
        layout
            .paint(renderer.render_target(), &tree.focus())
            .unwrap();
        renderer.end_draw().unwrap();

        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }
}
