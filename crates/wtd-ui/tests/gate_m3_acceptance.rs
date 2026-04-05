//! M3 Acceptance Gate — Rendering spike complete (§37.5)
//!
//! This test proves the M3 milestone: a rendering technology decision has been
//! made with benchmarks, and a minimal prototype renders VT output bytes in a
//! window using the chosen renderer (Win32 + DirectWrite).
//!
//! Criteria validated (§37.5 M3):
//!   1. Decision document is written with benchmark data
//!   2. Decision document evaluates all three candidates (§7.9)
//!   3. A technology is selected with a GO verdict
//!   4. Prototype creates Direct2D/DirectWrite rendering resources
//!   5. Prototype renders VT output (plain text) in a pane viewport
//!   6. Prototype renders VT output with ANSI colors and attributes

#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use wtd_pty::ScreenBuffer;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

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
    let class_name_str = format!("WtdM3Gate_{label}_{n}\0");
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
            w!("M3 Gate Test"),
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

// ── Decision Document Path ───────────────────────────────────────────────

/// Path to the decision document relative to the workspace root.
const DECISION_DOC_RELATIVE: &str = "docs/decisions/001-rendering-technology.md";

/// Resolve the workspace root from this test file's location.
fn workspace_root() -> std::path::PathBuf {
    // This test is at crates/wtd-ui/tests/gate_m3_acceptance.rs
    // Workspace root is three levels up.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

// ── M3 Acceptance Test ───────────────────────────────────────────────────

/// **M3 Acceptance Gate (§37.5)**
///
/// Proves the rendering spike is complete:
///   - Decision document exists with benchmarks for all three candidates
///   - Minimal prototype renders VT output in a window using Direct2D + DirectWrite
///
/// Steps:
///   1. Verify the decision document exists and contains required content
///   2. Create a hidden window and TerminalRenderer (Direct2D + DirectWrite)
///   3. Feed plain text VT bytes into a ScreenBuffer and render
///   4. Feed colored/attributed VT bytes and render with full attribute pipeline
#[test]
fn m3_rendering_spike_acceptance() {
    // ── Criterion 1: Decision document exists with benchmarks ─────────
    let doc_path = workspace_root().join(DECISION_DOC_RELATIVE);
    assert!(
        doc_path.exists(),
        "M3 criterion 1: Decision document must exist at {DECISION_DOC_RELATIVE}"
    );

    let doc_content = std::fs::read_to_string(&doc_path)
        .expect("M3 criterion 1: Decision document must be readable");

    // Must contain benchmark data (frame times, FPS)
    assert!(
        doc_content.contains("ms/frame") || doc_content.contains("Avg frame"),
        "M3 criterion 1: Decision document must contain benchmark data (frame times)"
    );
    assert!(
        doc_content.contains("FPS"),
        "M3 criterion 1: Decision document must contain FPS measurements"
    );
    assert!(
        doc_content.contains("Memory") || doc_content.contains("Working set"),
        "M3 criterion 1: Decision document must contain memory measurements"
    );

    // ── Criterion 2: All three candidates evaluated (§7.9) ───────────
    assert!(
        doc_content.contains("wezterm"),
        "M3 criterion 2: Decision document must evaluate wezterm components"
    );
    assert!(
        doc_content.contains("DirectWrite") || doc_content.contains("Direct2D"),
        "M3 criterion 2: Decision document must evaluate Win32 + DirectWrite"
    );
    assert!(
        doc_content.contains("WebView2") || doc_content.contains("xterm.js"),
        "M3 criterion 2: Decision document must evaluate WebView2 + xterm.js"
    );

    // ── Criterion 3: A technology is selected ────────────────────────
    assert!(
        doc_content.contains("Accepted") || doc_content.contains("GO (Recommended)"),
        "M3 criterion 3: Decision document must contain a GO/Accepted verdict"
    );
    assert!(
        doc_content.contains("Win32 + DirectWrite") && doc_content.contains("selected"),
        "M3 criterion 3: Win32 + DirectWrite must be the selected technology"
    );

    // ── Criterion 4: Create renderer (Direct2D + DirectWrite) ────────
    let hwnd = create_test_window("m3_acceptance");
    let config = RendererConfig::default();
    let renderer = TerminalRenderer::new(hwnd, &config)
        .expect("M3 criterion 4: TerminalRenderer must initialise D2D/DWrite resources");

    let (cell_w, cell_h) = renderer.cell_size();
    assert!(
        cell_w > 0.0 && cell_h > 0.0,
        "M3 criterion 4: Cell dimensions must be positive (got {cell_w}x{cell_h})"
    );

    // ── Criterion 5: Render plain VT output in a pane viewport ───────
    let mut screen = ScreenBuffer::new(80, 24, 1000);

    // Simulate plain terminal output (command prompt + echoed text)
    let plain_vt = b"C:\\Users\\test> echo M3_GATE_READY\r\nM3_GATE_READY\r\n\r\nC:\\Users\\test> ";
    screen.advance(plain_vt);

    let visible = screen.visible_text();
    assert!(
        visible.contains("M3_GATE_READY"),
        "M3 criterion 5: ScreenBuffer must contain plain text after advance. Got:\n{visible}"
    );

    // Render the plain content to the window viewport
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&screen, 0.0, 0.0, 800.0, 600.0, None)
        .expect("M3 criterion 5: paint_pane_viewport must render plain VT output without error");
    renderer
        .end_draw()
        .expect("M3 criterion 5: end_draw must succeed");

    // ── Criterion 6: Render colored/attributed VT output ─────────────
    let mut styled_screen = ScreenBuffer::new(80, 24, 1000);

    // Feed VT sequences with attributes:
    //   - Green foreground:   ESC[32m
    //   - Bold:               ESC[1m
    //   - Red background:     ESC[41m
    //   - Italic:             ESC[3m
    //   - Underline:          ESC[4m
    //   - 256-color fg (cyan): ESC[38;5;14m
    //   - RGB fg:             ESC[38;2;255;165;0m  (orange)
    //   - Reset:              ESC[0m
    let styled_vt = b"\x1b[32mGreen text\x1b[0m \
                       \x1b[1m\x1b[31mBold red\x1b[0m \
                       \x1b[3mItalic\x1b[0m \
                       \x1b[4mUnderline\x1b[0m \
                       \x1b[41mRed background\x1b[0m\r\n\
                       \x1b[38;5;14m256-color cyan\x1b[0m \
                       \x1b[38;2;255;165;0mTruecolor orange\x1b[0m\r\n\
                       \x1b[1;3;4;32mBold italic underline green\x1b[0m";
    styled_screen.advance(styled_vt);

    let styled_visible = styled_screen.visible_text();
    assert!(
        styled_visible.contains("Green text")
            && styled_visible.contains("Bold red")
            && styled_visible.contains("256-color cyan")
            && styled_visible.contains("Truecolor orange"),
        "M3 criterion 6: ScreenBuffer must parse styled VT output. Got:\n{styled_visible}"
    );

    // Render styled content
    renderer.begin_draw();
    renderer.clear_background();
    renderer
        .paint_pane_viewport(&styled_screen, 0.0, 0.0, 800.0, 600.0, None)
        .expect(
            "M3 criterion 6: paint_pane_viewport must render styled VT output \
             (colors, bold, italic, underline, 256-color, truecolor) without error",
        );
    renderer
        .end_draw()
        .expect("M3 criterion 6: end_draw must succeed");

    // Cleanup
    destroy_test_window(hwnd);
}
