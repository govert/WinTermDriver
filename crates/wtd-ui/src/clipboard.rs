//! Clipboard operations for copy, paste, VT stripping, and bracketed paste.
//!
//! Copy extracts selected text from a [`ScreenBuffer`] with VT formatting
//! stripped (§24.7). Paste reads from the Windows clipboard and optionally
//! wraps content in bracketed-paste markers when the session has enabled
//! DECSET 2004.

use wtd_pty::ScreenBuffer;

use crate::renderer::TextSelection;

// ── Text extraction ─────────────────────────────────────────────────────────

/// Extract plain text from a selection range in the screen buffer.
///
/// Characters are taken directly from parsed cells, so VT formatting is
/// inherently stripped.  Wide-character continuation cells are skipped.
/// Trailing whitespace on each line is trimmed.
pub fn extract_selection_text(screen: &ScreenBuffer, selection: &TextSelection) -> String {
    extract_selection_text_at_offset(screen, selection, 0)
}

/// Extract plain text from a selection range in a viewport scrolled back from
/// the live screen by `scrollback_offset` rows.
pub fn extract_selection_text_at_offset(
    screen: &ScreenBuffer,
    selection: &TextSelection,
    scrollback_offset: usize,
) -> String {
    let (sr, sc, er, ec) = selection.normalised();
    let rows = screen.rows();
    let cols = screen.cols();
    let base_row = screen.scrollback_len().saturating_sub(scrollback_offset);
    let mut result = String::new();

    for row in sr..=er {
        if row >= rows {
            break;
        }

        let col_start = if row == sr { sc } else { 0 };
        let col_end = if row == er {
            ec.min(cols.saturating_sub(1))
        } else {
            cols.saturating_sub(1)
        };

        let mut line = String::new();
        for col in col_start..=col_end {
            if col >= cols {
                break;
            }
            if let Some(cell) = screen.cell_at_virtual(base_row + row, col) {
                if !cell.attrs.is_wide_continuation() {
                    line.push_str(cell.text.as_str());
                }
            }
        }

        // Trim trailing whitespace per line.
        let trimmed = line.trim_end();
        result.push_str(trimmed);
        if row < er {
            result.push('\n');
        }
    }

    result
}

// ── VT stripping ────────────────────────────────────────────────────────────

/// Strip ANSI/VT escape sequences from text.
///
/// The screen buffer already stores parsed characters, so this is a safety
/// measure for edge cases where literal ESC bytes might appear (e.g. from
/// raw PTY output that wasn't fully consumed by the VT parser).
pub fn strip_vt(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    // CSI sequence: ESC [ ... (final byte 0x40–0x7E)
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    // OSC sequence: ESC ] ... (terminated by BEL or ST)
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        if next == '\x07' {
                            chars.next();
                            break;
                        }
                        if next == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                Some(_) => {
                    // Simple two-byte ESC sequence.
                    chars.next();
                }
                None => {}
            }
        } else {
            result.push(c);
        }
    }

    result
}

// ── Bracketed paste ─────────────────────────────────────────────────────────

/// Bracketed paste start marker: `ESC [ 200 ~`
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";

/// Bracketed paste end marker: `ESC [ 201 ~`
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// Wrap raw bytes in bracketed-paste markers.
pub fn wrap_bracketed_paste(data: &[u8]) -> Vec<u8> {
    let mut result =
        Vec::with_capacity(BRACKETED_PASTE_START.len() + data.len() + BRACKETED_PASTE_END.len());
    result.extend_from_slice(BRACKETED_PASTE_START);
    result.extend_from_slice(data);
    result.extend_from_slice(BRACKETED_PASTE_END);
    result
}

/// Prepare paste data: encode as UTF-8 bytes, optionally wrapped in
/// bracketed-paste markers if the session has DECSET 2004 active.
pub fn prepare_paste(text: &str, bracketed_paste_active: bool) -> Vec<u8> {
    let bytes = text.as_bytes();
    if bracketed_paste_active {
        wrap_bracketed_paste(bytes)
    } else {
        bytes.to_vec()
    }
}

// ── Win32 clipboard ─────────────────────────────────────────────────────────

/// Error type for clipboard operations.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("failed to open clipboard")]
    Open,
    #[error("failed to set clipboard data")]
    SetData,
    #[error("clipboard does not contain text")]
    NoText,
    #[error("failed to allocate global memory")]
    Alloc,
    #[error("failed to lock global memory")]
    Lock,
}

#[cfg(windows)]
mod win32 {
    use super::ClipboardError;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;

    const CF_UNICODETEXT: u32 = 13;

    /// Copy UTF-16 text to the Windows clipboard.
    ///
    /// # Safety
    /// Uses Win32 clipboard API.  Must be called from a thread that owns a
    /// message queue or passes `HWND(0)` (which is fine for console/detached).
    pub fn copy_to_clipboard(text: &str) -> Result<(), ClipboardError> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * 2;

        unsafe {
            if !OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                return Err(ClipboardError::Open);
            }

            let result = (|| -> Result<(), ClipboardError> {
                let _ = EmptyClipboard();

                let hmem =
                    GlobalAlloc(GMEM_MOVEABLE, byte_len).map_err(|_| ClipboardError::Alloc)?;

                let ptr = GlobalLock(hmem);
                if ptr.is_null() {
                    let _ = GlobalFree(hmem);
                    return Err(ClipboardError::Lock);
                }

                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, byte_len);
                let _ = GlobalUnlock(hmem);

                // SetClipboardData takes ownership of the memory handle.
                let handle = HANDLE(hmem.0);
                if SetClipboardData(CF_UNICODETEXT, handle).is_err() {
                    let _ = GlobalFree(hmem);
                    return Err(ClipboardError::SetData);
                }

                Ok(())
            })();

            let _ = CloseClipboard();
            result
        }
    }

    /// Read UTF-16 text from the Windows clipboard.
    pub fn read_from_clipboard() -> Result<String, ClipboardError> {
        unsafe {
            if !OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                return Err(ClipboardError::Open);
            }

            let result = (|| -> Result<String, ClipboardError> {
                let handle =
                    GetClipboardData(CF_UNICODETEXT).map_err(|_| ClipboardError::NoText)?;

                // The HANDLE from GetClipboardData is an HGLOBAL.
                let hmem = HGLOBAL(handle.0);

                let ptr = GlobalLock(hmem);
                if ptr.is_null() {
                    return Err(ClipboardError::Lock);
                }

                // Find the null terminator to determine string length.
                let mut len = 0usize;
                let wptr = ptr as *const u16;
                while *wptr.add(len) != 0 {
                    len += 1;
                }

                let slice = std::slice::from_raw_parts(wptr, len);
                let text = String::from_utf16_lossy(slice);

                let _ = GlobalUnlock(hmem);
                Ok(text)
            })();

            let _ = CloseClipboard();
            result
        }
    }
}

#[cfg(windows)]
pub use win32::{copy_to_clipboard, read_from_clipboard};

// Stubs for non-Windows (allows cargo check on other platforms).
#[cfg(not(windows))]
pub fn copy_to_clipboard(_text: &str) -> Result<(), ClipboardError> {
    Err(ClipboardError::Open)
}

#[cfg(not(windows))]
pub fn read_from_clipboard() -> Result<String, ClipboardError> {
    Err(ClipboardError::NoText)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_vt ────────────────────────────────────────────────────────

    #[test]
    fn strip_vt_plain_text_unchanged() {
        assert_eq!(strip_vt("hello world"), "hello world");
    }

    #[test]
    fn strip_vt_removes_csi_sgr() {
        // Bold red text: ESC[1;31m hello ESC[0m
        assert_eq!(strip_vt("\x1b[1;31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn strip_vt_removes_cursor_movement() {
        // CSI 5;10H (cursor position)
        assert_eq!(strip_vt("\x1b[5;10Hworld"), "world");
    }

    #[test]
    fn strip_vt_removes_osc_bel() {
        // OSC 0 ; title BEL
        assert_eq!(strip_vt("\x1b]0;my title\x07rest"), "rest");
    }

    #[test]
    fn strip_vt_removes_osc_st() {
        // OSC 2 ; title ST (ESC \)
        assert_eq!(strip_vt("\x1b]2;my title\x1b\\rest"), "rest");
    }

    #[test]
    fn strip_vt_removes_simple_esc() {
        // DECSC = ESC 7
        assert_eq!(strip_vt("\x1b7hello"), "hello");
    }

    #[test]
    fn strip_vt_mixed_sequences() {
        let input = "\x1b[1mBold\x1b[0m \x1b]0;title\x07normal \x1b[31mred\x1b[0m";
        assert_eq!(strip_vt(input), "Bold normal red");
    }

    #[test]
    fn strip_vt_preserves_newlines() {
        assert_eq!(strip_vt("line1\nline2\n"), "line1\nline2\n");
    }

    #[test]
    fn strip_vt_empty_string() {
        assert_eq!(strip_vt(""), "");
    }

    #[test]
    fn strip_vt_trailing_esc() {
        // Lone ESC at end of string.
        assert_eq!(strip_vt("text\x1b"), "text");
    }

    // ── bracketed paste ─────────────────────────────────────────────────

    #[test]
    fn wrap_bracketed_paste_wraps_correctly() {
        let data = b"hello world";
        let result = wrap_bracketed_paste(data);
        assert_eq!(result, b"\x1b[200~hello world\x1b[201~");
    }

    #[test]
    fn prepare_paste_without_bracketed() {
        let result = prepare_paste("hello", false);
        assert_eq!(result, b"hello");
    }

    #[test]
    fn prepare_paste_with_bracketed() {
        let result = prepare_paste("hello", true);
        assert_eq!(result, b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn prepare_paste_empty_string() {
        let result = prepare_paste("", true);
        assert_eq!(result, b"\x1b[200~\x1b[201~");
    }

    // ── extract_selection_text ──────────────────────────────────────────

    #[test]
    fn extract_single_row_selection() {
        let mut screen = ScreenBuffer::new(20, 5, 0);
        screen.advance(b"Hello, world!");
        let sel = TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 4,
        };
        assert_eq!(extract_selection_text(&screen, &sel), "Hello");
    }

    #[test]
    fn extract_multi_row_selection() {
        let mut screen = ScreenBuffer::new(10, 5, 0);
        // Write two rows.
        screen.advance(b"AAAAAAAAAA");
        screen.advance(b"BBBBBBBBBB");
        let sel = TextSelection {
            start_row: 0,
            start_col: 3,
            end_row: 1,
            end_col: 2,
        };
        let text = extract_selection_text(&screen, &sel);
        assert_eq!(text, "AAAAAAA\nBBB");
    }

    #[test]
    fn extract_trims_trailing_whitespace() {
        let mut screen = ScreenBuffer::new(20, 5, 0);
        screen.advance(b"Hi");
        let sel = TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 19,
        };
        assert_eq!(extract_selection_text(&screen, &sel), "Hi");
    }

    #[test]
    fn extract_reversed_selection() {
        let mut screen = ScreenBuffer::new(20, 5, 0);
        screen.advance(b"Hello, world!");
        // Selection is end-before-start; normalised() handles it.
        let sel = TextSelection {
            start_row: 0,
            start_col: 4,
            end_row: 0,
            end_col: 0,
        };
        assert_eq!(extract_selection_text(&screen, &sel), "Hello");
    }

    #[test]
    fn extract_empty_screen() {
        let screen = ScreenBuffer::new(10, 5, 0);
        let sel = TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 9,
        };
        // All spaces, trimmed to empty.
        assert_eq!(extract_selection_text(&screen, &sel), "");
    }

    #[test]
    fn extract_selection_past_screen_bounds() {
        let mut screen = ScreenBuffer::new(10, 3, 0);
        screen.advance(b"ABC");
        let sel = TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: 10, // way past the screen
            end_col: 5,
        };
        let text = extract_selection_text(&screen, &sel);
        // Should get row 0 (ABC), rows 1-2 (empty, trimmed), and stop at row 3.
        assert!(text.starts_with("ABC"));
    }

    #[test]
    fn extract_selection_from_scrollback_offset() {
        let mut screen = ScreenBuffer::new(5, 2, 10);
        screen.advance(b"11111\r\n22222\r\n33333\r\n");
        let sel = TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: 1,
            end_col: 4,
        };
        assert_eq!(
            extract_selection_text_at_offset(&screen, &sel, 1),
            "22222\n33333"
        );
    }

    // ── Win32 clipboard round-trip (integration) ────────────────────────
    // All clipboard tests in a single function to avoid concurrent access
    // to the global clipboard from parallel test threads.

    #[cfg(windows)]
    #[test]
    fn clipboard_round_trip() {
        // Basic text round-trip.
        let test_text = "WinTermDriver clipboard test 🦀";
        copy_to_clipboard(test_text).expect("copy should succeed");
        let read = read_from_clipboard().expect("read should succeed");
        assert_eq!(read, test_text);

        // Empty string.
        copy_to_clipboard("").expect("copy empty should succeed");
        let read = read_from_clipboard().expect("read should succeed");
        assert_eq!(read, "");

        // Multiline text.
        let text = "line 1\nline 2\nline 3";
        copy_to_clipboard(text).expect("copy should succeed");
        let read = read_from_clipboard().expect("read should succeed");
        assert_eq!(read, text);
    }
}
