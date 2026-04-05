//! Integration tests for the rendering prototype.
//!
//! These tests verify that the full rendering pipeline (VT bytes -> ScreenBuffer
//! -> DirectWrite -> window) works end-to-end without crashing, and that color
//! mapping and attribute resolution are correct.

use wtd_pty::{Cell, CellAttrs, Color, ScreenBuffer};
use wtd_ui::renderer::{
    color_to_rgb, exited_pane_message, failed_pane_message, resolve_cell_colors, RendererConfig,
    TerminalRenderer, TextSelection, RESTART_HINT,
};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::sync::atomic::{AtomicU32, Ordering};

// Unique class name counter to avoid RegisterClass collisions across tests.
static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Create a hidden test window with a unique class name.
fn create_test_window(label: &str) -> HWND {
    let n = CLASS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let class_name_str = format!("WtdTest_{label}_{n}\0");
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

fn read_window_pixel(hwnd: HWND, x: i32, y: i32) -> (u8, u8, u8) {
    unsafe {
        let hdc = GetDC(hwnd);
        assert!(!hdc.0.is_null(), "GetDC must succeed");
        let color = GetPixel(hdc, x, y);
        let _ = ReleaseDC(hwnd, hdc);
        assert_ne!(color.0, CLR_INVALID, "GetPixel must succeed");
        let raw = color.0;
        let r = (raw & 0x0000_00ff) as u8;
        let g = ((raw & 0x0000_ff00) >> 8) as u8;
        let b = ((raw & 0x00ff_0000) >> 16) as u8;
        (r, g, b)
    }
}

// ── Color mapping tests ──────────────────────────────────────────────────────

#[test]
fn all_16_ansi_colors_produce_distinct_rgb() {
    let mut seen = std::collections::HashSet::new();
    for i in 0..8u8 {
        let rgb = color_to_rgb(&Color::Ansi(i), true);
        seen.insert(rgb);
    }
    for i in 0..8u8 {
        let rgb = color_to_rgb(&Color::AnsiBright(i), true);
        seen.insert(rgb);
    }
    assert_eq!(seen.len(), 16, "Expected 16 distinct ANSI colors");
}

#[test]
fn indexed_256_color_covers_full_range() {
    for i in 0..=255u8 {
        let (r, g, b) = color_to_rgb(&Color::Indexed(i), true);
        assert!(r <= 255 && g <= 255 && b <= 255, "Index {i} out of range");
    }
}

#[test]
fn indexed_colors_0_to_15_match_ansi() {
    for i in 0..8u8 {
        assert_eq!(
            color_to_rgb(&Color::Indexed(i), true),
            color_to_rgb(&Color::Ansi(i), true),
            "Indexed({i}) should match Ansi({i})"
        );
    }
    for i in 0..8u8 {
        assert_eq!(
            color_to_rgb(&Color::Indexed(i + 8), true),
            color_to_rgb(&Color::AnsiBright(i), true),
            "Indexed({}) should match AnsiBright({i})",
            i + 8
        );
    }
}

#[test]
fn truecolor_passthrough() {
    assert_eq!(color_to_rgb(&Color::Rgb(0, 0, 0), true), (0, 0, 0));
    assert_eq!(
        color_to_rgb(&Color::Rgb(255, 255, 255), true),
        (255, 255, 255)
    );
    assert_eq!(
        color_to_rgb(&Color::Rgb(42, 100, 200), true),
        (42, 100, 200)
    );
}

// ── Attribute resolution tests ───────────────────────────────────────────────

#[test]
fn inverse_swaps_fg_and_bg() {
    let mut attrs = CellAttrs::default();
    attrs.set(CellAttrs::INVERSE);
    let cell = Cell {
        character: 'X',
        text: "X".to_string(),
        fg: Color::Rgb(100, 200, 50),
        bg: Color::Rgb(10, 20, 30),
        attrs,
        wide: false,
        wide_continuation: false,
    };
    let (fg, bg) = resolve_cell_colors(&cell);
    assert_eq!(fg, (10, 20, 30), "Inverse should swap: fg becomes old bg");
    assert_eq!(bg, (100, 200, 50), "Inverse should swap: bg becomes old fg");
}

#[test]
fn dim_halves_foreground() {
    let mut attrs = CellAttrs::default();
    attrs.set(CellAttrs::DIM);
    let cell = Cell {
        character: 'D',
        text: "D".to_string(),
        fg: Color::Rgb(200, 100, 50),
        bg: Color::Default,
        attrs,
        wide: false,
        wide_continuation: false,
    };
    let (fg, _) = resolve_cell_colors(&cell);
    assert_eq!(fg, (100, 50, 25), "Dim should halve each fg component");
}

#[test]
fn dim_plus_inverse() {
    let mut attrs = CellAttrs::default();
    attrs.set(CellAttrs::DIM);
    attrs.set(CellAttrs::INVERSE);
    let cell = Cell {
        character: 'X',
        text: "X".to_string(),
        fg: Color::Rgb(200, 100, 50),
        bg: Color::Rgb(80, 40, 20),
        attrs,
        wide: false,
        wide_continuation: false,
    };
    let (fg, bg) = resolve_cell_colors(&cell);
    // Inverse first: fg=(80,40,20), bg=(200,100,50)
    // Dim applied to fg: (40,20,10)
    assert_eq!(fg, (40, 20, 10));
    assert_eq!(bg, (200, 100, 50));
}

// ── ScreenBuffer + VT parsing integration ────────────────────────────────────

#[test]
fn screen_buffer_parses_colored_text() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[31mHello\x1b[0m World");

    let cell_h = screen.cell(0, 0).unwrap();
    assert_eq!(cell_h.character, 'H');
    assert_eq!(cell_h.fg, Color::Ansi(1));

    let cell_w = screen.cell(0, 6).unwrap();
    assert_eq!(cell_w.character, 'W');
    assert_eq!(cell_w.fg, Color::Default);
}

#[test]
fn screen_buffer_parses_bold_attribute() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[1mBOLD\x1b[0m plain");

    let bold_cell = screen.cell(0, 0).unwrap();
    assert!(
        bold_cell.attrs.is_set(CellAttrs::BOLD),
        "First char should be bold"
    );

    let plain_cell = screen.cell(0, 5).unwrap();
    assert!(
        !plain_cell.attrs.is_set(CellAttrs::BOLD),
        "After reset, text should not be bold"
    );
}

#[test]
fn screen_buffer_parses_multiple_attributes() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[1;3;4mABC\x1b[0m");

    let cell = screen.cell(0, 0).unwrap();
    assert!(cell.attrs.is_set(CellAttrs::BOLD));
    assert!(cell.attrs.is_set(CellAttrs::ITALIC));
    assert!(cell.attrs.is_set(CellAttrs::UNDERLINE));
}

#[test]
fn screen_buffer_parses_256_color() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[38;5;196mX\x1b[0m");

    let cell = screen.cell(0, 0).unwrap();
    assert_eq!(cell.fg, Color::Indexed(196));
}

#[test]
fn screen_buffer_parses_truecolor() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[38;2;42;128;255mY\x1b[0m");

    let cell = screen.cell(0, 0).unwrap();
    assert_eq!(cell.fg, Color::Rgb(42, 128, 255));
}

#[test]
fn screen_buffer_parses_background_color() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[44mZ\x1b[0m");

    let cell = screen.cell(0, 0).unwrap();
    assert_eq!(cell.bg, Color::Ansi(4));
}

// ── End-to-end rendering tests ───────────────────────────────────────────────

#[test]
fn end_to_end_render_does_not_crash() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b[1;31mBold Red\x1b[0m ");
    screen.advance(b"\x1b[3;32mItalic Green\x1b[0m ");
    screen.advance(b"\x1b[4;34mUnderline Blue\x1b[0m ");
    screen.advance(b"\x1b[7mInverse\x1b[0m ");
    screen.advance(b"\x1b[9mStrikethrough\x1b[0m\r\n");
    screen.advance(b"\x1b[38;2;255;128;0mTruecolor\x1b[0m ");
    screen.advance(b"\x1b[38;5;196mIndexed\x1b[0m\r\n");
    screen.advance(b"Plain text on line 3");

    let hwnd = create_test_window("e2e");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).expect("renderer creation should succeed");

    let (cell_w, cell_h) = renderer.cell_size();
    assert!(cell_w > 0.0, "Cell width should be positive");
    assert!(cell_h > 0.0, "Cell height should be positive");

    renderer.paint(&screen).expect("paint should succeed");
    renderer
        .paint(&screen)
        .expect("second paint should succeed");

    destroy_test_window(hwnd);
}

#[test]
fn render_empty_screen_buffer() {
    let screen = ScreenBuffer::new(80, 24, 0);

    let hwnd = create_test_window("empty");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    renderer
        .paint(&screen)
        .expect("painting empty buffer should succeed");

    destroy_test_window(hwnd);
}

#[test]
fn render_full_screen_of_colored_text() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    // Fill every row with colored text.
    for i in 0..24u8 {
        let color = 31 + (i % 7);
        let line =
            format!("\x1b[{color}mRow {i:02} filled with colored text padding xxxx\x1b[0m\r\n");
        screen.advance(line.as_bytes());
    }

    let hwnd = create_test_window("fullscreen");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    renderer
        .paint(&screen)
        .expect("painting full colored screen should succeed");

    destroy_test_window(hwnd);
}

#[test]
fn render_mixed_attributes() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    // Line with every attribute combination.
    screen.advance(b"\x1b[1mBold\x1b[0m ");
    screen.advance(b"\x1b[2mDim\x1b[0m ");
    screen.advance(b"\x1b[3mItalic\x1b[0m ");
    screen.advance(b"\x1b[4mUnderline\x1b[0m ");
    screen.advance(b"\x1b[7mInverse\x1b[0m ");
    screen.advance(b"\x1b[9mStrike\x1b[0m ");
    screen.advance(b"\x1b[1;3mBold+Italic\x1b[0m ");
    screen.advance(b"\x1b[1;4mBold+UL\x1b[0m ");
    screen.advance(b"\x1b[1;3;4;9mAll\x1b[0m");

    let hwnd = create_test_window("mixed_attrs");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    renderer
        .paint(&screen)
        .expect("painting mixed attributes should succeed");

    destroy_test_window(hwnd);
}

#[test]
fn renderer_resize_succeeds() {
    let screen = ScreenBuffer::new(80, 24, 0);

    let hwnd = create_test_window("resize");
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config).unwrap();

    renderer.resize(1024, 768).expect("resize should succeed");
    renderer
        .paint(&screen)
        .expect("paint after resize should succeed");

    destroy_test_window(hwnd);
}

// ── Pane viewport rendering ─────────────────────────────────────────────────

#[test]
fn paint_pane_viewport_basic() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"Hello viewport");

    let hwnd = create_test_window("viewport_basic");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 10.0, 32.0, cw * 40.0, ch * 12.0, None)
        .expect("viewport paint should succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_with_selection() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"Line 1 text\r\nLine 2 text\r\nLine 3 text");

    let sel = TextSelection {
        start_row: 0,
        start_col: 5,
        end_row: 1,
        end_col: 8,
    };

    let hwnd = create_test_window("viewport_sel");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 0.0, 0.0, cw * 80.0, ch * 24.0, Some(&sel))
        .expect("viewport with selection should succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_multiple_panes() {
    let mut screen1 = ScreenBuffer::new(40, 12, 0);
    screen1.advance(b"\x1b[32mPane 1\x1b[0m");
    let mut screen2 = ScreenBuffer::new(40, 12, 0);
    screen2.advance(b"\x1b[31mPane 2\x1b[0m");

    let hwnd = create_test_window("viewport_multi");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    // Left pane
    renderer
        .paint_pane_viewport(&screen1, 0.0, 0.0, cw * 40.0, ch * 12.0, None)
        .expect("left pane should paint");
    // Right pane
    renderer
        .paint_pane_viewport(&screen2, cw * 40.0, 0.0, cw * 40.0, ch * 12.0, None)
        .expect("right pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_with_colors_and_attributes() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    // Bold + red + underline
    screen.advance(b"\x1b[1;31;4mBold red underline\x1b[0m\r\n");
    // Italic + green + strikethrough
    screen.advance(b"\x1b[3;32;9mItalic green strike\x1b[0m\r\n");
    // Inverse
    screen.advance(b"\x1b[7mInverse text\x1b[0m\r\n");
    // 256-color
    screen.advance(b"\x1b[38;5;208mOrange 256\x1b[0m\r\n");
    // Truecolor
    screen.advance(b"\x1b[38;2;100;200;50mTruecolor\x1b[0m");

    let hwnd = create_test_window("viewport_attrs");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 5.0, 5.0, cw * 80.0, ch * 24.0, None)
        .expect("viewport with colors/attributes should succeed");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_with_cursor_shapes() {
    // Block cursor
    let mut screen_block = ScreenBuffer::new(40, 12, 0);
    screen_block.advance(b"Block cursor");

    // Underline cursor
    let mut screen_uline = ScreenBuffer::new(40, 12, 0);
    screen_uline.advance(b"\x1b[3 qUnderline");

    // Bar cursor
    let mut screen_bar = ScreenBuffer::new(40, 12, 0);
    screen_bar.advance(b"\x1b[5 qBar cursor");

    let hwnd = create_test_window("viewport_cursors");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen_block, 0.0, 0.0, cw * 40.0, ch * 12.0, None)
        .expect("block cursor pane should paint");
    renderer
        .paint_pane_viewport(&screen_uline, cw * 40.0, 0.0, cw * 40.0, ch * 12.0, None)
        .expect("underline cursor pane should paint");
    renderer
        .paint_pane_viewport(&screen_bar, 0.0, ch * 12.0, cw * 40.0, ch * 12.0, None)
        .expect("bar cursor pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_alternate_screen() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    // Write on primary
    screen.advance(b"primary content");
    // Switch to alternate screen (like vim/htop)
    screen.advance(b"\x1b[?1049h");
    // Write on alternate
    screen.advance(b"\x1b[1;1HTUI App Header\r\n\x1b[32mStatus: OK\x1b[0m");

    let hwnd = create_test_window("viewport_altscreen");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 0.0, 0.0, cw * 80.0, ch * 24.0, None)
        .expect("alternate screen viewport should paint");
    renderer.end_draw().unwrap();

    // Verify we're on alternate screen
    assert!(screen.on_alternate());

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_clears_stale_background_without_global_clear() {
    let mut red_screen = ScreenBuffer::new(8, 2, 0);
    red_screen.advance(b"\x1b[?1049h\x1b[41m        \r\n        \x1b[0m");

    let mut default_screen = ScreenBuffer::new(8, 2, 0);
    default_screen.advance(b"\x1b[?1049hZ");

    let hwnd = create_test_window("viewport_self_clear");
    let config = RendererConfig {
        software_rendering: true,
        ..RendererConfig::default()
    };
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);
    }
    let (cw, ch) = renderer.cell_size();
    let width = cw * 8.0;
    let height = ch * 2.0;

    renderer.begin_draw();
    renderer
        .paint_pane_viewport(&red_screen, 0.0, 0.0, width, height, None)
        .expect("red viewport should paint");
    renderer.end_draw().unwrap();

    renderer.begin_draw();
    renderer
        .paint_pane_viewport(&default_screen, 0.0, 0.0, width, height, None)
        .expect("default viewport should repaint without global clear");
    renderer.end_draw().unwrap();

    let sample = read_window_pixel(hwnd, (cw * 6.0) as i32, (ch * 1.0) as i32);
    assert_eq!(
        sample,
        color_to_rgb(&Color::Default, false),
        "viewport repaint should restore default background instead of leaving stale red fill"
    );

    destroy_test_window(hwnd);
}

#[test]
fn paint_pane_viewport_hidden_cursor() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"text");
    screen.advance(b"\x1b[?25l"); // hide cursor
    assert!(!screen.cursor().visible);

    let hwnd = create_test_window("viewport_hidden_cursor");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 0.0, 0.0, cw * 80.0, ch * 24.0, None)
        .expect("hidden cursor viewport should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

// ── TextSelection unit tests ────────────────────────────────────────────────

#[test]
fn selection_normalised_forward() {
    let sel = TextSelection {
        start_row: 0,
        start_col: 5,
        end_row: 2,
        end_col: 10,
    };
    assert_eq!(sel.normalised(), (0, 5, 2, 10));
}

#[test]
fn selection_normalised_backward() {
    let sel = TextSelection {
        start_row: 3,
        start_col: 8,
        end_row: 1,
        end_col: 2,
    };
    assert_eq!(sel.normalised(), (1, 2, 3, 8));
}

#[test]
fn selection_contains_single_row() {
    let sel = TextSelection {
        start_row: 2,
        start_col: 3,
        end_row: 2,
        end_col: 7,
    };
    assert!(!sel.contains(2, 2));
    assert!(sel.contains(2, 3));
    assert!(sel.contains(2, 5));
    assert!(sel.contains(2, 7));
    assert!(!sel.contains(2, 8));
    assert!(!sel.contains(1, 5));
    assert!(!sel.contains(3, 5));
}

#[test]
fn selection_contains_multi_row() {
    let sel = TextSelection {
        start_row: 1,
        start_col: 5,
        end_row: 3,
        end_col: 10,
    };
    // Row 0: outside
    assert!(!sel.contains(0, 5));
    // Row 1: from col 5 to end
    assert!(!sel.contains(1, 4));
    assert!(sel.contains(1, 5));
    assert!(sel.contains(1, 79));
    // Row 2: entire row
    assert!(sel.contains(2, 0));
    assert!(sel.contains(2, 79));
    // Row 3: from start to col 10
    assert!(sel.contains(3, 0));
    assert!(sel.contains(3, 10));
    assert!(!sel.contains(3, 11));
    // Row 4: outside
    assert!(!sel.contains(4, 0));
}

#[test]
fn selection_contains_backward() {
    // Same as multi-row but with reversed start/end
    let sel = TextSelection {
        start_row: 3,
        start_col: 10,
        end_row: 1,
        end_col: 5,
    };
    assert!(sel.contains(2, 0));
    assert!(sel.contains(1, 5));
    assert!(sel.contains(3, 10));
    assert!(!sel.contains(3, 11));
}

// ── Failed pane rendering tests ─────────────────────────────────────────────

#[test]
fn paint_failed_pane_exited() {
    let hwnd = create_test_window("failed_pane_exited");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&exited_pane_message(0), 0.0, 0.0, cw * 80.0, ch * 24.0)
        .expect("exited pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_failed_pane_error() {
    let hwnd = create_test_window("failed_pane_error");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(
            &failed_pane_message("CreateProcess failed: file not found"),
            0.0,
            0.0,
            cw * 80.0,
            ch * 24.0,
        )
        .expect("failed pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_failed_pane_small_viewport() {
    let hwnd = create_test_window("failed_pane_small");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    // Very small pane — should not panic or error.
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(&exited_pane_message(1), 10.0, 10.0, cw * 10.0, ch * 3.0)
        .expect("small failed pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_failed_pane_at_offset() {
    let hwnd = create_test_window("failed_pane_offset");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    // Render at an offset (as if a tab strip is above and pane is offset).
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_failed_pane(
            &failed_pane_message("profile not found"),
            cw * 5.0,
            ch * 3.0,
            cw * 40.0,
            ch * 12.0,
        )
        .expect("offset failed pane should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn paint_failed_pane_alongside_normal_pane() {
    let mut screen = ScreenBuffer::new(40, 24, 0);
    screen.advance(b"Hello, world!");

    let hwnd = create_test_window("failed_alongside_normal");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config).unwrap();
    let (cw, ch) = renderer.cell_size();

    renderer.begin_draw();
    renderer.clear_background();
    // Left pane: normal terminal content.
    renderer
        .paint_pane_viewport(&screen, 0.0, 0.0, cw * 40.0, ch * 24.0, None)
        .expect("normal pane should paint");
    // Right pane: failed overlay.
    renderer
        .paint_failed_pane(
            &exited_pane_message(127),
            cw * 40.0,
            0.0,
            cw * 40.0,
            ch * 24.0,
        )
        .expect("failed pane next to normal should paint");
    renderer.end_draw().unwrap();

    destroy_test_window(hwnd);
}

#[test]
fn message_helpers_produce_expected_strings() {
    assert_eq!(exited_pane_message(0), "Session exited (code 0)");
    assert_eq!(exited_pane_message(42), "Session exited (code 42)");
    assert_eq!(
        failed_pane_message("out of memory"),
        "Session failed: out of memory"
    );
    assert!(RESTART_HINT.contains("Enter"));
    assert!(RESTART_HINT.contains("Ctrl+B"));
}
