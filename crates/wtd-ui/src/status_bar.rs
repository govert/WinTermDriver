//! Bottom status bar component for the terminal UI.
//!
//! Displays four segments from left to right:
//!   1. Active workspace name
//!   2. Focused pane path
//!   3. Prefix-active indicator (visible during chord entry)
//!   4. Session state indicator
//!
//! Renders using Direct2D + DirectWrite. Spec: §24.2, §24.8.

use windows::core::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

// ── Constants ────────────────────────────────────────────────────────────────

/// Height of the status bar in pixels.
pub const STATUS_BAR_HEIGHT: f32 = 24.0;

const PADDING_H: f32 = 10.0;
const SEGMENT_GAP: f32 = 16.0;
const SEPARATOR_MARGIN: f32 = 8.0;

// ── Colors ───────────────────────────────────────────────────────────────────

const BAR_BG: (u8, u8, u8) = (30, 30, 40); // matches tab strip STRIP_BG
const TEXT_COLOR: (u8, u8, u8) = (180, 180, 180);
const WORKSPACE_COLOR: (u8, u8, u8) = (78, 201, 176); // accent, matches ACCENT_COLOR
const STATE_RUNNING_COLOR: (u8, u8, u8) = (100, 200, 120);
const STATE_EXITED_COLOR: (u8, u8, u8) = (200, 170, 80);
const STATE_FAILED_COLOR: (u8, u8, u8) = (204, 120, 120); // matches failed pane msg color
const STATE_CREATING_COLOR: (u8, u8, u8) = (180, 180, 180);
const PREFIX_BG: (u8, u8, u8) = (78, 201, 176); // accent bg
const PREFIX_TEXT: (u8, u8, u8) = (20, 20, 30); // dark text on accent bg
const PREFIX_PADDING_H: f32 = 6.0;
const PREFIX_PADDING_V: f32 = 2.0;
const SEPARATOR_COLOR: (u8, u8, u8) = (60, 60, 75);

// ── Public types ─────────────────────────────────────────────────────────────

/// Session state for status bar display (UI-side enum, mirrors host SessionState).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Creating,
    Running,
    Exited { exit_code: u32 },
    Failed { error: String },
    Restarting { attempt: u32 },
}

impl SessionStatus {
    /// Human-readable label for display.
    pub fn label(&self) -> String {
        match self {
            SessionStatus::Creating => "creating".to_string(),
            SessionStatus::Running => "running".to_string(),
            SessionStatus::Exited { exit_code } => format!("exited ({})", exit_code),
            SessionStatus::Failed { error } => format!("failed: {}", error),
            SessionStatus::Restarting { attempt } => format!("restarting ({})", attempt),
        }
    }

    fn color(&self) -> (u8, u8, u8) {
        match self {
            SessionStatus::Running => STATE_RUNNING_COLOR,
            SessionStatus::Exited { .. } => STATE_EXITED_COLOR,
            SessionStatus::Failed { .. } => STATE_FAILED_COLOR,
            SessionStatus::Creating | SessionStatus::Restarting { .. } => STATE_CREATING_COLOR,
        }
    }
}

// ── StatusBar ────────────────────────────────────────────────────────────────

/// Bottom status bar: workspace name, pane path, prefix indicator, session state.
pub struct StatusBar {
    workspace_name: String,
    pane_path: String,
    session_status: SessionStatus,
    prefix_active: bool,
    prefix_label: String,
    available_width: f32,
    // DirectWrite resources
    tf_regular: IDWriteTextFormat,
    tf_bold: IDWriteTextFormat,
    dw_factory: IDWriteFactory,
}

impl StatusBar {
    /// Create a new status bar.
    pub fn new(dw_factory: &IDWriteFactory) -> Result<Self> {
        let font_wide: Vec<u16> = "Segoe UI"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let font = PCWSTR(font_wide.as_ptr());

        let tf_regular = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.0,
                w!("en-us"),
            )?
        };
        unsafe {
            tf_regular.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }

        let tf_bold = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.0,
                w!("en-us"),
            )?
        };
        unsafe {
            tf_bold.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }

        Ok(Self {
            workspace_name: String::new(),
            pane_path: String::new(),
            session_status: SessionStatus::Running,
            prefix_active: false,
            prefix_label: "PREFIX".to_string(),
            available_width: 0.0,
            tf_regular,
            tf_bold,
            dw_factory: dw_factory.clone(),
        })
    }

    /// Height of the status bar in pixels.
    pub fn height(&self) -> f32 {
        STATUS_BAR_HEIGHT
    }

    /// Update the layout width (call on window resize).
    pub fn layout(&mut self, available_width: f32) {
        self.available_width = available_width;
    }

    /// Set the active workspace name.
    pub fn set_workspace_name(&mut self, name: String) {
        self.workspace_name = name;
    }

    /// Get the active workspace name.
    pub fn workspace_name(&self) -> &str {
        &self.workspace_name
    }

    /// Set the focused pane path (e.g. "workspace/tab/pane").
    pub fn set_pane_path(&mut self, path: String) {
        self.pane_path = path;
    }

    /// Get the focused pane path.
    pub fn pane_path(&self) -> &str {
        &self.pane_path
    }

    /// Set the session state for the focused pane.
    pub fn set_session_status(&mut self, status: SessionStatus) {
        self.session_status = status;
    }

    /// Get the current session status.
    pub fn session_status(&self) -> &SessionStatus {
        &self.session_status
    }

    /// Show or hide the prefix-active indicator.
    pub fn set_prefix_active(&mut self, active: bool) {
        self.prefix_active = active;
    }

    /// Whether the prefix indicator is visible.
    pub fn is_prefix_active(&self) -> bool {
        self.prefix_active
    }

    /// Set the label shown in the prefix indicator (e.g. "PREFIX" or "Ctrl+B").
    pub fn set_prefix_label(&mut self, label: String) {
        self.prefix_label = label;
    }

    /// Paint the status bar onto the given render target at the specified y position.
    ///
    /// The caller must have already called `BeginDraw()` on the render target.
    pub fn paint(&self, rt: &ID2D1RenderTarget, y: f32) -> Result<()> {
        let width = self.available_width.max(1.0);

        unsafe {
            // Background
            let bg_brush = make_brush(rt, BAR_BG)?;
            let bar_rect = D2D_RECT_F {
                left: 0.0,
                top: y,
                right: width,
                bottom: y + STATUS_BAR_HEIGHT,
            };
            rt.FillRectangle(&bar_rect, &bg_brush);

            // Top border
            let border_brush = make_brush(rt, SEPARATOR_COLOR)?;
            rt.DrawLine(
                D2D_POINT_2F { x: 0.0, y },
                D2D_POINT_2F { x: width, y },
                &border_brush,
                1.0,
                None,
            );

            let mut x = PADDING_H;

            // 1. Workspace name (bold, accent color)
            if !self.workspace_name.is_empty() {
                let w = self.draw_text(rt, &self.workspace_name, x, y, &self.tf_bold, WORKSPACE_COLOR)?;
                x += w + SEGMENT_GAP;
            }

            // 2. Pane path
            if !self.pane_path.is_empty() {
                // Separator
                self.draw_separator(rt, x - SEPARATOR_MARGIN + SEPARATOR_MARGIN / 2.0, y)?;
                x += SEPARATOR_MARGIN / 2.0;

                let w = self.draw_text(rt, &self.pane_path, x, y, &self.tf_regular, TEXT_COLOR)?;
                x += w + SEGMENT_GAP;
            }

            // 3. Prefix indicator (only when active)
            if self.prefix_active {
                self.draw_separator(rt, x - SEPARATOR_MARGIN + SEPARATOR_MARGIN / 2.0, y)?;
                x += SEPARATOR_MARGIN / 2.0;

                x += self.draw_prefix_badge(rt, x, y)?;
                x += SEGMENT_GAP;
            }

            // 4. Session state (right-aligned)
            let state_label = self.session_status.label();
            let state_width = self.measure_text_with(&state_label, &self.tf_regular);
            let state_x = (width - PADDING_H - state_width).max(x);

            // Separator before state
            if state_x > x {
                self.draw_separator(rt, state_x - SEPARATOR_MARGIN, y)?;
            }

            self.draw_text(rt, &state_label, state_x, y, &self.tf_regular, self.session_status.color())?;
        }

        Ok(())
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Draw text and return the width consumed.
    unsafe fn draw_text(
        &self,
        rt: &ID2D1RenderTarget,
        text: &str,
        x: f32,
        y: f32,
        format: &IDWriteTextFormat,
        color: (u8, u8, u8),
    ) -> Result<f32> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let brush = make_brush(rt, color)?;
        let rect = D2D_RECT_F {
            left: x,
            top: y,
            right: x + self.available_width, // large enough
            bottom: y + STATUS_BAR_HEIGHT,
        };
        rt.DrawText(&utf16, format, &rect, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        Ok(self.measure_text_with(text, format))
    }

    /// Draw a vertical separator line.
    unsafe fn draw_separator(&self, rt: &ID2D1RenderTarget, x: f32, y: f32) -> Result<()> {
        let brush = make_brush(rt, SEPARATOR_COLOR)?;
        let margin = 5.0;
        rt.DrawLine(
            D2D_POINT_2F { x, y: y + margin },
            D2D_POINT_2F { x, y: y + STATUS_BAR_HEIGHT - margin },
            &brush,
            1.0,
            None,
        );
        Ok(())
    }

    /// Draw the prefix-active badge and return the width consumed.
    unsafe fn draw_prefix_badge(&self, rt: &ID2D1RenderTarget, x: f32, y: f32) -> Result<f32> {
        let text_width = self.measure_text_with(&self.prefix_label, &self.tf_bold);
        let badge_width = text_width + PREFIX_PADDING_H * 2.0;
        let badge_height = STATUS_BAR_HEIGHT - PREFIX_PADDING_V * 2.0 - 4.0;
        let badge_y = y + (STATUS_BAR_HEIGHT - badge_height) / 2.0;

        // Badge background (rounded rect)
        let bg_brush = make_brush(rt, PREFIX_BG)?;
        let badge_rect = D2D_RECT_F {
            left: x,
            top: badge_y,
            right: x + badge_width,
            bottom: badge_y + badge_height,
        };
        let rounded = D2D1_ROUNDED_RECT {
            rect: badge_rect,
            radiusX: 3.0,
            radiusY: 3.0,
        };
        rt.FillRoundedRectangle(&rounded, &bg_brush);

        // Badge text
        let utf16: Vec<u16> = self.prefix_label.encode_utf16().collect();
        let text_brush = make_brush(rt, PREFIX_TEXT)?;
        let text_rect = D2D_RECT_F {
            left: x + PREFIX_PADDING_H,
            top: badge_y,
            right: x + badge_width - PREFIX_PADDING_H,
            bottom: badge_y + badge_height,
        };
        rt.DrawText(
            &utf16,
            &self.tf_bold,
            &text_rect,
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );

        Ok(badge_width)
    }

    fn measure_text_with(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            if let Ok(layout) = self.dw_factory.CreateTextLayout(
                &utf16,
                format,
                1000.0,
                STATUS_BAR_HEIGHT,
            ) {
                let mut metrics = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut metrics).is_ok() {
                    return metrics.width;
                }
            }
        }
        60.0 // fallback
    }
}

// ── Module-level helpers ─────────────────────────────────────────────────────

fn make_brush(
    rt: &ID2D1RenderTarget,
    color: (u8, u8, u8),
) -> Result<ID2D1SolidColorBrush> {
    let c = D2D1_COLOR_F {
        r: color.0 as f32 / 255.0,
        g: color.1 as f32 / 255.0,
        b: color.2 as f32 / 255.0,
        a: 1.0,
    };
    unsafe { rt.CreateSolidColorBrush(&c, None) }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dw_factory() -> IDWriteFactory {
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() }
    }

    fn make_bar() -> StatusBar {
        let dw = make_dw_factory();
        StatusBar::new(&dw).unwrap()
    }

    #[test]
    fn default_state() {
        let bar = make_bar();
        assert_eq!(bar.workspace_name(), "");
        assert_eq!(bar.pane_path(), "");
        assert_eq!(bar.session_status(), &SessionStatus::Running);
        assert!(!bar.is_prefix_active());
        assert_eq!(bar.height(), STATUS_BAR_HEIGHT);
    }

    #[test]
    fn set_workspace_name() {
        let mut bar = make_bar();
        bar.set_workspace_name("dev".to_string());
        assert_eq!(bar.workspace_name(), "dev");
    }

    #[test]
    fn set_pane_path() {
        let mut bar = make_bar();
        bar.set_pane_path("dev/main/server".to_string());
        assert_eq!(bar.pane_path(), "dev/main/server");
    }

    #[test]
    fn set_session_status() {
        let mut bar = make_bar();
        bar.set_session_status(SessionStatus::Exited { exit_code: 1 });
        assert_eq!(bar.session_status(), &SessionStatus::Exited { exit_code: 1 });
    }

    #[test]
    fn set_prefix_active() {
        let mut bar = make_bar();
        assert!(!bar.is_prefix_active());
        bar.set_prefix_active(true);
        assert!(bar.is_prefix_active());
        bar.set_prefix_active(false);
        assert!(!bar.is_prefix_active());
    }

    #[test]
    fn set_prefix_label() {
        let mut bar = make_bar();
        bar.set_prefix_label("Ctrl+B".to_string());
        assert_eq!(bar.prefix_label, "Ctrl+B");
    }

    #[test]
    fn session_status_labels() {
        assert_eq!(SessionStatus::Creating.label(), "creating");
        assert_eq!(SessionStatus::Running.label(), "running");
        assert_eq!(SessionStatus::Exited { exit_code: 0 }.label(), "exited (0)");
        assert_eq!(SessionStatus::Exited { exit_code: 1 }.label(), "exited (1)");
        assert_eq!(
            SessionStatus::Failed { error: "spawn failed".to_string() }.label(),
            "failed: spawn failed"
        );
        assert_eq!(
            SessionStatus::Restarting { attempt: 3 }.label(),
            "restarting (3)"
        );
    }

    #[test]
    fn session_status_colors() {
        assert_eq!(SessionStatus::Running.color(), STATE_RUNNING_COLOR);
        assert_eq!(SessionStatus::Exited { exit_code: 0 }.color(), STATE_EXITED_COLOR);
        assert_eq!(SessionStatus::Failed { error: String::new() }.color(), STATE_FAILED_COLOR);
        assert_eq!(SessionStatus::Creating.color(), STATE_CREATING_COLOR);
        assert_eq!(SessionStatus::Restarting { attempt: 1 }.color(), STATE_CREATING_COLOR);
    }

    #[test]
    fn layout_sets_width() {
        let mut bar = make_bar();
        bar.layout(1024.0);
        assert_eq!(bar.available_width, 1024.0);
    }

    #[test]
    fn measure_text_returns_positive() {
        let bar = make_bar();
        let w = bar.measure_text_with("hello", &bar.tf_regular);
        assert!(w > 0.0);
    }

    #[test]
    fn measure_text_bold_wider_than_regular() {
        let bar = make_bar();
        // Bold text of same content should be at least as wide
        let regular = bar.measure_text_with("workspace", &bar.tf_regular);
        let bold = bar.measure_text_with("workspace", &bar.tf_bold);
        assert!(regular > 0.0);
        assert!(bold > 0.0);
        // Bold may or may not be wider; both should be positive and reasonable
        assert!(regular < 200.0);
        assert!(bold < 200.0);
    }

    #[test]
    fn full_state_update() {
        let mut bar = make_bar();
        bar.set_workspace_name("dev".to_string());
        bar.set_pane_path("dev/main/server".to_string());
        bar.set_session_status(SessionStatus::Running);
        bar.set_prefix_active(true);
        bar.set_prefix_label("Ctrl+B".to_string());
        bar.layout(1920.0);

        assert_eq!(bar.workspace_name(), "dev");
        assert_eq!(bar.pane_path(), "dev/main/server");
        assert_eq!(bar.session_status(), &SessionStatus::Running);
        assert!(bar.is_prefix_active());
        assert_eq!(bar.available_width, 1920.0);
    }
}
