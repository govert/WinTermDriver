//! Terminal renderer using Direct2D + DirectWrite.
//!
//! [`TerminalRenderer`] takes a reference to a [`ScreenBuffer`] and paints its
//! contents (characters, colors, bold/italic/underline) onto a Direct2D render
//! target backed by an HWND.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use wtd_pty::{Cell, CellAttrs, Color, Cursor, ScreenBuffer};

// ── Default theme colors ─────────────────────────────────────────────────────

/// Standard 16-color ANSI palette (r, g, b) in 0–255.
const ANSI_PALETTE: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // 0  Black
    (170, 0, 0),     // 1  Red
    (0, 170, 0),     // 2  Green
    (170, 85, 0),    // 3  Yellow/Brown
    (0, 0, 170),     // 4  Blue
    (170, 0, 170),   // 5  Magenta
    (0, 170, 170),   // 6  Cyan
    (170, 170, 170), // 7  White (light gray)
    (85, 85, 85),    // 8  Bright Black (dark gray)
    (255, 85, 85),   // 9  Bright Red
    (85, 255, 85),   // 10 Bright Green
    (255, 255, 85),  // 11 Bright Yellow
    (85, 85, 255),   // 12 Bright Blue
    (255, 85, 255),  // 13 Bright Magenta
    (85, 255, 255),  // 14 Bright Cyan
    (255, 255, 255), // 15 Bright White
];

const DEFAULT_FG: (u8, u8, u8) = (204, 204, 204);
const DEFAULT_BG: (u8, u8, u8) = (26, 26, 38);
const CURSOR_COLOR: (u8, u8, u8) = (204, 204, 204);

// ── Public types ─────────────────────────────────────────────────────────────

/// Terminal renderer backed by Direct2D + DirectWrite.
pub struct TerminalRenderer {
    // Kept alive — dropping these invalidates render target and text formats.
    #[allow(dead_code)]
    d2d_factory: ID2D1Factory,
    #[allow(dead_code)]
    dw_factory: IDWriteFactory,
    hwnd_rt: ID2D1HwndRenderTarget,
    rt: ID2D1RenderTarget,
    tf_regular: IDWriteTextFormat,
    tf_bold: IDWriteTextFormat,
    tf_italic: IDWriteTextFormat,
    tf_bold_italic: IDWriteTextFormat,
    cell_width: f32,
    cell_height: f32,
}

/// Configuration for creating a [`TerminalRenderer`].
pub struct RendererConfig {
    pub font_family: String,
    pub font_size: f32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            font_family: "Cascadia Mono".to_string(),
            font_size: 14.0,
        }
    }
}

// ── Color helpers (public for testing) ───────────────────────────────────────

/// Convert a terminal [`Color`] to an (r, g, b) triple (0–255).
pub fn color_to_rgb(color: &Color, is_foreground: bool) -> (u8, u8, u8) {
    match *color {
        Color::Default => {
            if is_foreground {
                DEFAULT_FG
            } else {
                DEFAULT_BG
            }
        }
        Color::Ansi(idx) => {
            let i = (idx as usize).min(7);
            ANSI_PALETTE[i]
        }
        Color::AnsiBright(idx) => {
            let i = (idx as usize).min(7) + 8;
            ANSI_PALETTE[i]
        }
        Color::Indexed(idx) => indexed_color(idx),
        Color::Rgb(r, g, b) => (r, g, b),
    }
}

/// Convert a 256-color index to (r, g, b).
fn indexed_color(idx: u8) -> (u8, u8, u8) {
    match idx {
        0..=15 => ANSI_PALETTE[idx as usize],
        // 6×6×6 color cube (indices 16–231)
        16..=231 => {
            let n = idx - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let to_byte = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (to_byte(r), to_byte(g), to_byte(b))
        }
        // Grayscale ramp (indices 232–255)
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            (v, v, v)
        }
    }
}

fn rgb_to_d2d(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

/// Resolve effective foreground and background colors for a cell, handling
/// the INVERSE attribute.
pub fn resolve_cell_colors(cell: &Cell) -> ((u8, u8, u8), (u8, u8, u8)) {
    let mut fg = color_to_rgb(&cell.fg, true);
    let mut bg = color_to_rgb(&cell.bg, false);

    if cell.attrs.is_set(CellAttrs::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.attrs.is_set(CellAttrs::DIM) {
        fg.0 = fg.0 / 2;
        fg.1 = fg.1 / 2;
        fg.2 = fg.2 / 2;
    }
    (fg, bg)
}

// ── TerminalRenderer ─────────────────────────────────────────────────────────

impl TerminalRenderer {
    /// Create a new renderer targeting the given window handle.
    pub fn new(hwnd: HWND, config: &RendererConfig) -> Result<Self> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dw_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect)? };
        let size = D2D_SIZE_U {
            width: (rect.right - rect.left) as u32,
            height: (rect.bottom - rect.top) as u32,
        };

        let rt_props = D2D1_RENDER_TARGET_PROPERTIES::default();
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: size,
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let hwnd_rt = unsafe { d2d_factory.CreateHwndRenderTarget(&rt_props, &hwnd_props)? };
        let rt: ID2D1RenderTarget = hwnd_rt.cast()?;

        let font_wide: Vec<u16> = config
            .font_family
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let font_pcwstr = PCWSTR(font_wide.as_ptr());

        let tf_regular = unsafe {
            dw_factory.CreateTextFormat(
                font_pcwstr,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                config.font_size,
                w!("en-us"),
            )?
        };
        let tf_bold = unsafe {
            dw_factory.CreateTextFormat(
                font_pcwstr,
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                config.font_size,
                w!("en-us"),
            )?
        };
        let tf_italic = unsafe {
            dw_factory.CreateTextFormat(
                font_pcwstr,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_ITALIC,
                DWRITE_FONT_STRETCH_NORMAL,
                config.font_size,
                w!("en-us"),
            )?
        };
        let tf_bold_italic = unsafe {
            dw_factory.CreateTextFormat(
                font_pcwstr,
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_ITALIC,
                DWRITE_FONT_STRETCH_NORMAL,
                config.font_size,
                w!("en-us"),
            )?
        };

        let (cell_width, cell_height) = Self::measure_cell(&dw_factory, &tf_regular)?;

        Ok(Self {
            d2d_factory,
            dw_factory,
            hwnd_rt,
            rt,
            tf_regular,
            tf_bold,
            tf_italic,
            tf_bold_italic,
            cell_width,
            cell_height,
        })
    }

    /// Cell dimensions in pixels.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_width, self.cell_height)
    }

    /// Resize the render target to match a new window size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let size = D2D_SIZE_U { width, height };
        unsafe { self.hwnd_rt.Resize(&size) }
    }

    /// Paint the contents of a [`ScreenBuffer`] to the window.
    ///
    /// This is the core rendering entry point. It clears the background,
    /// draws per-cell backgrounds where non-default, draws text runs with
    /// appropriate fonts/colors, and draws the cursor.
    pub fn paint(&self, screen: &ScreenBuffer) -> Result<()> {
        let rows = screen.rows();
        let cols = screen.cols();

        unsafe {
            self.rt.BeginDraw();

            let bg = rgb_to_d2d(DEFAULT_BG.0, DEFAULT_BG.1, DEFAULT_BG.2);
            self.rt.Clear(Some(&bg));

            // Draw cell backgrounds + text row by row using run-based batching.
            for row in 0..rows {
                let y = row as f32 * self.cell_height;
                self.paint_row_backgrounds(screen, row, cols, y)?;
                self.paint_row_text(screen, row, cols, y)?;
            }

            // Draw cursor
            let cursor = screen.cursor();
            if cursor.visible && cursor.row < rows && cursor.col < cols {
                self.paint_cursor(cursor)?;
            }

            self.rt.EndDraw(None, None)?;
        }
        Ok(())
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Paint non-default cell backgrounds for a single row.
    unsafe fn paint_row_backgrounds(
        &self,
        screen: &ScreenBuffer,
        row: usize,
        cols: usize,
        y: f32,
    ) -> Result<()> {
        let mut col = 0;
        while col < cols {
            let cell = match screen.cell(row, col) {
                Some(c) => c,
                None => {
                    col += 1;
                    continue;
                }
            };
            let (_, bg_rgb) = resolve_cell_colors(cell);
            if bg_rgb == DEFAULT_BG {
                col += 1;
                continue;
            }

            // Extend run while the background color is the same.
            let run_start = col;
            col += 1;
            while col < cols {
                if let Some(next) = screen.cell(row, col) {
                    let (_, next_bg) = resolve_cell_colors(next);
                    if next_bg == bg_rgb {
                        col += 1;
                        continue;
                    }
                }
                break;
            }

            let brush = self
                .rt
                .CreateSolidColorBrush(&rgb_to_d2d(bg_rgb.0, bg_rgb.1, bg_rgb.2), None)?;
            let rect = D2D_RECT_F {
                left: run_start as f32 * self.cell_width,
                top: y,
                right: col as f32 * self.cell_width,
                bottom: y + self.cell_height,
            };
            self.rt.FillRectangle(&rect, &brush);
        }
        Ok(())
    }

    /// Paint text for a single row using run-based batching.
    ///
    /// Adjacent cells with the same foreground color and font style are
    /// batched into a single `DrawText` call for performance.
    unsafe fn paint_row_text(
        &self,
        screen: &ScreenBuffer,
        row: usize,
        cols: usize,
        y: f32,
    ) -> Result<()> {
        let mut col = 0;
        while col < cols {
            let cell = match screen.cell(row, col) {
                Some(c) => c,
                None => {
                    col += 1;
                    continue;
                }
            };
            if cell.wide_continuation {
                col += 1;
                continue;
            }
            if cell.character == ' ' && cell.attrs == CellAttrs::default() {
                col += 1;
                continue;
            }

            let (fg_rgb, _) = resolve_cell_colors(cell);
            let tf = self.text_format_for_attrs(&cell.attrs);
            let run_start = col;
            let mut run_text = String::new();
            run_text.push(cell.character);

            col += 1;
            // Extend the run while color and font match.
            while col < cols {
                if let Some(next) = screen.cell(row, col) {
                    if next.wide_continuation {
                        col += 1;
                        continue;
                    }
                    let (next_fg, _) = resolve_cell_colors(next);
                    let next_tf_matches = self.attrs_same_format(&cell.attrs, &next.attrs);
                    if next_fg == fg_rgb && next_tf_matches {
                        run_text.push(next.character);
                        col += 1;
                        continue;
                    }
                }
                break;
            }

            let utf16: Vec<u16> = run_text.encode_utf16().collect();
            let brush = self
                .rt
                .CreateSolidColorBrush(&rgb_to_d2d(fg_rgb.0, fg_rgb.1, fg_rgb.2), None)?;
            let rect = D2D_RECT_F {
                left: run_start as f32 * self.cell_width,
                top: y,
                right: (run_start + run_text.chars().count()) as f32 * self.cell_width,
                bottom: y + self.cell_height,
            };
            self.rt.DrawText(
                &utf16,
                tf,
                &rect,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );

            // Draw underline
            if cell.attrs.is_set(CellAttrs::UNDERLINE) {
                let underline_y = y + self.cell_height - 2.0;
                let p0 = D2D_POINT_2F {
                    x: run_start as f32 * self.cell_width,
                    y: underline_y,
                };
                let p1 = D2D_POINT_2F {
                    x: (run_start + run_text.chars().count()) as f32 * self.cell_width,
                    y: underline_y,
                };
                self.rt.DrawLine(p0, p1, &brush, 1.0, None);
            }

            // Draw strikethrough
            if cell.attrs.is_set(CellAttrs::STRIKETHROUGH) {
                let strike_y = y + self.cell_height / 2.0;
                let p0 = D2D_POINT_2F {
                    x: run_start as f32 * self.cell_width,
                    y: strike_y,
                };
                let p1 = D2D_POINT_2F {
                    x: (run_start + run_text.chars().count()) as f32 * self.cell_width,
                    y: strike_y,
                };
                self.rt.DrawLine(p0, p1, &brush, 1.0, None);
            }
        }
        Ok(())
    }

    /// Paint the cursor as a filled rectangle.
    unsafe fn paint_cursor(&self, cursor: &Cursor) -> Result<()> {
        let (r, g, b) = CURSOR_COLOR;
        let brush = self
            .rt
            .CreateSolidColorBrush(&rgb_to_d2d(r, g, b), None)?;
        let rect = D2D_RECT_F {
            left: cursor.col as f32 * self.cell_width,
            top: cursor.row as f32 * self.cell_height,
            right: (cursor.col + 1) as f32 * self.cell_width,
            bottom: (cursor.row + 1) as f32 * self.cell_height,
        };
        // Use 50% opacity for the cursor so text underneath is visible.
        brush.SetOpacity(0.5);
        self.rt.FillRectangle(&rect, &brush);
        Ok(())
    }

    fn text_format_for_attrs(&self, attrs: &CellAttrs) -> &IDWriteTextFormat {
        let bold = attrs.is_set(CellAttrs::BOLD);
        let italic = attrs.is_set(CellAttrs::ITALIC);
        match (bold, italic) {
            (false, false) => &self.tf_regular,
            (true, false) => &self.tf_bold,
            (false, true) => &self.tf_italic,
            (true, true) => &self.tf_bold_italic,
        }
    }

    fn attrs_same_format(&self, a: &CellAttrs, b: &CellAttrs) -> bool {
        let a_bold = a.is_set(CellAttrs::BOLD);
        let a_italic = a.is_set(CellAttrs::ITALIC);
        let b_bold = b.is_set(CellAttrs::BOLD);
        let b_italic = b.is_set(CellAttrs::ITALIC);
        a_bold == b_bold && a_italic == b_italic
    }

    fn measure_cell(
        dw: &IDWriteFactory,
        tf: &IDWriteTextFormat,
    ) -> Result<(f32, f32)> {
        let text: Vec<u16> = "M".encode_utf16().collect();
        let layout = unsafe { dw.CreateTextLayout(&text, tf, 1000.0, 1000.0)? };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok((metrics.width, metrics.height))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_to_rgb_default_fg() {
        assert_eq!(color_to_rgb(&Color::Default, true), DEFAULT_FG);
    }

    #[test]
    fn color_to_rgb_default_bg() {
        assert_eq!(color_to_rgb(&Color::Default, false), DEFAULT_BG);
    }

    #[test]
    fn color_to_rgb_ansi() {
        // Red
        assert_eq!(color_to_rgb(&Color::Ansi(1), true), (170, 0, 0));
        // Green
        assert_eq!(color_to_rgb(&Color::Ansi(2), true), (0, 170, 0));
    }

    #[test]
    fn color_to_rgb_ansi_bright() {
        // Bright red
        assert_eq!(color_to_rgb(&Color::AnsiBright(1), true), (255, 85, 85));
        // Bright white
        assert_eq!(
            color_to_rgb(&Color::AnsiBright(7), true),
            (255, 255, 255)
        );
    }

    #[test]
    fn color_to_rgb_indexed_palette() {
        // Index 0 = black
        assert_eq!(color_to_rgb(&Color::Indexed(0), true), (0, 0, 0));
        // Index 15 = bright white
        assert_eq!(
            color_to_rgb(&Color::Indexed(15), true),
            (255, 255, 255)
        );
    }

    #[test]
    fn color_to_rgb_indexed_cube() {
        // Index 16 = (0,0,0) in cube
        assert_eq!(color_to_rgb(&Color::Indexed(16), true), (0, 0, 0));
        // Index 196 = (5,0,0) = (255,0,0) in cube
        // 196 - 16 = 180 => r=180/36=5, g=0, b=0
        assert_eq!(color_to_rgb(&Color::Indexed(196), true), (255, 0, 0));
    }

    #[test]
    fn color_to_rgb_indexed_grayscale() {
        // Index 232 = darkest gray
        assert_eq!(color_to_rgb(&Color::Indexed(232), true), (8, 8, 8));
        // Index 255 = lightest gray
        assert_eq!(color_to_rgb(&Color::Indexed(255), true), (238, 238, 238));
    }

    #[test]
    fn color_to_rgb_truecolor() {
        assert_eq!(color_to_rgb(&Color::Rgb(42, 128, 255), true), (42, 128, 255));
    }

    #[test]
    fn resolve_colors_normal_cell() {
        let cell = Cell {
            character: 'A',
            fg: Color::Ansi(1),
            bg: Color::Default,
            attrs: CellAttrs::default(),
            wide: false,
            wide_continuation: false,
        };
        let (fg, bg) = resolve_cell_colors(&cell);
        assert_eq!(fg, (170, 0, 0));
        assert_eq!(bg, DEFAULT_BG);
    }

    #[test]
    fn resolve_colors_inverse() {
        let mut attrs = CellAttrs::default();
        attrs.set(CellAttrs::INVERSE);
        let cell = Cell {
            character: 'A',
            fg: Color::Ansi(1),
            bg: Color::Ansi(2),
            attrs,
            wide: false,
            wide_continuation: false,
        };
        let (fg, bg) = resolve_cell_colors(&cell);
        // Swapped
        assert_eq!(fg, (0, 170, 0));   // was bg
        assert_eq!(bg, (170, 0, 0));   // was fg
    }

    #[test]
    fn resolve_colors_dim() {
        let mut attrs = CellAttrs::default();
        attrs.set(CellAttrs::DIM);
        let cell = Cell {
            character: 'A',
            fg: Color::Rgb(200, 100, 50),
            bg: Color::Default,
            attrs,
            wide: false,
            wide_continuation: false,
        };
        let (fg, _bg) = resolve_cell_colors(&cell);
        assert_eq!(fg, (100, 50, 25));
    }

    #[test]
    fn rgb_to_d2d_conversion() {
        let c = rgb_to_d2d(255, 0, 128);
        assert!((c.r - 1.0).abs() < 0.01);
        assert!(c.g.abs() < 0.01);
        assert!((c.b - 0.502).abs() < 0.01);
        assert!((c.a - 1.0).abs() < 0.01);
    }
}
