//! Host-side VT screen buffer (§14.4, §15).
//!
//! [`ScreenBuffer`] maintains the full terminal state: active grid, alternate
//! screen, scrollback ring, cursor, and title.  Feed raw PTY output bytes via
//! [`ScreenBuffer::advance`]; query state at any time.

use std::collections::VecDeque;

use regex::Regex;
use unicode_display_width::width as unicode_display_width;
use unicode_segmentation::UnicodeSegmentation;
use vte::{Params, Perform};

// ── Color ────────────────────────────────────────────────────────────────────

/// A terminal color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Default foreground / background (terminal decides).
    Default,
    /// One of the 8 ANSI named colors (index 0–7).
    Ansi(u8),
    /// One of the 16 bright ANSI colors (index 0–7, bright variant).
    AnsiBright(u8),
    /// 256-color palette entry.
    Indexed(u8),
    /// 24-bit RGB truecolor.
    Rgb(u8, u8, u8),
}

// ── CellAttrs ────────────────────────────────────────────────────────────────

/// Visual attributes for a single cell (bitfield).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs(u16);

impl CellAttrs {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKETHROUGH: u16 = 1 << 7;
    pub const WIDE: u16 = 1 << 8;
    pub const WIDE_CONTINUATION: u16 = 1 << 9;

    /// Mask covering only the SGR attribute bits (0–7), excluding wide flags.
    const SGR_MASK: u16 = 0xFF;

    pub fn is_set(self, flag: u16) -> bool {
        self.0 & flag != 0
    }
    pub fn set(&mut self, flag: u16) {
        self.0 |= flag;
    }
    pub fn clear(&mut self, flag: u16) {
        self.0 &= !flag;
    }

    pub fn is_wide(self) -> bool {
        self.0 & Self::WIDE != 0
    }
    pub fn is_wide_continuation(self) -> bool {
        self.0 & Self::WIDE_CONTINUATION != 0
    }
    pub fn set_wide(&mut self) {
        self.0 |= Self::WIDE;
    }
    pub fn set_wide_continuation(&mut self) {
        self.0 |= Self::WIDE_CONTINUATION;
    }
    pub fn clear_wide(&mut self) {
        self.0 &= !Self::WIDE;
    }
    pub fn clear_wide_continuation(&mut self) {
        self.0 &= !Self::WIDE_CONTINUATION;
    }

    /// Compare only the SGR attribute bits, ignoring wide-char flags.
    /// Used for style-run detection in snapshots and rendering.
    pub fn sgr_eq(self, other: Self) -> bool {
        (self.0 & Self::SGR_MASK) == (other.0 & Self::SGR_MASK)
    }
}

// ── CompactText ─────────────────────────────────────────────────────────────

/// Inline small-string for terminal cell text (8 bytes, `Copy`).
///
/// Stores up to 7 bytes of UTF-8 inline.  Byte 0 is the length; bytes 1–7
/// hold the UTF-8 payload.  For the rare grapheme cluster exceeding 7 bytes
/// (complex ZWJ emoji), only the first codepoint is stored.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompactText {
    data: [u8; 8],
}

impl CompactText {
    /// Maximum number of inline UTF-8 bytes.
    const MAX_INLINE: usize = 7;

    /// Create a CompactText from a string slice.
    pub fn new(s: &str) -> Self {
        let mut data = [0u8; 8];
        if s.len() <= Self::MAX_INLINE {
            data[0] = s.len() as u8;
            data[1..1 + s.len()].copy_from_slice(s.as_bytes());
        } else {
            // Fallback: store first codepoint only.
            if let Some(ch) = s.chars().next() {
                let mut buf = [0u8; 4];
                let encoded = ch.encode_utf8(&mut buf);
                data[0] = encoded.len() as u8;
                data[1..1 + encoded.len()].copy_from_slice(encoded.as_bytes());
            }
            // else: empty string (len stays 0)
        }
        CompactText { data }
    }

    /// A space character, the default cell content.
    #[inline]
    pub const fn space() -> Self {
        CompactText {
            data: [1, b' ', 0, 0, 0, 0, 0, 0],
        }
    }

    /// Return the stored text as a `&str`.
    #[inline]
    pub fn as_str(&self) -> &str {
        let len = self.data[0] as usize;
        // SAFETY: We only store valid UTF-8 via `new()` or `space()`.
        unsafe { std::str::from_utf8_unchecked(&self.data[1..1 + len]) }
    }

    /// The number of stored bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.data[0] as usize
    }

    /// Whether the stored text is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data[0] == 0
    }
}

impl PartialEq<&str> for CompactText {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Default for CompactText {
    fn default() -> Self {
        Self::space()
    }
}

impl std::fmt::Debug for CompactText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl std::fmt::Display for CompactText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Cell ─────────────────────────────────────────────────────────────────────

/// A single terminal cell.
///
/// 18 bytes (+ padding): `CompactText`(8) + `Color`(4) × 2 + `CellAttrs`(2).
/// Wide-character flags are packed into `attrs`.  This struct is `Copy`,
/// enabling bulk `memcpy` for scroll and clear operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Full grapheme rendered in this cell (inline small-string).
    pub text: CompactText,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Visual attributes (includes wide-character flags).
    pub attrs: CellAttrs,
    /// Hyperlink identifier associated with this cell (0 = none).
    pub hyperlink_id: u16,
    /// Inline image identifier associated with this cell (0 = none).
    pub image_id: u16,
}

// Cell is derived Copy — if the derive fails, the struct has a non-Copy field.

impl Cell {
    /// A blank (space) cell with default colors and no attributes.
    pub fn blank() -> Self {
        Cell {
            text: CompactText::space(),
            fg: Color::Default,
            bg: Color::Default,
            attrs: CellAttrs::default(),
            hyperlink_id: 0,
            image_id: 0,
        }
    }

    /// The first Unicode codepoint of this cell's text.
    ///
    /// Convenience accessor for tests and diagnostics; equivalent to
    /// `self.text.as_str().chars().next().unwrap_or(' ')`.
    pub fn first_char(&self) -> char {
        self.text.as_str().chars().next().unwrap_or(' ')
    }
}

// ── MouseMode ───────────────────────────────────────────────────────────────

/// Mouse tracking mode requested by the application via DECSET.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// No mouse reporting (default).
    #[default]
    None,
    /// Normal tracking (DECSET 1000): report press and release.
    Normal,
    /// Button-event tracking (DECSET 1002): report press, release, and drag.
    ButtonEvent,
    /// Any-event tracking (DECSET 1003): report all motion even without buttons.
    AnyEvent,
}

/// Keyboard input protocol negotiated by the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardProtocolMode {
    /// Legacy xterm-compatible keyboard behavior.
    #[default]
    Legacy,
    /// CSI-u enhanced keyboard reporting.
    CsiU,
    /// Kitty keyboard protocol.
    Kitty,
}

/// Stored inline image record parsed from a terminal graphics protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    /// Protocol identifier, currently `kitty`.
    pub protocol: String,
    /// Parameter string before the payload separator.
    pub params: String,
    /// Raw payload body after the separator.
    pub payload: String,
}

/// Progress indicator requested by the application via OSC 9;4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalProgress {
    /// Normal progress (WT state 1).
    Normal(u8),
    /// Error progress (WT state 2).
    Error(u8),
    /// Indeterminate progress (WT state 3).
    Indeterminate,
    /// Warning progress (WT state 4).
    Warning(u8),
}

/// Notification requested by the application via OSC sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalNotification {
    pub message: String,
}

// ── CaptureExtendedResult ─────────────────────────────────────────────────────

/// Result returned by [`ScreenBuffer::capture_extended`].
#[derive(Debug, Clone)]
pub struct CaptureExtendedResult {
    /// Captured text, one line per row, each terminated with `\n`.
    /// Empty string when `count_only` is `true`.
    pub text: String,
    /// Number of lines captured (rows from `cursor` to end of buffer).
    pub lines: u32,
    /// Total lines in the combined buffer (`scrollback.len() + rows`).
    pub total_lines: u32,
    /// Whether the `after`/`after_regex` anchor was found.
    /// `None` when no anchor was specified.
    pub anchor_found: Option<bool>,
    /// Absolute line index of the capture start (0 = oldest scrollback row).
    pub cursor: u32,
}

// ── Grid ─────────────────────────────────────────────────────────────────────

/// A rectangular grid of cells.
#[derive(Debug, Clone)]
struct Grid {
    cols: usize,
    rows: usize,
    /// Row-major: `cells[row * cols + col]`.
    cells: Vec<Cell>,
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Self {
        let cells = vec![Cell::blank(); cols * rows];
        Grid { cols, rows, cells }
    }

    fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[row * self.cols + col]
    }

    fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        &mut self.cells[row * self.cols + col]
    }

    /// Scroll up by `n` lines, pushing displaced rows into `scrollback`.
    fn scroll_up(&mut self, n: usize, scrollback: &mut VecDeque<Vec<Cell>>, max_scrollback: usize) {
        let n = n.min(self.rows);
        for row_idx in 0..n {
            let start = row_idx * self.cols;
            let row: Vec<Cell> = self.cells[start..start + self.cols].to_vec();
            if max_scrollback > 0 {
                if scrollback.len() >= max_scrollback {
                    scrollback.pop_front();
                }
                scrollback.push_back(row);
            }
        }
        self.cells.drain(0..n * self.cols);
        let new_cells = vec![Cell::blank(); n * self.cols];
        self.cells.extend(new_cells);
    }

    /// Scroll the region [top, bottom] (inclusive, 0-based rows) up by n.
    fn scroll_region_up(
        &mut self,
        top: usize,
        bottom: usize,
        n: usize,
        scrollback: &mut VecDeque<Vec<Cell>>,
        max_scrollback: usize,
    ) {
        if top == 0 && bottom == self.rows.saturating_sub(1) {
            self.scroll_up(n, scrollback, max_scrollback);
            return;
        }
        let n = n.min(bottom + 1 - top);
        let cols = self.cols;
        // Shift rows [top+n..=bottom] up to [top..=bottom-n], blank the vacated rows.
        let src_start = (top + n) * cols;
        let src_end = (bottom + 1) * cols;
        let dst_start = top * cols;
        self.cells.copy_within(src_start..src_end, dst_start);
        let blank_start = (bottom + 1 - n) * cols;
        self.cells[blank_start..(bottom + 1) * cols].fill(Cell::blank());
    }

    /// Scroll the region [top, bottom] (inclusive, 0-based rows) down by n.
    fn scroll_region_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        let cols = self.cols;
        // Shift rows [top..=bottom-n] down to [top+n..=bottom], blank the vacated rows.
        let src_start = top * cols;
        let src_end = (bottom + 1 - n) * cols;
        let dst_start = (top + n) * cols;
        self.cells.copy_within(src_start..src_end, dst_start);
        self.cells[src_start..(top + n) * cols].fill(Cell::blank());
    }

    /// Clear from (row, col) to end of screen.
    fn clear_from(&mut self, row: usize, col: usize) {
        let start = row * self.cols + col;
        self.cells[start..].fill(Cell::blank());
    }

    /// Clear from start of screen to (row, col) inclusive.
    fn clear_to(&mut self, row: usize, col: usize) {
        let end = row * self.cols + col + 1;
        self.cells[..end].fill(Cell::blank());
    }

    /// Clear an entire row.
    fn clear_row(&mut self, row: usize) {
        let start = row * self.cols;
        self.cells[start..start + self.cols].fill(Cell::blank());
    }

    /// Clear from column to end of row.
    fn clear_row_from(&mut self, row: usize, col: usize) {
        let start = row * self.cols + col;
        let end = (row + 1) * self.cols;
        self.cells[start..end].fill(Cell::blank());
    }

    /// Clear from start of row to column (inclusive).
    fn clear_row_to(&mut self, row: usize, col: usize) {
        let start = row * self.cols;
        let end = row * self.cols + col + 1;
        self.cells[start..end].fill(Cell::blank());
    }

    fn row_slice(&self, row: usize) -> &[Cell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut new_cells = vec![Cell::blank(); new_cols * new_rows];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for r in 0..copy_rows {
            let src_start = r * self.cols;
            let dst_start = r * new_cols;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.cells = new_cells;
    }
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Build an SGR escape sequence for the given foreground, background, and attrs.
///
/// Always starts with `\x1b[0m` (reset) to avoid leaking attributes across runs.
fn build_sgr_params(fg: Color, bg: Color, attrs: CellAttrs) -> Vec<u8> {
    use std::io::Write;
    // Pre-allocate for a typical SGR sequence (e.g. "\x1b[0;1;38;2;r;g;bm").
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(b"\x1b[0");

    if attrs.is_set(CellAttrs::BOLD) {
        out.extend_from_slice(b";1");
    }
    if attrs.is_set(CellAttrs::DIM) {
        out.extend_from_slice(b";2");
    }
    if attrs.is_set(CellAttrs::ITALIC) {
        out.extend_from_slice(b";3");
    }
    if attrs.is_set(CellAttrs::UNDERLINE) {
        out.extend_from_slice(b";4");
    }
    if attrs.is_set(CellAttrs::BLINK) {
        out.extend_from_slice(b";5");
    }
    if attrs.is_set(CellAttrs::INVERSE) {
        out.extend_from_slice(b";7");
    }
    if attrs.is_set(CellAttrs::HIDDEN) {
        out.extend_from_slice(b";8");
    }
    if attrs.is_set(CellAttrs::STRIKETHROUGH) {
        out.extend_from_slice(b";9");
    }

    match fg {
        Color::Default => {}
        Color::Ansi(n) => {
            let _ = write!(out, ";3{}", n);
        }
        Color::AnsiBright(n) => {
            let _ = write!(out, ";9{}", n);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";38;5;{}", n);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";38;2;{};{};{}", r, g, b);
        }
    }
    match bg {
        Color::Default => {}
        Color::Ansi(n) => {
            let _ = write!(out, ";4{}", n);
        }
        Color::AnsiBright(n) => {
            let _ = write!(out, ";10{}", n);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";48;5;{}", n);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";48;2;{};{};{}", r, g, b);
        }
    }

    out.push(b'm');
    out
}

fn emit_cells_as_vt_line(out: &mut Vec<u8>, cells: &[Cell], cols: usize) {
    let cols = cols.min(cells.len());
    let mut col = 0usize;
    while col < cols {
        let cell = &cells[col];

        if cell.attrs.is_wide_continuation() {
            col += 1;
            continue;
        }

        let run_fg = cell.fg;
        let run_bg = cell.bg;
        let run_attrs = cell.attrs;
        let run_start = col;
        let mut run_end = col + 1;

        while run_end < cols {
            let next = &cells[run_end];
            if next.fg == run_fg
                && next.bg == run_bg
                && next.attrs.sgr_eq(run_attrs)
                && !next.attrs.is_wide_continuation()
            {
                run_end += 1;
            } else {
                break;
            }
        }

        out.extend_from_slice(&build_sgr_params(run_fg, run_bg, run_attrs));
        for idx in run_start..run_end {
            let rc = &cells[idx];
            if !rc.attrs.is_wide_continuation() {
                out.extend_from_slice(rc.text.as_str().as_bytes());
            }
        }

        col = run_end;
    }
}

/// Extract plain text from a cell slice, skipping wide-char continuation cells.
pub fn cells_to_string(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    cells_to_string_buf(cells, &mut s);
    s
}

/// Append plain text from a cell slice into `buf`, skipping wide-char continuation.
fn cells_to_string_buf(cells: &[Cell], buf: &mut String) {
    for cell in cells {
        if !cell.attrs.is_wide_continuation() {
            buf.push_str(cell.text.as_str());
        }
    }
}

/// Compute the default capture start line based on `lines`/`all` flags.
fn default_capture_start(lines: Option<u32>, all: bool, sb_len: usize, total: usize) -> usize {
    if let Some(n) = lines {
        total.saturating_sub(n as usize)
    } else if all {
        0
    } else {
        sb_len
    }
}

// ── Cursor ───────────────────────────────────────────────────────────────────

/// Cursor shape as set by DECSCUSR (`CSI Ps SP q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Default blinking block (DECSCUSR 0 or 1).
    #[default]
    Block,
    /// Underline (DECSCUSR 3 or 4).
    Underline,
    /// Vertical bar / I-beam (DECSCUSR 5 or 6).
    Bar,
}

impl CursorShape {
    fn from_config_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "block" => Some(CursorShape::Block),
            "underline" | "under" => Some(CursorShape::Underline),
            "bar" | "beam" | "line" => Some(CursorShape::Bar),
            _ => None,
        }
    }
}

fn configured_default_cursor_shape() -> CursorShape {
    std::env::var("WTD_DEFAULT_CURSOR_SHAPE")
        .ok()
        .as_deref()
        .and_then(CursorShape::from_config_name)
        .unwrap_or(CursorShape::Block)
}

/// Cursor state.
#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
    pub shape: CursorShape,
}

impl Cursor {
    fn default_visible() -> Self {
        Cursor {
            row: 0,
            col: 0,
            visible: true,
            shape: configured_default_cursor_shape(),
        }
    }
}

/// Saved cursor (for DECSC / DECRC).
#[derive(Debug, Clone)]
struct SavedCursor {
    row: usize,
    col: usize,
    fg: Color,
    bg: Color,
    attrs: CellAttrs,
}

// ── ScreenBuffer ──────────────────────────────────────────────────────────────

/// The host-side terminal screen buffer for one session (§15).
///
/// Maintains primary screen, alternate screen, scrollback ring, cursor,
/// current SGR pen, and window title.
pub struct ScreenBuffer {
    cols: usize,
    rows: usize,

    primary: Grid,
    alternate: Grid,
    on_alternate: bool,

    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,

    cursor: Cursor,
    saved_cursor: Option<SavedCursor>,
    /// Alternate-screen saved cursor.
    alt_saved_cursor: Option<SavedCursor>,

    /// Current SGR pen: fg/bg/attrs applied to newly printed characters.
    pen_fg: Color,
    pen_bg: Color,
    pen_attrs: CellAttrs,

    /// Scroll region: top/bottom row (inclusive, 0-based).
    scroll_top: usize,
    scroll_bottom: usize,

    /// Window title from OSC sequences.
    pub title: String,

    /// Hyperlink targets referenced by visible cells (index + 1 = hyperlink_id).
    hyperlinks: Vec<String>,
    /// Active OSC 8 hyperlink identifier while printing text.
    active_hyperlink_id: Option<u16>,

    /// Inline images referenced by visible cells (index + 1 = image_id).
    inline_images: Vec<InlineImage>,

    /// Progress indicator state from OSC 9;4.
    progress: Option<TerminalProgress>,

    /// Pending notifications from OSC 9 and OSC 777.
    notifications: Vec<TerminalNotification>,

    /// Mouse tracking mode (DECSET 1000/1002/1003).
    mouse_mode: MouseMode,
    /// SGR extended mouse format (DECSET 1006).
    sgr_mouse: bool,

    /// Bracketed paste mode (DECSET 2004).
    bracketed_paste: bool,

    /// Negotiated keyboard input protocol.
    keyboard_protocol: KeyboardProtocolMode,

    /// VT parser.
    parser: vte::Parser,

    /// Pending character for wide-char continuation tracking.
    /// After printing a wide char we advance cursor by 2.
    _wide_pending: bool,
    /// Buffered print text so grapheme clusters can be committed atomically.
    pending_print: String,
}

impl ScreenBuffer {
    /// Create a new screen buffer with the given dimensions and scrollback depth.
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Self {
        let cols = cols as usize;
        let rows = rows as usize;
        ScreenBuffer {
            cols,
            rows,
            primary: Grid::new(cols, rows),
            alternate: Grid::new(cols, rows),
            on_alternate: false,
            scrollback: VecDeque::new(),
            max_scrollback,
            cursor: Cursor::default_visible(),
            saved_cursor: None,
            alt_saved_cursor: None,
            pen_fg: Color::Default,
            pen_bg: Color::Default,
            pen_attrs: CellAttrs::default(),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            title: String::new(),
            hyperlinks: Vec::new(),
            active_hyperlink_id: None,
            inline_images: Vec::new(),
            progress: None,
            notifications: Vec::new(),
            mouse_mode: MouseMode::None,
            sgr_mouse: false,
            bracketed_paste: false,
            keyboard_protocol: KeyboardProtocolMode::Legacy,
            parser: vte::Parser::new(),
            _wide_pending: false,
            pending_print: String::new(),
        }
    }

    /// Feed raw bytes from the PTY output into the screen buffer.
    pub fn advance(&mut self, bytes: &[u8]) {
        let bytes = self.preprocess_inline_graphics(bytes);
        // vte::Perform requires a mutable reference to self, but Parser::advance
        // also takes &mut self.  We swap the parser out, advance, swap back.
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        for &b in &bytes {
            parser.advance(self, b);
        }
        self.parser = parser;
        self.flush_pending_print(true);
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn on_alternate(&self) -> bool {
        self.on_alternate
    }

    /// Current progress indicator requested by the application, if any.
    pub fn progress(&self) -> Option<TerminalProgress> {
        self.progress
    }

    /// Drain pending terminal notifications detected from OSC sequences.
    pub fn drain_notifications(&mut self) -> Vec<TerminalNotification> {
        std::mem::take(&mut self.notifications)
    }

    /// Current cursor state.
    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Current mouse tracking mode.
    pub fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    /// Whether SGR extended mouse format (mode 1006) is active.
    pub fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }

    /// Whether bracketed paste mode (DECSET 2004) is active.
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Negotiated keyboard input protocol.
    pub fn keyboard_protocol(&self) -> KeyboardProtocolMode {
        self.keyboard_protocol
    }

    /// Cell at (row, col) in the visible screen (0-based).
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(self.active_grid().cell(row, col))
    }

    /// Hyperlink target at (row, col) in the visible screen, if present.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<&str> {
        let cell = self.cell(row, col)?;
        if cell.hyperlink_id == 0 {
            None
        } else {
            self.hyperlinks
                .get((cell.hyperlink_id - 1) as usize)
                .map(String::as_str)
        }
    }

    /// Inline image record at (row, col) in the visible screen, if present.
    pub fn inline_image_at(&self, row: usize, col: usize) -> Option<&InlineImage> {
        let cell = self.cell(row, col)?;
        if cell.image_id == 0 {
            None
        } else {
            self.inline_images.get((cell.image_id - 1) as usize)
        }
    }

    /// Read the visible screen as plain text (newline-separated rows).
    pub fn visible_text(&self) -> String {
        let g = self.active_grid();
        let mut out = String::with_capacity(self.rows * (self.cols + 1));
        for r in 0..self.rows {
            for c in 0..self.cols {
                let cell = g.cell(r, c);
                if !cell.attrs.is_wide_continuation() {
                    out.push_str(cell.text.as_str());
                }
            }
            out.push('\n');
        }
        out
    }

    /// Read a single row as plain text (without the trailing newline).
    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.rows {
            return None;
        }
        let g = self.active_grid();
        let mut s = String::with_capacity(self.cols);
        for c in 0..self.cols {
            let cell = g.cell(row, c);
            if !cell.attrs.is_wide_continuation() {
                s.push_str(cell.text.as_str());
            }
        }
        Some(s)
    }

    /// Number of scrollback rows currently stored.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// A row from scrollback (0 = oldest).
    pub fn scrollback_row(&self, idx: usize) -> Option<&Vec<Cell>> {
        self.scrollback.get(idx)
    }

    /// Total rows in the combined buffer (`scrollback + visible screen`).
    pub fn total_rows(&self) -> usize {
        self.scrollback.len() + self.rows
    }

    /// Clear retained scrollback history without changing visible cells.
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    /// Clear the active visible grid and retained scrollback.
    ///
    /// This leaves terminal modes, title, hyperlinks table, and other negotiated
    /// state intact; it only changes the user-visible buffer content and cursor.
    pub fn clear_buffer(&mut self) {
        self.clear_scrollback();
        self.active_grid_mut().clear_from(0, 0);
        self.cursor.row = 0;
        self.cursor.col = 0;
        self.pending_print.clear();
    }

    /// Cell at `(row, col)` in the combined buffer where rows are addressed as
    /// `scrollback[0..N]` followed by visible screen rows `0..rows`.
    pub fn cell_at_virtual(&self, row: usize, col: usize) -> Option<&Cell> {
        if col >= self.cols {
            return None;
        }
        let sb_len = self.scrollback.len();
        if row < sb_len {
            return self.scrollback.get(row).and_then(|cells| cells.get(col));
        }
        self.cell(row.saturating_sub(sb_len), col)
    }

    // ── Capture extended ─────────────────────────────────────────────────────

    /// Capture text from the combined buffer (scrollback + visible screen).
    ///
    /// The virtual line space is: `scrollback[0]` (oldest) …
    /// `scrollback[N-1]` (newest), then visible rows 0 … `rows-1`.
    /// `total_lines = scrollback.len() + self.rows`.
    ///
    /// Selection logic (in priority order):
    /// 1. **Anchor** (`after` / `after_regex`): search newest-first; on match
    ///    `start = match_index`.  If not found, fall through to 2–4.
    /// 2. **`lines`**: `start = total_lines - lines`.
    /// 3. **`all`**: `start = 0`.
    /// 4. **Default**: `start = scrollback.len()` (visible screen only).
    ///
    /// `max_lines` clamps the capture window by advancing `start` forward.
    /// `count_only` returns `""` for `text` but still computes `lines`.
    pub fn capture_extended(
        &self,
        lines: Option<u32>,
        all: bool,
        after: Option<&str>,
        after_regex: Option<&Regex>,
        max_lines: Option<u32>,
        count_only: bool,
    ) -> CaptureExtendedResult {
        let sb_len = self.scrollback.len();
        let total = sb_len + self.rows;
        let total_lines = total as u32;
        let active_grid = self.active_grid();

        let mut anchor_found: Option<bool> = None;

        // ── Anchor search (newest-first) ──────────────────────────────────
        let anchor_start: Option<usize> = if after.is_some() || after_regex.is_some() {
            let mut found_at: Option<usize> = None;
            let mut line_buf = String::with_capacity(self.cols);
            for i in (0..total).rev() {
                line_buf.clear();
                if i < sb_len {
                    cells_to_string_buf(&self.scrollback[i], &mut line_buf);
                } else {
                    cells_to_string_buf(active_grid.row_slice(i - sb_len), &mut line_buf);
                }

                let matched = if let Some(pattern) = after {
                    line_buf.contains(pattern)
                } else if let Some(re) = after_regex {
                    re.is_match(&line_buf)
                } else {
                    false
                };

                if matched {
                    found_at = Some(i);
                    break;
                }
            }
            anchor_found = Some(found_at.is_some());
            found_at
        } else {
            None
        };

        let mut start =
            anchor_start.unwrap_or_else(|| default_capture_start(lines, all, sb_len, total));

        // ── Apply max_lines cap ───────────────────────────────────────────
        if let Some(max) = max_lines {
            let max = max as usize;
            if total - start > max {
                start = total - max;
            }
        }

        let cursor = start as u32;
        let line_count = (total - start) as u32;

        // ── Build text ────────────────────────────────────────────────────
        let text = if count_only {
            String::new()
        } else {
            let mut out = String::new();
            for i in start..total {
                if i < sb_len {
                    let raw = cells_to_string(&self.scrollback[i]);
                    out.push_str(raw.trim_end_matches(' '));
                    out.push('\n');
                } else {
                    let row = i - sb_len;
                    let raw = cells_to_string(active_grid.row_slice(row));
                    out.push_str(&raw);
                    out.push('\n');
                }
            }
            out
        };

        CaptureExtendedResult {
            text,
            lines: line_count,
            total_lines,
            anchor_found,
            cursor,
        }
    }

    // ── VT snapshot ─────────────────────────────────────────────────────────

    /// Serialize the current visible screen as a self-contained VT byte stream.
    ///
    /// The returned bytes can be fed directly into a fresh `ScreenBuffer::advance()`
    /// to reconstruct the visible state (text, colors, attributes) on another end —
    /// for example, seeding a UI-side screen buffer when a UI attaches to a running
    /// workspace.
    ///
    /// The stream uses `CSI 2 J` to clear, then emits SGR + character sequences
    /// row by row, finishing with a cursor-position command at the current cursor.
    pub fn to_vt_snapshot(&self) -> Vec<u8> {
        let mut out = Vec::new();

        if !self.title.is_empty() {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(self.title.as_bytes());
            out.push(0x07);
        }

        if let Some(progress) = self.progress {
            match progress {
                TerminalProgress::Normal(value) => {
                    out.extend_from_slice(format!("\x1b]9;4;1;{value}\x07").as_bytes());
                }
                TerminalProgress::Error(value) => {
                    out.extend_from_slice(format!("\x1b]9;4;2;{value}\x07").as_bytes());
                }
                TerminalProgress::Indeterminate => out.extend_from_slice(b"\x1b]9;4;3\x07"),
                TerminalProgress::Warning(value) => {
                    out.extend_from_slice(format!("\x1b]9;4;4;{value}\x07").as_bytes());
                }
            }
        }

        if self.on_alternate {
            out.extend_from_slice(b"\x1b[?1049h");
        }

        // Clear screen and move to top-left.
        out.extend_from_slice(b"\x1b[2J\x1b[H");

        let g = self.active_grid();

        for row in 0..self.rows {
            if row > 0 {
                // Position cursor at start of this row (rows are 1-based in VT).
                out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
            }
            emit_cells_as_vt_line(&mut out, g.row_slice(row), self.cols);
        }

        // Reset SGR so subsequent output starts clean.
        out.extend_from_slice(b"\x1b[0m");

        // Restore cursor position.
        out.extend_from_slice(
            format!("\x1b[{};{}H", self.cursor.row + 1, self.cursor.col + 1).as_bytes(),
        );

        let cursor_shape = match self.cursor.shape {
            CursorShape::Block => 1,
            CursorShape::Underline => 3,
            CursorShape::Bar => 5,
        };
        out.extend_from_slice(format!("\x1b[{} q", cursor_shape).as_bytes());

        if self.cursor.visible {
            out.extend_from_slice(b"\x1b[?25h");
        } else {
            out.extend_from_slice(b"\x1b[?25l");
        }

        match self.mouse_mode {
            MouseMode::None => {}
            MouseMode::Normal => out.extend_from_slice(b"\x1b[?1000h"),
            MouseMode::ButtonEvent => out.extend_from_slice(b"\x1b[?1002h"),
            MouseMode::AnyEvent => out.extend_from_slice(b"\x1b[?1003h"),
        }

        if self.sgr_mouse {
            out.extend_from_slice(b"\x1b[?1006h");
        }

        if self.bracketed_paste {
            out.extend_from_slice(b"\x1b[?2004h");
        }

        out
    }

    /// Serialize retained scrollback rows as a VT replay stream.
    ///
    /// Replaying these bytes into a fresh `ScreenBuffer` rebuilds local
    /// scrollback history. Consumers should then apply `to_vt_snapshot()` to
    /// restore the exact current viewport, cursor, title, and mode state.
    pub fn to_vt_scrollback(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for row in &self.scrollback {
            emit_cells_as_vt_line(&mut out, row, self.cols);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\x1b[0m");
        out
    }

    // ── Resize ───────────────────────────────────────────────────────────────

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols as usize;
        let rows = rows as usize;
        self.primary.resize(cols, rows);
        self.alternate.resize(cols, rows);
        self.cols = cols;
        self.rows = rows;
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn active_grid(&self) -> &Grid {
        if self.on_alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_grid_mut(&mut self) -> &mut Grid {
        if self.on_alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    /// Write a grapheme at cursor position and advance cursor.
    fn print_grapheme(&mut self, grapheme: &str) {
        let char_width = grapheme_width(grapheme);
        if char_width == 0 {
            return;
        }
        let is_wide = char_width == 2;

        // If we'd overflow column, wrap.
        if self.cursor.col >= self.cols {
            self.cursor.col = 0;
            self.cursor.row += 1;
        }

        // If we'd overflow rows, scroll.
        if self.cursor.row > self.scroll_bottom {
            let top = self.scroll_top;
            let bot = self.scroll_bottom;
            let max_sb = self.max_scrollback;
            let on_alt = self.on_alternate;
            // Only add to scrollback from primary screen's top margin.
            if top == 0 && !on_alt {
                let grid = if on_alt {
                    &mut self.alternate
                } else {
                    &mut self.primary
                };
                grid.scroll_region_up(top, bot, 1, &mut self.scrollback, max_sb);
            } else {
                let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                let grid = if on_alt {
                    &mut self.alternate
                } else {
                    &mut self.primary
                };
                grid.scroll_region_up(top, bot, 1, &mut dummy, 0);
            }
            self.cursor.row = self.scroll_bottom;
        }

        let row = self.cursor.row;
        let col = self.cursor.col;
        let fg = self.pen_fg;
        let bg = self.pen_bg;
        let attrs = self.pen_attrs;
        let hyperlink_id = self.active_hyperlink_id.unwrap_or(0);

        {
            let grid = self.active_grid_mut();
            let cell = grid.cell_mut(row, col);
            cell.text = CompactText::new(grapheme);
            cell.fg = fg;
            cell.bg = bg;
            cell.attrs = attrs;
            cell.hyperlink_id = hyperlink_id;
            if is_wide {
                cell.attrs.set_wide();
            } else {
                cell.attrs.clear_wide();
            }
            cell.attrs.clear_wide_continuation();
        }

        if is_wide {
            // Fill continuation if within bounds.
            if col + 1 < self.cols {
                let grid = self.active_grid_mut();
                let cont = grid.cell_mut(row, col + 1);
                *cont = Cell::blank();
                cont.attrs.set_wide_continuation();
            }
            self.cursor.col += 2;
        } else {
            self.cursor.col += 1;
        }
    }

    fn preprocess_inline_graphics(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if i + 3 < bytes.len()
                && bytes[i] == 0x1B
                && bytes[i + 1] == b'_'
                && bytes[i + 2] == b'G'
            {
                let start = i + 3;
                if let Some(relative_end) = bytes[start..]
                    .windows(2)
                    .position(|window| window == [0x1B, b'\\'])
                {
                    let body = &bytes[start..start + relative_end];
                    self.handle_kitty_graphics(body);
                    i = start + relative_end + 2;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    fn handle_kitty_graphics(&mut self, body: &[u8]) {
        let Ok(body_str) = std::str::from_utf8(body) else {
            return;
        };
        let mut parts = body_str.splitn(2, ';');
        let params = parts.next().unwrap_or("");
        let payload = parts.next().unwrap_or("");

        let image = InlineImage {
            protocol: "kitty".to_string(),
            params: params.to_string(),
            payload: payload.to_string(),
        };
        self.inline_images.push(image);
        let image_id = self.inline_images.len() as u16;

        let row = self.cursor.row.min(self.rows.saturating_sub(1));
        let col = self.cursor.col.min(self.cols.saturating_sub(1));
        let fg = self.pen_fg;
        let bg = self.pen_bg;
        let attrs = self.pen_attrs;
        let cell = self.active_grid_mut().cell_mut(row, col);
        cell.text = CompactText::new("▣");
        cell.fg = fg;
        cell.bg = bg;
        cell.attrs = attrs;
        cell.hyperlink_id = 0;
        cell.image_id = image_id;
        self.cursor.col = (col + 1).min(self.cols.saturating_sub(1));
    }

    fn flush_pending_print(&mut self, final_flush: bool) {
        if self.pending_print.is_empty() {
            return;
        }

        // Take ownership to avoid double-borrowing self.
        let taken = std::mem::take(&mut self.pending_print);
        let graphemes: Vec<&str> = taken.graphemes(true).collect();
        if graphemes.is_empty() {
            return;
        }

        let retain = if final_flush { 0 } else { 1 };
        if graphemes.len() <= retain {
            // Nothing to flush; put it back.
            self.pending_print = taken;
            return;
        }

        let flush_count = graphemes.len() - retain;
        for &g in &graphemes[..flush_count] {
            self.print_grapheme(g);
        }

        // Only allocate the remainder if we retained a trailing grapheme.
        if retain > 0 {
            self.pending_print = graphemes[flush_count..].concat();
        }
        // else: pending_print stays empty (from mem::take)
    }

    /// Apply SGR (Select Graphic Rendition) parameters.
    fn apply_sgr(&mut self, params: &Params) {
        // Borrow sub-param slices directly from Params instead of copying.
        let flat: Vec<&[u16]> = params.iter().collect();
        // If params is empty, reset.
        if flat.is_empty() {
            self.reset_sgr();
            return;
        }

        let mut i = 0;
        while i < flat.len() {
            let top = flat[i][0];
            match top {
                0 => self.reset_sgr(),
                1 => self.pen_attrs.set(CellAttrs::BOLD),
                2 => self.pen_attrs.set(CellAttrs::DIM),
                3 => self.pen_attrs.set(CellAttrs::ITALIC),
                4 => self.pen_attrs.set(CellAttrs::UNDERLINE),
                5 | 6 => self.pen_attrs.set(CellAttrs::BLINK),
                7 => self.pen_attrs.set(CellAttrs::INVERSE),
                8 => self.pen_attrs.set(CellAttrs::HIDDEN),
                9 => self.pen_attrs.set(CellAttrs::STRIKETHROUGH),
                21 => self.pen_attrs.clear(CellAttrs::BOLD),
                22 => {
                    self.pen_attrs.clear(CellAttrs::BOLD);
                    self.pen_attrs.clear(CellAttrs::DIM);
                }
                23 => self.pen_attrs.clear(CellAttrs::ITALIC),
                24 => self.pen_attrs.clear(CellAttrs::UNDERLINE),
                25 => self.pen_attrs.clear(CellAttrs::BLINK),
                27 => self.pen_attrs.clear(CellAttrs::INVERSE),
                28 => self.pen_attrs.clear(CellAttrs::HIDDEN),
                29 => self.pen_attrs.clear(CellAttrs::STRIKETHROUGH),
                // ANSI fg (30–37)
                30..=37 => self.pen_fg = Color::Ansi(top as u8 - 30),
                38 => {
                    // Extended fg color: 38;5;n (256) or 38;2;r;g;b (truecolor)
                    // vte passes sub-params with ':' separator as sub-params of param 38.
                    // Also handles ';' separated: next params are 5;n or 2;r;g;b.
                    let subs = &flat[i];
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.pen_fg = Color::Indexed(subs[2] as u8);
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        self.pen_fg = Color::Rgb(subs[2] as u8, subs[3] as u8, subs[4] as u8);
                    } else if i + 1 < flat.len() && flat[i + 1][0] == 5 {
                        let n = if i + 2 < flat.len() {
                            flat[i + 2][0] as u8
                        } else {
                            0
                        };
                        self.pen_fg = Color::Indexed(n);
                        i += 2;
                    } else if i + 1 < flat.len() && flat[i + 1][0] == 2 {
                        let r = if i + 2 < flat.len() {
                            flat[i + 2][0] as u8
                        } else {
                            0
                        };
                        let g = if i + 3 < flat.len() {
                            flat[i + 3][0] as u8
                        } else {
                            0
                        };
                        let b = if i + 4 < flat.len() {
                            flat[i + 4][0] as u8
                        } else {
                            0
                        };
                        self.pen_fg = Color::Rgb(r, g, b);
                        i += 4;
                    }
                }
                39 => self.pen_fg = Color::Default,
                // ANSI bg (40–47)
                40..=47 => self.pen_bg = Color::Ansi(top as u8 - 40),
                48 => {
                    let subs = &flat[i];
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.pen_bg = Color::Indexed(subs[2] as u8);
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        self.pen_bg = Color::Rgb(subs[2] as u8, subs[3] as u8, subs[4] as u8);
                    } else if i + 1 < flat.len() && flat[i + 1][0] == 5 {
                        let n = if i + 2 < flat.len() {
                            flat[i + 2][0] as u8
                        } else {
                            0
                        };
                        self.pen_bg = Color::Indexed(n);
                        i += 2;
                    } else if i + 1 < flat.len() && flat[i + 1][0] == 2 {
                        let r = if i + 2 < flat.len() {
                            flat[i + 2][0] as u8
                        } else {
                            0
                        };
                        let g = if i + 3 < flat.len() {
                            flat[i + 3][0] as u8
                        } else {
                            0
                        };
                        let b = if i + 4 < flat.len() {
                            flat[i + 4][0] as u8
                        } else {
                            0
                        };
                        self.pen_bg = Color::Rgb(r, g, b);
                        i += 4;
                    }
                }
                49 => self.pen_bg = Color::Default,
                // Bright fg (90–97)
                90..=97 => self.pen_fg = Color::AnsiBright(top as u8 - 90),
                // Bright bg (100–107)
                100..=107 => self.pen_bg = Color::AnsiBright(top as u8 - 100),
                _ => {}
            }
            i += 1;
        }
    }

    fn reset_sgr(&mut self) {
        self.pen_fg = Color::Default;
        self.pen_bg = Color::Default;
        self.pen_attrs = CellAttrs::default();
    }

    fn save_cursor(&mut self) {
        let s = SavedCursor {
            row: self.cursor.row,
            col: self.cursor.col,
            fg: self.pen_fg,
            bg: self.pen_bg,
            attrs: self.pen_attrs,
        };
        if self.on_alternate {
            self.alt_saved_cursor = Some(s);
        } else {
            self.saved_cursor = Some(s);
        }
    }

    fn restore_cursor(&mut self) {
        let saved = if self.on_alternate {
            self.alt_saved_cursor.take()
        } else {
            self.saved_cursor.take()
        };
        if let Some(s) = saved {
            self.cursor.row = s.row.min(self.rows.saturating_sub(1));
            self.cursor.col = s.col.min(self.cols.saturating_sub(1));
            self.pen_fg = s.fg;
            self.pen_bg = s.bg;
            self.pen_attrs = s.attrs;
        }
    }

    fn enter_alternate_screen(&mut self) {
        if !self.on_alternate {
            self.on_alternate = true;
            // Clear alternate screen and reset cursor.
            self.alternate = Grid::new(self.cols, self.rows);
            self.cursor = Cursor::default_visible();
        }
    }

    fn leave_alternate_screen(&mut self) {
        if self.on_alternate {
            self.on_alternate = false;
            // Primary screen and its cursor are automatically restored by virtue
            // of self.cursor being the primary cursor (we saved/restored on enter).
        }
    }
}

// ── unicode width helper ──────────────────────────────────────────────────────

fn is_zero_width_codepoint(c: char) -> bool {
    if c.is_ascii() {
        return !matches!(c, '\t' | '\n' | '\r' | ' '..='~');
    }

    let u = c as u32;
    matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
        || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
        || matches!(u, 0xFE20..=0xFE2F)
        || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
        || matches!(
            u,
            0x00AD | 0x034F | 0x180E | 0x200B | 0x200C | 0x200D | 0x200E | 0x200F | 0x2060 | 0xFEFF
        )
        || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
}

fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_ascii() {
        return grapheme
            .bytes()
            .map(|b| match b {
                b'\t' | b'\n' | b'\r' => 1,
                0x20..=0x7E => 1,
                _ => 0,
            })
            .sum();
    }

    if grapheme.chars().all(is_zero_width_codepoint) {
        return 0;
    }

    if grapheme.contains('\u{FE0F}') {
        // Build a stack buffer with FE0F stripped, avoiding a heap allocation.
        let mut buf = [0u8; 32];
        let mut len = 0usize;
        for ch in grapheme.chars() {
            if ch == '\u{FE0F}' {
                continue;
            }
            let encoded = ch.encode_utf8(&mut buf[len..]);
            len += encoded.len();
            if len >= buf.len() {
                break;
            }
        }
        if len == 0 {
            return 0;
        }
        let stripped = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
        return unicode_display_width(stripped) as usize;
    }

    unicode_display_width(grapheme) as usize
}

fn progress_with_value(
    raw: Option<&&[u8]>,
    ctor: impl FnOnce(u8) -> TerminalProgress,
) -> Option<TerminalProgress> {
    let raw = raw?;
    let text = std::str::from_utf8(raw).ok()?;
    let value = text.parse::<u16>().ok()?.min(100) as u8;
    Some(ctor(value))
}

// ── Perform impl ──────────────────────────────────────────────────────────────

impl Perform for ScreenBuffer {
    fn print(&mut self, c: char) {
        self.pending_print.push(c);
        self.flush_pending_print(false);
    }

    fn execute(&mut self, byte: u8) {
        self.flush_pending_print(true);
        match byte {
            // BEL — ignore
            0x07 => {}
            // BS
            0x08 => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            // HT (tab)
            0x09 => {
                let next_tab = ((self.cursor.col / 8) + 1) * 8;
                self.cursor.col = next_tab.min(self.cols.saturating_sub(1));
            }
            // LF / VT / FF
            0x0A | 0x0B | 0x0C => {
                if self.cursor.row == self.scroll_bottom {
                    let top = self.scroll_top;
                    let bot = self.scroll_bottom;
                    let max_sb = self.max_scrollback;
                    let on_alt = self.on_alternate;
                    if top == 0 && !on_alt {
                        let grid = if on_alt {
                            &mut self.alternate
                        } else {
                            &mut self.primary
                        };
                        grid.scroll_region_up(top, bot, 1, &mut self.scrollback, max_sb);
                    } else {
                        let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                        let grid = if on_alt {
                            &mut self.alternate
                        } else {
                            &mut self.primary
                        };
                        grid.scroll_region_up(top, bot, 1, &mut dummy, 0);
                    }
                } else {
                    self.cursor.row += 1;
                    if self.cursor.row >= self.rows {
                        self.cursor.row = self.rows.saturating_sub(1);
                    }
                }
            }
            // CR
            0x0D => {
                self.cursor.col = 0;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.flush_pending_print(true);
        // Collect flat params as Vec<u16> for easy indexed access.
        let p: Vec<u16> = params.iter().map(|sub| sub[0]).collect();
        let p1 = |i: usize, def: u16| -> usize { *p.get(i).unwrap_or(&def).max(&1) as usize };
        let p0 = |i: usize, def: u16| -> usize { *p.get(i).unwrap_or(&def) as usize };

        match (intermediates, action) {
            // CUU — cursor up
            ([], 'A') => {
                let n = p1(0, 1);
                self.cursor.row = self.cursor.row.saturating_sub(n).max(self.scroll_top);
            }
            // CUD — cursor down
            ([], 'B') => {
                let n = p1(0, 1);
                self.cursor.row = (self.cursor.row + n).min(self.scroll_bottom);
            }
            // CUF — cursor forward
            ([], 'C') => {
                let n = p1(0, 1);
                self.cursor.col = (self.cursor.col + n).min(self.cols.saturating_sub(1));
            }
            // CUB — cursor back
            ([], 'D') => {
                let n = p1(0, 1);
                self.cursor.col = self.cursor.col.saturating_sub(n);
            }
            // CNL — cursor next line
            ([], 'E') => {
                let n = p1(0, 1);
                self.cursor.row = (self.cursor.row + n).min(self.rows.saturating_sub(1));
                self.cursor.col = 0;
            }
            // CPL — cursor previous line
            ([], 'F') => {
                let n = p1(0, 1);
                self.cursor.row = self.cursor.row.saturating_sub(n);
                self.cursor.col = 0;
            }
            // CHA — cursor horizontal absolute
            ([], 'G') => {
                let col = p1(0, 1) - 1;
                self.cursor.col = col.min(self.cols.saturating_sub(1));
            }
            // CUP — cursor position (row, col), 1-based
            ([], 'H') | ([], 'f') => {
                let row = p1(0, 1) - 1;
                let col = p1(1, 1) - 1;
                self.cursor.row = row.min(self.rows.saturating_sub(1));
                self.cursor.col = col.min(self.cols.saturating_sub(1));
            }
            // ED — erase in display
            ([], 'J') => {
                let row = self.cursor.row;
                let col = self.cursor.col;
                match p0(0, 0) {
                    0 => self.active_grid_mut().clear_from(row, col),
                    1 => self.active_grid_mut().clear_to(row, col),
                    2 | 3 => {
                        let cols = self.cols;
                        let rows = self.rows;
                        let g = self.active_grid_mut();
                        g.clear_from(0, 0);
                        let _ = (cols, rows); // already clears all
                    }
                    _ => {}
                }
            }
            // EL — erase in line
            ([], 'K') => {
                let row = self.cursor.row;
                let col = self.cursor.col;
                match p0(0, 0) {
                    0 => self.active_grid_mut().clear_row_from(row, col),
                    1 => self.active_grid_mut().clear_row_to(row, col),
                    2 => self.active_grid_mut().clear_row(row),
                    _ => {}
                }
            }
            // IL — insert lines
            ([], 'L') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let bot = self.scroll_bottom;
                let cols = self.cols;
                let grid = self.active_grid_mut();
                grid.scroll_region_down(row, bot, n);
                for r in row..row + n {
                    if r > bot {
                        break;
                    }
                    for c in 0..cols {
                        *grid.cell_mut(r, c) = Cell::blank();
                    }
                }
            }
            // DL — delete lines
            ([], 'M') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let bot = self.scroll_bottom;
                let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                self.active_grid_mut()
                    .scroll_region_up(row, bot, n, &mut dummy, 0);
            }
            // DCH — delete characters
            ([], 'P') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let col = self.cursor.col;
                let end = self.cols;
                let g = self.active_grid_mut();
                let row_start = row * g.cols;
                if col + n < end {
                    g.cells
                        .copy_within(row_start + col + n..row_start + end, row_start + col);
                }
                let blank_start = row_start + end.saturating_sub(n).max(col);
                g.cells[blank_start..row_start + end].fill(Cell::blank());
            }
            // SU — scroll up
            ([], 'S') => {
                let n = p1(0, 1);
                let top = self.scroll_top;
                let bot = self.scroll_bottom;
                let max_sb = self.max_scrollback;
                let on_alt = self.on_alternate;
                if top == 0 && !on_alt {
                    let grid = if on_alt {
                        &mut self.alternate
                    } else {
                        &mut self.primary
                    };
                    grid.scroll_region_up(top, bot, n, &mut self.scrollback, max_sb);
                } else {
                    let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                    let grid = if on_alt {
                        &mut self.alternate
                    } else {
                        &mut self.primary
                    };
                    grid.scroll_region_up(top, bot, n, &mut dummy, 0);
                }
            }
            // SD — scroll down
            ([], 'T') => {
                let n = p1(0, 1);
                let top = self.scroll_top;
                let bot = self.scroll_bottom;
                self.active_grid_mut().scroll_region_down(top, bot, n);
            }
            // ECH — erase characters
            ([], 'X') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let col = self.cursor.col;
                let end = (col + n).min(self.cols);
                let g = self.active_grid_mut();
                for c in col..end {
                    *g.cell_mut(row, c) = Cell::blank();
                }
            }
            // VPA — vertical position absolute
            ([], 'd') => {
                let row = p1(0, 1) - 1;
                self.cursor.row = row.min(self.rows.saturating_sub(1));
            }
            // SGR — select graphic rendition
            ([], 'm') => {
                self.apply_sgr(params);
            }
            // DSR — device status report (cursor position query): ignore
            ([], 'n') => {}
            // DECSTBM — set scrolling region
            ([], 'r') => {
                let top = p1(0, 1) - 1;
                let bot = p1(1, self.rows as u16) - 1;
                if top < bot && bot < self.rows {
                    self.scroll_top = top;
                    self.scroll_bottom = bot;
                    self.cursor.row = 0;
                    self.cursor.col = 0;
                }
            }
            // DECSC — save cursor
            ([b'7'], 's') | ([], 's') => {
                self.save_cursor();
            }
            // DECRC — restore cursor
            ([b'8'], 'u') | ([], 'u') => {
                self.restore_cursor();
            }
            // ICH — insert character
            ([], '@') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let col = self.cursor.col;
                let end = self.cols;
                let g = self.active_grid_mut();
                let row_start = row * g.cols;
                if col + n < end {
                    g.cells
                        .copy_within(row_start + col..row_start + end - n, row_start + col + n);
                }
                let blank_end = (row_start + col + n).min(row_start + end);
                g.cells[row_start + col..blank_end].fill(Cell::blank());
            }
            // DECSCUSR — set cursor style (CSI Ps SP q)
            ([b' '], 'q') => {
                self.cursor.shape = match p0(0, 0) {
                    0 | 1 | 2 => CursorShape::Block,
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            // Keyboard protocol negotiation
            ([b'>'], 'u') => {
                self.keyboard_protocol = match p0(0, 0) {
                    1 => KeyboardProtocolMode::CsiU,
                    31 => KeyboardProtocolMode::Kitty,
                    _ => KeyboardProtocolMode::Legacy,
                };
            }
            // DEC private modes
            ([b'?'], 'h') => {
                for sub in params.iter() {
                    match sub[0] {
                        25 => self.cursor.visible = true,
                        1000 => self.mouse_mode = MouseMode::Normal,
                        1002 => self.mouse_mode = MouseMode::ButtonEvent,
                        1003 => self.mouse_mode = MouseMode::AnyEvent,
                        1006 => self.sgr_mouse = true,
                        2004 => self.bracketed_paste = true,
                        1049 => {
                            // Save primary cursor, enter alternate screen
                            self.save_cursor();
                            self.enter_alternate_screen();
                        }
                        _ => {}
                    }
                }
            }
            ([b'?'], 'l') => {
                for sub in params.iter() {
                    match sub[0] {
                        25 => self.cursor.visible = false,
                        1000 | 1002 | 1003 => self.mouse_mode = MouseMode::None,
                        1006 => self.sgr_mouse = false,
                        2004 => self.bracketed_paste = false,
                        1049 => {
                            // Leave alternate screen, restore primary cursor
                            self.leave_alternate_screen();
                            self.restore_cursor();
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.flush_pending_print(true);
        match (intermediates, byte) {
            // DECSC — save cursor
            ([], b'7') => self.save_cursor(),
            // DECRC — restore cursor
            ([], b'8') => self.restore_cursor(),
            // RIS — reset to initial state
            ([], b'c') => {
                self.primary = Grid::new(self.cols, self.rows);
                self.alternate = Grid::new(self.cols, self.rows);
                self.on_alternate = false;
                self.scrollback.clear();
                self.cursor = Cursor::default_visible();
                self.saved_cursor = None;
                self.alt_saved_cursor = None;
                self.reset_sgr();
                self.scroll_top = 0;
                self.scroll_bottom = self.rows.saturating_sub(1);
                self.title.clear();
                self.hyperlinks.clear();
                self.active_hyperlink_id = None;
                self.inline_images.clear();
                self.progress = None;
                self.mouse_mode = MouseMode::None;
                self.sgr_mouse = false;
                self.bracketed_paste = false;
                self.keyboard_protocol = KeyboardProtocolMode::Legacy;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.flush_pending_print(true);
        if let Some(&code_bytes) = params.first() {
            let code_str = std::str::from_utf8(code_bytes).unwrap_or("");
            if code_str == "8" && params.len() >= 3 {
                let uri = std::str::from_utf8(params[2]).unwrap_or("");
                if uri.is_empty() {
                    self.active_hyperlink_id = None;
                } else {
                    let hyperlink_id = if let Some((index, _)) = self
                        .hyperlinks
                        .iter()
                        .enumerate()
                        .find(|(_, existing)| existing.as_str() == uri)
                    {
                        (index + 1) as u16
                    } else {
                        self.hyperlinks.push(uri.to_owned());
                        self.hyperlinks.len() as u16
                    };
                    self.active_hyperlink_id = Some(hyperlink_id);
                }
                return;
            }
            if (code_str == "0" || code_str == "2") && params.len() >= 2 {
                if let Ok(title) = std::str::from_utf8(params[1]) {
                    self.title = title.to_owned();
                }
                return;
            }
            if code_str == "9"
                && params.len() >= 3
                && std::str::from_utf8(params[1]).unwrap_or("") == "4"
            {
                let next_progress = match std::str::from_utf8(params[2]).unwrap_or("") {
                    "0" => None,
                    "1" => progress_with_value(params.get(3), TerminalProgress::Normal),
                    "2" => progress_with_value(params.get(3), TerminalProgress::Error),
                    "3" => Some(TerminalProgress::Indeterminate),
                    "4" => progress_with_value(params.get(3), TerminalProgress::Warning),
                    _ => self.progress,
                };
                self.progress = next_progress;
                return;
            }
            if code_str == "9" && params.len() >= 2 {
                if let Ok(message) = std::str::from_utf8(params[1]) {
                    let message = message.trim();
                    if !message.is_empty() {
                        self.notifications.push(TerminalNotification {
                            message: message.to_string(),
                        });
                    }
                }
                return;
            }
            if code_str == "777"
                && params.len() >= 2
                && std::str::from_utf8(params[1]).unwrap_or("") == "notify"
            {
                let message = match (params.get(2), params.get(3)) {
                    (Some(title), Some(body)) => {
                        let title = std::str::from_utf8(title).unwrap_or("").trim();
                        let body = std::str::from_utf8(body).unwrap_or("").trim();
                        match (title.is_empty(), body.is_empty()) {
                            (true, true) => String::new(),
                            (true, false) => body.to_string(),
                            (false, true) => title.to_string(),
                            (false, false) => format!("{title}: {body}"),
                        }
                    }
                    (Some(message), None) => std::str::from_utf8(message)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    _ => String::new(),
                };
                if !message.is_empty() {
                    self.notifications.push(TerminalNotification { message });
                }
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        self.flush_pending_print(true);
    }
    fn put(&mut self, _byte: u8) {
        self.flush_pending_print(true);
    }
    fn unhook(&mut self) {
        self.flush_pending_print(true);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(cols: u16, rows: u16) -> ScreenBuffer {
        ScreenBuffer::new(cols, rows, 1000)
    }

    fn feed(b: &mut ScreenBuffer, s: &str) {
        b.advance(s.as_bytes());
    }

    // ── Basic text printing ───────────────────────────────────────────────────

    #[test]
    fn basic_text_at_origin() {
        let mut b = buf(80, 24);
        feed(&mut b, "Hello");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'H');
        assert_eq!(b.cell(0, 4).unwrap().first_char(), 'o');
        assert_eq!(b.cursor().col, 5);
        assert_eq!(b.cursor().row, 0);
    }

    #[test]
    fn newline_advances_row() {
        let mut b = buf(80, 24);
        feed(&mut b, "line1\r\nline2");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'l');
        assert_eq!(b.cell(1, 0).unwrap().first_char(), 'l');
        assert_eq!(b.cell(1, 4).unwrap().first_char(), '2');
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    #[test]
    fn cursor_position_cup() {
        let mut b = buf(80, 24);
        // CUP \e[5;10H → row 4, col 9 (1-based → 0-based)
        feed(&mut b, "\x1b[5;10H");
        assert_eq!(b.cursor().row, 4);
        assert_eq!(b.cursor().col, 9);
    }

    #[test]
    fn cursor_up_down_left_right() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[5;5H"); // row=4, col=4
        feed(&mut b, "\x1b[2A"); // up 2 → row=2
        assert_eq!(b.cursor().row, 2);
        feed(&mut b, "\x1b[3B"); // down 3 → row=5
        assert_eq!(b.cursor().row, 5);
        feed(&mut b, "\x1b[2C"); // right 2 → col=6
        assert_eq!(b.cursor().col, 6);
        feed(&mut b, "\x1b[1D"); // left 1 → col=5
        assert_eq!(b.cursor().col, 5);
    }

    // ── SGR: ANSI 8 colors ────────────────────────────────────────────────────

    #[test]
    fn sgr_ansi_fg_bg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[31;42mX"); // fg=red(1), bg=green(2)
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.first_char(), 'X');
        assert_eq!(c.fg, Color::Ansi(1));
        assert_eq!(c.bg, Color::Ansi(2));
    }

    #[test]
    fn sgr_reset() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[31mA\x1b[0mB");
        assert_eq!(b.cell(0, 0).unwrap().fg, Color::Ansi(1));
        assert_eq!(b.cell(0, 1).unwrap().fg, Color::Default);
    }

    // ── SGR: 256-color ────────────────────────────────────────────────────────

    #[test]
    fn sgr_256_color_fg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[38;5;200mX");
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.fg, Color::Indexed(200));
    }

    #[test]
    fn sgr_256_color_bg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[48;5;100mX");
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.bg, Color::Indexed(100));
    }

    // ── SGR: truecolor ────────────────────────────────────────────────────────

    #[test]
    fn sgr_truecolor_fg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[38;2;10;20;30mX");
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn sgr_truecolor_bg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[48;2;100;150;200mX");
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.bg, Color::Rgb(100, 150, 200));
    }

    // ── SGR: attributes ───────────────────────────────────────────────────────

    #[test]
    fn sgr_bold_italic_underline() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[1;3;4mX");
        let c = b.cell(0, 0).unwrap();
        assert!(c.attrs.is_set(CellAttrs::BOLD));
        assert!(c.attrs.is_set(CellAttrs::ITALIC));
        assert!(c.attrs.is_set(CellAttrs::UNDERLINE));
        assert!(!c.attrs.is_set(CellAttrs::DIM));
    }

    #[test]
    fn sgr_bright_colors() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[91;101mX"); // bright fg red, bright bg red
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.fg, Color::AnsiBright(1));
        assert_eq!(c.bg, Color::AnsiBright(1));
    }

    // ── Alternate screen ──────────────────────────────────────────────────────

    #[test]
    fn alternate_screen_enter_exit() {
        let mut b = buf(80, 24);
        feed(&mut b, "primary");
        // Enter alternate screen
        feed(&mut b, "\x1b[?1049h");
        assert!(b.on_alternate());
        // Alternate screen should be blank
        assert_eq!(b.cell(0, 0).unwrap().first_char(), ' ');
        feed(&mut b, "alternate");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'a');
        // Exit alternate screen
        feed(&mut b, "\x1b[?1049l");
        assert!(!b.on_alternate());
        // Primary screen content restored
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'p');
    }

    #[test]
    fn alternate_screen_does_not_pollute_scrollback() {
        let mut b = buf(80, 5);
        // Fill primary with lines to generate scrollback
        for i in 0..10u8 {
            feed(&mut b, &format!("line {}\r\n", i));
        }
        let sb_before = b.scrollback_len();
        // Enter alternate, scroll a lot, exit
        feed(&mut b, "\x1b[?1049h");
        for _ in 0..20 {
            feed(&mut b, "alt line\r\n");
        }
        feed(&mut b, "\x1b[?1049l");
        // Scrollback should not have grown from alternate screen activity
        assert_eq!(b.scrollback_len(), sb_before);
    }

    #[test]
    fn capture_extended_visible_uses_alternate_screen() {
        let mut b = buf(40, 5);
        feed(&mut b, "primary-visible");
        feed(&mut b, "\x1b[?1049h");
        feed(&mut b, "alternate-visible");

        let capture = b.capture_extended(None, false, None, None, None, false);

        assert!(
            capture.text.contains("alternate-visible"),
            "capture should include alternate screen text; got:\n{}",
            capture.text
        );
        assert!(
            !capture.text.contains("primary-visible"),
            "visible-only capture should not include primary screen text while on alternate"
        );
    }

    #[test]
    fn capture_extended_all_uses_alternate_visible_rows() {
        let mut b = buf(40, 5);
        feed(&mut b, "primary-visible");
        feed(&mut b, "\x1b[?1049h");
        feed(&mut b, "alternate-visible");

        let capture = b.capture_extended(None, true, None, None, None, false);

        assert!(
            capture.text.contains("alternate-visible"),
            "all capture should include alternate screen text; got:\n{}",
            capture.text
        );
    }

    // ── Scrollback ────────────────────────────────────────────────────────────

    #[test]
    fn scrollback_fills_on_overflow() {
        let mut b = ScreenBuffer::new(80, 5, 100);
        // Write 10 lines into a 5-row screen → 5 scroll off
        for i in 0..10usize {
            feed(&mut b, &format!("line{:04}\r\n", i));
        }
        // Some rows should have scrolled into scrollback
        assert!(b.scrollback_len() > 0);
    }

    #[test]
    fn scrollback_discards_oldest_when_full() {
        let max = 10;
        let mut b = ScreenBuffer::new(80, 5, max);
        // Fill 5 + max + 5 = 20 lines → ring overflows
        for i in 0..20usize {
            feed(&mut b, &format!("L{:04}\r\n", i));
        }
        assert_eq!(b.scrollback_len(), max);
    }

    #[test]
    fn vt_scrollback_handles_rows_captured_before_resize() {
        let mut b = ScreenBuffer::new(13, 2, 20);
        for i in 0..6usize {
            feed(&mut b, &format!("line{:02}\r\n", i));
        }
        assert!(b.scrollback_len() > 0);

        b.resize(14, 2);

        let vt = b.to_vt_scrollback();
        assert!(
            !vt.is_empty(),
            "scrollback VT should still serialize after resize"
        );
    }

    // ── Title extraction ──────────────────────────────────────────────────────

    #[test]
    fn title_osc0_bell_terminated() {
        let mut b = buf(80, 24);
        // \e]0;My Title\a
        feed(&mut b, "\x1b]0;My Title\x07");
        assert_eq!(b.title, "My Title");
    }

    #[test]
    fn title_osc2() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]2;Window Name\x07");
        assert_eq!(b.title, "Window Name");
    }

    #[test]
    fn osc8_hyperlink_tracks_cells_and_closing_sequence() {
        let mut b = buf(80, 24);
        feed(
            &mut b,
            "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        );

        assert_eq!(b.hyperlink_at(0, 0), Some("https://example.com"));
        assert_eq!(b.hyperlink_at(0, 1), Some("https://example.com"));
        assert_eq!(b.hyperlink_at(0, 2), Some("https://example.com"));
        assert_eq!(b.hyperlink_at(0, 3), Some("https://example.com"));
        assert_eq!(b.hyperlink_at(0, 4), None);
    }

    #[test]
    fn osc8_hyperlinks_reset_on_ris() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]8;;https://example.com\x1b\\x\x1b]8;;\x1b\\");
        assert_eq!(b.hyperlink_at(0, 0), Some("https://example.com"));

        feed(&mut b, "\x1bc");
        assert_eq!(b.hyperlink_at(0, 0), None);
    }

    #[test]
    fn kitty_inline_image_sequence_creates_placeholder_cell() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b_Gi=1,a=t,f=100;payload\x1b\\");

        let image = b.inline_image_at(0, 0).expect("inline image record");
        assert_eq!(image.protocol, "kitty");
        assert_eq!(image.params, "i=1,a=t,f=100");
        assert_eq!(image.payload, "payload");
        assert_eq!(b.cell(0, 0).unwrap().text.as_str(), "▣");
    }

    #[test]
    fn kitty_inline_images_reset_on_ris() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b_Gi=1,a=t,f=100;payload\x1b\\");
        assert!(b.inline_image_at(0, 0).is_some());

        feed(&mut b, "\x1bc");
        assert!(b.inline_image_at(0, 0).is_none());
    }

    #[test]
    fn title_updated_multiple_times() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]0;First\x07");
        feed(&mut b, "\x1b]0;Second\x07");
        assert_eq!(b.title, "Second");
    }

    #[test]
    fn progress_osc_9_4_normal() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]9;4;1;42\x07");
        assert_eq!(b.progress(), Some(TerminalProgress::Normal(42)));
    }

    #[test]
    fn progress_osc_9_4_indeterminate() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]9;4;3\x07");
        assert_eq!(b.progress(), Some(TerminalProgress::Indeterminate));
    }

    #[test]
    fn progress_osc_9_4_clear() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]9;4;4;75\x07");
        feed(&mut b, "\x1b]9;4;0\x07");
        assert_eq!(b.progress(), None);
    }

    #[test]
    fn osc9_notification_can_be_drained() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]9;Tests complete\x07");

        let notifications = b.drain_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].message, "Tests complete");
        assert!(b.drain_notifications().is_empty());
    }

    #[test]
    fn osc777_notify_combines_title_and_body() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]777;notify;Build;Failed\x07");

        let notifications = b.drain_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].message, "Build: Failed");
    }

    // ── Wide characters ───────────────────────────────────────────────────────

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut b = buf(80, 24);
        // '中' (U+4E2D) is a CJK character — 2 columns wide
        feed(&mut b, "中");
        let left = b.cell(0, 0).unwrap();
        let right = b.cell(0, 1).unwrap();
        assert_eq!(left.first_char(), '中');
        assert!(left.attrs.is_wide());
        assert!(!left.attrs.is_wide_continuation());
        assert!(right.attrs.is_wide_continuation());
        // Cursor advanced by 2
        assert_eq!(b.cursor().col, 2);
    }

    #[test]
    fn mixed_wide_and_narrow() {
        let mut b = buf(80, 24);
        feed(&mut b, "A中B");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'A');
        assert_eq!(b.cell(0, 1).unwrap().first_char(), '中');
        assert!(b.cell(0, 2).unwrap().attrs.is_wide_continuation());
        assert_eq!(b.cell(0, 3).unwrap().first_char(), 'B');
        assert_eq!(b.cursor().col, 4);
    }

    #[test]
    fn variation_selector_is_zero_width() {
        let mut b = buf(80, 24);
        feed(&mut b, "\u{fe0f}A");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'A');
        assert_eq!(b.cursor().col, 1);
    }

    #[test]
    fn sparkles_emoji_is_double_width() {
        let mut b = buf(80, 24);
        feed(&mut b, "\u{2728}A");
        assert_eq!(b.cell(0, 0).unwrap().first_char(), '\u{2728}');
        assert_eq!(b.cell(0, 0).unwrap().text, "\u{2728}");
        assert!(b.cell(0, 1).unwrap().attrs.is_wide_continuation());
        assert_eq!(b.cell(0, 2).unwrap().first_char(), 'A');
        assert_eq!(b.cursor().col, 3);
    }

    #[test]
    fn combining_sequence_stays_in_one_cell() {
        let mut b = buf(80, 24);
        feed(&mut b, "e\u{0301}A");
        assert_eq!(b.cell(0, 0).unwrap().text, "e\u{0301}");
        assert!(!b.cell(0, 0).unwrap().attrs.is_wide());
        assert_eq!(b.cell(0, 1).unwrap().first_char(), 'A');
        assert_eq!(b.cursor().col, 2);
    }

    #[test]
    fn zwj_cluster_stays_in_one_wide_cell() {
        let mut b = buf(80, 24);
        feed(&mut b, "\u{1f469}\u{200d}\u{1f4bb}A");
        // ZWJ cluster exceeds CompactText inline capacity (11 bytes > 7);
        // only the first codepoint (U+1F469) is retained.
        assert_eq!(b.cell(0, 0).unwrap().first_char(), '\u{1f469}');
        assert!(b.cell(0, 0).unwrap().attrs.is_wide());
        assert!(b.cell(0, 1).unwrap().attrs.is_wide_continuation());
        assert_eq!(b.cell(0, 2).unwrap().first_char(), 'A');
        assert_eq!(b.cursor().col, 3);
    }

    // ── Erase operations ──────────────────────────────────────────────────────

    #[test]
    fn erase_in_line_from_cursor() {
        let mut b = buf(80, 24);
        feed(&mut b, "ABCDE");
        feed(&mut b, "\x1b[1;3H"); // row=0, col=2
        feed(&mut b, "\x1b[0K"); // EL 0: erase to end of line
        assert_eq!(b.cell(0, 0).unwrap().first_char(), 'A');
        assert_eq!(b.cell(0, 1).unwrap().first_char(), 'B');
        assert_eq!(b.cell(0, 2).unwrap().first_char(), ' ');
        assert_eq!(b.cell(0, 4).unwrap().first_char(), ' ');
    }

    #[test]
    fn erase_entire_display() {
        let mut b = buf(80, 24);
        feed(&mut b, "Hello World");
        feed(&mut b, "\x1b[2J");
        for c in 0..11 {
            assert_eq!(b.cell(0, c).unwrap().first_char(), ' ');
        }
    }

    // ── Cursor visibility ─────────────────────────────────────────────────────

    #[test]
    fn cursor_visibility_toggle() {
        let mut b = buf(80, 24);
        assert!(b.cursor().visible);
        feed(&mut b, "\x1b[?25l"); // hide
        assert!(!b.cursor().visible);
        feed(&mut b, "\x1b[?25h"); // show
        assert!(b.cursor().visible);
    }

    // ── Visible text ──────────────────────────────────────────────────────────

    #[test]
    fn visible_text_rows() {
        let mut b = buf(10, 3);
        feed(&mut b, "Hello");
        let text = b.visible_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(&lines[0][..5], "Hello");
    }

    #[test]
    fn row_text() {
        let mut b = buf(10, 3);
        feed(&mut b, "Row0\r\nRow1");
        let r = b.row_text(1).unwrap();
        assert!(r.starts_with("Row1"));
    }

    // ── Scroll region ─────────────────────────────────────────────────────────

    #[test]
    fn scroll_region_limits_scrolling() {
        let mut b = ScreenBuffer::new(80, 5, 1000);
        // Set scroll region rows 2–4 (1-based), i.e. 0-based: top=1, bot=3
        feed(&mut b, "\x1b[2;4r");
        assert_eq!(b.scroll_top, 1);
        assert_eq!(b.scroll_bottom, 3);
    }

    // ── DECSC / DECRC save/restore cursor ─────────────────────────────────────

    #[test]
    fn save_restore_cursor() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[5;10H"); // row=4, col=9
        feed(&mut b, "\x1b7"); // DECSC save
        feed(&mut b, "\x1b[1;1H"); // move away
        feed(&mut b, "\x1b8"); // DECRC restore
        assert_eq!(b.cursor().row, 4);
        assert_eq!(b.cursor().col, 9);
    }

    // ── DECSCUSR cursor shape ────────────────────────────────────────────────

    #[test]
    fn cursor_shape_default_is_block() {
        let b = buf(80, 24);
        assert_eq!(b.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn cursor_shape_block() {
        let mut b = buf(80, 24);
        // DECSCUSR 0 → block (default)
        feed(&mut b, "\x1b[0 q");
        assert_eq!(b.cursor().shape, CursorShape::Block);
        // DECSCUSR 1 → blinking block
        feed(&mut b, "\x1b[1 q");
        assert_eq!(b.cursor().shape, CursorShape::Block);
        // DECSCUSR 2 → steady block
        feed(&mut b, "\x1b[2 q");
        assert_eq!(b.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn cursor_shape_underline() {
        let mut b = buf(80, 24);
        // DECSCUSR 3 → blinking underline
        feed(&mut b, "\x1b[3 q");
        assert_eq!(b.cursor().shape, CursorShape::Underline);
        // DECSCUSR 4 → steady underline
        feed(&mut b, "\x1b[4 q");
        assert_eq!(b.cursor().shape, CursorShape::Underline);
    }

    #[test]
    fn cursor_shape_bar() {
        let mut b = buf(80, 24);
        // DECSCUSR 5 → blinking bar
        feed(&mut b, "\x1b[5 q");
        assert_eq!(b.cursor().shape, CursorShape::Bar);
        // DECSCUSR 6 → steady bar
        feed(&mut b, "\x1b[6 q");
        assert_eq!(b.cursor().shape, CursorShape::Bar);
    }

    #[test]
    fn cursor_shape_resets_on_ris() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[5 q"); // bar
        assert_eq!(b.cursor().shape, CursorShape::Bar);
        feed(&mut b, "\x1bc"); // RIS
        assert_eq!(b.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn cursor_shape_changes_across_sequences() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[3 q"); // underline
        assert_eq!(b.cursor().shape, CursorShape::Underline);
        feed(&mut b, "\x1b[5 q"); // bar
        assert_eq!(b.cursor().shape, CursorShape::Bar);
        feed(&mut b, "\x1b[2 q"); // block
        assert_eq!(b.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn configured_default_cursor_shape_parses_supported_values() {
        assert_eq!(
            CursorShape::from_config_name("block"),
            Some(CursorShape::Block)
        );
        assert_eq!(
            CursorShape::from_config_name("underline"),
            Some(CursorShape::Underline)
        );
        assert_eq!(
            CursorShape::from_config_name("line"),
            Some(CursorShape::Bar)
        );
        assert_eq!(
            CursorShape::from_config_name("beam"),
            Some(CursorShape::Bar)
        );
        assert_eq!(CursorShape::from_config_name("unknown"), None);
    }

    #[test]
    fn bracketed_paste_mode_enable_disable() {
        let mut b = buf(80, 24);
        assert!(!b.bracketed_paste());
        feed(&mut b, "\x1b[?2004h"); // enable
        assert!(b.bracketed_paste());
        feed(&mut b, "\x1b[?2004l"); // disable
        assert!(!b.bracketed_paste());
    }

    #[test]
    fn bracketed_paste_mode_resets_on_ris() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[?2004h");
        assert!(b.bracketed_paste());
        feed(&mut b, "\x1bc"); // RIS
        assert!(!b.bracketed_paste());
    }

    #[test]
    fn keyboard_protocol_negotiates_csi_u_and_kitty_modes() {
        let mut b = buf(80, 24);
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::Legacy);

        feed(&mut b, "\x1b[>1u");
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::CsiU);

        feed(&mut b, "\x1b[>31u");
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::Kitty);

        feed(&mut b, "\x1b[>0u");
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::Legacy);
    }

    #[test]
    fn keyboard_protocol_resets_on_ris() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[>31u");
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::Kitty);

        feed(&mut b, "\x1bc");
        assert_eq!(b.keyboard_protocol(), KeyboardProtocolMode::Legacy);
    }

    #[test]
    fn combined_dec_private_modes_enable_from_ftui_sequence() {
        let mut b = buf(80, 24);
        feed(
            &mut b,
            "\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l\x1b[?1006;1000;1002h\x1b[?1006h\x1b[?1000h\x1b[?1002h",
        );
        assert_eq!(b.mouse_mode(), MouseMode::ButtonEvent);
        assert!(b.sgr_mouse());
    }

    // ── capture_extended ──────────────────────────────────────────────────────

    /// Build a 10-wide, 3-row buffer with 4 scrollback rows and 3 visible rows.
    ///
    /// After writing lines 0-5 (each ending \r\n):
    /// - scrollback[0..3] (oldest→newest): "line0", "line1", "line2", "line3"
    /// - visible rows 0-2: "line4     ", "line5     ", "          "
    /// - total_lines = 7
    fn make_capture_buf() -> ScreenBuffer {
        let mut b = ScreenBuffer::new(10, 3, 100);
        for i in 0..6_u32 {
            feed(&mut b, &format!("line{}\r\n", i));
        }
        b
    }

    #[test]
    fn capture_extended_default_visible_only() {
        let b = make_capture_buf();
        let r = b.capture_extended(None, false, None, None, None, false);
        assert_eq!(r.total_lines, 7);
        assert_eq!(r.lines, 3);
        assert_eq!(r.cursor, 4);
        assert!(r.anchor_found.is_none());
        // Visible rows keep full width (trailing spaces to cols=10)
        assert!(r.text.contains("line4     "), "got: {:?}", r.text);
        assert!(r.text.contains("line5     "), "got: {:?}", r.text);
    }

    #[test]
    fn clear_actions_have_explicit_scope() {
        let mut b = make_capture_buf();
        assert!(b.scrollback_len() > 0);
        assert!(b.visible_text().contains("line5"));

        b.clear_scrollback();
        assert_eq!(b.scrollback_len(), 0);
        assert!(b.visible_text().contains("line5"));
        assert_eq!(
            b.capture_extended(None, true, None, None, None, false)
                .total_lines,
            3
        );

        b.clear_buffer();
        assert_eq!(b.scrollback_len(), 0);
        assert!(!b.visible_text().contains("line5"));
        assert_eq!(b.cursor().row, 0);
        assert_eq!(b.cursor().col, 0);
    }

    #[test]
    fn capture_extended_lines_5() {
        let b = make_capture_buf();
        let r = b.capture_extended(Some(5), false, None, None, None, false);
        assert_eq!(r.total_lines, 7);
        assert_eq!(r.lines, 5);
        assert_eq!(r.cursor, 2); // total(7) - 5 = 2
                                 // Scrollback rows are trimmed; visible rows keep full width
        assert!(r.text.starts_with("line2\n"), "got: {:?}", r.text);
        assert!(r.text.contains("line3\n"), "got: {:?}", r.text);
        assert!(r.text.contains("line4     \n"), "got: {:?}", r.text);
    }

    #[test]
    fn capture_extended_all() {
        let b = make_capture_buf();
        let r = b.capture_extended(None, true, None, None, None, false);
        assert_eq!(r.total_lines, 7);
        assert_eq!(r.lines, 7);
        assert_eq!(r.cursor, 0);
        assert!(r.text.starts_with("line0\n"), "got: {:?}", r.text);
        assert!(r.text.contains("line3\n"), "got: {:?}", r.text);
        assert!(r.text.contains("line4     \n"), "got: {:?}", r.text);
    }

    #[test]
    fn capture_extended_after_found() {
        let b = make_capture_buf();
        // "line2" is in scrollback[2]; search newest-first finds it
        let r = b.capture_extended(None, false, Some("line2"), None, None, false);
        assert_eq!(r.anchor_found, Some(true));
        assert_eq!(r.cursor, 2);
        assert_eq!(r.lines, 5); // lines 2..6 (total 7)
        assert!(r.text.starts_with("line2\n"), "got: {:?}", r.text);
    }

    #[test]
    fn capture_extended_after_not_found_falls_back_to_lines() {
        let b = make_capture_buf();
        let r = b.capture_extended(Some(10), false, Some("MISSING"), None, None, false);
        assert_eq!(r.anchor_found, Some(false));
        // lines=10 with total=7 → start = 7.saturating_sub(10) = 0
        assert_eq!(r.cursor, 0);
        assert_eq!(r.lines, 7);
    }

    #[test]
    fn capture_extended_after_regex_found() {
        use regex::Regex;
        let b = make_capture_buf();
        // Matches "line3" in scrollback[3] (newest-first search finds it first)
        let re = Regex::new(r"line[23]").unwrap();
        let r = b.capture_extended(None, false, None, Some(&re), None, false);
        assert_eq!(r.anchor_found, Some(true));
        assert_eq!(r.cursor, 3); // scrollback[3] = "line3     "
        assert_eq!(r.lines, 4); // lines 3..6
        assert!(r.text.starts_with("line3\n"), "got: {:?}", r.text);
    }

    #[test]
    fn capture_extended_max_lines_caps_output() {
        let b = make_capture_buf();
        let r = b.capture_extended(None, true, None, None, Some(3), false);
        assert_eq!(r.lines, 3);
        assert_eq!(r.cursor, 4); // total(7) - max(3) = 4
                                 // Only visible rows returned
        assert!(r.text.contains("line4     "), "got: {:?}", r.text);
        assert!(
            !r.text.contains("line3"),
            "should not contain scrollback: {:?}",
            r.text
        );
    }

    #[test]
    fn capture_extended_count_only() {
        let b = make_capture_buf();
        let r = b.capture_extended(None, false, None, None, None, true);
        assert_eq!(r.text, "");
        assert_eq!(r.lines, 3); // visible only (default)
        assert_eq!(r.cursor, 4);
        assert_eq!(r.total_lines, 7);
    }

    #[test]
    fn capture_extended_cursor_value_all() {
        let b = make_capture_buf();
        let r = b.capture_extended(None, true, None, None, None, false);
        assert_eq!(r.cursor, 0);
    }

    #[test]
    fn capture_extended_cursor_value_lines() {
        let b = make_capture_buf();
        let r = b.capture_extended(Some(3), false, None, None, None, false);
        assert_eq!(r.cursor, 4); // total(7) - 3 = 4
    }

    // ── to_vt_snapshot ───────────────────────────────────────────────────────

    #[test]
    fn vt_snapshot_round_trips_plain_text() {
        let mut orig = buf(80, 24);
        feed(&mut orig, "Hello, World!");

        let snapshot = orig.to_vt_snapshot();
        assert!(!snapshot.is_empty());

        // Feed the snapshot into a fresh buffer and verify the text appears.
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);
        assert!(
            copy.visible_text().contains("Hello, World!"),
            "expected 'Hello, World!' in:\n{}",
            copy.visible_text()
        );
    }

    #[test]
    fn vt_snapshot_round_trips_sgr_colors() {
        let mut orig = buf(80, 24);
        // Bold red text.
        feed(&mut orig, "\x1b[1;31mRED\x1b[0m normal");

        let snapshot = orig.to_vt_snapshot();
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);

        let text = copy.visible_text();
        assert!(text.contains("RED"), "expected 'RED' in:\n{text}");
        assert!(text.contains("normal"), "expected 'normal' in:\n{text}");

        // Check color attribute was preserved on the 'R' cell.
        let cell = copy.cell(0, 0).unwrap();
        assert!(cell.attrs.is_set(CellAttrs::BOLD));
        assert_eq!(cell.fg, Color::Ansi(1)); // red
    }

    #[test]
    fn vt_snapshot_round_trips_progress() {
        let mut orig = buf(80, 24);
        feed(&mut orig, "\x1b]9;4;4;64\x07");

        let snapshot = orig.to_vt_snapshot();
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);

        assert_eq!(copy.progress(), Some(TerminalProgress::Warning(64)));
    }

    #[test]
    fn vt_snapshot_cursor_position_preserved() {
        let mut orig = buf(80, 24);
        // Move cursor to row 3, col 5 (1-based in VT → 0-based in state).
        feed(&mut orig, "\x1b[4;6Htest");

        let snapshot = orig.to_vt_snapshot();
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);

        // Cursor should be at end of "test" on row 3, starting at col 5.
        assert_eq!(copy.cursor().row, 3);
        assert_eq!(copy.cell(3, 5).unwrap().first_char(), 't');
    }

    #[test]
    fn vt_snapshot_empty_screen_is_valid() {
        let orig = buf(40, 10);
        let snapshot = orig.to_vt_snapshot();
        // An empty screen snapshot should still be parseable.
        let mut copy = buf(40, 10);
        copy.advance(&snapshot);
        // All cells should remain blank.
        assert_eq!(copy.cell(0, 0).unwrap().first_char(), ' ');
    }

    #[test]
    fn vt_snapshot_multiline_text() {
        let mut orig = buf(80, 24);
        feed(&mut orig, "line one\r\nline two\r\nline three");

        let snapshot = orig.to_vt_snapshot();
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);

        let text = copy.visible_text();
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
        assert!(text.contains("line three"));
    }

    #[test]
    fn vt_snapshot_round_trips_alternate_screen_and_input_modes() {
        let mut orig = buf(80, 24);
        feed(
            &mut orig,
            "\x1b]2;Alt Title\x07\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?25l\x1b[5 qALT-SNAPSHOT",
        );

        let snapshot = orig.to_vt_snapshot();
        let mut copy = buf(80, 24);
        copy.advance(&snapshot);

        assert!(
            copy.on_alternate(),
            "snapshot should restore alternate screen"
        );
        assert_eq!(
            copy.mouse_mode(),
            MouseMode::AnyEvent,
            "snapshot should restore mouse tracking mode"
        );
        assert!(copy.sgr_mouse(), "snapshot should restore SGR mouse mode");
        assert!(
            copy.bracketed_paste(),
            "snapshot should restore bracketed paste mode"
        );
        assert!(
            !copy.cursor().visible,
            "snapshot should restore cursor visibility"
        );
        assert_eq!(
            copy.cursor().shape,
            CursorShape::Bar,
            "snapshot should restore cursor shape"
        );
        assert_eq!(copy.title, "Alt Title");
        assert!(
            copy.visible_text().contains("ALT-SNAPSHOT"),
            "snapshot should restore alternate-screen text; got:\n{}",
            copy.visible_text()
        );
    }
}
