//! Tab strip component for the terminal UI.
//!
//! Manages a horizontal strip of named tabs with support for switching,
//! creation, closing, drag-to-reorder, and overflow scrolling.
//! Renders using Direct2D + DirectWrite.

use windows::core::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use wtd_ipc::message::{ProgressInfo, ProgressState};

// ── Constants ────────────────────────────────────────────────────────────────

/// Height of the tab strip in pixels.
pub const TAB_STRIP_HEIGHT: f32 = 32.0;

const TAB_PADDING_H: f32 = 12.0;
const TAB_CLOSE_SIZE: f32 = 16.0;
const TAB_CLOSE_MARGIN: f32 = 6.0;
const TAB_TEXT_TO_CLOSE_GAP: f32 = 10.0;
const TAB_GAP: f32 = 1.0;
const ADD_BUTTON_WIDTH: f32 = 32.0;
const SCROLL_ARROW_WIDTH: f32 = 20.0;
const MIN_TAB_WIDTH: f32 = 80.0;
const MAX_TAB_WIDTH: f32 = 200.0;
const DRAG_THRESHOLD: f32 = 5.0;
const TAB_PROGRESS_SIZE: f32 = 11.0;
const TAB_PROGRESS_GAP: f32 = 8.0;
const TAB_INNER_TOP: f32 = 2.0;
const WORKSPACE_BADGE_PADDING_H: f32 = 12.0;
const WORKSPACE_BADGE_MIN_WIDTH: f32 = 88.0;
const WORKSPACE_BADGE_MAX_WIDTH: f32 = 220.0;
const WORKSPACE_GAP: f32 = 10.0;
const WINDOW_BUTTON_WIDTH: f32 = 44.0;
const WINDOW_BUTTON_COUNT: usize = 3;
const WINDOW_BUTTON_TOTAL_WIDTH: f32 = WINDOW_BUTTON_WIDTH * WINDOW_BUTTON_COUNT as f32;

// ── Colors ───────────────────────────────────────────────────────────────────

const STRIP_BG: (u8, u8, u8) = (30, 30, 40);
const TAB_INACTIVE_BG: (u8, u8, u8) = (45, 45, 58);
const TAB_ACTIVE_BG: (u8, u8, u8) = (26, 26, 38); // matches terminal DEFAULT_BG
const TAB_HOVER_BG: (u8, u8, u8) = (55, 55, 70);
const TAB_TEXT_COLOR: (u8, u8, u8) = (180, 180, 180);
const TAB_ACTIVE_TEXT: (u8, u8, u8) = (230, 230, 230);
const ACCENT_COLOR: (u8, u8, u8) = (78, 201, 176);
const CLOSE_NORMAL_COLOR: (u8, u8, u8) = (232, 232, 240);
const CLOSE_HOVER_COLOR: (u8, u8, u8) = (255, 85, 85);
const ADD_TEXT_COLOR: (u8, u8, u8) = (150, 150, 150);
const ADD_HOVER_COLOR: (u8, u8, u8) = (230, 230, 230);
const WORKSPACE_TEXT_COLOR: (u8, u8, u8) = ACCENT_COLOR;
const WINDOW_BUTTON_HOVER_BG: (u8, u8, u8) = (55, 55, 70);
const WINDOW_CLOSE_HOVER_BG: (u8, u8, u8) = (196, 70, 70);
const WINDOW_BUTTON_TEXT: (u8, u8, u8) = (178, 178, 188);

// ── Public types ─────────────────────────────────────────────────────────────

/// A single tab entry.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    pub name: String,
    pub progress: Option<ProgressInfo>,
}

/// Action resulting from user interaction with the tab strip.
#[derive(Debug, Clone, PartialEq)]
pub enum TabAction {
    /// Switch to the tab at the given index.
    SwitchTo(usize),
    /// A tab was closed at the given index.
    Close(usize),
    /// Create a new tab.
    Create,
    /// Minimize the window.
    MinimizeWindow,
    /// Toggle maximized/restored state.
    ToggleMaximizeWindow,
    /// Reorder: tab moved from one index to another.
    Reorder { from: usize, to: usize },
    /// The last tab was closed — the window should close.
    WindowClose,
}

// ── Internal types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct HitRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl HitRect {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

#[derive(Debug, Clone)]
struct TabZone {
    rect: HitRect,
    close_rect: HitRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowButtonKind {
    Minimize,
    MaximizeRestore,
    Close,
}

#[derive(Debug)]
struct DragState {
    tab_index: usize,
    start_x: f32,
    current_x: f32,
    active: bool,
}

// ── TabStrip ─────────────────────────────────────────────────────────────────

/// Tab strip component: manages tabs and renders them using Direct2D.
pub struct TabStrip {
    tabs: Vec<Tab>,
    active_index: usize,
    next_id: u64,
    workspace_name: String,
    window_maximized: bool,
    // Layout zones (recomputed by `layout()`)
    zones: Vec<TabZone>,
    workspace_zone: HitRect,
    add_zone: HitRect,
    scroll_left_zone: Option<HitRect>,
    scroll_right_zone: Option<HitRect>,
    minimize_zone: HitRect,
    maximize_zone: HitRect,
    close_window_zone: HitRect,
    // Visual state
    hover_tab: Option<usize>,
    hover_close: Option<usize>,
    hover_add: bool,
    hover_window_button: Option<WindowButtonKind>,
    // Drag state
    drag: Option<DragState>,
    // Scroll
    scroll_offset: f32,
    total_tabs_width: f32,
    available_width: f32,
    // DirectWrite resources
    tf_tab: IDWriteTextFormat,
    tf_tab_bold: IDWriteTextFormat,
    tf_close: IDWriteTextFormat,
    dw_factory: IDWriteFactory,
}

impl TabStrip {
    /// Create a new empty tab strip.
    pub fn new(dw_factory: &IDWriteFactory) -> Result<Self> {
        let font_wide: Vec<u16> = "Segoe UI"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let font = PCWSTR(font_wide.as_ptr());

        let tf_tab = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-us"),
            )?
        };
        unsafe {
            tf_tab.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }

        let tf_tab_bold = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-us"),
            )?
        };
        unsafe {
            tf_tab_bold.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }

        let tf_close = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.5,
                w!("en-us"),
            )?
        };
        unsafe {
            tf_close.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            tf_close.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }

        Ok(Self {
            tabs: Vec::new(),
            active_index: 0,
            next_id: 1,
            workspace_name: String::new(),
            window_maximized: false,
            zones: Vec::new(),
            workspace_zone: HitRect::default(),
            add_zone: HitRect::default(),
            scroll_left_zone: None,
            scroll_right_zone: None,
            minimize_zone: HitRect::default(),
            maximize_zone: HitRect::default(),
            close_window_zone: HitRect::default(),
            hover_tab: None,
            hover_close: None,
            hover_add: false,
            hover_window_button: None,
            drag: None,
            scroll_offset: 0.0,
            total_tabs_width: 0.0,
            available_width: 0.0,
            tf_tab,
            tf_tab_bold,
            tf_close,
            dw_factory: dw_factory.clone(),
        })
    }

    /// Height of the tab strip in pixels.
    pub fn height(&self) -> f32 {
        TAB_STRIP_HEIGHT
    }

    /// Number of tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Get all tabs.
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn tab_index_at(&self, x: f32, y: f32) -> Option<usize> {
        if y < 0.0 || y >= TAB_STRIP_HEIGHT {
            return None;
        }
        self.zones.iter().position(|zone| zone.rect.contains(x, y))
    }

    /// Get the active tab, if any.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_index)
    }

    /// Get the active tab index.
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// Add a new tab with the given name. Returns the tab's unique ID.
    pub fn add_tab(&mut self, name: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            name,
            progress: None,
        });
        id
    }

    /// Set the progress indicator for a tab by index.
    pub fn set_progress(&mut self, index: usize, progress: Option<ProgressInfo>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.progress = progress;
        }
    }

    /// Set the active tab by index. No-op if out of bounds.
    pub fn set_active(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_index = index;
        }
    }

    /// Set the workspace name shown on the left side of the chrome strip.
    pub fn set_workspace_name(&mut self, name: String) {
        self.workspace_name = name;
    }

    /// Update whether the host window is currently maximized.
    pub fn set_window_maximized(&mut self, maximized: bool) {
        self.window_maximized = maximized;
    }

    /// Close a tab by index. Returns the resulting action.
    ///
    /// If this was the last tab, returns [`TabAction::WindowClose`].
    pub fn close_tab(&mut self, index: usize) -> TabAction {
        if index >= self.tabs.len() {
            return TabAction::Close(index);
        }
        if self.tabs.len() <= 1 {
            self.tabs.clear();
            self.active_index = 0;
            return TabAction::WindowClose;
        }
        self.tabs.remove(index);
        if self.active_index >= self.tabs.len() {
            self.active_index = self.tabs.len() - 1;
        } else if self.active_index > index {
            self.active_index -= 1;
        }
        TabAction::Close(index)
    }

    /// Reorder: move tab from `from` to `to`.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Keep active_index following the active tab
        if self.active_index == from {
            self.active_index = to;
        } else if from < self.active_index && to >= self.active_index {
            self.active_index -= 1;
        } else if from > self.active_index && to <= self.active_index {
            self.active_index += 1;
        }
    }

    /// Generate a window title: "workspace \u{2014} tab_name".
    pub fn window_title(&self, workspace_name: &str) -> String {
        match self.active_tab() {
            Some(tab) => format!("{} \u{2014} {}", workspace_name, tab.name),
            None => workspace_name.to_string(),
        }
    }

    /// Whether a drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().map_or(false, |d| d.active)
    }

    /// Return true when a point hits an interactive tab-strip target.
    pub fn hits_interactive_target(&self, x: f32, y: f32) -> bool {
        if y < 0.0 || y >= TAB_STRIP_HEIGHT {
            return false;
        }

        if self.add_zone.contains(x, y) {
            return true;
        }
        if self.minimize_zone.contains(x, y)
            || self.maximize_zone.contains(x, y)
            || self.close_window_zone.contains(x, y)
        {
            return true;
        }
        if self
            .scroll_left_zone
            .as_ref()
            .is_some_and(|zone| zone.contains(x, y))
            || self
                .scroll_right_zone
                .as_ref()
                .is_some_and(|zone| zone.contains(x, y))
        {
            return true;
        }

        self.zones
            .iter()
            .any(|zone| zone.rect.contains(x, y) || zone.close_rect.contains(x, y))
    }

    // ── Layout ───────────────────────────────────────────────────────────────

    /// Recompute layout zones for the current set of tabs.
    ///
    /// Call after adding/removing tabs, reordering, or resizing the window.
    pub fn layout(&mut self, available_width: f32) {
        self.available_width = available_width;
        self.zones.clear();

        let workspace_width = if self.workspace_name.is_empty() {
            0.0
        } else {
            (self.measure_text(&self.workspace_name) + WORKSPACE_BADGE_PADDING_H * 2.0)
                .clamp(WORKSPACE_BADGE_MIN_WIDTH, WORKSPACE_BADGE_MAX_WIDTH)
        };
        self.workspace_zone = HitRect {
            x: 0.0,
            y: 0.0,
            width: workspace_width,
            height: TAB_STRIP_HEIGHT,
        };
        let buttons_left = (available_width - WINDOW_BUTTON_TOTAL_WIDTH).max(0.0);
        self.minimize_zone = HitRect {
            x: buttons_left,
            y: 0.0,
            width: WINDOW_BUTTON_WIDTH,
            height: TAB_STRIP_HEIGHT,
        };
        self.maximize_zone = HitRect {
            x: buttons_left + WINDOW_BUTTON_WIDTH,
            y: 0.0,
            width: WINDOW_BUTTON_WIDTH,
            height: TAB_STRIP_HEIGHT,
        };
        self.close_window_zone = HitRect {
            x: buttons_left + WINDOW_BUTTON_WIDTH * 2.0,
            y: 0.0,
            width: WINDOW_BUTTON_WIDTH,
            height: TAB_STRIP_HEIGHT,
        };

        let left_inset = if workspace_width > 0.0 {
            workspace_width + WORKSPACE_GAP
        } else {
            0.0
        };
        let right_inset = WINDOW_BUTTON_TOTAL_WIDTH;

        if self.tabs.is_empty() {
            self.add_zone = HitRect {
                x: left_inset + TAB_GAP,
                y: 0.0,
                width: ADD_BUTTON_WIDTH,
                height: TAB_STRIP_HEIGHT,
            };
            self.total_tabs_width = ADD_BUTTON_WIDTH + TAB_GAP;
            self.scroll_left_zone = None;
            self.scroll_right_zone = None;
            return;
        }

        // Measure text widths
        let text_widths: Vec<f32> = self
            .tabs
            .iter()
            .map(|t| {
                let indicator_width = if t.progress.is_some() {
                    TAB_PROGRESS_SIZE + TAB_PROGRESS_GAP
                } else {
                    0.0
                };
                self.measure_text(&t.name) + indicator_width
            })
            .collect();

        // Compute tab widths (text + padding + close button)
        let tab_widths: Vec<f32> = text_widths
            .iter()
            .map(|tw| {
                (tw + 2.0 * TAB_PADDING_H + TAB_CLOSE_SIZE + TAB_CLOSE_MARGIN)
                    .clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH)
            })
            .collect();

        let total: f32 = tab_widths.iter().sum::<f32>()
            + (tab_widths.len().saturating_sub(1)) as f32 * TAB_GAP
            + ADD_BUTTON_WIDTH
            + TAB_GAP;
        self.total_tabs_width = total;

        let content_width = (available_width - left_inset - right_inset).max(0.0);
        let needs_scroll = total > content_width;
        let content_left;
        let content_right;

        if needs_scroll {
            content_left = left_inset + SCROLL_ARROW_WIDTH;
            content_right = available_width - right_inset - SCROLL_ARROW_WIDTH;
            self.scroll_left_zone = Some(HitRect {
                x: left_inset,
                y: 0.0,
                width: SCROLL_ARROW_WIDTH,
                height: TAB_STRIP_HEIGHT,
            });
            self.scroll_right_zone = Some(HitRect {
                x: available_width - right_inset - SCROLL_ARROW_WIDTH,
                y: 0.0,
                width: SCROLL_ARROW_WIDTH,
                height: TAB_STRIP_HEIGHT,
            });
            let max_scroll = total - (content_right - content_left);
            self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll.max(0.0));
        } else {
            content_left = left_inset;
            self.scroll_left_zone = None;
            self.scroll_right_zone = None;
            self.scroll_offset = 0.0;
        }

        // Position tabs
        let mut x = content_left - self.scroll_offset;
        for tab_w in &tab_widths {
            let close_x = x + tab_w - TAB_CLOSE_MARGIN - TAB_CLOSE_SIZE;
            let close_y = TAB_INNER_TOP + (TAB_STRIP_HEIGHT - TAB_INNER_TOP - TAB_CLOSE_SIZE) / 2.0;

            self.zones.push(TabZone {
                rect: HitRect {
                    x,
                    y: 0.0,
                    width: *tab_w,
                    height: TAB_STRIP_HEIGHT,
                },
                close_rect: HitRect {
                    x: close_x,
                    y: close_y,
                    width: TAB_CLOSE_SIZE,
                    height: TAB_CLOSE_SIZE,
                },
            });

            x += tab_w + TAB_GAP;
        }

        // "+" button after last tab
        self.add_zone = HitRect {
            x,
            y: 0.0,
            width: ADD_BUTTON_WIDTH,
            height: TAB_STRIP_HEIGHT,
        };
    }

    // ── Mouse interaction ────────────────────────────────────────────────────

    /// Handle a mouse-down event. Returns an action if one is triggered.
    pub fn on_mouse_down(&mut self, x: f32, y: f32) -> Option<TabAction> {
        if y < 0.0 || y >= TAB_STRIP_HEIGHT {
            return None;
        }

        // Scroll arrows
        if let Some(ref zone) = self.scroll_left_zone {
            if zone.contains(x, y) {
                self.scroll_offset = (self.scroll_offset - 60.0).max(0.0);
                self.layout(self.available_width);
                return None;
            }
        }
        if let Some(ref zone) = self.scroll_right_zone {
            if zone.contains(x, y) {
                self.scroll_offset += 60.0;
                self.layout(self.available_width);
                return None;
            }
        }

        // "+" button
        if self.minimize_zone.contains(x, y) {
            return Some(TabAction::MinimizeWindow);
        }
        if self.maximize_zone.contains(x, y) {
            return Some(TabAction::ToggleMaximizeWindow);
        }
        if self.close_window_zone.contains(x, y) {
            return Some(TabAction::WindowClose);
        }

        // "+" button
        if self.add_zone.contains(x, y) {
            return Some(TabAction::Create);
        }

        // Tabs: check close button first, then tab body
        for (i, zone) in self.zones.iter().enumerate() {
            if zone.close_rect.contains(x, y) {
                return Some(TabAction::Close(i));
            }
            if zone.rect.contains(x, y) {
                self.drag = Some(DragState {
                    tab_index: i,
                    start_x: x,
                    current_x: x,
                    active: false,
                });
                if i != self.active_index {
                    return Some(TabAction::SwitchTo(i));
                }
                return None;
            }
        }

        None
    }

    /// Handle a mouse-move event. Returns an action if drag reorder completes.
    pub fn on_mouse_move(&mut self, x: f32, y: f32) -> Option<TabAction> {
        // Update hover state
        self.hover_tab = None;
        self.hover_close = None;
        self.hover_add = false;
        self.hover_window_button = None;

        if y >= 0.0 && y < TAB_STRIP_HEIGHT {
            if self.add_zone.contains(x, y) {
                self.hover_add = true;
            }
            if self.minimize_zone.contains(x, y) {
                self.hover_window_button = Some(WindowButtonKind::Minimize);
            } else if self.maximize_zone.contains(x, y) {
                self.hover_window_button = Some(WindowButtonKind::MaximizeRestore);
            } else if self.close_window_zone.contains(x, y) {
                self.hover_window_button = Some(WindowButtonKind::Close);
            }
            for (i, zone) in self.zones.iter().enumerate() {
                if zone.close_rect.contains(x, y) {
                    self.hover_close = Some(i);
                    self.hover_tab = Some(i);
                    break;
                }
                if zone.rect.contains(x, y) {
                    self.hover_tab = Some(i);
                    break;
                }
            }
        }

        // Handle drag
        if let Some(ref mut drag) = self.drag {
            drag.current_x = x;
            if !drag.active && (x - drag.start_x).abs() > DRAG_THRESHOLD {
                drag.active = true;
            }
        }

        None
    }

    /// Handle a mouse-up event. Returns an action if drag reorder completes.
    pub fn on_mouse_up(&mut self, x: f32, _y: f32) -> Option<TabAction> {
        if let Some(drag) = self.drag.take() {
            if drag.active {
                let drop_index = self.drop_index_at(x);
                if drop_index != drag.tab_index {
                    self.reorder(drag.tab_index, drop_index);
                    return Some(TabAction::Reorder {
                        from: drag.tab_index,
                        to: drop_index,
                    });
                }
            }
        }
        None
    }

    /// Handle mouse leaving the window.
    pub fn on_mouse_leave(&mut self) {
        self.hover_tab = None;
        self.hover_close = None;
        self.hover_add = false;
        self.hover_window_button = None;
        self.drag = None;
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Paint the tab strip onto the given render target.
    ///
    /// The caller must have already called `BeginDraw()` on the render target.
    pub fn paint(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        unsafe {
            // Strip background
            let strip_bg = make_brush(rt, STRIP_BG)?;
            let strip_rect = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: self.available_width.max(1.0),
                bottom: TAB_STRIP_HEIGHT,
            };
            rt.FillRectangle(&strip_rect, &strip_bg);

            // Tabs
            if self.workspace_zone.width > 0.0 {
                self.paint_workspace_badge(rt)?;
            }
            for (i, zone) in self.zones.iter().enumerate() {
                self.paint_tab(rt, i, zone)?;
            }

            // "+" button
            self.paint_add_button(rt)?;

            // Scroll arrows (painted on top so they cover partially-visible tabs)
            if let Some(ref zone) = self.scroll_left_zone {
                self.paint_scroll_arrow(rt, zone, true)?;
            }
            if let Some(ref zone) = self.scroll_right_zone {
                self.paint_scroll_arrow(rt, zone, false)?;
            }

            // Drag indicator
            if let Some(ref drag) = self.drag {
                if drag.active {
                    self.paint_drag_indicator(rt, drag)?;
                }
            }

            self.paint_window_buttons(rt)?;

            // Accent baseline with a gap beneath the active tab so the tab
            // reads as continuous with the pane surface below.
            let border_brush = make_brush(rt, ACCENT_COLOR)?;
            let baseline_y = TAB_STRIP_HEIGHT - 1.0;
            if let Some(active_zone) = self.zones.get(self.active_index) {
                let left_end = active_zone.rect.x.max(0.0);
                let right_start =
                    (active_zone.rect.x + active_zone.rect.width).min(self.available_width);
                if left_end > 0.0 {
                    rt.DrawLine(
                        D2D_POINT_2F {
                            x: 0.0,
                            y: baseline_y,
                        },
                        D2D_POINT_2F {
                            x: left_end,
                            y: baseline_y,
                        },
                        &border_brush,
                        1.0,
                        None,
                    );
                }
                if right_start < self.available_width {
                    rt.DrawLine(
                        D2D_POINT_2F {
                            x: right_start,
                            y: baseline_y,
                        },
                        D2D_POINT_2F {
                            x: self.available_width,
                            y: baseline_y,
                        },
                        &border_brush,
                        1.0,
                        None,
                    );
                }
            } else {
                rt.DrawLine(
                    D2D_POINT_2F {
                        x: 0.0,
                        y: baseline_y,
                    },
                    D2D_POINT_2F {
                        x: self.available_width,
                        y: baseline_y,
                    },
                    &border_brush,
                    1.0,
                    None,
                );
            }
        }
        Ok(())
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn measure_text(&self, text: &str) -> f32 {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            if let Ok(layout) =
                self.dw_factory
                    .CreateTextLayout(&utf16, &self.tf_tab, 1000.0, TAB_STRIP_HEIGHT)
            {
                let mut metrics = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut metrics).is_ok() {
                    return metrics.width;
                }
            }
        }
        60.0 // fallback
    }

    fn drop_index_at(&self, x: f32) -> usize {
        for (i, zone) in self.zones.iter().enumerate() {
            let mid = zone.rect.x + zone.rect.width / 2.0;
            if x < mid {
                return i;
            }
        }
        self.zones.len().saturating_sub(1)
    }

    unsafe fn paint_tab(&self, rt: &ID2D1RenderTarget, index: usize, zone: &TabZone) -> Result<()> {
        let is_active = index == self.active_index;
        let is_hover = self.hover_tab == Some(index) && !is_active;
        let is_dragging = self
            .drag
            .as_ref()
            .map_or(false, |d| d.active && d.tab_index == index);

        let bg_color = if is_dragging {
            TAB_HOVER_BG
        } else if is_active {
            TAB_ACTIVE_BG
        } else if is_hover {
            TAB_HOVER_BG
        } else {
            TAB_INACTIVE_BG
        };

        // Dragged tab follows the mouse
        let offset_x = if is_dragging {
            let drag = self.drag.as_ref().unwrap();
            drag.current_x - drag.start_x
        } else {
            0.0
        };

        let bg_brush = make_brush(rt, bg_color)?;
        let rect = D2D_RECT_F {
            left: zone.rect.x + offset_x,
            top: TAB_INNER_TOP,
            right: zone.rect.x + zone.rect.width + offset_x,
            bottom: TAB_STRIP_HEIGHT,
        };
        rt.FillRectangle(&rect, &bg_brush);

        // Active tab outline
        if is_active && !is_dragging {
            let accent = make_brush(rt, ACCENT_COLOR)?;
            rt.DrawLine(
                D2D_POINT_2F {
                    x: zone.rect.x + 0.5 + offset_x,
                    y: rect.bottom - 0.5,
                },
                D2D_POINT_2F {
                    x: zone.rect.x + 0.5 + offset_x,
                    y: rect.top + 0.5,
                },
                &accent,
                1.0,
                None,
            );
            rt.DrawLine(
                D2D_POINT_2F {
                    x: rect.left + 0.5,
                    y: rect.top + 0.5,
                },
                D2D_POINT_2F {
                    x: rect.right - 0.5,
                    y: rect.top + 0.5,
                },
                &accent,
                1.0,
                None,
            );
            rt.DrawLine(
                D2D_POINT_2F {
                    x: rect.right - 0.5,
                    y: rect.top + 0.5,
                },
                D2D_POINT_2F {
                    x: rect.right - 0.5,
                    y: rect.bottom - 0.5,
                },
                &accent,
                1.0,
                None,
            );
        }

        // Tab text
        let text_color = if is_active {
            TAB_ACTIVE_TEXT
        } else {
            TAB_TEXT_COLOR
        };
        let text_brush = make_brush(rt, text_color)?;
        let tf = if is_active {
            &self.tf_tab_bold
        } else {
            &self.tf_tab
        };

        let tab = &self.tabs[index];
        let progress_offset = if tab.progress.is_some() {
            TAB_PROGRESS_SIZE + TAB_PROGRESS_GAP
        } else {
            0.0
        };

        if let Some(progress) = tab.progress.as_ref() {
            self.paint_progress_indicator(
                rt,
                zone.rect.x + TAB_PADDING_H + offset_x,
                11.5,
                progress,
                is_active,
            )?;
        }

        let utf16: Vec<u16> = tab.name.encode_utf16().collect();
        let text_rect = D2D_RECT_F {
            left: zone.rect.x + TAB_PADDING_H + offset_x + progress_offset,
            top: TAB_INNER_TOP,
            right: zone.close_rect.x - TAB_TEXT_TO_CLOSE_GAP + offset_x,
            bottom: TAB_STRIP_HEIGHT,
        };
        rt.DrawText(
            &utf16,
            tf,
            &text_rect,
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );

        // Close button "×"
        let close_hover = self.hover_close == Some(index);
        let close_color = if close_hover {
            CLOSE_HOVER_COLOR
        } else {
            CLOSE_NORMAL_COLOR
        };
        let close_brush = make_brush(rt, close_color)?;
        let close_utf16: Vec<u16> = "×".encode_utf16().collect();
        let close_rect = D2D_RECT_F {
            left: zone.close_rect.x + offset_x,
            top: zone.close_rect.y - 1.0,
            right: zone.close_rect.x + zone.close_rect.width + offset_x,
            bottom: zone.close_rect.y + zone.close_rect.height - 1.0,
        };
        rt.DrawText(
            &close_utf16,
            &self.tf_close,
            &close_rect,
            &close_brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );

        Ok(())
    }

    unsafe fn paint_workspace_badge(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        let text_brush = make_brush(rt, WORKSPACE_TEXT_COLOR)?;
        let rect = D2D_RECT_F {
            left: self.workspace_zone.x + WORKSPACE_BADGE_PADDING_H,
            top: 0.0,
            right: self.workspace_zone.x + self.workspace_zone.width - WORKSPACE_BADGE_PADDING_H,
            bottom: TAB_STRIP_HEIGHT,
        };
        let utf16: Vec<u16> = self.workspace_name.encode_utf16().collect();
        rt.DrawText(
            &utf16,
            &self.tf_tab_bold,
            &rect,
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        Ok(())
    }

    unsafe fn paint_progress_indicator(
        &self,
        rt: &ID2D1RenderTarget,
        x: f32,
        y: f32,
        progress: &ProgressInfo,
        is_active: bool,
    ) -> Result<()> {
        let color = progress_color(progress, is_active);
        let brush = make_brush(rt, color)?;
        let muted = make_brush(rt, (72, 72, 88))?;
        let radius = TAB_PROGRESS_SIZE / 2.0;
        let center = D2D_POINT_2F {
            x: x + radius,
            y: y + radius,
        };

        draw_ring_segments(rt, &muted, center, radius, 16, 16)?;
        match progress.state {
            ProgressState::Indeterminate => draw_ring_segments(rt, &brush, center, radius, 4, 16)?,
            ProgressState::Normal | ProgressState::Error | ProgressState::Warning => {
                let value = progress.value.unwrap_or(0).min(100);
                let lit = ((value as usize * 16) + 99) / 100;
                if lit > 0 {
                    draw_ring_segments(rt, &brush, center, radius, lit, 16)?;
                }
            }
        }
        Ok(())
    }

    unsafe fn paint_add_button(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        let color = if self.hover_add {
            ADD_HOVER_COLOR
        } else {
            ADD_TEXT_COLOR
        };
        let brush = make_brush(rt, color)?;

        let cx = self.add_zone.x + ADD_BUTTON_WIDTH / 2.0;
        let cy = TAB_STRIP_HEIGHT / 2.0;
        let arm = 6.0;

        // Horizontal line
        rt.DrawLine(
            D2D_POINT_2F { x: cx - arm, y: cy },
            D2D_POINT_2F { x: cx + arm, y: cy },
            &brush,
            1.5,
            None,
        );
        // Vertical line
        rt.DrawLine(
            D2D_POINT_2F { x: cx, y: cy - arm },
            D2D_POINT_2F { x: cx, y: cy + arm },
            &brush,
            1.5,
            None,
        );

        Ok(())
    }

    unsafe fn paint_scroll_arrow(
        &self,
        rt: &ID2D1RenderTarget,
        zone: &HitRect,
        is_left: bool,
    ) -> Result<()> {
        // Opaque background so it covers partially-visible tabs
        let bg = make_brush(rt, STRIP_BG)?;
        let rect = D2D_RECT_F {
            left: zone.x,
            top: zone.y,
            right: zone.x + zone.width,
            bottom: zone.y + zone.height,
        };
        rt.FillRectangle(&rect, &bg);

        // Chevron arrow
        let arrow_brush = make_brush(rt, TAB_TEXT_COLOR)?;
        let cx = zone.x + zone.width / 2.0;
        let cy = zone.y + zone.height / 2.0;
        let arm = 5.0;

        if is_left {
            rt.DrawLine(
                D2D_POINT_2F {
                    x: cx + arm,
                    y: cy - arm,
                },
                D2D_POINT_2F { x: cx - arm, y: cy },
                &arrow_brush,
                1.5,
                None,
            );
            rt.DrawLine(
                D2D_POINT_2F { x: cx - arm, y: cy },
                D2D_POINT_2F {
                    x: cx + arm,
                    y: cy + arm,
                },
                &arrow_brush,
                1.5,
                None,
            );
        } else {
            rt.DrawLine(
                D2D_POINT_2F {
                    x: cx - arm,
                    y: cy - arm,
                },
                D2D_POINT_2F { x: cx + arm, y: cy },
                &arrow_brush,
                1.5,
                None,
            );
            rt.DrawLine(
                D2D_POINT_2F { x: cx + arm, y: cy },
                D2D_POINT_2F {
                    x: cx - arm,
                    y: cy + arm,
                },
                &arrow_brush,
                1.5,
                None,
            );
        }

        Ok(())
    }

    unsafe fn paint_window_buttons(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        self.paint_window_button(rt, &self.minimize_zone, WindowButtonKind::Minimize)?;
        self.paint_window_button(rt, &self.maximize_zone, WindowButtonKind::MaximizeRestore)?;
        self.paint_window_button(rt, &self.close_window_zone, WindowButtonKind::Close)?;
        Ok(())
    }

    unsafe fn paint_window_button(
        &self,
        rt: &ID2D1RenderTarget,
        rect: &HitRect,
        kind: WindowButtonKind,
    ) -> Result<()> {
        let hovered = self.hover_window_button == Some(kind);
        if hovered {
            let bg_color = if kind == WindowButtonKind::Close {
                WINDOW_CLOSE_HOVER_BG
            } else {
                WINDOW_BUTTON_HOVER_BG
            };
            let bg = make_brush(rt, bg_color)?;
            let fill = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.width,
                bottom: rect.y + rect.height,
            };
            rt.FillRectangle(&fill, &bg);
        }

        let brush = make_brush(rt, WINDOW_BUTTON_TEXT)?;
        let cx = rect.x + rect.width / 2.0;
        let cy = rect.y + rect.height / 2.0;
        match kind {
            WindowButtonKind::Minimize => {
                rt.DrawLine(
                    D2D_POINT_2F {
                        x: cx - 5.0,
                        y: cy + 3.5,
                    },
                    D2D_POINT_2F {
                        x: cx + 5.0,
                        y: cy + 3.5,
                    },
                    &brush,
                    1.0,
                    None,
                );
            }
            WindowButtonKind::MaximizeRestore => {
                if self.window_maximized {
                    let back = D2D_RECT_F {
                        left: cx - 4.0,
                        top: cy - 3.5,
                        right: cx + 4.0,
                        bottom: cy + 3.5,
                    };
                    let front = D2D_RECT_F {
                        left: cx - 1.5,
                        top: cy - 1.0,
                        right: cx + 6.5,
                        bottom: cy + 6.0,
                    };
                    rt.DrawRectangle(&back, &brush, 1.0, None);
                    rt.DrawRectangle(&front, &brush, 1.0, None);
                } else {
                    let square = D2D_RECT_F {
                        left: cx - 4.5,
                        top: cy - 4.0,
                        right: cx + 4.5,
                        bottom: cy + 4.0,
                    };
                    rt.DrawRectangle(&square, &brush, 1.0, None);
                }
            }
            WindowButtonKind::Close => {
                rt.DrawLine(
                    D2D_POINT_2F {
                        x: cx - 4.0,
                        y: cy - 4.0,
                    },
                    D2D_POINT_2F {
                        x: cx + 4.0,
                        y: cy + 4.0,
                    },
                    &brush,
                    1.0,
                    None,
                );
                rt.DrawLine(
                    D2D_POINT_2F {
                        x: cx + 4.0,
                        y: cy - 4.0,
                    },
                    D2D_POINT_2F {
                        x: cx - 4.0,
                        y: cy + 4.0,
                    },
                    &brush,
                    1.0,
                    None,
                );
            }
        }
        Ok(())
    }

    unsafe fn paint_drag_indicator(&self, rt: &ID2D1RenderTarget, drag: &DragState) -> Result<()> {
        let drop_idx = self.drop_index_at(drag.current_x);
        let indicator_x = if drop_idx < self.zones.len() {
            self.zones[drop_idx].rect.x
        } else if let Some(last) = self.zones.last() {
            last.rect.x + last.rect.width
        } else {
            0.0
        };

        let brush = make_brush(rt, ACCENT_COLOR)?;
        let rect = D2D_RECT_F {
            left: indicator_x - 1.5,
            top: 4.0,
            right: indicator_x + 1.5,
            bottom: TAB_STRIP_HEIGHT - 4.0,
        };
        rt.FillRectangle(&rect, &brush);

        Ok(())
    }
}

// ── Utility ──────────────────────────────────────────────────────────────────

fn make_brush(rt: &ID2D1RenderTarget, color: (u8, u8, u8)) -> Result<ID2D1SolidColorBrush> {
    let c = D2D1_COLOR_F {
        r: color.0 as f32 / 255.0,
        g: color.1 as f32 / 255.0,
        b: color.2 as f32 / 255.0,
        a: 1.0,
    };
    unsafe { rt.CreateSolidColorBrush(&c, None) }
}

fn progress_color(progress: &ProgressInfo, is_active: bool) -> (u8, u8, u8) {
    let active_bump = if is_active { 20 } else { 0 };
    match progress.state {
        ProgressState::Normal => (100 + active_bump, 200 + active_bump, 120 + active_bump / 2),
        ProgressState::Error => (220, 96 + active_bump / 2, 96 + active_bump / 2),
        ProgressState::Indeterminate => (120 + active_bump, 180 + active_bump / 2, 220),
        ProgressState::Warning => (220, 176 + active_bump / 3, 92),
    }
}

unsafe fn draw_ring_segments(
    rt: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    center: D2D_POINT_2F,
    radius: f32,
    lit_segments: usize,
    total_segments: usize,
) -> Result<()> {
    if total_segments == 0 {
        return Ok(());
    }

    let lit_segments = lit_segments.min(total_segments);
    let start_angle = -std::f32::consts::FRAC_PI_2;
    let segment_sweep = (std::f32::consts::TAU / total_segments as f32) * 0.72;
    let segment_step = std::f32::consts::TAU / total_segments as f32;
    for index in 0..lit_segments {
        let angle = start_angle + segment_step * index as f32;
        let inner = radius - 1.4;
        let outer = radius + 1.4;
        let p1 = D2D_POINT_2F {
            x: center.x + inner * angle.cos(),
            y: center.y + inner * angle.sin(),
        };
        let p2 = D2D_POINT_2F {
            x: center.x + outer * (angle + segment_sweep).cos(),
            y: center.y + outer * (angle + segment_sweep).sin(),
        };
        rt.DrawLine(p1, p2, brush, 1.8, None);
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dw_factory() -> IDWriteFactory {
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() }
    }

    fn make_strip() -> TabStrip {
        let dw = make_dw_factory();
        TabStrip::new(&dw).unwrap()
    }

    #[test]
    fn add_tab_returns_sequential_ids() {
        let mut strip = make_strip();
        let id1 = strip.add_tab("a".into());
        let id2 = strip.add_tab("b".into());
        let id3 = strip.add_tab("c".into());
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(strip.tab_count(), 3);
    }

    #[test]
    fn set_active_updates_index() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.add_tab("c".into());
        strip.set_active(2);
        assert_eq!(strip.active_index(), 2);
        assert_eq!(strip.active_tab().unwrap().name, "c");
    }

    #[test]
    fn set_active_out_of_bounds_is_noop() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.set_active(0);
        strip.set_active(99);
        assert_eq!(strip.active_index(), 0);
    }

    #[test]
    fn close_last_tab_returns_window_close() {
        let mut strip = make_strip();
        strip.add_tab("only".into());
        let action = strip.close_tab(0);
        assert_eq!(action, TabAction::WindowClose);
        assert_eq!(strip.tab_count(), 0);
    }

    #[test]
    fn close_tab_adjusts_active_when_after() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.add_tab("c".into());
        strip.set_active(2);
        let action = strip.close_tab(0);
        assert_eq!(action, TabAction::Close(0));
        assert_eq!(strip.active_index(), 1); // was 2, shifted left
        assert_eq!(strip.tabs()[strip.active_index()].name, "c");
    }

    #[test]
    fn close_tab_adjusts_active_when_at_end() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.set_active(1);
        let action = strip.close_tab(1);
        assert_eq!(action, TabAction::Close(1));
        assert_eq!(strip.active_index(), 0);
    }

    #[test]
    fn close_tab_active_before_stays() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.add_tab("c".into());
        strip.set_active(0);
        strip.close_tab(2);
        assert_eq!(strip.active_index(), 0);
        assert_eq!(strip.tab_count(), 2);
    }

    #[test]
    fn reorder_moves_tab() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.add_tab("c".into());
        strip.set_active(0);
        strip.reorder(0, 2);
        assert_eq!(strip.tabs()[0].name, "b");
        assert_eq!(strip.tabs()[1].name, "c");
        assert_eq!(strip.tabs()[2].name, "a");
        assert_eq!(strip.active_index(), 2); // followed the moved tab
    }

    #[test]
    fn reorder_same_index_is_noop() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.reorder(0, 0);
        assert_eq!(strip.tabs()[0].name, "a");
    }

    #[test]
    fn window_title_with_active_tab() {
        let mut strip = make_strip();
        strip.add_tab("main".into());
        strip.add_tab("logs".into());
        strip.set_active(1);
        assert_eq!(
            strip.window_title("MyWorkspace"),
            "MyWorkspace \u{2014} logs"
        );
    }

    #[test]
    fn window_title_no_tabs() {
        let strip = make_strip();
        assert_eq!(strip.window_title("Workspace"), "Workspace");
    }

    #[test]
    fn set_progress_updates_tab_state() {
        let mut strip = make_strip();
        strip.add_tab("main".into());
        strip.set_progress(
            0,
            Some(ProgressInfo {
                state: ProgressState::Warning,
                value: Some(64),
            }),
        );
        assert_eq!(
            strip.tabs()[0].progress,
            Some(ProgressInfo {
                state: ProgressState::Warning,
                value: Some(64),
            })
        );
    }

    #[test]
    fn layout_no_overflow() {
        let mut strip = make_strip();
        strip.add_tab("tab1".into());
        strip.add_tab("tab2".into());
        strip.layout(1000.0);
        assert_eq!(strip.zones.len(), 2);
        assert!(strip.scroll_left_zone.is_none());
        assert!(strip.scroll_right_zone.is_none());
        // Tabs should start at x=0
        assert!(strip.zones[0].rect.x >= 0.0);
        // Second tab should be after first + gap
        assert!(strip.zones[1].rect.x > strip.zones[0].rect.x);
    }

    #[test]
    fn layout_overflow_shows_scroll_arrows() {
        let mut strip = make_strip();
        for i in 0..20 {
            strip.add_tab(format!("long tab name {i}"));
        }
        strip.layout(400.0);
        assert!(strip.scroll_left_zone.is_some());
        assert!(strip.scroll_right_zone.is_some());
    }

    #[test]
    fn mouse_down_on_tab_switches() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.set_active(0);
        strip.layout(500.0);

        // Click on second tab (somewhere in its zone)
        let zone = &strip.zones[1];
        let x = zone.rect.x + zone.rect.width / 2.0;
        let y = TAB_STRIP_HEIGHT / 2.0;
        let action = strip.on_mouse_down(x, y);
        assert_eq!(action, Some(TabAction::SwitchTo(1)));
        assert_eq!(strip.active_index(), 0);
    }

    #[test]
    fn tab_index_at_returns_hit_tab() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.layout(500.0);

        let zone = &strip.zones[1];
        let x = zone.rect.x + zone.rect.width / 2.0;
        let y = TAB_STRIP_HEIGHT / 2.0;

        assert_eq!(strip.tab_index_at(x, y), Some(1));
        assert_eq!(strip.tab_index_at(5.0, TAB_STRIP_HEIGHT + 1.0), None);
    }

    #[test]
    fn mouse_down_on_close_closes_tab() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.set_active(0);
        strip.layout(500.0);

        let close = &strip.zones[1].close_rect;
        let x = close.x + close.width / 2.0;
        let y = close.y + close.height / 2.0;
        let action = strip.on_mouse_down(x, y);
        assert_eq!(action, Some(TabAction::Close(1)));
        assert_eq!(strip.tab_count(), 2);
    }

    #[test]
    fn mouse_down_on_add_creates() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.layout(500.0);

        let x = strip.add_zone.x + strip.add_zone.width / 2.0;
        let y = TAB_STRIP_HEIGHT / 2.0;
        let action = strip.on_mouse_down(x, y);
        assert_eq!(action, Some(TabAction::Create));
    }

    #[test]
    fn mouse_down_outside_strip_is_none() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.layout(500.0);
        assert_eq!(strip.on_mouse_down(50.0, TAB_STRIP_HEIGHT + 10.0), None);
    }

    #[test]
    fn drag_reorder() {
        let mut strip = make_strip();
        strip.add_tab("a".into());
        strip.add_tab("b".into());
        strip.add_tab("c".into());
        strip.set_active(0);
        strip.layout(500.0);

        // Mouse down on first tab
        let zone0 = strip.zones[0].clone();
        let start_x = zone0.rect.x + zone0.rect.width / 2.0;
        let y = TAB_STRIP_HEIGHT / 2.0;
        strip.on_mouse_down(start_x, y);

        // Move past second tab (exceed threshold)
        let zone2 = strip.zones[2].clone();
        let end_x = zone2.rect.x + zone2.rect.width / 2.0;
        strip.on_mouse_move(end_x, y);

        // Mouse up
        let action = strip.on_mouse_up(end_x, y);
        assert!(matches!(action, Some(TabAction::Reorder { .. })));
        // Tab "a" should have moved
        assert_eq!(strip.tabs()[0].name, "b");
    }

    #[test]
    fn height_returns_constant() {
        let strip = make_strip();
        assert_eq!(strip.height(), TAB_STRIP_HEIGHT);
    }
}
