//! Pane layout rendering: borders, splitter bars, and focus indicators (§24.4).
//!
//! [`PaneLayout`] takes a [`LayoutTree`] and renders the pane chrome on top of
//! (or around) the terminal content: thin borders on each pane, a colored accent
//! border on the focused pane, and draggable splitter bars between adjacent
//! panes.

use std::collections::HashMap;

use wtd_core::ids::PaneId;
use wtd_core::layout::{LayoutTree, Rect, ResizeDirection};
use wtd_core::workspace::Orientation;

// ── Constants ────────────────────────────────────────────────────────────────

/// Splitter bar thickness in pixels (visual line drawn between panes).
const SPLITTER_THICKNESS: f32 = 2.0;

/// Hit-test zone half-width (pixels each side of the splitter center).
const SPLITTER_HIT_HALF: f32 = 4.0;

/// Focused pane border thickness in pixels.
const FOCUS_BORDER_THICKNESS: f32 = 2.0;

/// Default resize increment in character cells when dragging (§18.9).
const RESIZE_INCREMENT_CELLS: u16 = 1;

// Colors — dark theme matching tab strip / renderer defaults.
const SPLITTER_COLOR: (u8, u8, u8) = (60, 60, 75);
const SPLITTER_HOVER_COLOR: (u8, u8, u8) = (90, 90, 110);
const FOCUS_BORDER_COLOR: (u8, u8, u8) = (78, 201, 176); // accent #4ec9b0
const PANE_BORDER_COLOR: (u8, u8, u8) = (45, 45, 58);

// ── Public types ─────────────────────────────────────────────────────────────

/// Pixel-space rectangle (f32 coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PixelRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// Action produced by mouse interaction with the pane layout.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneLayoutAction {
    /// User clicked inside a pane — focus it.
    FocusPane(PaneId),
    /// Splitter drag produced a resize.
    Resize {
        pane_id: PaneId,
        direction: ResizeDirection,
        cells: u16,
    },
}

/// What cursor shape the window should show at a given position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorHint {
    /// Normal arrow cursor.
    Arrow,
    /// Horizontal resize cursor (left-right).
    ResizeHorizontal,
    /// Vertical resize cursor (up-down).
    ResizeVertical,
}

// ── Internal types ───────────────────────────────────────────────────────────

/// A detected splitter bar between adjacent panes.
#[derive(Debug, Clone)]
struct SplitterInfo {
    /// Orientation of the split that created this splitter.
    orientation: Orientation,
    /// Pixel position of the splitter line (x for vertical, y for horizontal).
    position: f32,
    /// Start of the splitter line extent (y for vertical, x for horizontal).
    start: f32,
    /// End of the splitter line extent.
    end: f32,
    /// A pane on the "before" side (left or above) used for resize.
    pane_before: PaneId,
    /// A pane on the "after" side (right or below) used for resize.
    #[allow(dead_code)]
    pane_after: PaneId,
}

/// Active splitter drag state.
#[derive(Debug, Clone)]
struct DragState {
    splitter_index: usize,
    /// Pixel position where the drag started.
    #[allow(dead_code)]
    start_pos: f32,
    /// Last reported pixel position.
    last_pos: f32,
    /// Accumulated fractional cells not yet emitted as a resize action.
    remainder: f32,
}

// ── PaneLayout ───────────────────────────────────────────────────────────────

/// Manages rendering and interaction for the pane layout area.
///
/// Call [`update`] whenever the layout tree or content area changes, then
/// [`paint`] within a Direct2D draw session. Forward mouse events via
/// [`on_mouse_down`] / [`on_mouse_move`] / [`on_mouse_up`].
pub struct PaneLayout {
    /// Pixel rectangles for each pane (full area including border).
    pane_rects: HashMap<PaneId, PixelRect>,
    /// Detected splitter bars.
    splitters: Vec<SplitterInfo>,
    /// Active drag, if any.
    drag: Option<DragState>,
    /// Splitter index currently hovered.
    hover_splitter: Option<usize>,
    /// Cell dimensions in pixels.
    cell_width: f32,
    cell_height: f32,
    /// Pixel origin of the content area (e.g. below tab strip).
    origin_x: f32,
    origin_y: f32,
    /// Content area in character cells.
    total_cols: u16,
    total_rows: u16,
}

impl PaneLayout {
    /// Create a new pane layout manager.
    pub fn new(cell_width: f32, cell_height: f32) -> Self {
        Self {
            pane_rects: HashMap::new(),
            splitters: Vec::new(),
            drag: None,
            hover_splitter: None,
            cell_width,
            cell_height,
            origin_x: 0.0,
            origin_y: 0.0,
            total_cols: 0,
            total_rows: 0,
        }
    }

    /// Recompute pane pixel rects and splitter positions from the layout tree.
    ///
    /// `origin_x/y` is the pixel offset of the content area within the window
    /// (e.g. below the tab strip). `cols/rows` is the content area in cells.
    pub fn update(
        &mut self,
        tree: &LayoutTree,
        origin_x: f32,
        origin_y: f32,
        cols: u16,
        rows: u16,
    ) {
        self.origin_x = origin_x;
        self.origin_y = origin_y;
        self.total_cols = cols;
        self.total_rows = rows;

        let total_rect = Rect::new(0, 0, cols, rows);
        let cell_rects = tree.compute_rects(total_rect);

        // Convert cell rects to pixel rects.
        self.pane_rects.clear();
        for (id, r) in &cell_rects {
            self.pane_rects.insert(
                id.clone(),
                PixelRect::new(
                    origin_x + r.x as f32 * self.cell_width,
                    origin_y + r.y as f32 * self.cell_height,
                    r.width as f32 * self.cell_width,
                    r.height as f32 * self.cell_height,
                ),
            );
        }

        // Detect splitters from adjacent pane edges.
        self.splitters.clear();
        self.detect_splitters(&cell_rects);
    }

    /// Get the pixel rectangle for a pane (the full area, not inset).
    pub fn pane_pixel_rect(&self, pane_id: &PaneId) -> Option<PixelRect> {
        self.pane_rects.get(pane_id).copied()
    }

    /// Get all pane pixel rectangles.
    pub fn pane_pixel_rects(&self) -> &HashMap<PaneId, PixelRect> {
        &self.pane_rects
    }

    /// Whether a splitter drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// What cursor shape should be shown at the given pixel position.
    pub fn cursor_hint(&self, x: f32, y: f32) -> CursorHint {
        if let Some(ref drag) = self.drag {
            return match self.splitters[drag.splitter_index].orientation {
                Orientation::Horizontal => CursorHint::ResizeHorizontal,
                Orientation::Vertical => CursorHint::ResizeVertical,
            };
        }
        if let Some(idx) = self.hit_test_splitter(x, y) {
            match self.splitters[idx].orientation {
                Orientation::Horizontal => CursorHint::ResizeHorizontal,
                Orientation::Vertical => CursorHint::ResizeVertical,
            }
        } else {
            CursorHint::Arrow
        }
    }

    /// Number of detected splitters (for testing).
    pub fn splitter_count(&self) -> usize {
        self.splitters.len()
    }

    // ── Mouse interaction ────────────────────────────────────────────────────

    /// Handle mouse button down. Returns an action if a pane was clicked.
    pub fn on_mouse_down(&mut self, x: f32, y: f32) -> Option<PaneLayoutAction> {
        // Check if we're clicking on a splitter.
        if let Some(idx) = self.hit_test_splitter(x, y) {
            let pos = match self.splitters[idx].orientation {
                Orientation::Horizontal => x,
                Orientation::Vertical => y,
            };
            self.drag = Some(DragState {
                splitter_index: idx,
                start_pos: pos,
                last_pos: pos,
                remainder: 0.0,
            });
            return None;
        }

        // Check if we're clicking inside a pane.
        for (id, rect) in &self.pane_rects {
            if rect.contains(x, y) {
                return Some(PaneLayoutAction::FocusPane(id.clone()));
            }
        }

        None
    }

    /// Handle mouse move. Returns a resize action if dragging a splitter.
    pub fn on_mouse_move(&mut self, x: f32, y: f32) -> Option<PaneLayoutAction> {
        // Update hover state.
        self.hover_splitter = self.hit_test_splitter(x, y);

        let drag = match self.drag.as_mut() {
            Some(d) => d,
            None => return None,
        };

        let splitter = &self.splitters[drag.splitter_index];
        let current_pos = match splitter.orientation {
            Orientation::Horizontal => x,
            Orientation::Vertical => y,
        };

        let delta_px = current_pos - drag.last_pos;
        let cell_dim = match splitter.orientation {
            Orientation::Horizontal => self.cell_width,
            Orientation::Vertical => self.cell_height,
        };

        // Accumulate fractional movement.
        drag.remainder += delta_px;
        drag.last_pos = current_pos;

        let cells = (drag.remainder / cell_dim).abs().floor() as u16;
        if cells < RESIZE_INCREMENT_CELLS {
            return None;
        }

        let growing = drag.remainder > 0.0;
        drag.remainder -= (cells as f32) * cell_dim * if growing { 1.0 } else { -1.0 };

        let direction = match (splitter.orientation.clone(), growing) {
            (Orientation::Horizontal, true) => ResizeDirection::GrowRight,
            (Orientation::Horizontal, false) => ResizeDirection::ShrinkRight,
            (Orientation::Vertical, true) => ResizeDirection::GrowDown,
            (Orientation::Vertical, false) => ResizeDirection::ShrinkDown,
        };

        Some(PaneLayoutAction::Resize {
            pane_id: splitter.pane_before.clone(),
            direction,
            cells,
        })
    }

    /// Handle mouse button up. Ends any active drag.
    pub fn on_mouse_up(&mut self, _x: f32, _y: f32) -> Option<PaneLayoutAction> {
        self.drag = None;
        None
    }

    /// Clear hover and drag state (e.g. on mouse leave).
    pub fn on_mouse_leave(&mut self) {
        self.hover_splitter = None;
        if self.drag.is_none() {
            // Only clear hover; drag persists until mouse up.
        }
    }

    // ── Painting ─────────────────────────────────────────────────────────────

    /// Paint pane borders, splitter bars, and focus indicator.
    ///
    /// Call this within an active `BeginDraw` / `EndDraw` session on the
    /// render target.
    #[cfg(windows)]
    pub fn paint(
        &self,
        rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget,
        focused_pane: &PaneId,
    ) -> windows::core::Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::*;

        // Draw pane borders (unfocused panes get a subtle border).
        for (id, rect) in &self.pane_rects {
            if id == focused_pane {
                continue; // Draw focused border last, on top.
            }
            let brush = unsafe {
                rt.CreateSolidColorBrush(
                    &rgb_f(
                        PANE_BORDER_COLOR.0,
                        PANE_BORDER_COLOR.1,
                        PANE_BORDER_COLOR.2,
                    ),
                    None,
                )?
            };
            let d2d_rect = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.width,
                bottom: rect.y + rect.height,
            };
            unsafe {
                rt.DrawRectangle(&d2d_rect, &brush, 1.0, None);
            }
        }

        // Draw splitter bars.
        for (i, splitter) in self.splitters.iter().enumerate() {
            let is_hovered = self.hover_splitter == Some(i)
                || self.drag.as_ref().map_or(false, |d| d.splitter_index == i);
            let color = if is_hovered {
                SPLITTER_HOVER_COLOR
            } else {
                SPLITTER_COLOR
            };
            let brush =
                unsafe { rt.CreateSolidColorBrush(&rgb_f(color.0, color.1, color.2), None)? };

            let half = SPLITTER_THICKNESS / 2.0;
            let d2d_rect = match splitter.orientation {
                Orientation::Horizontal => D2D_RECT_F {
                    left: splitter.position - half,
                    top: splitter.start,
                    right: splitter.position + half,
                    bottom: splitter.end,
                },
                Orientation::Vertical => D2D_RECT_F {
                    left: splitter.start,
                    top: splitter.position - half,
                    right: splitter.end,
                    bottom: splitter.position + half,
                },
            };
            unsafe {
                rt.FillRectangle(&d2d_rect, &brush);
            }
        }

        // Draw focused pane border (accent color, on top of everything).
        if let Some(rect) = self.pane_rects.get(focused_pane) {
            let brush = unsafe {
                rt.CreateSolidColorBrush(
                    &rgb_f(
                        FOCUS_BORDER_COLOR.0,
                        FOCUS_BORDER_COLOR.1,
                        FOCUS_BORDER_COLOR.2,
                    ),
                    None,
                )?
            };
            let d2d_rect = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.width,
                bottom: rect.y + rect.height,
            };
            unsafe {
                rt.DrawRectangle(&d2d_rect, &brush, FOCUS_BORDER_THICKNESS, None);
            }
        }

        Ok(())
    }

    // ── Internals ────────────────────────────────────────────────────────────

    /// Detect splitter positions from pairs of adjacent pane cell rects.
    fn detect_splitters(&mut self, cell_rects: &HashMap<PaneId, Rect>) {
        let rects: Vec<(PaneId, Rect)> =
            cell_rects.iter().map(|(id, r)| (id.clone(), *r)).collect();

        // For vertical splitters (between horizontally split panes):
        // Pane A's right edge == Pane B's left edge, and they overlap vertically.
        // For horizontal splitters (between vertically split panes):
        // Pane A's bottom edge == Pane B's top edge, and they overlap horizontally.

        // Collect raw edge segments, then merge overlapping ones.
        let mut v_segments: Vec<(u16, u16, u16, PaneId, PaneId)> = Vec::new(); // (x, y_start, y_end, before, after)
        let mut h_segments: Vec<(u16, u16, u16, PaneId, PaneId)> = Vec::new(); // (y, x_start, x_end, before, after)

        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (ref id_a, a) = rects[i];
                let (ref id_b, b) = rects[j];

                // Check vertical splitter: A right edge == B left edge
                let a_right = a.x + a.width;
                let b_right = b.x + b.width;

                if a_right == b.x {
                    let overlap_start = a.y.max(b.y);
                    let overlap_end = (a.y + a.height).min(b.y + b.height);
                    if overlap_start < overlap_end {
                        v_segments.push((
                            a_right,
                            overlap_start,
                            overlap_end,
                            id_a.clone(),
                            id_b.clone(),
                        ));
                    }
                } else if b_right == a.x {
                    let overlap_start = a.y.max(b.y);
                    let overlap_end = (a.y + a.height).min(b.y + b.height);
                    if overlap_start < overlap_end {
                        v_segments.push((
                            b_right,
                            overlap_start,
                            overlap_end,
                            id_b.clone(),
                            id_a.clone(),
                        ));
                    }
                }

                // Check horizontal splitter: A bottom edge == B top edge
                let a_bottom = a.y + a.height;
                let b_bottom = b.y + b.height;

                if a_bottom == b.y {
                    let overlap_start = a.x.max(b.x);
                    let overlap_end = (a.x + a.width).min(b.x + b.width);
                    if overlap_start < overlap_end {
                        h_segments.push((
                            a_bottom,
                            overlap_start,
                            overlap_end,
                            id_a.clone(),
                            id_b.clone(),
                        ));
                    }
                } else if b_bottom == a.y {
                    let overlap_start = a.x.max(b.x);
                    let overlap_end = (a.x + a.width).min(b.x + b.width);
                    if overlap_start < overlap_end {
                        h_segments.push((
                            b_bottom,
                            overlap_start,
                            overlap_end,
                            id_b.clone(),
                            id_a.clone(),
                        ));
                    }
                }
            }
        }

        // Merge co-linear vertical segments at the same x into full splitters.
        self.merge_segments_into_splitters(&v_segments, Orientation::Horizontal);
        self.merge_segments_into_splitters_h(&h_segments);
    }

    /// Merge vertical splitter segments (same x) into full splitter bars.
    fn merge_segments_into_splitters(
        &mut self,
        segments: &[(u16, u16, u16, PaneId, PaneId)],
        _orientation: Orientation,
    ) {
        // Group by position (x for vertical splitters).
        let mut by_pos: HashMap<u16, Vec<(u16, u16, PaneId, PaneId)>> = HashMap::new();
        for (pos, start, end, before, after) in segments {
            by_pos
                .entry(*pos)
                .or_default()
                .push((*start, *end, before.clone(), after.clone()));
        }

        for (pos, mut segs) in by_pos {
            segs.sort_by_key(|(start, _, _, _)| *start);
            // Merge adjacent/overlapping segments.
            let mut merged_start = segs[0].0;
            let mut merged_end = segs[0].1;
            let mut first_before = segs[0].2.clone();
            let mut first_after = segs[0].3.clone();

            for seg in segs.iter().skip(1) {
                if seg.0 <= merged_end {
                    merged_end = merged_end.max(seg.1);
                } else {
                    // Emit previous merged segment.
                    self.splitters.push(SplitterInfo {
                        orientation: Orientation::Horizontal, // split is horizontal → splitter is vertical line
                        position: self.origin_x + pos as f32 * self.cell_width,
                        start: self.origin_y + merged_start as f32 * self.cell_height,
                        end: self.origin_y + merged_end as f32 * self.cell_height,
                        pane_before: first_before.clone(),
                        pane_after: first_after.clone(),
                    });
                    merged_start = seg.0;
                    merged_end = seg.1;
                    first_before = seg.2.clone();
                    first_after = seg.3.clone();
                }
            }
            // Emit last.
            self.splitters.push(SplitterInfo {
                orientation: Orientation::Horizontal,
                position: self.origin_x + pos as f32 * self.cell_width,
                start: self.origin_y + merged_start as f32 * self.cell_height,
                end: self.origin_y + merged_end as f32 * self.cell_height,
                pane_before: first_before,
                pane_after: first_after,
            });
        }
    }

    /// Merge horizontal splitter segments (same y) into full splitter bars.
    fn merge_segments_into_splitters_h(&mut self, segments: &[(u16, u16, u16, PaneId, PaneId)]) {
        let mut by_pos: HashMap<u16, Vec<(u16, u16, PaneId, PaneId)>> = HashMap::new();
        for (pos, start, end, before, after) in segments {
            by_pos
                .entry(*pos)
                .or_default()
                .push((*start, *end, before.clone(), after.clone()));
        }

        for (pos, mut segs) in by_pos {
            segs.sort_by_key(|(start, _, _, _)| *start);
            let mut merged_start = segs[0].0;
            let mut merged_end = segs[0].1;
            let mut first_before = segs[0].2.clone();
            let mut first_after = segs[0].3.clone();

            for seg in segs.iter().skip(1) {
                if seg.0 <= merged_end {
                    merged_end = merged_end.max(seg.1);
                } else {
                    self.splitters.push(SplitterInfo {
                        orientation: Orientation::Vertical,
                        position: self.origin_y + pos as f32 * self.cell_height,
                        start: self.origin_x + merged_start as f32 * self.cell_width,
                        end: self.origin_x + merged_end as f32 * self.cell_width,
                        pane_before: first_before.clone(),
                        pane_after: first_after.clone(),
                    });
                    merged_start = seg.0;
                    merged_end = seg.1;
                    first_before = seg.2.clone();
                    first_after = seg.3.clone();
                }
            }
            self.splitters.push(SplitterInfo {
                orientation: Orientation::Vertical,
                position: self.origin_y + pos as f32 * self.cell_height,
                start: self.origin_x + merged_start as f32 * self.cell_width,
                end: self.origin_x + merged_end as f32 * self.cell_width,
                pane_before: first_before,
                pane_after: first_after,
            });
        }
    }

    /// Hit-test a pixel position against splitter bars.
    fn hit_test_splitter(&self, x: f32, y: f32) -> Option<usize> {
        for (i, splitter) in self.splitters.iter().enumerate() {
            let hit = match splitter.orientation {
                Orientation::Horizontal => {
                    // Vertical line at splitter.position
                    (x - splitter.position).abs() <= SPLITTER_HIT_HALF
                        && y >= splitter.start
                        && y <= splitter.end
                }
                Orientation::Vertical => {
                    // Horizontal line at splitter.position
                    (y - splitter.position).abs() <= SPLITTER_HIT_HALF
                        && x >= splitter.start
                        && x <= splitter.end
                }
            };
            if hit {
                return Some(i);
            }
        }
        None
    }
}

/// Convert (r, g, b) u8 to D2D1_COLOR_F.
#[cfg(windows)]
fn rgb_f(r: u8, g: u8, b: u8) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_size() -> (f32, f32) {
        (8.0, 16.0) // typical monospace cell size
    }

    #[test]
    fn single_pane_no_splitters() {
        let tree = LayoutTree::new();
        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 32.0, 80, 24);

        assert_eq!(layout.splitter_count(), 0);
        assert_eq!(layout.pane_pixel_rects().len(), 1);

        let p1 = tree.focus();
        let rect = layout.pane_pixel_rect(&p1).unwrap();
        assert!((rect.x - 0.0).abs() < 0.01);
        assert!((rect.y - 32.0).abs() < 0.01);
        assert!((rect.width - 640.0).abs() < 0.01); // 80 * 8
        assert!((rect.height - 384.0).abs() < 0.01); // 24 * 16
    }

    #[test]
    fn horizontal_split_creates_vertical_splitter() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        assert_eq!(layout.splitter_count(), 1);
        assert_eq!(layout.pane_pixel_rects().len(), 2);

        // Splitter should be at x = 40 * 8 = 320
        let s = &layout.splitters[0];
        assert_eq!(s.orientation, Orientation::Horizontal);
        assert!((s.position - 320.0).abs() < 0.01);
    }

    #[test]
    fn vertical_split_creates_horizontal_splitter() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_down(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        assert_eq!(layout.splitter_count(), 1);

        let s = &layout.splitters[0];
        assert_eq!(s.orientation, Orientation::Vertical);
        assert!((s.position - 192.0).abs() < 0.01); // 12 * 16 = 192
    }

    #[test]
    fn four_pane_layout_splitters() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();
        let _p3 = tree.split_down(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // 3 panes, 2 splitters: one vertical (between left/right columns),
        // one horizontal (within left column between p1 and p3).
        assert_eq!(layout.pane_pixel_rects().len(), 3);
        assert_eq!(layout.splitter_count(), 2);
    }

    #[test]
    fn origin_offset_applied_to_rects() {
        let tree = LayoutTree::new();
        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 10.0, 42.0, 80, 24);

        let p1 = tree.focus();
        let rect = layout.pane_pixel_rect(&p1).unwrap();
        assert!((rect.x - 10.0).abs() < 0.01);
        assert!((rect.y - 42.0).abs() < 0.01);
    }

    #[test]
    fn click_inside_pane_returns_focus_action() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Click in the right pane (x=400, which is past 320 midpoint).
        let action = layout.on_mouse_down(400.0, 100.0);
        assert_eq!(action, Some(PaneLayoutAction::FocusPane(p2)));
    }

    #[test]
    fn click_on_splitter_starts_drag() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Click on the splitter at x=320.
        let action = layout.on_mouse_down(320.0, 100.0);
        assert_eq!(action, None); // No action emitted, but drag started.
        assert!(layout.is_dragging());
    }

    #[test]
    fn drag_splitter_produces_resize() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Start drag at splitter (x=320).
        layout.on_mouse_down(320.0, 100.0);
        assert!(layout.is_dragging());

        // Move right by one cell width (8px).
        let action = layout.on_mouse_move(328.0, 100.0);
        assert!(action.is_some());
        if let Some(PaneLayoutAction::Resize {
            direction, cells, ..
        }) = action
        {
            assert_eq!(direction, ResizeDirection::GrowRight);
            assert_eq!(cells, 1);
        } else {
            panic!("expected Resize action");
        }
    }

    #[test]
    fn mouse_up_ends_drag() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        layout.on_mouse_down(320.0, 100.0);
        assert!(layout.is_dragging());

        layout.on_mouse_up(330.0, 100.0);
        assert!(!layout.is_dragging());
    }

    #[test]
    fn cursor_hint_on_splitter() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // On the vertical splitter at x=320.
        assert_eq!(
            layout.cursor_hint(320.0, 100.0),
            CursorHint::ResizeHorizontal
        );
        // Away from splitter.
        assert_eq!(layout.cursor_hint(200.0, 100.0), CursorHint::Arrow);
    }

    #[test]
    fn cursor_hint_on_horizontal_splitter() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_down(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // On the horizontal splitter at y=192.
        assert_eq!(layout.cursor_hint(200.0, 192.0), CursorHint::ResizeVertical);
    }

    #[test]
    fn sub_cell_drag_accumulates() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Start drag at splitter.
        layout.on_mouse_down(320.0, 100.0);

        // Move less than one cell — no action yet.
        let action = layout.on_mouse_move(323.0, 100.0);
        assert_eq!(action, None);

        // Move more — now crosses cell boundary.
        let action = layout.on_mouse_move(329.0, 100.0);
        assert!(action.is_some());
    }

    #[test]
    fn pixel_rect_contains() {
        let r = PixelRect::new(10.0, 20.0, 100.0, 50.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(50.0, 40.0));
        assert!(!r.contains(9.9, 20.0));
        assert!(!r.contains(10.0, 70.1));
        assert!(!r.contains(110.0, 20.0)); // right edge exclusive
    }

    #[test]
    fn zoomed_pane_single_rect() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();
        tree.toggle_zoom();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Zoomed: only one pane rect, no splitters.
        assert_eq!(layout.pane_pixel_rects().len(), 1);
        assert_eq!(layout.splitter_count(), 0);
    }

    #[test]
    fn drag_left_produces_shrink() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        let (cw, ch) = cell_size();
        let mut layout = PaneLayout::new(cw, ch);
        layout.update(&tree, 0.0, 0.0, 80, 24);

        // Start drag at splitter.
        layout.on_mouse_down(320.0, 100.0);

        // Move left by one cell width.
        let action = layout.on_mouse_move(312.0, 100.0);
        assert!(action.is_some());
        if let Some(PaneLayoutAction::Resize {
            direction, cells, ..
        }) = action
        {
            assert_eq!(direction, ResizeDirection::ShrinkRight);
            assert_eq!(cells, 1);
        } else {
            panic!("expected Resize action");
        }
    }
}
