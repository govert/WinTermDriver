//! Mouse input handling: focus, selection, scroll, paste, and VT mouse reporting (§21.6).
//!
//! [`MouseHandler`] is the central coordinator for all mouse interactions in the
//! terminal UI. It receives raw [`MouseEvent`]s and produces [`MouseOutput`]
//! actions that the main loop dispatches.

use std::collections::HashMap;

use wtd_core::ids::PaneId;
use wtd_pty::MouseMode;

use crate::pane_layout::{CursorHint, PaneLayout, PaneLayoutAction, PixelRect};
use crate::renderer::TextSelection;
use crate::tab_strip::{TabAction, TabStrip};
use crate::window::MouseEvent;
use crate::window::MouseEventKind;

// ── Constants ────────────────────────────────────────────────────────────────

/// Number of scrollback lines per wheel notch (WHEEL_DELTA = 120).
const SCROLL_LINES_PER_NOTCH: i32 = 3;

/// Win32 WHEEL_DELTA constant.
const WHEEL_DELTA: i32 = 120;

// ── MouseOutput ─────────────────────────────────────────────────────────────

/// Action produced by mouse handling for the main loop to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum MouseOutput {
    /// Focus a specific pane.
    FocusPane(PaneId),
    /// Update (or clear) the text selection for a pane.
    SelectionChanged(PaneId, Option<TextSelection>),
    /// Splitter resize action (forwarded from PaneLayout).
    PaneResize(PaneLayoutAction),
    /// Send raw bytes to a session (VT mouse sequences).
    SendToSession(PaneId, Vec<u8>),
    /// Scroll a pane's scrollback view by the given number of lines
    /// (positive = scroll up / back in history, negative = scroll down / towards live).
    ScrollPane(PaneId, i32),
    /// Paste clipboard contents into a pane.
    PasteClipboard(PaneId),
    /// Tab strip action.
    Tab(TabAction),
    /// Change the window cursor shape.
    SetCursor(CursorHint),
}

// ── Per-pane state ──────────────────────────────────────────────────────────

/// Tracks per-pane mouse-related state (scroll offset, selection, mouse mode).
#[derive(Debug, Clone)]
struct PaneMouseState {
    /// Current scroll offset into scrollback (0 = live, positive = lines above live).
    scroll_offset: i32,
    /// Active text selection, if any.
    selection: Option<SelectionDrag>,
}

/// Active selection drag.
#[derive(Debug, Clone)]
struct SelectionDrag {
    /// Screen coordinates where selection started.
    start_row: usize,
    start_col: usize,
    /// Current end of selection (updated as mouse moves).
    end_row: usize,
    end_col: usize,
}

impl SelectionDrag {
    fn to_text_selection(&self) -> TextSelection {
        TextSelection {
            start_row: self.start_row,
            start_col: self.start_col,
            end_row: self.end_row,
            end_col: self.end_col,
        }
    }
}

// ── MouseHandler ────────────────────────────────────────────────────────────

/// Central mouse input handler coordinating focus, selection, scroll, paste,
/// splitter drag, and VT mouse reporting.
pub struct MouseHandler {
    pane_states: HashMap<PaneId, PaneMouseState>,
    /// Which pane has an active selection drag (only one at a time).
    selecting_pane: Option<PaneId>,
    /// Whether a left button is currently held down.
    left_down: bool,
    /// Whether we're in a splitter drag (managed by PaneLayout).
    splitter_dragging: bool,
}

impl MouseHandler {
    pub fn new() -> Self {
        MouseHandler {
            pane_states: HashMap::new(),
            selecting_pane: None,
            left_down: false,
            splitter_dragging: false,
        }
    }

    /// Current scroll offset for a pane (0 = live).
    pub fn scroll_offset(&self, pane_id: &PaneId) -> i32 {
        self.pane_states.get(pane_id).map_or(0, |s| s.scroll_offset)
    }

    /// Current text selection for a pane, if any.
    pub fn selection(&self, pane_id: &PaneId) -> Option<TextSelection> {
        self.pane_states
            .get(pane_id)
            .and_then(|s| s.selection.as_ref())
            .map(|d| d.to_text_selection())
    }

    /// Remove a pane from tracking (e.g., when closed).
    pub fn remove_pane(&mut self, pane_id: &PaneId) {
        self.pane_states.remove(pane_id);
        if self.selecting_pane.as_ref() == Some(pane_id) {
            self.selecting_pane = None;
        }
    }

    /// Clear selection for a pane (e.g., when new input arrives).
    pub fn clear_selection(&mut self, pane_id: &PaneId) {
        if let Some(state) = self.pane_states.get_mut(pane_id) {
            state.selection = None;
        }
        if self.selecting_pane.as_ref() == Some(pane_id) {
            self.selecting_pane = None;
        }
    }

    /// Process a mouse event and return any actions the main loop should execute.
    ///
    /// Parameters:
    /// - `event`: the raw mouse event
    /// - `tab_strip`: the tab strip component (for hit-testing the top bar)
    /// - `pane_layout`: the pane layout component (for splitter and pane hit-testing)
    /// - `tab_strip_height`: pixel height of the tab strip
    /// - `status_bar_height`: pixel height of the status bar
    /// - `window_height`: total window pixel height
    /// - `focused_pane`: currently focused pane ID
    /// - `mouse_modes`: map of pane ID → mouse mode (from ScreenBuffer)
    /// - `cell_width`/`cell_height`: cell dimensions in pixels
    pub fn handle_event(
        &mut self,
        event: &MouseEvent,
        tab_strip: &mut TabStrip,
        pane_layout: &mut PaneLayout,
        tab_strip_height: f32,
        status_bar_height: f32,
        window_height: f32,
        focused_pane: &PaneId,
        mouse_modes: &HashMap<PaneId, MouseMode>,
        cell_width: f32,
        cell_height: f32,
        pane_margin_x_cells: f32,
        pane_margin_y_cells: f32,
    ) -> Vec<MouseOutput> {
        let mut outputs = Vec::new();
        let content_bottom = window_height - status_bar_height;

        match event.kind {
            // ── Left button down ─────────────────────────────────────────
            MouseEventKind::LeftDown => {
                self.left_down = true;

                // Tab strip area
                if event.y < tab_strip_height {
                    if let Some(action) = tab_strip.on_mouse_down(event.x, event.y) {
                        outputs.push(MouseOutput::Tab(action));
                    }
                    return outputs;
                }

                // Status bar — ignore
                if event.y >= content_bottom {
                    return outputs;
                }

                // Pane/splitter area
                if let Some(action) = pane_layout.on_mouse_down(event.x, event.y) {
                    match action {
                        PaneLayoutAction::FocusPane(pane_id) => {
                            outputs.push(MouseOutput::FocusPane(pane_id.clone()));

                            // Check if this pane has mouse reporting enabled
                            let mode = mouse_modes
                                .get(&pane_id)
                                .copied()
                                .unwrap_or(MouseMode::None);
                            if mode != MouseMode::None {
                                // Forward as VT mouse press
                                if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
                                    let content_rect = inset_pane_rect(
                                        rect,
                                        cell_width,
                                        cell_height,
                                        pane_margin_x_cells,
                                        pane_margin_y_cells,
                                    );
                                    let (col, row) = pixel_to_cell(
                                        event.x,
                                        event.y,
                                        content_rect,
                                        cell_width,
                                        cell_height,
                                    );
                                    let sgr = mouse_modes_use_sgr(mouse_modes, &pane_id);
                                    let seq = encode_mouse_event(
                                        MouseButton::Left,
                                        true,
                                        col,
                                        row,
                                        0,
                                        sgr,
                                    );
                                    outputs.push(MouseOutput::SendToSession(pane_id, seq));
                                }
                            } else {
                                // Start text selection
                                if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
                                    let content_rect = inset_pane_rect(
                                        rect,
                                        cell_width,
                                        cell_height,
                                        pane_margin_x_cells,
                                        pane_margin_y_cells,
                                    );
                                    let (col, row) = pixel_to_cell(
                                        event.x,
                                        event.y,
                                        content_rect,
                                        cell_width,
                                        cell_height,
                                    );
                                    let state = self
                                        .pane_states
                                        .entry(pane_id.clone())
                                        .or_insert_with(|| PaneMouseState {
                                            scroll_offset: 0,
                                            selection: None,
                                        });
                                    state.selection = Some(SelectionDrag {
                                        start_row: row,
                                        start_col: col,
                                        end_row: row,
                                        end_col: col,
                                    });
                                    self.selecting_pane = Some(pane_id.clone());
                                    // Initial click clears any visible selection
                                    outputs.push(MouseOutput::SelectionChanged(pane_id, None));
                                }
                            }
                        }
                        action @ PaneLayoutAction::Resize { .. } => {
                            self.splitter_dragging = true;
                            outputs.push(MouseOutput::PaneResize(action));
                        }
                    }
                } else if pane_layout.is_dragging() {
                    self.splitter_dragging = true;
                }
            }

            // ── Left button up ───────────────────────────────────────────
            MouseEventKind::LeftUp => {
                self.left_down = false;

                // Tab strip release
                if event.y < tab_strip_height {
                    if let Some(action) = tab_strip.on_mouse_up(event.x, event.y) {
                        outputs.push(MouseOutput::Tab(action));
                    }
                }

                // End splitter drag
                if self.splitter_dragging {
                    pane_layout.on_mouse_up(event.x, event.y);
                    self.splitter_dragging = false;
                }

                // End selection drag — finalize
                if let Some(pane_id) = self.selecting_pane.take() {
                    let sel_result = self
                        .pane_states
                        .get(&pane_id)
                        .and_then(|s| s.selection.as_ref())
                        .map(|d| d.to_text_selection());
                    if let Some(sel) = sel_result {
                        if sel.start_row != sel.end_row || sel.start_col != sel.end_col {
                            outputs.push(MouseOutput::SelectionChanged(pane_id, Some(sel)));
                        } else {
                            if let Some(state) = self.pane_states.get_mut(&pane_id) {
                                state.selection = None;
                            }
                            outputs.push(MouseOutput::SelectionChanged(pane_id, None));
                        }
                    }
                }

                // VT mouse release
                let mode = mouse_modes
                    .get(&focused_pane)
                    .copied()
                    .unwrap_or(MouseMode::None);
                if mode != MouseMode::None {
                    if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                        if rect_contains(&rect, event.x, event.y) {
                            let content_rect = inset_pane_rect(
                                rect,
                                cell_width,
                                cell_height,
                                pane_margin_x_cells,
                                pane_margin_y_cells,
                            );
                            let (col, row) = pixel_to_cell(
                                event.x,
                                event.y,
                                content_rect,
                                cell_width,
                                cell_height,
                            );
                            let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                            let seq =
                                encode_mouse_event(MouseButton::Left, false, col, row, 0, sgr);
                            outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                        }
                    }
                }
            }

            // ── Mouse move ───────────────────────────────────────────────
            MouseEventKind::Move => {
                // Tab strip hover
                if event.y < tab_strip_height {
                    tab_strip.on_mouse_move(event.x, event.y);
                }

                // Splitter drag
                if self.splitter_dragging {
                    if let Some(action) = pane_layout.on_mouse_move(event.x, event.y) {
                        outputs.push(MouseOutput::PaneResize(action));
                    }
                    return outputs;
                }

                // Update cursor hint
                if event.y >= tab_strip_height && event.y < content_bottom {
                    let hint = pane_layout.cursor_hint(event.x, event.y);
                    outputs.push(MouseOutput::SetCursor(hint));

                    // Update pane layout hover state
                    pane_layout.on_mouse_move(event.x, event.y);
                }

                // Selection drag
                if self.left_down {
                    if let Some(pane_id) = &self.selecting_pane {
                        if let Some(rect) = pane_layout.pane_pixel_rect(pane_id) {
                            let content_rect = inset_pane_rect(
                                rect,
                                cell_width,
                                cell_height,
                                pane_margin_x_cells,
                                pane_margin_y_cells,
                            );
                            let (col, row) = pixel_to_cell(
                                event.x,
                                event.y,
                                content_rect,
                                cell_width,
                                cell_height,
                            );
                            let pane_id = pane_id.clone();
                            if let Some(state) = self.pane_states.get_mut(&pane_id) {
                                if let Some(drag) = &mut state.selection {
                                    drag.end_row = row;
                                    drag.end_col = col;
                                    let sel = drag.to_text_selection();
                                    outputs.push(MouseOutput::SelectionChanged(pane_id, Some(sel)));
                                }
                            }
                        }
                    }
                }

                // VT mouse motion reporting
                let mode = mouse_modes
                    .get(&focused_pane)
                    .copied()
                    .unwrap_or(MouseMode::None);
                let report_motion = match mode {
                    MouseMode::AnyEvent => true,
                    MouseMode::ButtonEvent => self.left_down,
                    _ => false,
                };
                if report_motion {
                    if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                        if rect_contains(&rect, event.x, event.y) {
                            let content_rect = inset_pane_rect(
                                rect,
                                cell_width,
                                cell_height,
                                pane_margin_x_cells,
                                pane_margin_y_cells,
                            );
                            let (col, row) = pixel_to_cell(
                                event.x,
                                event.y,
                                content_rect,
                                cell_width,
                                cell_height,
                            );
                            let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                            // Motion events use button 0 + 32 (motion flag)
                            let button = if self.left_down {
                                MouseButton::Left
                            } else {
                                MouseButton::None
                            };
                            let seq = encode_mouse_motion(button, col, row, 0, sgr);
                            outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                        }
                    }
                }
            }

            // ── Right button ─────────────────────────────────────────────
            MouseEventKind::RightDown => {
                if event.y >= tab_strip_height && event.y < content_bottom {
                    let mode = mouse_modes
                        .get(&focused_pane)
                        .copied()
                        .unwrap_or(MouseMode::None);
                    if mode != MouseMode::None {
                        // Forward as VT mouse event
                        if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                            if rect_contains(&rect, event.x, event.y) {
                                let content_rect = inset_pane_rect(
                                    rect,
                                    cell_width,
                                    cell_height,
                                    pane_margin_x_cells,
                                    pane_margin_y_cells,
                                );
                                let (col, row) = pixel_to_cell(
                                    event.x,
                                    event.y,
                                    content_rect,
                                    cell_width,
                                    cell_height,
                                );
                                let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                                let seq =
                                    encode_mouse_event(MouseButton::Right, true, col, row, 0, sgr);
                                outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                            }
                        }
                    } else {
                        // Paste clipboard into focused pane
                        outputs.push(MouseOutput::PasteClipboard(focused_pane.clone()));
                    }
                }
            }
            MouseEventKind::RightUp => {
                if event.y >= tab_strip_height && event.y < content_bottom {
                    let mode = mouse_modes
                        .get(&focused_pane)
                        .copied()
                        .unwrap_or(MouseMode::None);
                    if mode != MouseMode::None {
                        if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                            if rect_contains(&rect, event.x, event.y) {
                                let content_rect = inset_pane_rect(
                                    rect,
                                    cell_width,
                                    cell_height,
                                    pane_margin_x_cells,
                                    pane_margin_y_cells,
                                );
                                let (col, row) = pixel_to_cell(
                                    event.x,
                                    event.y,
                                    content_rect,
                                    cell_width,
                                    cell_height,
                                );
                                let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                                let seq =
                                    encode_mouse_event(MouseButton::Right, false, col, row, 0, sgr);
                                outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                            }
                        }
                    }
                }
            }

            // ── Middle button ────────────────────────────────────────────
            MouseEventKind::MiddleDown => {
                if event.y >= tab_strip_height && event.y < content_bottom {
                    let mode = mouse_modes
                        .get(&focused_pane)
                        .copied()
                        .unwrap_or(MouseMode::None);
                    if mode != MouseMode::None {
                        if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                            if rect_contains(&rect, event.x, event.y) {
                                let content_rect = inset_pane_rect(
                                    rect,
                                    cell_width,
                                    cell_height,
                                    pane_margin_x_cells,
                                    pane_margin_y_cells,
                                );
                                let (col, row) = pixel_to_cell(
                                    event.x,
                                    event.y,
                                    content_rect,
                                    cell_width,
                                    cell_height,
                                );
                                let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                                let seq =
                                    encode_mouse_event(MouseButton::Middle, true, col, row, 0, sgr);
                                outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                            }
                        }
                    }
                }
            }
            MouseEventKind::MiddleUp => {
                if event.y >= tab_strip_height && event.y < content_bottom {
                    let mode = mouse_modes
                        .get(&focused_pane)
                        .copied()
                        .unwrap_or(MouseMode::None);
                    if mode != MouseMode::None {
                        if let Some(rect) = pane_layout.pane_pixel_rect(&focused_pane) {
                            if rect_contains(&rect, event.x, event.y) {
                                let content_rect = inset_pane_rect(
                                    rect,
                                    cell_width,
                                    cell_height,
                                    pane_margin_x_cells,
                                    pane_margin_y_cells,
                                );
                                let (col, row) = pixel_to_cell(
                                    event.x,
                                    event.y,
                                    content_rect,
                                    cell_width,
                                    cell_height,
                                );
                                let sgr = mouse_modes_use_sgr(mouse_modes, &focused_pane);
                                let seq = encode_mouse_event(
                                    MouseButton::Middle,
                                    false,
                                    col,
                                    row,
                                    0,
                                    sgr,
                                );
                                outputs.push(MouseOutput::SendToSession(focused_pane.clone(), seq));
                            }
                        }
                    }
                }
            }

            // ── Scroll wheel ─────────────────────────────────────────────
            MouseEventKind::Wheel(delta) => {
                if event.y >= tab_strip_height && event.y < content_bottom {
                    // Find which pane the cursor is over
                    let target_pane = pane_at_point(pane_layout, event.x, event.y)
                        .unwrap_or_else(|| focused_pane.clone());

                    let mode = mouse_modes
                        .get(&target_pane)
                        .copied()
                        .unwrap_or(MouseMode::None);
                    if mode != MouseMode::None {
                        // Forward as VT scroll events
                        if let Some(rect) = pane_layout.pane_pixel_rect(&target_pane) {
                            let content_rect = inset_pane_rect(
                                rect,
                                cell_width,
                                cell_height,
                                pane_margin_x_cells,
                                pane_margin_y_cells,
                            );
                            let (col, row) = pixel_to_cell(
                                event.x,
                                event.y,
                                content_rect,
                                cell_width,
                                cell_height,
                            );
                            let sgr = mouse_modes_use_sgr(mouse_modes, &target_pane);
                            let button = if delta > 0 {
                                MouseButton::WheelUp
                            } else {
                                MouseButton::WheelDown
                            };
                            let notches =
                                (delta.unsigned_abs() as i32 + WHEEL_DELTA - 1) / WHEEL_DELTA;
                            for _ in 0..notches {
                                let seq = encode_mouse_event(button, true, col, row, 0, sgr);
                                outputs.push(MouseOutput::SendToSession(target_pane.clone(), seq));
                            }
                        }
                    } else {
                        // Scroll scrollback buffer
                        let notches = delta as i32 / WHEEL_DELTA;
                        let lines = notches * SCROLL_LINES_PER_NOTCH;
                        // Positive delta = wheel up = scroll back (increase offset)
                        let state =
                            self.pane_states
                                .entry(target_pane.clone())
                                .or_insert_with(|| PaneMouseState {
                                    scroll_offset: 0,
                                    selection: None,
                                });
                        state.scroll_offset = (state.scroll_offset + lines).max(0);
                        outputs.push(MouseOutput::ScrollPane(target_pane, state.scroll_offset));
                    }
                }
            }
        }

        outputs
    }

    /// Reset scroll offset for a pane back to live (0).
    pub fn reset_scroll(&mut self, pane_id: &PaneId) {
        if let Some(state) = self.pane_states.get_mut(pane_id) {
            state.scroll_offset = 0;
        }
    }

    /// Clamp scroll offset to valid range given the pane's scrollback length.
    pub fn clamp_scroll(&mut self, pane_id: &PaneId, max_scrollback: i32) {
        if let Some(state) = self.pane_states.get_mut(pane_id) {
            state.scroll_offset = state.scroll_offset.clamp(0, max_scrollback);
        }
    }
}

impl Default for MouseHandler {
    fn default() -> Self {
        Self::new()
    }
}

// ── VT mouse encoding ──────────────────────────────────────────────────────

/// Mouse button identifiers for VT encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// No button (motion-only events).
    None,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    /// VT button code for SGR and normal modes (without modifier bits).
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    }
}

/// Encode a mouse press/release event as a VT escape sequence.
///
/// Uses SGR format (`\x1b[<...M` / `\x1b[<...m`) when `sgr` is true,
/// otherwise uses the legacy X10 format (`\x1b[M...`).
///
/// `col` and `row` are 0-based; the encoding converts to 1-based as needed.
/// `modifier_bits`: 4=shift, 8=alt, 16=ctrl (OR'd into button code).
pub fn encode_mouse_event(
    button: MouseButton,
    press: bool,
    col: usize,
    row: usize,
    modifier_bits: u8,
    sgr: bool,
) -> Vec<u8> {
    let cb = button.code() | modifier_bits;
    if sgr {
        // SGR format: \x1b[<Cb;Cx;CyM (press) or \x1b[<Cb;Cx;Cym (release)
        let suffix = if press { 'M' } else { 'm' };
        format!("\x1b[<{};{};{}{}", cb, col + 1, row + 1, suffix).into_bytes()
    } else {
        // Legacy X10 format: \x1b[M Cb Cx Cy (all + 32)
        // Release is button code 3 in legacy mode
        let cb = if press { cb + 32 } else { 3 + 32 };
        let cx = ((col + 1) as u8).saturating_add(32);
        let cy = ((row + 1) as u8).saturating_add(32);
        vec![0x1b, b'[', b'M', cb, cx, cy]
    }
}

/// Encode a mouse motion event as a VT escape sequence.
///
/// Motion events add 32 to the button code to indicate movement.
pub fn encode_mouse_motion(
    button: MouseButton,
    col: usize,
    row: usize,
    modifier_bits: u8,
    sgr: bool,
) -> Vec<u8> {
    let cb = button.code() | modifier_bits | 32; // motion flag
    if sgr {
        format!("\x1b[<{};{};{}M", cb, col + 1, row + 1).into_bytes()
    } else {
        let cb = cb + 32;
        let cx = ((col + 1) as u8).saturating_add(32);
        let cy = ((row + 1) as u8).saturating_add(32);
        vec![0x1b, b'[', b'M', cb, cx, cy]
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn inset_pane_rect(
    rect: PixelRect,
    cell_width: f32,
    cell_height: f32,
    pane_margin_x_cells: f32,
    pane_margin_y_cells: f32,
) -> PixelRect {
    let desired_inset_x = (cell_width * pane_margin_x_cells).max(0.0);
    let desired_inset_y = (cell_height * pane_margin_y_cells).max(0.0);
    let max_inset_x = ((rect.width - cell_width).max(0.0)) * 0.5;
    let max_inset_y = ((rect.height - cell_height).max(0.0)) * 0.5;
    let inset_x = desired_inset_x.min(max_inset_x);
    let inset_y = desired_inset_y.min(max_inset_y);

    PixelRect::new(
        rect.x + inset_x,
        rect.y + inset_y,
        (rect.width - inset_x * 2.0).max(cell_width.min(rect.width)),
        (rect.height - inset_y * 2.0).max(cell_height.min(rect.height)),
    )
}

/// Convert pixel coordinates to cell (col, row) within a pane's rectangle.
fn pixel_to_cell(
    px: f32,
    py: f32,
    rect: PixelRect,
    cell_width: f32,
    cell_height: f32,
) -> (usize, usize) {
    let local_x = (px - rect.x).max(0.0);
    let local_y = (py - rect.y).max(0.0);
    let max_col = ((rect.width / cell_width).ceil() as usize).saturating_sub(1);
    let max_row = ((rect.height / cell_height).ceil() as usize).saturating_sub(1);
    let col = ((local_x / cell_width) as usize).min(max_col);
    let row = ((local_y / cell_height) as usize).min(max_row);
    (col, row)
}

/// Check if a pixel point is inside a PixelRect.
fn rect_contains(rect: &PixelRect, x: f32, y: f32) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Find which pane contains the given pixel coordinates.
fn pane_at_point(pane_layout: &PaneLayout, x: f32, y: f32) -> Option<PaneId> {
    for (pane_id, rect) in pane_layout.pane_pixel_rects() {
        if rect_contains(rect, x, y) {
            return Some(pane_id.clone());
        }
    }
    None
}

/// Check if a pane is using SGR mouse format.
fn mouse_modes_use_sgr(mouse_modes: &HashMap<PaneId, MouseMode>, pane_id: &PaneId) -> bool {
    // SGR format is tracked separately in ScreenBuffer; for now we always prefer
    // SGR when mouse mode is enabled, as it is the modern standard. The caller
    // should pass accurate sgr state via a parallel map if needed. For this
    // implementation, we default to SGR=true when mouse mode is active, since
    // most modern applications enable both 1006 and 100x together.
    mouse_modes.get(pane_id).copied().unwrap_or(MouseMode::None) != MouseMode::None
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── VT encoding tests ───────────────────────────────────────────────

    #[test]
    fn sgr_left_press_at_origin() {
        let seq = encode_mouse_event(MouseButton::Left, true, 0, 0, 0, true);
        assert_eq!(seq, b"\x1b[<0;1;1M");
    }

    #[test]
    fn sgr_left_release_at_origin() {
        let seq = encode_mouse_event(MouseButton::Left, false, 0, 0, 0, true);
        assert_eq!(seq, b"\x1b[<0;1;1m");
    }

    #[test]
    fn sgr_right_press() {
        let seq = encode_mouse_event(MouseButton::Right, true, 5, 10, 0, true);
        assert_eq!(seq, b"\x1b[<2;6;11M");
    }

    #[test]
    fn sgr_middle_press() {
        let seq = encode_mouse_event(MouseButton::Middle, true, 3, 7, 0, true);
        assert_eq!(seq, b"\x1b[<1;4;8M");
    }

    #[test]
    fn sgr_wheel_up() {
        let seq = encode_mouse_event(MouseButton::WheelUp, true, 10, 20, 0, true);
        assert_eq!(seq, b"\x1b[<64;11;21M");
    }

    #[test]
    fn sgr_wheel_down() {
        let seq = encode_mouse_event(MouseButton::WheelDown, true, 10, 20, 0, true);
        assert_eq!(seq, b"\x1b[<65;11;21M");
    }

    #[test]
    fn sgr_with_ctrl_modifier() {
        let seq = encode_mouse_event(MouseButton::Left, true, 0, 0, 16, true);
        assert_eq!(seq, b"\x1b[<16;1;1M");
    }

    #[test]
    fn sgr_with_shift_modifier() {
        let seq = encode_mouse_event(MouseButton::Left, true, 0, 0, 4, true);
        assert_eq!(seq, b"\x1b[<4;1;1M");
    }

    #[test]
    fn sgr_with_alt_modifier() {
        let seq = encode_mouse_event(MouseButton::Left, true, 0, 0, 8, true);
        assert_eq!(seq, b"\x1b[<8;1;1M");
    }

    #[test]
    fn sgr_motion_event() {
        let seq = encode_mouse_motion(MouseButton::Left, 5, 3, 0, true);
        // Motion flag adds 32 to button code: 0 + 32 = 32
        assert_eq!(seq, b"\x1b[<32;6;4M");
    }

    #[test]
    fn sgr_motion_no_button() {
        let seq = encode_mouse_motion(MouseButton::None, 5, 3, 0, true);
        // None=3, motion=32 → 35
        assert_eq!(seq, b"\x1b[<35;6;4M");
    }

    #[test]
    fn legacy_left_press() {
        let seq = encode_mouse_event(MouseButton::Left, true, 0, 0, 0, false);
        // button 0 + 32 = 32, col 1+32=33, row 1+32=33
        assert_eq!(seq, vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn legacy_left_release() {
        let seq = encode_mouse_event(MouseButton::Left, false, 0, 0, 0, false);
        // release is button 3 + 32 = 35
        assert_eq!(seq, vec![0x1b, b'[', b'M', 35, 33, 33]);
    }

    #[test]
    fn legacy_right_press_at_offset() {
        let seq = encode_mouse_event(MouseButton::Right, true, 10, 5, 0, false);
        // button 2 + 32 = 34, col 11+32=43, row 6+32=38
        assert_eq!(seq, vec![0x1b, b'[', b'M', 34, 43, 38]);
    }

    #[test]
    fn legacy_motion_event() {
        let seq = encode_mouse_motion(MouseButton::Left, 2, 4, 0, false);
        // button 0 | 32 (motion) = 32, +32 encoding = 64, col 3+32=35, row 5+32=37
        assert_eq!(seq, vec![0x1b, b'[', b'M', 64, 35, 37]);
    }

    // ── pixel_to_cell tests ─────────────────────────────────────────────

    #[test]
    fn pixel_to_cell_basic() {
        let rect = PixelRect::new(100.0, 50.0, 400.0, 300.0);
        let (col, row) = pixel_to_cell(108.0, 66.0, rect, 8.0, 16.0);
        assert_eq!(col, 1); // (108-100)/8 = 1
        assert_eq!(row, 1); // (66-50)/16 = 1
    }

    #[test]
    fn inset_pane_rect_keeps_one_cell_visible_for_small_panes() {
        let rect = PixelRect::new(0.0, 0.0, 8.0, 16.0);
        let inset = inset_pane_rect(rect, 8.0, 16.0, 0.5, 0.5);
        assert_eq!(inset.width, 8.0);
        assert_eq!(inset.height, 16.0);
    }

    #[test]
    fn pixel_to_cell_origin() {
        let rect = PixelRect::new(100.0, 50.0, 400.0, 300.0);
        let (col, row) = pixel_to_cell(100.0, 50.0, rect, 8.0, 16.0);
        assert_eq!(col, 0);
        assert_eq!(row, 0);
    }

    #[test]
    fn pixel_to_cell_clamps_negative() {
        let rect = PixelRect::new(100.0, 50.0, 400.0, 300.0);
        let (col, row) = pixel_to_cell(50.0, 20.0, rect, 8.0, 16.0);
        assert_eq!(col, 0);
        assert_eq!(row, 0);
    }

    #[test]
    fn pixel_to_cell_clamps_to_last_visible_cell() {
        let rect = PixelRect::new(100.0, 50.0, 80.0, 48.0);
        let (col, row) = pixel_to_cell(500.0, 500.0, rect, 8.0, 16.0);
        assert_eq!((col, row), (9, 2));
    }

    // ── MouseHandler unit tests ─────────────────────────────────────────

    #[test]
    fn new_handler_has_no_state() {
        let handler = MouseHandler::new();
        let pane = PaneId(1);
        assert_eq!(handler.scroll_offset(&pane), 0);
        assert!(handler.selection(&pane).is_none());
    }

    #[test]
    fn default_is_new() {
        let handler = MouseHandler::default();
        assert_eq!(handler.scroll_offset(&PaneId(1)), 0);
    }

    #[test]
    fn clear_selection_no_panic_on_unknown_pane() {
        let mut handler = MouseHandler::new();
        handler.clear_selection(&PaneId(99));
    }

    #[test]
    fn remove_pane_cleans_up() {
        let mut handler = MouseHandler::new();
        let pane = PaneId(1);
        handler.pane_states.insert(
            pane.clone(),
            PaneMouseState {
                scroll_offset: 5,
                selection: None,
            },
        );
        assert_eq!(handler.scroll_offset(&pane), 5);
        handler.remove_pane(&pane);
        assert_eq!(handler.scroll_offset(&pane), 0);
    }

    #[test]
    fn reset_scroll_sets_zero() {
        let mut handler = MouseHandler::new();
        let pane = PaneId(1);
        handler.pane_states.insert(
            pane.clone(),
            PaneMouseState {
                scroll_offset: 10,
                selection: None,
            },
        );
        handler.reset_scroll(&pane);
        assert_eq!(handler.scroll_offset(&pane), 0);
    }

    #[test]
    fn clamp_scroll_bounds() {
        let mut handler = MouseHandler::new();
        let pane = PaneId(1);
        handler.pane_states.insert(
            pane.clone(),
            PaneMouseState {
                scroll_offset: 100,
                selection: None,
            },
        );
        handler.clamp_scroll(&pane, 50);
        assert_eq!(handler.scroll_offset(&pane), 50);
    }

    #[test]
    fn clamp_scroll_no_negative() {
        let mut handler = MouseHandler::new();
        let pane = PaneId(1);
        handler.pane_states.insert(
            pane.clone(),
            PaneMouseState {
                scroll_offset: -5,
                selection: None,
            },
        );
        handler.clamp_scroll(&pane, 50);
        assert_eq!(handler.scroll_offset(&pane), 0);
    }

    // ── ScreenBuffer mouse mode tests (via VT sequences) ────────────────

    #[test]
    fn screen_buffer_mouse_mode_tracking() {
        use wtd_pty::ScreenBuffer;

        let mut buf = ScreenBuffer::new(80, 24, 0);

        // Default is no mouse mode
        assert_eq!(buf.mouse_mode(), MouseMode::None);
        assert!(!buf.sgr_mouse());

        // Enable normal tracking
        buf.advance(b"\x1b[?1000h");
        assert_eq!(buf.mouse_mode(), MouseMode::Normal);

        // Enable SGR format
        buf.advance(b"\x1b[?1006h");
        assert!(buf.sgr_mouse());

        // Upgrade to button-event tracking
        buf.advance(b"\x1b[?1002h");
        assert_eq!(buf.mouse_mode(), MouseMode::ButtonEvent);

        // Upgrade to any-event tracking
        buf.advance(b"\x1b[?1003h");
        assert_eq!(buf.mouse_mode(), MouseMode::AnyEvent);

        // Disable any-event tracking
        buf.advance(b"\x1b[?1003l");
        assert_eq!(buf.mouse_mode(), MouseMode::None);

        // SGR still active
        assert!(buf.sgr_mouse());

        // Disable SGR
        buf.advance(b"\x1b[?1006l");
        assert!(!buf.sgr_mouse());
    }

    #[test]
    fn screen_buffer_ris_resets_mouse() {
        use wtd_pty::ScreenBuffer;

        let mut buf = ScreenBuffer::new(80, 24, 0);
        buf.advance(b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(buf.mouse_mode(), MouseMode::AnyEvent);
        assert!(buf.sgr_mouse());

        // Full reset
        buf.advance(b"\x1bc");
        assert_eq!(buf.mouse_mode(), MouseMode::None);
        assert!(!buf.sgr_mouse());
    }

    // ── TextSelection tests ─────────────────────────────────────────────

    #[test]
    fn selection_drag_to_text_selection() {
        let drag = SelectionDrag {
            start_row: 2,
            start_col: 5,
            end_row: 4,
            end_col: 10,
        };
        let sel = drag.to_text_selection();
        assert_eq!(sel.start_row, 2);
        assert_eq!(sel.start_col, 5);
        assert_eq!(sel.end_row, 4);
        assert_eq!(sel.end_col, 10);
    }

    #[test]
    fn rect_contains_inside() {
        let r = PixelRect::new(10.0, 20.0, 100.0, 50.0);
        assert!(rect_contains(&r, 50.0, 40.0));
    }

    #[test]
    fn rect_contains_outside() {
        let r = PixelRect::new(10.0, 20.0, 100.0, 50.0);
        assert!(!rect_contains(&r, 5.0, 40.0));
        assert!(!rect_contains(&r, 50.0, 75.0));
    }
}
