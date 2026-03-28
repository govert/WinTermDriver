//! Host-side VT screen buffer (§14.4, §15).
//!
//! [`ScreenBuffer`] maintains the full terminal state: active grid, alternate
//! screen, scrollback ring, cursor, and title.  Feed raw PTY output bytes via
//! [`ScreenBuffer::advance`]; query state at any time.

use std::collections::VecDeque;

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

    pub fn is_set(self, flag: u16) -> bool { self.0 & flag != 0 }
    pub fn set(&mut self, flag: u16) { self.0 |= flag; }
    pub fn clear(&mut self, flag: u16) { self.0 &= !flag; }
}

// ── Cell ─────────────────────────────────────────────────────────────────────

/// A single terminal cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Displayed character; `' '` for empty cells.
    pub character: char,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Visual attributes.
    pub attrs: CellAttrs,
    /// True if this is the left half of a wide (CJK) character.
    pub wide: bool,
    /// True if this is the right-half placeholder of a wide character.
    pub wide_continuation: bool,
}

impl Cell {
    fn blank() -> Self {
        Cell {
            character: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: CellAttrs::default(),
            wide: false,
            wide_continuation: false,
        }
    }
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
    fn scroll_region_up(&mut self, top: usize, bottom: usize, n: usize,
                         scrollback: &mut VecDeque<Vec<Cell>>, max_scrollback: usize) {
        if top == 0 && bottom == self.rows.saturating_sub(1) {
            self.scroll_up(n, scrollback, max_scrollback);
            return;
        }
        let n = n.min(bottom + 1 - top);
        // Rows [top..top+n] leave; rows [top+n..=bottom] shift up; blank fills at bottom.
        for _ in 0..n {
            for r in top..bottom {
                for c in 0..self.cols {
                    let src = (r + 1) * self.cols + c;
                    let dst = r * self.cols + c;
                    self.cells[dst] = self.cells[src].clone();
                }
            }
            for c in 0..self.cols {
                self.cells[bottom * self.cols + c] = Cell::blank();
            }
        }
    }

    /// Scroll the region [top, bottom] (inclusive, 0-based rows) down by n.
    fn scroll_region_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for _ in 0..n {
            for r in (top..bottom).rev() {
                for c in 0..self.cols {
                    let src = r * self.cols + c;
                    let dst = (r + 1) * self.cols + c;
                    self.cells[dst] = self.cells[src].clone();
                }
            }
            for c in 0..self.cols {
                self.cells[top * self.cols + c] = Cell::blank();
            }
        }
    }

    /// Clear from (row, col) to end of screen.
    fn clear_from(&mut self, row: usize, col: usize) {
        let start = row * self.cols + col;
        for cell in &mut self.cells[start..] {
            *cell = Cell::blank();
        }
    }

    /// Clear from start of screen to (row, col) inclusive.
    fn clear_to(&mut self, row: usize, col: usize) {
        let end = row * self.cols + col + 1;
        for cell in &mut self.cells[..end] {
            *cell = Cell::blank();
        }
    }

    /// Clear an entire row.
    fn clear_row(&mut self, row: usize) {
        let start = row * self.cols;
        for cell in &mut self.cells[start..start + self.cols] {
            *cell = Cell::blank();
        }
    }

    /// Clear from column to end of row.
    fn clear_row_from(&mut self, row: usize, col: usize) {
        let start = row * self.cols + col;
        let end = (row + 1) * self.cols;
        for cell in &mut self.cells[start..end] {
            *cell = Cell::blank();
        }
    }

    /// Clear from start of row to column (inclusive).
    fn clear_row_to(&mut self, row: usize, col: usize) {
        let start = row * self.cols;
        let end = row * self.cols + col + 1;
        for cell in &mut self.cells[start..end] {
            *cell = Cell::blank();
        }
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut new_cells = vec![Cell::blank(); new_cols * new_rows];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                new_cells[r * new_cols + c] = self.cells[r * self.cols + c].clone();
            }
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.cells = new_cells;
    }
}

// ── Cursor ───────────────────────────────────────────────────────────────────

/// Cursor state.
#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
}

impl Cursor {
    fn default_visible() -> Self {
        Cursor { row: 0, col: 0, visible: true }
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

    /// VT parser.
    parser: vte::Parser,

    /// Pending character for wide-char continuation tracking.
    /// After printing a wide char we advance cursor by 2.
    _wide_pending: bool,
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
            parser: vte::Parser::new(),
            _wide_pending: false,
        }
    }

    /// Feed raw bytes from the PTY output into the screen buffer.
    pub fn advance(&mut self, bytes: &[u8]) {
        // vte::Perform requires a mutable reference to self, but Parser::advance
        // also takes &mut self.  We swap the parser out, advance, swap back.
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        for &b in bytes {
            parser.advance(self, b);
        }
        self.parser = parser;
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    pub fn cols(&self) -> usize { self.cols }
    pub fn rows(&self) -> usize { self.rows }
    pub fn on_alternate(&self) -> bool { self.on_alternate }

    /// Current cursor state.
    pub fn cursor(&self) -> &Cursor { &self.cursor }

    /// Cell at (row, col) in the visible screen (0-based).
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols { return None; }
        Some(self.active_grid().cell(row, col))
    }

    /// Read the visible screen as plain text (newline-separated rows).
    pub fn visible_text(&self) -> String {
        let g = self.active_grid();
        let mut out = String::with_capacity(self.rows * (self.cols + 1));
        for r in 0..self.rows {
            for c in 0..self.cols {
                let cell = g.cell(r, c);
                if !cell.wide_continuation {
                    out.push(cell.character);
                }
            }
            out.push('\n');
        }
        out
    }

    /// Read a single row as plain text (without the trailing newline).
    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.rows { return None; }
        let g = self.active_grid();
        let mut s = String::with_capacity(self.cols);
        for c in 0..self.cols {
            let cell = g.cell(row, c);
            if !cell.wide_continuation {
                s.push(cell.character);
            }
        }
        Some(s)
    }

    /// Number of scrollback rows currently stored.
    pub fn scrollback_len(&self) -> usize { self.scrollback.len() }

    /// A row from scrollback (0 = oldest).
    pub fn scrollback_row(&self, idx: usize) -> Option<&Vec<Cell>> {
        self.scrollback.get(idx)
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
        if self.on_alternate { &self.alternate } else { &self.primary }
    }

    fn active_grid_mut(&mut self) -> &mut Grid {
        if self.on_alternate { &mut self.alternate } else { &mut self.primary }
    }

    /// Write a character at cursor position and advance cursor.
    fn print_char(&mut self, c: char) {
        let is_wide = unicode_width(c) == 2;

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
                let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
                grid.scroll_region_up(top, bot, 1, &mut self.scrollback, max_sb);
            } else {
                let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
                grid.scroll_region_up(top, bot, 1, &mut dummy, 0);
            }
            self.cursor.row = self.scroll_bottom;
        }

        let row = self.cursor.row;
        let col = self.cursor.col;
        let fg = self.pen_fg;
        let bg = self.pen_bg;
        let attrs = self.pen_attrs;

        {
            let grid = self.active_grid_mut();
            let cell = grid.cell_mut(row, col);
            cell.character = c;
            cell.fg = fg;
            cell.bg = bg;
            cell.attrs = attrs;
            cell.wide = is_wide;
            cell.wide_continuation = false;
        }

        if is_wide {
            // Fill continuation if within bounds.
            if col + 1 < self.cols {
                let grid = self.active_grid_mut();
                let cont = grid.cell_mut(row, col + 1);
                *cont = Cell::blank();
                cont.wide_continuation = true;
            }
            self.cursor.col += 2;
        } else {
            self.cursor.col += 1;
        }
    }

    /// Apply SGR (Select Graphic Rendition) parameters.
    fn apply_sgr(&mut self, params: &Params) {
        let iter = params.iter();
        // Flatten sub-params: collect top-level params and their sub-params.
        // We build a flat Vec<Vec<u16>> where each inner vec is a param+subparams.
        let mut flat: Vec<Vec<u16>> = Vec::new();
        for p in params.iter() {
            let sub: Vec<u16> = p.iter().copied().collect();
            flat.push(sub);
        }
        // If params is empty, reset.
        if flat.is_empty() {
            self.reset_sgr();
            return;
        }

        let _ = iter;

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
                22 => { self.pen_attrs.clear(CellAttrs::BOLD); self.pen_attrs.clear(CellAttrs::DIM); }
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
                    } else if i + 1 < flat.len() && flat[i+1][0] == 5 {
                        let n = if i + 2 < flat.len() { flat[i+2][0] as u8 } else { 0 };
                        self.pen_fg = Color::Indexed(n);
                        i += 2;
                    } else if i + 1 < flat.len() && flat[i+1][0] == 2 {
                        let r = if i+2 < flat.len() { flat[i+2][0] as u8 } else { 0 };
                        let g = if i+3 < flat.len() { flat[i+3][0] as u8 } else { 0 };
                        let b = if i+4 < flat.len() { flat[i+4][0] as u8 } else { 0 };
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
                    } else if i + 1 < flat.len() && flat[i+1][0] == 5 {
                        let n = if i + 2 < flat.len() { flat[i+2][0] as u8 } else { 0 };
                        self.pen_bg = Color::Indexed(n);
                        i += 2;
                    } else if i + 1 < flat.len() && flat[i+1][0] == 2 {
                        let r = if i+2 < flat.len() { flat[i+2][0] as u8 } else { 0 };
                        let g = if i+3 < flat.len() { flat[i+3][0] as u8 } else { 0 };
                        let b = if i+4 < flat.len() { flat[i+4][0] as u8 } else { 0 };
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

fn unicode_width(c: char) -> usize {
    // Fast path for common ASCII.
    if (c as u32) < 0x300 { return 1; }
    // CJK Unified Ideographs, Fullwidth Forms, etc. — very conservative subset.
    match c as u32 {
        0x1100..=0x115F |  // Hangul Jamo
        0x2329..=0x232A |  // Angle brackets
        0x2E80..=0x303E |  // CJK Radicals
        0x3040..=0x33FF |  // Hiragana/Katakana/CJK
        0x3400..=0x4DBF |  // CJK Extension A
        0x4E00..=0x9FFF |  // CJK Unified
        0xA000..=0xA4CF |  // Yi
        0xA960..=0xA97F |  // Hangul Jamo Extended-A
        0xAC00..=0xD7FF |  // Hangul Syllables
        0xF900..=0xFAFF |  // CJK Compatibility
        0xFE10..=0xFE19 |  // Vertical forms
        0xFE30..=0xFE6F |  // CJK Compatibility Forms
        0xFF00..=0xFF60 |  // Fullwidth
        0xFFE0..=0xFFE6 |  // Fullwidth signs
        0x1B000..=0x1B0FF | // Kana Supplement
        0x1F004..=0x1F0CF | // Mahjong/Playing cards (emoji)
        0x1F300..=0x1F9FF | // Misc symbols/emoji
        0x20000..=0x2FFFD | // CJK Ext B-F + compat
        0x30000..=0x3FFFD   // CJK Ext G+
        => 2,
        _ => 1,
    }
}

// ── Perform impl ──────────────────────────────────────────────────────────────

impl Perform for ScreenBuffer {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // BEL — ignore
            0x07 => {}
            // BS
            0x08 => {
                if self.cursor.col > 0 { self.cursor.col -= 1; }
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
                        let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
                        grid.scroll_region_up(top, bot, 1, &mut self.scrollback, max_sb);
                    } else {
                        let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                        let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
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
            0x0D => { self.cursor.col = 0; }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
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
                for r in row..row+n {
                    if r > bot { break; }
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
                self.active_grid_mut().scroll_region_up(row, bot, n, &mut dummy, 0);
            }
            // DCH — delete characters
            ([], 'P') => {
                let n = p1(0, 1);
                let row = self.cursor.row;
                let col = self.cursor.col;
                let end = self.cols;
                let g = self.active_grid_mut();
                for c in col..end {
                    if c + n < end {
                        let src = row * g.cols + c + n;
                        g.cells[row * g.cols + c] = g.cells[src].clone();
                    } else {
                        *g.cell_mut(row, c) = Cell::blank();
                    }
                }
            }
            // SU — scroll up
            ([], 'S') => {
                let n = p1(0, 1);
                let top = self.scroll_top;
                let bot = self.scroll_bottom;
                let max_sb = self.max_scrollback;
                let on_alt = self.on_alternate;
                if top == 0 && !on_alt {
                    let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
                    grid.scroll_region_up(top, bot, n, &mut self.scrollback, max_sb);
                } else {
                    let mut dummy: VecDeque<Vec<Cell>> = VecDeque::new();
                    let grid = if on_alt { &mut self.alternate } else { &mut self.primary };
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
                for c in (col..end).rev() {
                    if c >= col + n {
                        let src = row * g.cols + c - n;
                        g.cells[row * g.cols + c] = g.cells[src].clone();
                    } else {
                        *g.cell_mut(row, c) = Cell::blank();
                    }
                }
            }
            // DEC private modes
            ([b'?'], 'h') => {
                for sub in params.iter() {
                    match sub[0] {
                        25 => self.cursor.visible = true,
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
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 0 or OSC 2: set window title
        if let Some(&code_bytes) = params.first() {
            let code_str = std::str::from_utf8(code_bytes).unwrap_or("");
            if (code_str == "0" || code_str == "2") && params.len() >= 2 {
                if let Ok(title) = std::str::from_utf8(params[1]) {
                    self.title = title.to_owned();
                }
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
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
        assert_eq!(b.cell(0, 0).unwrap().character, 'H');
        assert_eq!(b.cell(0, 4).unwrap().character, 'o');
        assert_eq!(b.cursor().col, 5);
        assert_eq!(b.cursor().row, 0);
    }

    #[test]
    fn newline_advances_row() {
        let mut b = buf(80, 24);
        feed(&mut b, "line1\r\nline2");
        assert_eq!(b.cell(0, 0).unwrap().character, 'l');
        assert_eq!(b.cell(1, 0).unwrap().character, 'l');
        assert_eq!(b.cell(1, 4).unwrap().character, '2');
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
        feed(&mut b, "\x1b[5;5H");  // row=4, col=4
        feed(&mut b, "\x1b[2A");    // up 2 → row=2
        assert_eq!(b.cursor().row, 2);
        feed(&mut b, "\x1b[3B");    // down 3 → row=5
        assert_eq!(b.cursor().row, 5);
        feed(&mut b, "\x1b[2C");    // right 2 → col=6
        assert_eq!(b.cursor().col, 6);
        feed(&mut b, "\x1b[1D");    // left 1 → col=5
        assert_eq!(b.cursor().col, 5);
    }

    // ── SGR: ANSI 8 colors ────────────────────────────────────────────────────

    #[test]
    fn sgr_ansi_fg_bg() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b[31;42mX"); // fg=red(1), bg=green(2)
        let c = b.cell(0, 0).unwrap();
        assert_eq!(c.character, 'X');
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
        assert_eq!(b.cell(0, 0).unwrap().character, ' ');
        feed(&mut b, "alternate");
        assert_eq!(b.cell(0, 0).unwrap().character, 'a');
        // Exit alternate screen
        feed(&mut b, "\x1b[?1049l");
        assert!(!b.on_alternate());
        // Primary screen content restored
        assert_eq!(b.cell(0, 0).unwrap().character, 'p');
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
    fn title_updated_multiple_times() {
        let mut b = buf(80, 24);
        feed(&mut b, "\x1b]0;First\x07");
        feed(&mut b, "\x1b]0;Second\x07");
        assert_eq!(b.title, "Second");
    }

    // ── Wide characters ───────────────────────────────────────────────────────

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut b = buf(80, 24);
        // '中' (U+4E2D) is a CJK character — 2 columns wide
        feed(&mut b, "中");
        let left = b.cell(0, 0).unwrap();
        let right = b.cell(0, 1).unwrap();
        assert_eq!(left.character, '中');
        assert!(left.wide);
        assert!(!left.wide_continuation);
        assert!(right.wide_continuation);
        // Cursor advanced by 2
        assert_eq!(b.cursor().col, 2);
    }

    #[test]
    fn mixed_wide_and_narrow() {
        let mut b = buf(80, 24);
        feed(&mut b, "A中B");
        assert_eq!(b.cell(0, 0).unwrap().character, 'A');
        assert_eq!(b.cell(0, 1).unwrap().character, '中');
        assert!(b.cell(0, 2).unwrap().wide_continuation);
        assert_eq!(b.cell(0, 3).unwrap().character, 'B');
        assert_eq!(b.cursor().col, 4);
    }

    // ── Erase operations ──────────────────────────────────────────────────────

    #[test]
    fn erase_in_line_from_cursor() {
        let mut b = buf(80, 24);
        feed(&mut b, "ABCDE");
        feed(&mut b, "\x1b[1;3H"); // row=0, col=2
        feed(&mut b, "\x1b[0K");   // EL 0: erase to end of line
        assert_eq!(b.cell(0, 0).unwrap().character, 'A');
        assert_eq!(b.cell(0, 1).unwrap().character, 'B');
        assert_eq!(b.cell(0, 2).unwrap().character, ' ');
        assert_eq!(b.cell(0, 4).unwrap().character, ' ');
    }

    #[test]
    fn erase_entire_display() {
        let mut b = buf(80, 24);
        feed(&mut b, "Hello World");
        feed(&mut b, "\x1b[2J");
        for c in 0..11 {
            assert_eq!(b.cell(0, c).unwrap().character, ' ');
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
        feed(&mut b, "\x1b7");      // DECSC save
        feed(&mut b, "\x1b[1;1H");  // move away
        feed(&mut b, "\x1b8");      // DECRC restore
        assert_eq!(b.cursor().row, 4);
        assert_eq!(b.cursor().col, 9);
    }
}
