//! Gate integration test: Command palette and clipboard operations (§24.6, §24.7).
//!
//! Verifies:
//! 1. Command palette opens, fuzzy-searches, and dispatches selected action
//! 2. Palette keyboard navigation (Up/Down/Enter/Escape/Backspace)
//! 3. Palette click-to-select and click-outside-to-dismiss
//! 4. Clipboard copy extracts text from ScreenBuffer with VT formatting stripped
//! 5. Clipboard paste wraps in bracketed-paste markers when DECSET 2004 active
//! 6. VT stripping removes CSI, OSC, and simple ESC sequences
//! 7. Full composited frame with palette overlay rendered
//! 8. Copy-on-select workflow: select → extract → clipboard round-trip
//!
//! Closes Slice 4.

#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use wtd_core::global_settings::{default_bindings, tmux_bindings};
use wtd_core::workspace::ActionReference;
use wtd_pty::ScreenBuffer;
use wtd_ui::clipboard::{
    copy_to_clipboard, extract_selection_text, prepare_paste, read_from_clipboard, strip_vt,
    wrap_bracketed_paste,
};
use wtd_ui::command_palette::{
    build_keybinding_hints, build_palette_entries, fuzzy_score, CommandPalette, PaletteResult,
};
use wtd_ui::input::{KeyEvent, KeyName, Modifiers};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer, TextSelection};
use wtd_ui::tab_strip::TabStrip;

// ── Unique class names ──────────────────────────────────────────────────

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(12000);

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
    let class_name_str = format!("WtdGatePalClip_{label}_{n}\0");
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
            w!("Gate Palette/Clipboard Test"),
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

// ── Key event helpers ───────────────────────────────────────────────────

fn make_key(key: KeyName, mods: Modifiers, character: Option<char>) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: mods,
        character,
    }
}

fn char_key(ch: char) -> KeyEvent {
    let key = if ch.is_ascii_alphabetic() {
        KeyName::Char(ch.to_ascii_uppercase())
    } else if ch.is_ascii_digit() {
        KeyName::Digit(ch as u8 - b'0')
    } else if ch == ' ' {
        KeyName::Space
    } else if ch == '-' {
        KeyName::Minus
    } else {
        KeyName::Char(ch.to_ascii_uppercase())
    };
    make_key(key, Modifiers::NONE, Some(ch))
}

fn action_name(action: &ActionReference) -> &str {
    match action {
        ActionReference::Simple(s) => s.as_str(),
        ActionReference::WithArgs { action, .. } => action.as_str(),
        ActionReference::Removed => "",
    }
}

// ── Test 1: Palette opens, searches, and dispatches action ──────────────

#[test]
fn palette_opens_searches_and_dispatches_action() {
    let hwnd = create_test_window("open_search_dispatch");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();

    // Initially not visible.
    assert!(!palette.is_visible());
    assert_eq!(
        palette.entry_count(),
        36,
        "palette shows the runnable action subset"
    );

    // Show the palette.
    palette.show();
    assert!(palette.is_visible());
    assert_eq!(
        palette.filtered_count(),
        36,
        "empty query shows all entries"
    );
    assert_eq!(palette.query(), "");

    // Type "split" to search.
    for ch in "split".chars() {
        let result = palette.on_key_event(&char_key(ch));
        assert_eq!(result, PaletteResult::Consumed);
    }
    assert_eq!(palette.query(), "split");

    // Should filter to actions containing "split".
    let count = palette.filtered_count();
    assert!(
        count > 0 && count < 36,
        "typing 'split' should narrow results (got {})",
        count
    );

    // Select with Enter — should dispatch the top result (split-related action).
    let result = palette.on_key_event(&make_key(KeyName::Enter, Modifiers::NONE, None));
    match result {
        PaletteResult::Action(ref action) => {
            let name = action_name(action);
            assert!(
                name.contains("split"),
                "first filtered result for 'split' should be a split action, got: {name}"
            );
        }
        other => panic!("expected Action for Enter, got: {:?}", other),
    }

    // Palette auto-hides after dispatch.
    assert!(!palette.is_visible());

    destroy_test_window(hwnd);
}

// ── Test 2: Palette keyboard navigation (Up/Down/Backspace/Escape) ──────

#[test]
fn palette_keyboard_navigation() {
    let hwnd = create_test_window("kbd_nav");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();
    palette.show();

    // Initially selected index is 0.
    assert_eq!(palette.selected_index(), 0);

    // Down arrow moves selection.
    let result = palette.on_key_event(&make_key(KeyName::Down, Modifiers::NONE, None));
    assert_eq!(result, PaletteResult::Consumed);
    assert_eq!(palette.selected_index(), 1);

    // Down again.
    palette.on_key_event(&make_key(KeyName::Down, Modifiers::NONE, None));
    assert_eq!(palette.selected_index(), 2);

    // Up arrow moves back.
    palette.on_key_event(&make_key(KeyName::Up, Modifiers::NONE, None));
    assert_eq!(palette.selected_index(), 1);

    // Type a character, then backspace should remove it.
    palette.on_key_event(&char_key('z'));
    assert_eq!(palette.query(), "z");
    palette.on_key_event(&make_key(KeyName::Backspace, Modifiers::NONE, None));
    assert_eq!(palette.query(), "");

    // Escape dismisses.
    let result = palette.on_key_event(&make_key(KeyName::Escape, Modifiers::NONE, None));
    assert_eq!(result, PaletteResult::Dismissed);
    assert!(!palette.is_visible());

    destroy_test_window(hwnd);
}

// ── Test 3: Palette click-to-select and click-outside-to-dismiss ────────

#[test]
fn palette_click_interactions() {
    let hwnd = create_test_window("click");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();
    palette.show();

    let window_w = 800.0_f32;
    let window_h = 600.0_f32;

    // Click outside the palette (top-left corner) should dismiss.
    let result = palette.on_click(0.0, 0.0, window_w, window_h);
    assert_eq!(result, Some(PaletteResult::Dismissed));
    assert!(!palette.is_visible());

    // Re-open and click on an item in the list.
    palette.show();

    // The palette is centered horizontally, starts at y=60 (TOP_OFFSET).
    // Input field is ~36px, then items start. Each item is ~34px.
    // Click on first item: center of palette, y = 60 + 36 + 8 + 17 (mid-item).
    let center_x = window_w / 2.0;
    let first_item_y = 60.0 + 36.0 + 8.0 + 17.0;

    let result = palette.on_click(center_x, first_item_y, window_w, window_h);
    match result {
        Some(PaletteResult::Action(ref action)) => {
            // First action in unfiltered list is "open-workspace" (workspace lifecycle group).
            assert_eq!(
                action_name(action),
                "open-workspace",
                "clicking first item should select open-workspace"
            );
        }
        other => panic!("expected Action for click on item, got: {:?}", other),
    }
    assert!(!palette.is_visible());

    destroy_test_window(hwnd);
}

// ── Test 4: Fuzzy search narrows to specific actions ────────────────────

#[test]
fn palette_fuzzy_search_targets_specific_actions() {
    let hwnd = create_test_window("fuzzy_target");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();

    // Search for "zoom" — should find "zoom-pane".
    palette.show();
    for ch in "zoom".chars() {
        palette.on_key_event(&char_key(ch));
    }
    assert!(
        palette.filtered_count() >= 1,
        "zoom should match at least one action"
    );
    let result = palette.on_key_event(&make_key(KeyName::Enter, Modifiers::NONE, None));
    match result {
        PaletteResult::Action(ref action) => {
            assert_eq!(action_name(action), "zoom-pane");
        }
        other => panic!("expected zoom-pane, got: {:?}", other),
    }

    // Search for "restart" — should find "restart-session".
    palette.show();
    for ch in "restart".chars() {
        palette.on_key_event(&char_key(ch));
    }
    assert!(palette.filtered_count() >= 1);
    let result = palette.on_key_event(&make_key(KeyName::Enter, Modifiers::NONE, None));
    match result {
        PaletteResult::Action(ref action) => {
            assert_eq!(action_name(action), "restart-session");
        }
        other => panic!("expected restart-session, got: {:?}", other),
    }

    // Search for "close" — should find close-tab or close-pane.
    palette.show();
    for ch in "close".chars() {
        palette.on_key_event(&char_key(ch));
    }
    assert!(palette.filtered_count() >= 1);
    let result = palette.on_key_event(&make_key(KeyName::Enter, Modifiers::NONE, None));
    match result {
        PaletteResult::Action(ref action) => {
            let name = action_name(action);
            assert!(
                name.starts_with("close-"),
                "expected a close-* action, got: {name}"
            );
        }
        other => panic!("expected a close action, got: {:?}", other),
    }

    destroy_test_window(hwnd);
}

// ── Test 5: Keybinding hints appear in palette entries ──────────────────

#[test]
fn palette_entries_include_keybinding_hints() {
    let bindings = tmux_bindings();
    let entries = build_palette_entries(&bindings);

    // split-right has single-stroke Alt+Shift+D in the tmux bindings.
    let split_right = entries.iter().find(|e| e.name == "split-right").unwrap();
    assert_eq!(
        split_right.keybinding,
        Some("Alt+Shift+D".to_string()),
        "split-right should have Alt+Shift+D keybinding hint"
    );

    // zoom-pane has chord Ctrl+B, z.
    let zoom = entries.iter().find(|e| e.name == "zoom-pane").unwrap();
    assert_eq!(
        zoom.keybinding,
        Some("Ctrl+B, z".to_string()),
        "zoom-pane should have Ctrl+B, z keybinding hint"
    );

    // Hints map should include both single-stroke and chord bindings.
    let hints = build_keybinding_hints(&bindings);
    assert!(hints.len() > 10, "should have many keybinding hints");
    assert!(hints.contains_key("split-right"));
    assert!(hints.contains_key("close-pane"));
    assert!(hints.contains_key("toggle-fullscreen"));
}

// ── Test 6: Clipboard copy extracts text from ScreenBuffer ──────────────

#[test]
fn clipboard_copy_extracts_text_from_screen_buffer() {
    let mut screen = ScreenBuffer::new(40, 10, 0);

    // Write text with ANSI formatting (bold green).
    screen.advance(b"\x1b[1;32mHello, World!\x1b[0m");

    // Extract via selection — cells are already VT-stripped.
    let selection = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 12,
    };
    let extracted = extract_selection_text(&screen, &selection);
    assert_eq!(
        extracted, "Hello, World!",
        "selection must extract plain text without VT"
    );

    // Verify extract trims trailing whitespace.
    let wide_sel = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 39,
    };
    let trimmed = extract_selection_text(&screen, &wide_sel);
    assert_eq!(trimmed, "Hello, World!", "trailing spaces must be trimmed");
}

// ── Test 7: VT stripping removes all sequence types ─────────────────────

#[test]
fn vt_stripping_comprehensive() {
    // CSI SGR (bold + color).
    assert_eq!(strip_vt("\x1b[1;31mERROR\x1b[0m"), "ERROR");

    // CSI cursor movement.
    assert_eq!(strip_vt("\x1b[10;5Htext"), "text");

    // OSC title set (BEL terminated).
    assert_eq!(strip_vt("\x1b]0;My Terminal\x07content"), "content");

    // OSC title set (ST terminated).
    assert_eq!(strip_vt("\x1b]2;Title\x1b\\content"), "content");

    // Simple ESC sequence (DECSC).
    assert_eq!(strip_vt("\x1b7saved\x1b8"), "saved");

    // Mixed sequences in realistic terminal output.
    let realistic = "\x1b[1;34muser@host\x1b[0m:\x1b[1;32m~/src\x1b[0m$ ls";
    assert_eq!(strip_vt(realistic), "user@host:~/src$ ls");

    // Multiple CSI sequences stacked.
    assert_eq!(
        strip_vt("\x1b[1m\x1b[4m\x1b[31mformatted\x1b[0m"),
        "formatted"
    );

    // Preserves newlines and tabs.
    assert_eq!(strip_vt("line1\n\x1b[32mline2\x1b[0m\n"), "line1\nline2\n");
}

// ── Test 8: Bracketed paste wraps content correctly ─────────────────────

#[test]
fn bracketed_paste_wrapping() {
    // Without bracketed paste — raw bytes.
    let plain = prepare_paste("hello world", false);
    assert_eq!(plain, b"hello world");

    // With bracketed paste — wrapped in markers.
    let bracketed = prepare_paste("hello world", true);
    assert_eq!(bracketed, b"\x1b[200~hello world\x1b[201~");

    // Empty paste with bracketed.
    let empty = prepare_paste("", true);
    assert_eq!(empty, b"\x1b[200~\x1b[201~");

    // Multiline paste with bracketed.
    let multi = prepare_paste("line1\nline2\nline3", true);
    assert_eq!(multi, b"\x1b[200~line1\nline2\nline3\x1b[201~");
}

// ── Test 9: ScreenBuffer tracks bracketed paste mode via DECSET 2004 ────

#[test]
fn screen_buffer_tracks_bracketed_paste_mode() {
    let mut screen = ScreenBuffer::new(80, 24, 0);

    // Initially disabled.
    assert!(!screen.bracketed_paste());

    // Enable DECSET 2004.
    screen.advance(b"\x1b[?2004h");
    assert!(
        screen.bracketed_paste(),
        "DECSET 2004 must enable bracketed paste"
    );

    // Prepare paste should wrap.
    let paste = prepare_paste("test", screen.bracketed_paste());
    assert_eq!(paste, b"\x1b[200~test\x1b[201~");

    // Disable DECSET 2004.
    screen.advance(b"\x1b[?2004l");
    assert!(
        !screen.bracketed_paste(),
        "DECRST 2004 must disable bracketed paste"
    );

    // Prepare paste should not wrap.
    let paste = prepare_paste("test", screen.bracketed_paste());
    assert_eq!(paste, b"test");

    // RIS resets bracketed paste.
    screen.advance(b"\x1b[?2004h");
    assert!(screen.bracketed_paste());
    screen.advance(b"\x1bc"); // RIS
    assert!(!screen.bracketed_paste(), "RIS must reset bracketed paste");
}

// ── Test 10: Clipboard round-trip with VT-stripped content ──────────────
// All clipboard operations in one test to avoid concurrent global clipboard access.

#[test]
fn clipboard_round_trip_with_vt_stripped_content() {
    // Step 1: Extract text from a ScreenBuffer that received VT-formatted input.
    let mut screen = ScreenBuffer::new(50, 10, 0);
    screen.advance(b"\x1b[1;33mWARNING\x1b[0m: disk full");

    let selection = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 17,
    };
    let extracted = extract_selection_text(&screen, &selection);
    assert_eq!(
        extracted, "WARNING: disk full",
        "must extract plain text from VT-styled cells"
    );

    // Step 2: Copy to clipboard.
    copy_to_clipboard(&extracted).expect("copy must succeed");

    // Step 3: Read back from clipboard.
    let read = read_from_clipboard().expect("read must succeed");
    assert_eq!(read, extracted, "clipboard round-trip must preserve text");

    // Step 4: Also test strip_vt as a safety layer on raw VT strings.
    let raw_vt = "\x1b[1;33mWARNING\x1b[0m: disk full";
    let stripped = strip_vt(raw_vt);
    assert_eq!(stripped, "WARNING: disk full");

    // Copy the stripped version.
    copy_to_clipboard(&stripped).expect("copy stripped must succeed");
    let read2 = read_from_clipboard().expect("read must succeed");
    assert_eq!(read2, "WARNING: disk full");
}

// ── Test 11: Multi-row selection to clipboard ───────────────────────────

#[test]
fn multi_row_selection_clipboard_copy() {
    let mut screen = ScreenBuffer::new(20, 5, 0);
    // Write two lines.
    screen.advance(b"First line\r\n");
    screen.advance(b"Second line");

    let selection = TextSelection {
        start_row: 0,
        start_col: 0,
        end_row: 1,
        end_col: 10,
    };
    let text = extract_selection_text(&screen, &selection);

    // Should be two lines separated by newline, trimmed.
    assert!(text.contains("First line"), "must contain first line");
    assert!(text.contains("Second line"), "must contain second line");
    assert!(text.contains('\n'), "multi-row must have newline separator");
}

// ── Test 12: Palette toggle behavior ────────────────────────────────────

#[test]
fn palette_toggle_show_hide() {
    let hwnd = create_test_window("toggle");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();

    assert!(!palette.is_visible());

    // Toggle on.
    palette.toggle();
    assert!(palette.is_visible());

    // Toggle off.
    palette.toggle();
    assert!(!palette.is_visible());

    // Toggle on again.
    palette.toggle();
    assert!(palette.is_visible());

    // Show resets query and selection.
    for ch in "test".chars() {
        palette.on_key_event(&char_key(ch));
    }
    assert_eq!(palette.query(), "test");
    palette.on_key_event(&make_key(KeyName::Down, Modifiers::NONE, None));

    // Re-show should reset.
    palette.show();
    assert_eq!(palette.query(), "");
    assert_eq!(palette.selected_index(), 0);
    assert_eq!(palette.filtered_count(), 36);

    destroy_test_window(hwnd);
}

// ── Test 13: Palette renders as overlay in composited frame ─────────────

#[test]
fn palette_renders_in_composited_frame() {
    let hwnd = create_test_window("render_composite");
    let renderer = TerminalRenderer::new(hwnd, &RendererConfig::default()).unwrap();
    let dw = renderer.dw_factory().clone();
    let rt = renderer.render_target();

    // Create components.
    let mut tab_strip = TabStrip::new(&dw).unwrap();
    tab_strip.add_tab("main".to_string());
    tab_strip.layout(800.0);

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(&dw, &bindings, vec![]).unwrap();
    palette.show();

    // Type a search query so we have filtered results.
    for ch in "tab".chars() {
        palette.on_key_event(&char_key(ch));
    }

    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"$ hello terminal");

    // Render composited frame: terminal content + tab strip + palette overlay.
    renderer.begin_draw();
    renderer.clear_background();
    tab_strip.paint(rt).unwrap();
    renderer
        .paint_pane_viewport(&screen, 0.0, 32.0, 800.0, 544.0, None)
        .unwrap();
    palette.paint(rt, 800.0, 600.0).unwrap();
    renderer.end_draw().unwrap();

    // Verify palette is still visible and has filtered results.
    assert!(palette.is_visible());
    assert!(palette.filtered_count() > 0, "filtered results for 'tab'");
    assert!(palette.filtered_count() < 36, "not all entries match 'tab'");

    destroy_test_window(hwnd);
}

// ── Test 14: Fuzzy score ranking prefers better matches ─────────────────

#[test]
fn fuzzy_score_ranking() {
    // Exact prefix match should score higher than mid-word match.
    let s_exact = fuzzy_score("close", "close-pane Close pane and kill session").unwrap();
    let s_mid = fuzzy_score("close", "enter-scrollback-mode Enter scrollback navigation");
    // "close" may or may not match the second string — if it doesn't, that's fine.
    match s_mid {
        Some(s) => assert!(
            s_exact > s,
            "exact prefix 'close' should rank higher: exact={}, mid={}",
            s_exact,
            s
        ),
        None => {} // no match is also correct
    }

    // Consecutive character bonus.
    let s_consec = fuzzy_score("split", "split-right Split pane on right").unwrap();
    assert!(
        s_consec > 10,
        "consecutive match should have substantial score"
    );

    // No match returns None.
    assert!(fuzzy_score("xyz123", "split-right").is_none());
}

// ── Test 15: Paste into session with bracketed paste mode ───────────────

#[test]
fn paste_workflow_with_bracketed_paste() {
    let mut screen = ScreenBuffer::new(80, 24, 0);

    // Application enables bracketed paste.
    screen.advance(b"\x1b[?2004h");
    assert!(screen.bracketed_paste());

    // User pastes text — should be wrapped.
    let paste_text = "echo hello && echo world";
    let to_send = prepare_paste(paste_text, screen.bracketed_paste());
    assert!(
        to_send.starts_with(b"\x1b[200~"),
        "must start with bracketed paste start marker"
    );
    assert!(
        to_send.ends_with(b"\x1b[201~"),
        "must end with bracketed paste end marker"
    );
    let inner = &to_send[6..to_send.len() - 6];
    assert_eq!(
        inner,
        paste_text.as_bytes(),
        "inner content must match paste text"
    );

    // Application disables bracketed paste (e.g. shell exits to raw mode).
    screen.advance(b"\x1b[?2004l");
    assert!(!screen.bracketed_paste());

    // Same paste text — should NOT be wrapped.
    let to_send = prepare_paste(paste_text, screen.bracketed_paste());
    assert_eq!(
        to_send,
        paste_text.as_bytes(),
        "without bracketed paste, raw bytes"
    );
}

// ── Test 16: wrap_bracketed_paste preserves binary content ──────────────

#[test]
fn wrap_bracketed_paste_preserves_binary() {
    let data = b"\x00\x01\x02\xff";
    let wrapped = wrap_bracketed_paste(data);
    assert_eq!(&wrapped[..6], b"\x1b[200~");
    assert_eq!(&wrapped[6..10], data);
    assert_eq!(&wrapped[10..], b"\x1b[201~");
}
