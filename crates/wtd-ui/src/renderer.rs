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

use wtd_pty::{Cell, CellAttrs, Color, Cursor, CursorShape, ScreenBuffer};

#[cfg(test)]
use wtd_pty::CompactText;

// ── Selection ────────────────────────────────────────────────────────────────

/// A text selection range in screen coordinates (row, col).
///
/// `start` is where the user began selecting, `end` is the current position.
/// They may be in any order — rendering normalises them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl TextSelection {
    /// Return the selection normalised so that start <= end in reading order.
    pub fn normalised(&self) -> (usize, usize, usize, usize) {
        if (self.start_row, self.start_col) <= (self.end_row, self.end_col) {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }

    /// Return true if the cell at (row, col) is within this selection.
    pub fn contains(&self, row: usize, col: usize) -> bool {
        let (sr, sc, er, ec) = self.normalised();
        if row < sr || row > er {
            return false;
        }
        if row == sr && row == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true
    }
}

const SELECTION_COLOR: (u8, u8, u8) = (58, 100, 150);

// Colors for the failed/exited pane overlay.
const FAILED_PANE_BG: (u8, u8, u8) = (30, 30, 42);
const FAILED_PANE_MSG_FG: (u8, u8, u8) = (204, 120, 120);
const FAILED_PANE_HINT_FG: (u8, u8, u8) = (140, 140, 160);
const PANE_OVERLAY_BG: (u8, u8, u8) = (12, 12, 20);
const PANE_OVERLAY_BORDER: (u8, u8, u8) = (100, 100, 120);
const PANE_OVERLAY_TEXT: (u8, u8, u8) = (220, 220, 235);
const SCROLLBAR_TRACK: (u8, u8, u8) = (46, 46, 54);
const SCROLLBAR_THUMB: (u8, u8, u8) = (138, 138, 148);
const SCROLLBAR_THUMB_HOVER: (u8, u8, u8) = (178, 178, 188);
const SCROLLBAR_THIN_WIDTH: f32 = 2.0;
const SCROLLBAR_THICK_WIDTH: f32 = 10.0;
const SCROLLBAR_RIGHT_INSET: f32 = 2.0;
const SCROLLBAR_MIN_THUMB: f32 = 28.0;

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
// Keep device-pixel-snapped pane viewports, but preserve the original
// DirectWrite cell advance behavior. GDI_CLASSIC + CLIP regressed dense box
// drawing on fixed-grid TUI surfaces like the FrankenTUI showcase.
const TEXT_MEASURING_MODE: DWRITE_MEASURING_MODE = DWRITE_MEASURING_MODE_NATURAL;
const TEXT_DRAW_OPTIONS: D2D1_DRAW_TEXT_OPTIONS = D2D1_DRAW_TEXT_OPTIONS_NONE;

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
    tf_overlay: IDWriteTextFormat,
    cell_width: f32,
    cell_height: f32,
}

/// Configuration for creating a [`TerminalRenderer`].
pub struct RendererConfig {
    pub font_family: String,
    pub font_size: f32,
    /// Use software rendering. Slower, but the render target is GDI-compatible
    /// which allows pixel capture via `BitBlt`/`GetDIBits`.
    pub software_rendering: bool,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            font_family: "Cascadia Mono".to_string(),
            font_size: 14.0,
            software_rendering: false,
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
    rgba_to_d2d(r, g, b, 1.0)
}

fn rgba_to_d2d(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
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

fn cell_display_cols(cell: &Cell) -> usize {
    if cell.attrs.is_wide() {
        2
    } else {
        1
    }
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

        let rt_props = if config.software_rendering {
            D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                ..Default::default()
            }
        } else {
            D2D1_RENDER_TARGET_PROPERTIES::default()
        };
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
        let tf_overlay = unsafe {
            dw_factory.CreateTextFormat(
                font_pcwstr,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                (config.font_size - 2.0).max(9.0),
                w!("en-us"),
            )?
        };

        unsafe {
            for tf in [
                &tf_regular,
                &tf_bold,
                &tf_italic,
                &tf_bold_italic,
                &tf_overlay,
            ] {
                tf.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                tf.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
                tf.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            }
        }

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
            tf_overlay,
            cell_width,
            cell_height,
        })
    }

    /// Cell dimensions in pixels.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_width, self.cell_height)
    }

    /// Access the underlying render target (for compositing with other
    /// components such as the tab strip).
    pub fn render_target(&self) -> &ID2D1RenderTarget {
        &self.rt
    }

    /// Access the DirectWrite factory (for creating text formats in other
    /// components such as the tab strip).
    pub fn dw_factory(&self) -> &IDWriteFactory {
        &self.dw_factory
    }

    /// Resize the render target to match a new window size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let size = D2D_SIZE_U { width, height };
        unsafe { self.hwnd_rt.Resize(&size) }
    }

    /// Begin a Direct2D draw session.
    pub fn begin_draw(&self) {
        unsafe {
            self.rt.BeginDraw();
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
    }

    /// Clear the render target to the default terminal background color.
    pub fn clear_background(&self) {
        let bg = rgb_to_d2d(DEFAULT_BG.0, DEFAULT_BG.1, DEFAULT_BG.2);
        unsafe {
            self.rt.Clear(Some(&bg));
        }
    }

    /// End the Direct2D draw session and present.
    pub fn end_draw(&self) -> Result<()> {
        unsafe { self.rt.EndDraw(None, None) }
    }

    /// Paint the terminal content at a vertical offset.
    ///
    /// Use this when compositing with other UI elements (e.g. a tab strip
    /// above the terminal content). Pass `y_offset = 0.0` for no offset.
    pub fn paint_screen(&self, screen: &ScreenBuffer, y_offset: f32) -> Result<()> {
        let rows = screen.rows();
        let cols = screen.cols();
        unsafe {
            for row in 0..rows {
                let y = y_offset + row as f32 * self.cell_height;
                self.paint_row_backgrounds(screen, row, cols, 0.0, y)?;
                self.paint_row_text(screen, row, cols, 0.0, y)?;
            }
            let cursor = screen.cursor();
            if cursor.visible && cursor.row < rows && cursor.col < cols {
                self.paint_shaped_cursor(cursor, 0.0, y_offset)?;
            }
        }
        Ok(())
    }

    /// Paint the contents of a [`ScreenBuffer`] to the window.
    ///
    /// Convenience method that calls [`begin_draw`], [`clear_background`],
    /// [`paint_screen`], and [`end_draw`] in sequence. For compositing with
    /// other components, use those methods individually.
    pub fn paint(&self, screen: &ScreenBuffer) -> Result<()> {
        self.begin_draw();
        self.clear_background();
        let paint_result = self.paint_screen(screen, 0.0);
        let end_result = self.end_draw();
        paint_result?;
        end_result
    }

    /// Paint a [`ScreenBuffer`] clipped to a pane viewport rectangle.
    ///
    /// The viewport is specified as pixel coordinates `(x, y, width, height)`.
    /// A D2D axis-aligned clip rect confines all drawing to this area.
    /// Only the rows/columns visible within the viewport are rendered.
    /// An optional [`TextSelection`] highlights selected cells.
    pub fn paint_pane_viewport(
        &self,
        screen: &ScreenBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        selection: Option<&TextSelection>,
    ) -> Result<()> {
        self.paint_pane_viewport_scrolled(screen, x, y, width, height, selection, 0)
    }

    /// Paint a [`ScreenBuffer`] clipped to a pane viewport rectangle with an
    /// optional scrollback offset in rows.
    pub fn paint_pane_viewport_scrolled(
        &self,
        screen: &ScreenBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        selection: Option<&TextSelection>,
        scrollback_offset: usize,
    ) -> Result<()> {
        let (x, y, width, height) = snap_viewport(x, y, width, height);
        let clip = D2D_RECT_F {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        unsafe {
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        }

        let result =
            self.paint_viewport_inner(screen, x, y, width, height, selection, scrollback_offset);

        unsafe {
            self.rt.PopAxisAlignedClip();
        }
        result
    }

    /// Paint a failed or exited pane overlay within a viewport rectangle.
    ///
    /// Displays a centered status message (e.g. "Session exited (code 0)" or
    /// "Session failed: error") and a restart hint below it. The pane area is
    /// filled with a dark background. The viewport is clipped via a D2D
    /// axis-aligned clip rect, just like [`paint_pane_viewport`].
    pub fn paint_failed_pane(
        &self,
        message: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<()> {
        let (x, y, width, height) = snap_viewport(x, y, width, height);
        let clip = D2D_RECT_F {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        unsafe {
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        }

        let result = self.paint_failed_pane_inner(message, x, y, width, height);

        unsafe {
            self.rt.PopAxisAlignedClip();
        }
        result
    }

    /// Paint a faint monitor-style pane label in the top-right corner.
    pub fn paint_pane_title_overlay(
        &self,
        label: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        is_focused: bool,
    ) -> Result<()> {
        let label = label.trim();
        if label.is_empty() || width < 180.0 || height < self.cell_height * 2.5 {
            return Ok(());
        }

        let (x, y, width, height) = snap_viewport(x, y, width, height);
        let clip = D2D_RECT_F {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        unsafe {
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }

        let result = self.paint_pane_title_overlay_inner(label, x, y, width, is_focused);

        unsafe {
            self.rt.PopAxisAlignedClip();
        }
        result
    }

    fn paint_failed_pane_inner(
        &self,
        message: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<()> {
        let restart_hint = "Press Enter to restart  \u{00B7}  Ctrl+B, r";

        unsafe {
            // Fill background.
            let bg_brush = self.rt.CreateSolidColorBrush(
                &rgb_to_d2d(FAILED_PANE_BG.0, FAILED_PANE_BG.1, FAILED_PANE_BG.2),
                None,
            )?;
            let bg_rect = D2D_RECT_F {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            };
            self.rt.FillRectangle(&bg_rect, &bg_brush);

            // Measure the message text.
            let msg_utf16: Vec<u16> = message.encode_utf16().collect();
            let msg_layout =
                self.dw_factory
                    .CreateTextLayout(&msg_utf16, &self.tf_regular, width, height)?;
            let mut msg_metrics = DWRITE_TEXT_METRICS::default();
            msg_layout.GetMetrics(&mut msg_metrics)?;

            // Measure the hint text.
            let hint_utf16: Vec<u16> = restart_hint.encode_utf16().collect();
            let hint_layout =
                self.dw_factory
                    .CreateTextLayout(&hint_utf16, &self.tf_regular, width, height)?;
            let mut hint_metrics = DWRITE_TEXT_METRICS::default();
            hint_layout.GetMetrics(&mut hint_metrics)?;

            // Vertical center: both lines as a block with a small gap.
            let line_gap = self.cell_height * 0.5;
            let total_text_height = msg_metrics.height + line_gap + hint_metrics.height;
            let top_y = y + (height - total_text_height) / 2.0;

            // Draw message (centered horizontally).
            let msg_x = x + (width - msg_metrics.width) / 2.0;
            let msg_brush = self.rt.CreateSolidColorBrush(
                &rgb_to_d2d(
                    FAILED_PANE_MSG_FG.0,
                    FAILED_PANE_MSG_FG.1,
                    FAILED_PANE_MSG_FG.2,
                ),
                None,
            )?;
            let msg_rect = D2D_RECT_F {
                left: msg_x,
                top: top_y,
                right: msg_x + msg_metrics.width,
                bottom: top_y + msg_metrics.height,
            };
            self.rt.DrawText(
                &msg_utf16,
                &self.tf_regular,
                &msg_rect,
                &msg_brush,
                TEXT_DRAW_OPTIONS,
                TEXT_MEASURING_MODE,
            );

            // Draw restart hint (centered horizontally, below message).
            let hint_y = top_y + msg_metrics.height + line_gap;
            let hint_x = x + (width - hint_metrics.width) / 2.0;
            let hint_brush = self.rt.CreateSolidColorBrush(
                &rgb_to_d2d(
                    FAILED_PANE_HINT_FG.0,
                    FAILED_PANE_HINT_FG.1,
                    FAILED_PANE_HINT_FG.2,
                ),
                None,
            )?;
            let hint_rect = D2D_RECT_F {
                left: hint_x,
                top: hint_y,
                right: hint_x + hint_metrics.width,
                bottom: hint_y + hint_metrics.height,
            };
            self.rt.DrawText(
                &hint_utf16,
                &self.tf_regular,
                &hint_rect,
                &hint_brush,
                TEXT_DRAW_OPTIONS,
                TEXT_MEASURING_MODE,
            );
        }
        Ok(())
    }

    fn paint_pane_title_overlay_inner(
        &self,
        label: &str,
        x: f32,
        y: f32,
        width: f32,
        is_focused: bool,
    ) -> Result<()> {
        let overlay_margin = (self.cell_width * 0.15).max(1.0);
        let horizontal_padding = (self.cell_width * 0.45).max(6.0);
        let vertical_padding = (self.cell_height * 0.15).max(2.0);
        let max_label_width = (width * 0.35).clamp(84.0, 180.0);
        let label_utf16: Vec<u16> = label.encode_utf16().collect();

        unsafe {
            let text_layout = self.dw_factory.CreateTextLayout(
                &label_utf16,
                &self.tf_overlay,
                max_label_width,
                self.cell_height * 1.2,
            )?;
            text_layout.SetMaxWidth(max_label_width)?;
            text_layout.SetMaxHeight(self.cell_height * 1.2)?;
            text_layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            text_layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;

            let mut text_metrics = DWRITE_TEXT_METRICS::default();
            text_layout.GetMetrics(&mut text_metrics)?;

            let bubble_width = (text_metrics.width + horizontal_padding * 2.0)
                .clamp(72.0, max_label_width + horizontal_padding * 2.0);
            let bubble_height = text_metrics.height + vertical_padding * 2.0;
            let bubble_left = x + width - bubble_width - overlay_margin;
            let bubble_top = y + overlay_margin;
            let bubble_rect = D2D_RECT_F {
                left: bubble_left,
                top: bubble_top,
                right: bubble_left + bubble_width,
                bottom: bubble_top + bubble_height,
            };

            let bg_alpha = if is_focused { 0.48 } else { 0.32 };
            let border_alpha = if is_focused { 0.30 } else { 0.18 };
            let text_alpha = if is_focused { 0.74 } else { 0.56 };
            let bg_brush = self.rt.CreateSolidColorBrush(
                &rgba_to_d2d(
                    PANE_OVERLAY_BG.0,
                    PANE_OVERLAY_BG.1,
                    PANE_OVERLAY_BG.2,
                    bg_alpha,
                ),
                None,
            )?;
            let border_brush = self.rt.CreateSolidColorBrush(
                &rgba_to_d2d(
                    PANE_OVERLAY_BORDER.0,
                    PANE_OVERLAY_BORDER.1,
                    PANE_OVERLAY_BORDER.2,
                    border_alpha,
                ),
                None,
            )?;
            let text_brush = self.rt.CreateSolidColorBrush(
                &rgba_to_d2d(
                    PANE_OVERLAY_TEXT.0,
                    PANE_OVERLAY_TEXT.1,
                    PANE_OVERLAY_TEXT.2,
                    text_alpha,
                ),
                None,
            )?;

            self.rt.FillRectangle(&bubble_rect, &bg_brush);
            self.rt
                .DrawRectangle(&bubble_rect, &border_brush, 1.0, None);

            let text_rect = D2D_RECT_F {
                left: bubble_left + horizontal_padding,
                top: bubble_top + vertical_padding,
                right: bubble_rect.right - horizontal_padding,
                bottom: bubble_rect.bottom - vertical_padding,
            };
            self.rt.DrawText(
                &label_utf16,
                &self.tf_overlay,
                &text_rect,
                &text_brush,
                TEXT_DRAW_OPTIONS,
                TEXT_MEASURING_MODE,
            );
        }
        Ok(())
    }

    fn paint_viewport_inner(
        &self,
        screen: &ScreenBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        selection: Option<&TextSelection>,
        scrollback_offset: usize,
    ) -> Result<()> {
        let rows = screen.rows();
        let cols = screen.cols();

        // Only render the rows/cols that fit in the viewport.
        let visible_rows = ((height / self.cell_height).ceil() as usize).min(rows);
        let visible_cols = ((width / self.cell_width).ceil() as usize).min(cols);
        let base_row = screen.scrollback_len().saturating_sub(scrollback_offset);

        unsafe {
            // Make pane rendering self-contained. This clears stale cell
            // backgrounds when a TUI repaints with default-colored cells or
            // when viewport dimensions shift during resize.
            let bg_brush = self.rt.CreateSolidColorBrush(
                &rgb_to_d2d(DEFAULT_BG.0, DEFAULT_BG.1, DEFAULT_BG.2),
                None,
            )?;
            let bg_rect = D2D_RECT_F {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            };
            self.rt.FillRectangle(&bg_rect, &bg_brush);

            for row in 0..visible_rows {
                let py = y + row as f32 * self.cell_height;
                self.paint_row_backgrounds(screen, base_row + row, visible_cols, x, py)?;
                self.paint_row_text(screen, base_row + row, visible_cols, x, py)?;
            }

            // Selection highlight.
            if let Some(sel) = selection {
                self.paint_selection(sel, x, y, visible_rows, visible_cols)?;
            }

            // Cursor.
            let cursor = screen.cursor();
            if scrollback_offset == 0
                && cursor.visible
                && cursor.row < visible_rows
                && cursor.col < visible_cols
            {
                self.paint_shaped_cursor(cursor, x, y)?;
            }
        }
        Ok(())
    }

    /// Paint a Windows Terminal-style scrollback indicator for a pane.
    ///
    /// The collapsed state draws only a thin thumb at the right edge. The
    /// expanded state draws a subtle track plus a thicker thumb.
    pub fn paint_scrollback_scrollbar(
        &self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        scrollback_rows: usize,
        screen_rows: usize,
        visible_rows: usize,
        scrollback_offset: usize,
        expanded: bool,
    ) -> Result<()> {
        if scrollback_rows == 0 || visible_rows == 0 || width <= 0.0 || height <= 0.0 {
            return Ok(());
        }

        let total_rows = scrollback_rows + screen_rows;
        let max_scroll = scrollback_rows.max(1);
        let thumb_height = (height * visible_rows as f32 / total_rows as f32)
            .clamp(SCROLLBAR_MIN_THUMB.min(height), height);
        let travel = (height - thumb_height).max(0.0);
        let progress = (max_scroll.saturating_sub(scrollback_offset.min(max_scroll)) as f32)
            / max_scroll as f32;
        let thumb_top = y + travel * progress;
        let bar_width = if expanded {
            SCROLLBAR_THICK_WIDTH
        } else {
            SCROLLBAR_THIN_WIDTH
        }
        .min(width.max(0.0));
        let bar_left = x + width - SCROLLBAR_RIGHT_INSET - bar_width;
        let radius = bar_width * 0.5;

        unsafe {
            if expanded {
                let track_brush = self.rt.CreateSolidColorBrush(
                    &rgb_to_d2d(SCROLLBAR_TRACK.0, SCROLLBAR_TRACK.1, SCROLLBAR_TRACK.2),
                    None,
                )?;
                track_brush.SetOpacity(0.55);
                let track = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: bar_left,
                        top: y,
                        right: bar_left + bar_width,
                        bottom: y + height,
                    },
                    radiusX: radius,
                    radiusY: radius,
                };
                self.rt.FillRoundedRectangle(&track, &track_brush);
            }

            let color = if expanded {
                SCROLLBAR_THUMB_HOVER
            } else {
                SCROLLBAR_THUMB
            };
            let thumb_brush = self
                .rt
                .CreateSolidColorBrush(&rgb_to_d2d(color.0, color.1, color.2), None)?;
            thumb_brush.SetOpacity(if expanded { 0.9 } else { 0.72 });
            let thumb = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: bar_left,
                    top: thumb_top,
                    right: bar_left + bar_width,
                    bottom: thumb_top + thumb_height,
                },
                radiusX: radius,
                radiusY: radius,
            };
            self.rt.FillRoundedRectangle(&thumb, &thumb_brush);
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
        x_origin: f32,
        y: f32,
    ) -> Result<()> {
        let mut col = 0;
        while col < cols {
            let cell = match screen.cell_at_virtual(row, col) {
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

            let run_start = col;
            col += 1;
            while col < cols {
                if let Some(next) = screen.cell_at_virtual(row, col) {
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
                left: x_origin + run_start as f32 * self.cell_width,
                top: y,
                right: x_origin + col as f32 * self.cell_width,
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
        x_origin: f32,
        y: f32,
    ) -> Result<()> {
        let mut col = 0;
        while col < cols {
            let cell = match screen.cell_at_virtual(row, col) {
                Some(c) => c,
                None => {
                    col += 1;
                    continue;
                }
            };
            if cell.attrs.is_wide_continuation() {
                col += 1;
                continue;
            }
            if cell.text.as_str() == " " && cell.attrs == CellAttrs::default() {
                col += 1;
                continue;
            }

            let (fg_rgb, _) = resolve_cell_colors(cell);
            let tf = self.text_format_for_attrs(&cell.attrs);
            let run_start = col;
            let mut run_text = String::new();
            let mut run_cols = 0usize;
            run_text.push_str(cell.text.as_str());
            run_cols += cell_display_cols(cell);

            col += 1;
            // Extend the run while color and font match.
            while col < cols {
                if let Some(next) = screen.cell_at_virtual(row, col) {
                    if next.attrs.is_wide_continuation() {
                        col += 1;
                        continue;
                    }
                    let (next_fg, _) = resolve_cell_colors(next);
                    let next_tf_matches = self.attrs_same_format(&cell.attrs, &next.attrs);
                    if next_fg == fg_rgb && next_tf_matches {
                        run_text.push_str(next.text.as_str());
                        run_cols += cell_display_cols(next);
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
                left: x_origin + run_start as f32 * self.cell_width,
                top: y,
                right: x_origin + (run_start + run_cols) as f32 * self.cell_width,
                bottom: y + self.cell_height,
            };
            self.rt.DrawText(
                &utf16,
                tf,
                &rect,
                &brush,
                TEXT_DRAW_OPTIONS,
                TEXT_MEASURING_MODE,
            );

            if cell.attrs.is_set(CellAttrs::UNDERLINE) {
                let underline_y = y + self.cell_height - 2.0;
                let p0 = D2D_POINT_2F {
                    x: x_origin + run_start as f32 * self.cell_width,
                    y: underline_y,
                };
                let p1 = D2D_POINT_2F {
                    x: x_origin + (run_start + run_cols) as f32 * self.cell_width,
                    y: underline_y,
                };
                self.rt.DrawLine(p0, p1, &brush, 1.0, None);
            }

            if cell.attrs.is_set(CellAttrs::STRIKETHROUGH) {
                let strike_y = y + self.cell_height / 2.0;
                let p0 = D2D_POINT_2F {
                    x: x_origin + run_start as f32 * self.cell_width,
                    y: strike_y,
                };
                let p1 = D2D_POINT_2F {
                    x: x_origin + (run_start + run_cols) as f32 * self.cell_width,
                    y: strike_y,
                };
                self.rt.DrawLine(p0, p1, &brush, 1.0, None);
            }
        }
        Ok(())
    }

    /// Paint the cursor with shape support at a given origin.
    unsafe fn paint_shaped_cursor(
        &self,
        cursor: &Cursor,
        x_origin: f32,
        y_origin: f32,
    ) -> Result<()> {
        let (r, g, b) = CURSOR_COLOR;
        let brush = self.rt.CreateSolidColorBrush(&rgb_to_d2d(r, g, b), None)?;

        let cell_left = x_origin + cursor.col as f32 * self.cell_width;
        let cell_top = y_origin + cursor.row as f32 * self.cell_height;

        match cursor.shape {
            CursorShape::Block => {
                let rect = D2D_RECT_F {
                    left: cell_left,
                    top: cell_top,
                    right: cell_left + self.cell_width,
                    bottom: cell_top + self.cell_height,
                };
                brush.SetOpacity(0.5);
                self.rt.FillRectangle(&rect, &brush);
            }
            CursorShape::Underline => {
                let thickness = 2.0_f32;
                let rect = D2D_RECT_F {
                    left: cell_left,
                    top: cell_top + self.cell_height - thickness,
                    right: cell_left + self.cell_width,
                    bottom: cell_top + self.cell_height,
                };
                self.rt.FillRectangle(&rect, &brush);
            }
            CursorShape::Bar => {
                let thickness = 2.0_f32;
                let rect = D2D_RECT_F {
                    left: cell_left,
                    top: cell_top,
                    right: cell_left + thickness,
                    bottom: cell_top + self.cell_height,
                };
                self.rt.FillRectangle(&rect, &brush);
            }
        }
        Ok(())
    }

    /// Paint selection highlight rectangles.
    unsafe fn paint_selection(
        &self,
        sel: &TextSelection,
        x_origin: f32,
        y_origin: f32,
        visible_rows: usize,
        visible_cols: usize,
    ) -> Result<()> {
        let (sr, sc, er, ec) = sel.normalised();
        let (r, g, b) = SELECTION_COLOR;
        let brush = self.rt.CreateSolidColorBrush(&rgb_to_d2d(r, g, b), None)?;
        brush.SetOpacity(0.5);

        for row in sr..=er {
            if row >= visible_rows {
                break;
            }
            let col_start = if row == sr { sc } else { 0 };
            let col_end = if row == er {
                (ec + 1).min(visible_cols)
            } else {
                visible_cols
            };
            if col_start >= col_end {
                continue;
            }
            let rect = D2D_RECT_F {
                left: x_origin + col_start as f32 * self.cell_width,
                top: y_origin + row as f32 * self.cell_height,
                right: x_origin + col_end as f32 * self.cell_width,
                bottom: y_origin + (row + 1) as f32 * self.cell_height,
            };
            self.rt.FillRectangle(&rect, &brush);
        }
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

    fn measure_cell(dw: &IDWriteFactory, tf: &IDWriteTextFormat) -> Result<(f32, f32)> {
        let text: Vec<u16> = "M".encode_utf16().collect();
        let layout = unsafe { dw.CreateTextLayout(&text, tf, 1000.0, 1000.0)? };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok((metrics.width, metrics.height))
    }
}

fn snap_viewport(x: f32, y: f32, width: f32, height: f32) -> (f32, f32, f32, f32) {
    let left = x.round();
    let top = y.round();
    let right = (x + width).round().max(left + 1.0);
    let bottom = (y + height).round().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

// ── Failed pane message helpers ──────────────────────────────────────────────

/// Format a message for a pane whose session exited with a given exit code.
pub fn exited_pane_message(exit_code: u32) -> String {
    format!("Session exited (code {exit_code})")
}

/// Format a message for a pane whose session failed to launch.
pub fn failed_pane_message(error: &str) -> String {
    format!("Session failed: {error}")
}

/// The restart hint shown below the status message in failed/exited panes.
pub const RESTART_HINT: &str = "Press Enter to restart  \u{00B7}  Ctrl+B, r";

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_viewport_aligns_fractional_offsets_to_device_pixels() {
        let (x, y, width, height) = snap_viewport(10.4, 20.6, 99.2, 40.2);
        assert_eq!((x, y, width, height), (10.0, 21.0, 100.0, 40.0));
    }

    #[test]
    fn snap_viewport_preserves_minimum_visible_extent() {
        let (x, y, width, height) = snap_viewport(5.49, 7.49, 0.1, 0.1);
        assert_eq!((x, y, width, height), (5.0, 7.0, 1.0, 1.0));
    }

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
        assert_eq!(color_to_rgb(&Color::AnsiBright(7), true), (255, 255, 255));
    }

    #[test]
    fn color_to_rgb_indexed_palette() {
        // Index 0 = black
        assert_eq!(color_to_rgb(&Color::Indexed(0), true), (0, 0, 0));
        // Index 15 = bright white
        assert_eq!(color_to_rgb(&Color::Indexed(15), true), (255, 255, 255));
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
        assert_eq!(
            color_to_rgb(&Color::Rgb(42, 128, 255), true),
            (42, 128, 255)
        );
    }

    #[test]
    fn resolve_colors_normal_cell() {
        let cell = Cell {
            text: CompactText::new("A"),
            fg: Color::Ansi(1),
            bg: Color::Default,
            attrs: CellAttrs::default(),
            hyperlink_id: 0,
            image_id: 0,
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
            text: CompactText::new("A"),
            fg: Color::Ansi(1),
            bg: Color::Ansi(2),
            attrs,
            hyperlink_id: 0,
            image_id: 0,
        };
        let (fg, bg) = resolve_cell_colors(&cell);
        // Swapped
        assert_eq!(fg, (0, 170, 0)); // was bg
        assert_eq!(bg, (170, 0, 0)); // was fg
    }

    #[test]
    fn resolve_colors_dim() {
        let mut attrs = CellAttrs::default();
        attrs.set(CellAttrs::DIM);
        let cell = Cell {
            text: CompactText::new("A"),
            fg: Color::Rgb(200, 100, 50),
            bg: Color::Default,
            attrs,
            hyperlink_id: 0,
            image_id: 0,
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

    #[test]
    fn exited_pane_message_format() {
        assert_eq!(exited_pane_message(0), "Session exited (code 0)");
        assert_eq!(exited_pane_message(1), "Session exited (code 1)");
        assert_eq!(exited_pane_message(255), "Session exited (code 255)");
    }

    #[test]
    fn failed_pane_message_format() {
        assert_eq!(
            failed_pane_message("CreateProcess failed"),
            "Session failed: CreateProcess failed"
        );
        assert_eq!(
            failed_pane_message("profile not found"),
            "Session failed: profile not found"
        );
    }

    #[test]
    fn restart_hint_contains_keybinding() {
        assert!(RESTART_HINT.contains("Enter"));
        assert!(RESTART_HINT.contains("Ctrl+B"));
    }
}
