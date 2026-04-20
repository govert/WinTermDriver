//! Command palette overlay (§24.6).
//!
//! Modal overlay triggered by the configured command-palette binding. Provides fuzzy search
//! over all available actions, displays keybinding hints, and dispatches
//! the selected action through the unified action system.

use std::collections::HashMap;

use windows::core::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

use wtd_core::workspace::{ActionReference, BindingsDefinition};

use crate::input::{KeyEvent, KeyName};

// ── Constants ────────────────────────────────────────────────────────────────

const PALETTE_WIDTH: f32 = 480.0;
const ITEM_HEIGHT: f32 = 34.0;
const INPUT_HEIGHT: f32 = 36.0;
const PADDING: f32 = 8.0;
const BORDER_RADIUS: f32 = 6.0;
const MAX_VISIBLE_ITEMS: usize = 10;
const TOP_OFFSET: f32 = 60.0;

// Colors
const OVERLAY_ALPHA: f32 = 0.35;
const PALETTE_BG: (u8, u8, u8) = (30, 30, 42);
const INPUT_BG: (u8, u8, u8) = (42, 42, 58);
const INPUT_TEXT_COLOR: (u8, u8, u8) = (220, 220, 220);
const INPUT_PLACEHOLDER_COLOR: (u8, u8, u8) = (120, 120, 140);
const ITEM_NAME_COLOR: (u8, u8, u8) = (210, 210, 210);
const ITEM_DESC_COLOR: (u8, u8, u8) = (140, 140, 155);
const ITEM_HINT_COLOR: (u8, u8, u8) = (78, 201, 176);
const SELECTED_BG: (u8, u8, u8) = (55, 55, 75);
const BORDER_COLOR: (u8, u8, u8) = (65, 65, 85);
const CURSOR_COLOR: (u8, u8, u8) = (78, 201, 176);

const RUNNABLE_ACTIONS: &[(&str, &str)] = &[
    // Workspace lifecycle
    ("open-workspace", "Open or attach to a workspace"),
    ("close-workspace", "Close workspace UI"),
    ("recreate-workspace", "Tear down and recreate workspace"),
    ("save-workspace", "Save current workspace state"),
    // Window actions
    ("new-window", "Create a new window"),
    ("close-window", "Close window and all tabs"),
    // Tab management
    ("new-tab", "Create a new tab"),
    ("close-tab", "Close the current tab"),
    ("next-tab", "Switch to next tab"),
    ("prev-tab", "Switch to previous tab"),
    ("goto-tab", "Switch to tab by index or name"),
    ("rename-tab", "Rename the tab"),
    ("move-tab-left", "Move tab one position left"),
    ("move-tab-right", "Move tab one position right"),
    // Pane management
    ("split-right", "Split pane, new pane on right"),
    ("split-down", "Split pane, new pane below"),
    ("close-pane", "Close pane and kill session"),
    ("focus-next-pane", "Move focus to next pane"),
    ("focus-prev-pane", "Move focus to previous pane"),
    ("focus-pane-up", "Move focus up"),
    ("focus-pane-down", "Move focus down"),
    ("focus-pane-left", "Move focus left"),
    ("focus-pane-right", "Move focus right"),
    ("focus-pane", "Move focus to named pane"),
    ("zoom-pane", "Toggle pane zoom"),
    ("swap-pane-up", "Swap pane with the nearest pane above"),
    ("swap-pane-down", "Swap pane with the nearest pane below"),
    (
        "swap-pane-left",
        "Swap pane with the nearest pane on the left",
    ),
    (
        "swap-pane-right",
        "Swap pane with the nearest pane on the right",
    ),
    (
        "toggle-split-orientation",
        "Toggle the nearest ancestor split orientation",
    ),
    (
        "equalize-pane-split",
        "Reset the nearest ancestor split to an even ratio",
    ),
    ("equalize-tab", "Reset all tab splits to even ratios"),
    (
        "retile-even-horizontal",
        "Retile panes into an even left-to-right layout",
    ),
    (
        "retile-even-vertical",
        "Retile panes into an even top-to-bottom layout",
    ),
    ("retile-grid", "Retile panes into a near-square grid"),
    (
        "retile-main-left",
        "Retile panes with the focused pane as the main left pane",
    ),
    (
        "retile-main-right",
        "Retile panes with the focused pane as the main right pane",
    ),
    (
        "retile-main-top",
        "Retile panes with the focused pane as the main top pane",
    ),
    (
        "retile-main-bottom",
        "Retile panes with the focused pane as the main bottom pane",
    ),
    ("rename-pane", "Rename pane"),
    ("change-profile", "Relaunch pane with a different profile"),
    ("resize-pane-right", "Move pane splitter to the right"),
    ("resize-pane-left", "Move pane splitter to the left"),
    ("resize-pane-down", "Move pane splitter downward"),
    ("resize-pane-up", "Move pane splitter upward"),
    ("resize-pane-grow-right", "Grow pane to the right"),
    ("resize-pane-grow-down", "Grow pane downward"),
    ("resize-pane-shrink-right", "Shrink pane from the right"),
    ("resize-pane-shrink-down", "Shrink pane from above"),
    // Session & clipboard
    ("restart-session", "Kill and relaunch session"),
    ("copy", "Copy selected text to clipboard"),
    ("paste", "Paste clipboard content"),
    // UI actions
    ("toggle-command-palette", "Toggle command palette"),
    ("toggle-fullscreen", "Toggle window fullscreen"),
    ("enter-scrollback-mode", "Enter scrollback navigation mode"),
    (
        "pass-through-next-key",
        "Send the next keypress directly to the app",
    ),
];

// ── Public types ─────────────────────────────────────────────────────────────

/// A single entry in the command palette.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub name: String,
    pub description: String,
    pub keybinding: Option<String>,
}

/// Result of a palette interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteResult {
    /// Palette was dismissed (Escape or click outside).
    Dismissed,
    /// An action was selected for dispatch.
    Action(ActionReference),
    /// Input was consumed by the palette (typing, navigation).
    Consumed,
}

// ── CommandPalette ───────────────────────────────────────────────────────────

struct FilteredEntry {
    index: usize,
    score: i32,
}

#[derive(Debug, Clone)]
enum PaletteMode {
    Search,
    Prompt {
        action: String,
        arg_name: String,
        label: String,
        placeholder: String,
    },
    Selector {
        action: String,
        arg_name: String,
        _label: String,
        placeholder: String,
        allow_custom: bool,
        entries: Vec<PaletteEntry>,
    },
}

/// Modal command palette overlay with fuzzy search and action dispatch.
pub struct CommandPalette {
    visible: bool,
    mode: PaletteMode,
    query: String,
    entries: Vec<PaletteEntry>,
    profile_entries: Vec<PaletteEntry>,
    filtered: Vec<FilteredEntry>,
    selected: usize,
    scroll_offset: usize,
    // DirectWrite resources
    dw_factory: IDWriteFactory,
    tf_input: IDWriteTextFormat,
    tf_name: IDWriteTextFormat,
    tf_desc: IDWriteTextFormat,
    tf_hint: IDWriteTextFormat,
}

impl CommandPalette {
    /// Create a new command palette. Builds the action catalog and keybinding
    /// hints from the provided bindings.
    pub fn new(
        dw_factory: &IDWriteFactory,
        bindings: &BindingsDefinition,
        profile_entries: Vec<PaletteEntry>,
    ) -> Result<Self> {
        let font_wide: Vec<u16> = "Segoe UI"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let font = PCWSTR(font_wide.as_ptr());
        let locale_wide: Vec<u16> = "en-us".encode_utf16().chain(std::iter::once(0)).collect();
        let locale = PCWSTR(locale_wide.as_ptr());

        let tf_input = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.0,
                locale,
            )?
        };
        let tf_name = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                locale,
            )?
        };
        let tf_desc = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.0,
                locale,
            )?
        };
        let tf_hint = unsafe {
            dw_factory.CreateTextFormat(
                font,
                None,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.0,
                locale,
            )?
        };

        let entries = build_palette_entries(bindings);
        let filtered: Vec<FilteredEntry> = (0..entries.len())
            .map(|i| FilteredEntry { index: i, score: 0 })
            .collect();

        Ok(Self {
            visible: false,
            mode: PaletteMode::Search,
            query: String::new(),
            entries,
            profile_entries,
            filtered,
            selected: 0,
            scroll_offset: 0,
            dw_factory: dw_factory.clone(),
            tf_input,
            tf_name,
            tf_desc,
            tf_hint,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Open the palette, resetting query and selection.
    pub fn show(&mut self) {
        self.visible = true;
        self.mode = PaletteMode::Search;
        self.query.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    pub fn show_prompt(
        &mut self,
        action: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        initial_value: impl Into<String>,
    ) {
        self.visible = true;
        self.mode = PaletteMode::Prompt {
            action: action.into(),
            arg_name: "name".to_string(),
            label: label.into(),
            placeholder: placeholder.into(),
        };
        self.query = initial_value.into();
        self.selected = 0;
        self.scroll_offset = 0;
        self.filtered.clear();
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.mode = PaletteMode::Search;
    }

    pub fn toggle(&mut self) {
        if self.visible {
            self.hide();
        } else {
            self.show();
        }
    }

    /// Returns true when the palette exposes the given action.
    pub fn has_action(&self, action_name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == action_name)
    }

    pub fn profile_entries(&self) -> &[PaletteEntry] {
        &self.profile_entries
    }

    pub fn show_profile_selector(
        &mut self,
        action: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
    ) {
        self.show_selector(
            action,
            "profile",
            label,
            placeholder,
            String::new(),
            self.profile_entries.clone(),
            true,
        );
    }

    pub fn show_selector(
        &mut self,
        action: impl Into<String>,
        arg_name: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        initial_value: impl Into<String>,
        entries: Vec<PaletteEntry>,
        allow_custom: bool,
    ) {
        self.visible = true;
        self.mode = PaletteMode::Selector {
            action: action.into(),
            arg_name: arg_name.into(),
            _label: label.into(),
            placeholder: placeholder.into(),
            allow_custom,
            entries,
        };
        self.query = initial_value.into();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    pub fn on_text_input(&mut self, text: &str) -> PaletteResult {
        if !self.visible {
            return PaletteResult::Consumed;
        }

        let append_char = |palette: &mut Self, ch: char| {
            if !ch.is_control() {
                palette.query.push(ch);
            }
        };

        match &self.mode {
            PaletteMode::Prompt { .. } => {
                for ch in text.chars() {
                    append_char(self, ch);
                }
                PaletteResult::Consumed
            }
            PaletteMode::Selector { .. } => {
                for ch in text.chars() {
                    append_char(self, ch);
                }
                self.refilter();
                PaletteResult::Consumed
            }
            _ => {
                for ch in text.chars() {
                    append_char(self, ch);
                }
                self.refilter();
                PaletteResult::Consumed
            }
        }
    }

    /// Process a keyboard event while the palette is visible.
    pub fn on_key_event(&mut self, event: &KeyEvent) -> PaletteResult {
        if !self.visible {
            return PaletteResult::Consumed;
        }

        if let PaletteMode::Prompt {
            action, arg_name, ..
        } = &self.mode
        {
            match event.key {
                KeyName::Escape => {
                    self.hide();
                    return PaletteResult::Dismissed;
                }
                KeyName::Enter => {
                    let action = action.clone();
                    let arg_name = arg_name.clone();
                    let name = self.query.trim().to_string();
                    self.hide();
                    return PaletteResult::Action(ActionReference::WithArgs {
                        action,
                        args: Some(HashMap::from([(arg_name, name)])),
                    });
                }
                KeyName::Backspace => {
                    self.query.pop();
                    return PaletteResult::Consumed;
                }
                _ => {
                    if !event.modifiers.ctrl() && !event.modifiers.alt() {
                        if let Some(ch) = event.character {
                            if !ch.is_control() {
                                self.query.push(ch);
                            }
                        }
                    }
                    return PaletteResult::Consumed;
                }
            }
        }

        if let PaletteMode::Selector {
            action,
            arg_name,
            allow_custom,
            ..
        } = &self.mode
        {
            match event.key {
                KeyName::Escape => {
                    self.hide();
                    return PaletteResult::Dismissed;
                }
                KeyName::Enter => {
                    let action = action.clone();
                    let arg_name = arg_name.clone();
                    let selected = self
                        .filtered
                        .get(self.selected)
                        .and_then(|fe| self.mode_entries().get(fe.index))
                        .map(|entry| entry.name.clone());
                    let value = selected.or_else(|| {
                        if *allow_custom {
                            let query = self.query.trim();
                            (!query.is_empty()).then(|| query.to_string())
                        } else {
                            None
                        }
                    });
                    self.hide();
                    return match value {
                        Some(value) => PaletteResult::Action(ActionReference::WithArgs {
                            action,
                            args: Some(HashMap::from([(arg_name, value)])),
                        }),
                        None => PaletteResult::Dismissed,
                    };
                }
                KeyName::Up => {
                    if self.selected > 0 {
                        self.selected -= 1;
                        self.ensure_visible();
                    }
                    return PaletteResult::Consumed;
                }
                KeyName::Down => {
                    if self.selected + 1 < self.filtered.len() {
                        self.selected += 1;
                        self.ensure_visible();
                    }
                    return PaletteResult::Consumed;
                }
                KeyName::Backspace => {
                    self.query.pop();
                    self.refilter();
                    return PaletteResult::Consumed;
                }
                _ => {
                    if !event.modifiers.ctrl() && !event.modifiers.alt() {
                        if let Some(ch) = event.character {
                            if !ch.is_control() {
                                self.query.push(ch);
                                self.refilter();
                            }
                        }
                    }
                    return PaletteResult::Consumed;
                }
            }
        }

        match event.key {
            KeyName::Escape => {
                self.hide();
                PaletteResult::Dismissed
            }
            KeyName::Enter => {
                if let Some(fe) = self.filtered.get(self.selected) {
                    let name = self.entries[fe.index].name.clone();
                    self.hide();
                    PaletteResult::Action(ActionReference::Simple(name))
                } else {
                    self.hide();
                    PaletteResult::Dismissed
                }
            }
            KeyName::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_visible();
                }
                PaletteResult::Consumed
            }
            KeyName::Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.ensure_visible();
                }
                PaletteResult::Consumed
            }
            KeyName::Backspace => {
                self.query.pop();
                self.refilter();
                PaletteResult::Consumed
            }
            _ => {
                // Append printable characters (no Ctrl/Alt modifiers).
                if !event.modifiers.ctrl() && !event.modifiers.alt() {
                    if let Some(ch) = event.character {
                        if !ch.is_control() {
                            self.query.push(ch);
                            self.refilter();
                        }
                    }
                }
                PaletteResult::Consumed
            }
        }
    }

    /// Handle a mouse click. Returns `Some(result)` if the palette consumed
    /// the click, `None` if the palette is not visible.
    pub fn on_click(
        &mut self,
        x: f32,
        y: f32,
        window_w: f32,
        window_h: f32,
    ) -> Option<PaletteResult> {
        if !self.visible {
            return None;
        }

        let (px, py, pw, ph) = self.palette_rect(window_w, window_h);

        // Click outside palette — dismiss.
        if x < px || x > px + pw || y < py || y > py + ph {
            self.hide();
            return Some(PaletteResult::Dismissed);
        }

        if matches!(self.mode, PaletteMode::Prompt { .. }) {
            return Some(PaletteResult::Consumed);
        }

        // Click on an item in the list.
        let items_y = py + INPUT_HEIGHT + PADDING;
        if y >= items_y {
            let item_idx = ((y - items_y) / ITEM_HEIGHT) as usize + self.scroll_offset;
            if item_idx < self.filtered.len() {
                let name = self.mode_entries()[self.filtered[item_idx].index]
                    .name
                    .clone();
                if let PaletteMode::Selector {
                    action, arg_name, ..
                } = &self.mode
                {
                    let action = action.clone();
                    let arg_name = arg_name.clone();
                    self.hide();
                    return Some(PaletteResult::Action(ActionReference::WithArgs {
                        action,
                        args: Some(HashMap::from([(arg_name, name)])),
                    }));
                }
                self.hide();
                return Some(PaletteResult::Action(ActionReference::Simple(name)));
            }
        }

        Some(PaletteResult::Consumed)
    }

    /// Handle mouse wheel scrolling while the palette is visible.
    pub fn on_wheel(
        &mut self,
        x: f32,
        y: f32,
        delta: i16,
        window_w: f32,
        window_h: f32,
    ) -> Option<PaletteResult> {
        if !self.visible {
            return None;
        }

        if matches!(self.mode, PaletteMode::Prompt { .. }) {
            return Some(PaletteResult::Consumed);
        }

        let (px, py, pw, ph) = self.palette_rect(window_w, window_h);
        if x < px || x > px + pw || y < py || y > py + ph {
            return Some(PaletteResult::Consumed);
        }

        if self.filtered.is_empty() || delta == 0 {
            return Some(PaletteResult::Consumed);
        }

        let steps = (delta / 120).abs().max(1) as usize;
        if delta > 0 {
            self.selected = self.selected.saturating_sub(steps);
        } else {
            self.selected = (self.selected + steps).min(self.filtered.len() - 1);
        }
        self.ensure_visible();
        Some(PaletteResult::Consumed)
    }

    /// Paint the palette overlay. Call within an active BeginDraw/EndDraw.
    pub fn paint(&self, rt: &ID2D1RenderTarget, window_w: f32, window_h: f32) -> Result<()> {
        if !self.visible {
            return Ok(());
        }

        unsafe {
            // Semi-transparent overlay covering the entire window.
            let overlay_brush = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: OVERLAY_ALPHA,
                },
                None,
            )?;
            rt.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: window_w,
                    bottom: window_h,
                },
                &overlay_brush,
            );

            let (px, py, pw, ph) = self.palette_rect(window_w, window_h);

            // Palette background.
            let bg_brush = make_brush(rt, PALETTE_BG)?;
            let border_brush = make_brush(rt, BORDER_COLOR)?;
            let palette_rounded = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: px,
                    top: py,
                    right: px + pw,
                    bottom: py + ph,
                },
                radiusX: BORDER_RADIUS,
                radiusY: BORDER_RADIUS,
            };
            rt.FillRoundedRectangle(&palette_rounded, &bg_brush);
            rt.DrawRoundedRectangle(&palette_rounded, &border_brush, 1.0, None);

            // Input field.
            self.paint_input(rt, px, py, pw)?;

            // Filtered action list.
            self.paint_items(rt, px, py, pw)?;
        }

        Ok(())
    }

    /// Number of entries matching the current query.
    pub fn filtered_count(&self) -> usize {
        self.filtered.len()
    }

    /// Currently selected index (into the filtered list).
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Current query string.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Total number of palette entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    // ── Private ──────────────────────────────────────────────────────────────

    fn palette_rect(&self, window_w: f32, _window_h: f32) -> (f32, f32, f32, f32) {
        let w = PALETTE_WIDTH.min(window_w - 40.0);
        let h = match self.mode {
            PaletteMode::Search => {
                let visible_items = self.filtered.len().min(MAX_VISIBLE_ITEMS);
                INPUT_HEIGHT + PADDING + (visible_items as f32 * ITEM_HEIGHT) + PADDING
            }
            PaletteMode::Selector { .. } => {
                let visible_items = self.filtered.len().min(MAX_VISIBLE_ITEMS).max(1);
                INPUT_HEIGHT + PADDING + (visible_items as f32 * ITEM_HEIGHT) + PADDING
            }
            PaletteMode::Prompt { .. } => INPUT_HEIGHT + PADDING + ITEM_HEIGHT + PADDING,
        };
        let x = (window_w - w) / 2.0;
        (x, TOP_OFFSET, w, h)
    }

    fn refilter(&mut self) {
        let entries = self.mode_entries();
        if self.query.is_empty() {
            self.filtered = (0..entries.len())
                .map(|i| FilteredEntry { index: i, score: 0 })
                .collect();
        } else {
            let mut results: Vec<FilteredEntry> = Vec::new();
            for (i, entry) in entries.iter().enumerate() {
                let target = format!("{} {}", entry.name, entry.description);
                if let Some(score) = fuzzy_score(&self.query, &target) {
                    results.push(FilteredEntry { index: i, score });
                }
            }
            results.sort_by(|a, b| b.score.cmp(&a.score));
            self.filtered = results;
        }
        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_VISIBLE_ITEMS {
            self.scroll_offset = self.selected + 1 - MAX_VISIBLE_ITEMS;
        }
    }

    unsafe fn paint_input(&self, rt: &ID2D1RenderTarget, px: f32, py: f32, pw: f32) -> Result<()> {
        let input_rect = D2D_RECT_F {
            left: px + PADDING,
            top: py + PADDING,
            right: px + pw - PADDING,
            bottom: py + INPUT_HEIGHT,
        };

        // Input background.
        let input_bg = make_brush(rt, INPUT_BG)?;
        let input_rounded = D2D1_ROUNDED_RECT {
            rect: input_rect,
            radiusX: 4.0,
            radiusY: 4.0,
        };
        rt.FillRoundedRectangle(&input_rounded, &input_bg);

        let text_rect = D2D_RECT_F {
            left: input_rect.left + 8.0,
            top: input_rect.top + 2.0,
            right: input_rect.right - 8.0,
            bottom: input_rect.bottom - 2.0,
        };

        let placeholder = match &self.mode {
            PaletteMode::Search => "Type to search actions…",
            PaletteMode::Selector { placeholder, .. } => placeholder.as_str(),
            PaletteMode::Prompt { placeholder, .. } => placeholder.as_str(),
        };

        if self.query.is_empty() {
            let brush = make_brush(rt, INPUT_PLACEHOLDER_COLOR)?;
            draw_text(rt, placeholder, &self.tf_input, &text_rect, &brush);
        } else {
            let brush = make_brush(rt, INPUT_TEXT_COLOR)?;
            draw_text(rt, &self.query, &self.tf_input, &text_rect, &brush);

            // Text cursor.
            let max_w = text_rect.right - text_rect.left;
            let (tw, _) = measure_text(&self.dw_factory, &self.query, &self.tf_input, max_w);
            let cursor_x = text_rect.left + tw;
            let cursor_brush = make_brush(rt, CURSOR_COLOR)?;
            rt.FillRectangle(
                &D2D_RECT_F {
                    left: cursor_x,
                    top: text_rect.top + 4.0,
                    right: cursor_x + 2.0,
                    bottom: text_rect.bottom - 4.0,
                },
                &cursor_brush,
            );
        }

        Ok(())
    }

    unsafe fn paint_items(&self, rt: &ID2D1RenderTarget, px: f32, py: f32, pw: f32) -> Result<()> {
        if let PaletteMode::Prompt { label, action, .. } = &self.mode {
            let prompt_rect = D2D_RECT_F {
                left: px + PADDING,
                top: py + INPUT_HEIGHT + PADDING,
                right: px + pw - PADDING,
                bottom: py + INPUT_HEIGHT + PADDING + ITEM_HEIGHT,
            };
            let name_brush = make_brush(rt, ITEM_NAME_COLOR)?;
            let desc_brush = make_brush(rt, ITEM_DESC_COLOR)?;
            let rounded = D2D1_ROUNDED_RECT {
                rect: prompt_rect,
                radiusX: 3.0,
                radiusY: 3.0,
            };
            let bg_brush = make_brush(rt, SELECTED_BG)?;
            rt.FillRoundedRectangle(&rounded, &bg_brush);

            let label_rect = D2D_RECT_F {
                left: prompt_rect.left + 8.0,
                top: prompt_rect.top + 4.0,
                right: prompt_rect.right - 8.0,
                bottom: prompt_rect.top + 18.0,
            };
            draw_text(rt, label, &self.tf_name, &label_rect, &name_brush);
            let hint_rect = D2D_RECT_F {
                left: prompt_rect.left + 8.0,
                top: prompt_rect.top + 18.0,
                right: prompt_rect.right - 8.0,
                bottom: prompt_rect.bottom - 3.0,
            };
            let instruction = format!("Press Enter to run {}", action);
            draw_text(rt, &instruction, &self.tf_desc, &hint_rect, &desc_brush);
            return Ok(());
        }

        let items_y = py + INPUT_HEIGHT + PADDING;
        let visible_count = self.filtered.len().min(MAX_VISIBLE_ITEMS);

        let name_brush = make_brush(rt, ITEM_NAME_COLOR)?;
        let desc_brush = make_brush(rt, ITEM_DESC_COLOR)?;
        let hint_brush = make_brush(rt, ITEM_HINT_COLOR)?;
        let sel_brush = make_brush(rt, SELECTED_BG)?;

        for vi in 0..visible_count {
            let fi = self.scroll_offset + vi;
            if fi >= self.filtered.len() {
                break;
            }

            let fe = &self.filtered[fi];
            let entry = &self.mode_entries()[fe.index];
            let item_y = items_y + (vi as f32 * ITEM_HEIGHT);

            let item_rect = D2D_RECT_F {
                left: px + PADDING,
                top: item_y,
                right: px + pw - PADDING,
                bottom: item_y + ITEM_HEIGHT,
            };

            // Selection highlight.
            if fi == self.selected {
                let rounded = D2D1_ROUNDED_RECT {
                    rect: item_rect,
                    radiusX: 3.0,
                    radiusY: 3.0,
                };
                rt.FillRoundedRectangle(&rounded, &sel_brush);
            }

            // Action name (top line, left).
            let name_rect = D2D_RECT_F {
                left: item_rect.left + 8.0,
                top: item_rect.top + 3.0,
                right: item_rect.right - 130.0,
                bottom: item_rect.top + 18.0,
            };
            draw_text(rt, &entry.name, &self.tf_name, &name_rect, &name_brush);

            // Description (bottom line, left).
            let desc_rect = D2D_RECT_F {
                left: item_rect.left + 8.0,
                top: item_rect.top + 17.0,
                right: item_rect.right - 130.0,
                bottom: item_rect.bottom - 2.0,
            };
            draw_text(
                rt,
                &entry.description,
                &self.tf_desc,
                &desc_rect,
                &desc_brush,
            );

            // Keybinding hint (right-aligned, vertically centered).
            if let Some(ref hint) = entry.keybinding {
                let hint_max_w = 120.0;
                let (hw, _) = measure_text(&self.dw_factory, hint, &self.tf_hint, hint_max_w);
                let hint_rect = D2D_RECT_F {
                    left: item_rect.right - 8.0 - hw,
                    top: item_rect.top + 10.0,
                    right: item_rect.right - 8.0,
                    bottom: item_rect.bottom - 8.0,
                };
                draw_text(rt, hint, &self.tf_hint, &hint_rect, &hint_brush);
            }
        }

        Ok(())
    }

    fn mode_entries(&self) -> &[PaletteEntry] {
        match &self.mode {
            PaletteMode::Search => &self.entries,
            PaletteMode::Prompt { .. } => &[],
            PaletteMode::Selector { entries, .. } => entries.as_slice(),
        }
    }
}

// ── Fuzzy matching ───────────────────────────────────────────────────────────

/// Score a query against a target string using fuzzy subsequence matching.
/// Returns `Some(score)` if all query characters appear in order in the target,
/// `None` otherwise. Higher scores indicate better matches.
pub fn fuzzy_score(query: &str, target: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let query_chars: Vec<char> = query.to_lowercase().chars().collect();
    let target_chars: Vec<char> = target.to_lowercase().chars().collect();

    let mut qi = 0;
    let mut score: i32 = 0;
    let mut consecutive: i32 = 0;

    for (ti, &tc) in target_chars.iter().enumerate() {
        if qi < query_chars.len() && tc == query_chars[qi] {
            qi += 1;
            consecutive += 1;
            score += consecutive; // bonus for consecutive matches

            // Bonus for word-boundary match (start of word).
            if ti == 0 || matches!(target_chars[ti - 1], '-' | ' ' | '_') {
                score += 3;
            }
        } else {
            consecutive = 0;
        }
    }

    if qi == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

// ── Action catalog ───────────────────────────────────────────────────────────

/// Build the full palette entry list from the v1 action catalog (§20.3),
/// annotated with keybinding hints derived from the provided bindings.
pub fn build_palette_entries(bindings: &BindingsDefinition) -> Vec<PaletteEntry> {
    let hints = build_keybinding_hints(bindings);
    RUNNABLE_ACTIONS
        .iter()
        .map(|(name, desc)| PaletteEntry {
            name: name.to_string(),
            description: desc.to_string(),
            keybinding: hints.get(*name).cloned(),
        })
        .collect()
}

/// Build a reverse map from action name to keybinding display string.
pub fn build_keybinding_hints(bindings: &BindingsDefinition) -> HashMap<String, String> {
    let bindings = wtd_core::effective_bindings(bindings);
    let mut hints: HashMap<String, String> = HashMap::new();

    // Single-stroke keys are preferred for display (more direct).
    if let Some(keys) = &bindings.keys {
        for (key_spec, action_ref) in keys {
            let action_name = action_name_from_ref(action_ref);
            hints.entry(action_name).or_insert_with(|| key_spec.clone());
        }
    }

    // Chord bindings displayed as "Prefix, key".
    let prefix = bindings.prefix.as_deref().unwrap_or("Ctrl+B");
    if let Some(chords) = &bindings.chords {
        for (chord_key, action_ref) in chords {
            let action_name = action_name_from_ref(action_ref);
            let hint = format!("{}, {}", prefix, chord_key);
            hints.entry(action_name).or_insert(hint);
        }
    }

    hints
}

fn action_name_from_ref(ar: &ActionReference) -> String {
    match ar {
        ActionReference::Simple(name) => name.clone(),
        ActionReference::WithArgs { action, .. } => action.clone(),
        ActionReference::Removed => String::new(),
    }
}

// ── Drawing helpers ──────────────────────────────────────────────────────────

fn make_brush(rt: &ID2D1RenderTarget, color: (u8, u8, u8)) -> Result<ID2D1SolidColorBrush> {
    let c = D2D1_COLOR_F {
        r: color.0 as f32 / 255.0,
        g: color.1 as f32 / 255.0,
        b: color.2 as f32 / 255.0,
        a: 1.0,
    };
    unsafe { rt.CreateSolidColorBrush(&c, None) }
}

fn draw_text(
    rt: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: &D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        rt.DrawText(
            &wide,
            format,
            rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

fn measure_text(
    factory: &IDWriteFactory,
    text: &str,
    format: &IDWriteTextFormat,
    max_width: f32,
) -> (f32, f32) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        match factory.CreateTextLayout(&wide[..wide.len() - 1], format, max_width, 1000.0) {
            Ok(layout) => {
                let mut metrics = DWRITE_TEXT_METRICS::default();
                let _ = layout.GetMetrics(&mut metrics);
                (metrics.width, metrics.height)
            }
            Err(_) => (0.0, 0.0),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_core::workspace::BindingsDefinition;

    // ── Fuzzy scoring ──

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn exact_match_scores_high() {
        let score = fuzzy_score("split-right", "split-right Split pane on right").unwrap();
        assert!(score > 0);
    }

    #[test]
    fn subsequence_match() {
        assert!(fuzzy_score("sr", "split-right").is_some());
        assert!(fuzzy_score("sprt", "split-right").is_some());
    }

    #[test]
    fn no_match_returns_none() {
        assert!(fuzzy_score("xyz", "split-right").is_none());
        assert!(fuzzy_score("zz", "split-right").is_none());
    }

    #[test]
    fn case_insensitive() {
        assert!(fuzzy_score("SPLIT", "split-right").is_some());
        assert!(fuzzy_score("Split", "split-right").is_some());
    }

    #[test]
    fn word_boundary_bonus() {
        // "sr" matching at word boundaries should score higher than mid-word
        let s1 = fuzzy_score("sr", "split-right").unwrap(); // s at start, r at boundary
        let s2 = fuzzy_score("sr", "posridge").unwrap(); // s and r mid-word
        assert!(s1 > s2);
    }

    #[test]
    fn consecutive_bonus() {
        // "split" has all consecutive matches — should score higher
        let s1 = fuzzy_score("split", "split-right").unwrap();
        let s2 = fuzzy_score("spirt", "split-right"); // non-consecutive
                                                      // spirt: s(0) p(1) i(3) r(6) t(10) — should match but lower
        match s2 {
            Some(s2) => assert!(s1 > s2),
            None => {} // also acceptable if 'i' before 'r' doesn't match order
        }
    }

    // ── Keybinding hints ──

    #[test]
    fn single_stroke_hint() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: Some(
                [(
                    "Ctrl+Shift+T".to_string(),
                    ActionReference::Simple("new-tab".to_string()),
                )]
                .into_iter()
                .collect(),
            ),
        };
        let hints = build_keybinding_hints(&bindings);
        assert_eq!(hints.get("new-tab"), Some(&"Ctrl+Shift+T".to_string()));
    }

    #[test]
    fn chord_hint_includes_prefix() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: Some(
                [(
                    "%".to_string(),
                    ActionReference::Simple("split-right".to_string()),
                )]
                .into_iter()
                .collect(),
            ),
            keys: None,
        };
        let hints = build_keybinding_hints(&bindings);
        assert_eq!(hints.get("split-right"), Some(&"Ctrl+B, %".to_string()));
    }

    #[test]
    fn single_stroke_preferred_over_chord() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: Some(
                [(
                    "c".to_string(),
                    ActionReference::Simple("new-tab".to_string()),
                )]
                .into_iter()
                .collect(),
            ),
            keys: Some(
                [(
                    "Ctrl+Shift+T".to_string(),
                    ActionReference::Simple("new-tab".to_string()),
                )]
                .into_iter()
                .collect(),
            ),
        };
        let hints = build_keybinding_hints(&bindings);
        // Single-stroke is inserted first, so it wins.
        assert_eq!(hints.get("new-tab"), Some(&"Ctrl+Shift+T".to_string()));
    }

    #[test]
    fn default_bindings_expand_to_palette_shortcut_hint() {
        let hints = build_keybinding_hints(&wtd_core::global_settings::default_bindings());
        assert_eq!(
            hints.get("toggle-command-palette"),
            Some(&"Ctrl+Shift+P".to_string())
        );
    }

    #[test]
    fn default_bindings_expand_to_pass_through_shortcut_hint() {
        let hints = build_keybinding_hints(&wtd_core::global_settings::default_bindings());
        assert_eq!(
            hints.get("pass-through-next-key"),
            Some(&"Alt+Shift+K".to_string())
        );
    }

    // ── Palette entries ──

    #[test]
    fn entry_count_matches_v1_catalog() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        assert_eq!(entries.len(), RUNNABLE_ACTIONS.len());
    }

    #[test]
    fn tab_and_pane_actions_included_in_palette() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        assert!(entries.iter().any(|e| e.name == "new-tab"));
        assert!(entries.iter().any(|e| e.name == "next-tab"));
        assert!(entries.iter().any(|e| e.name == "prev-tab"));
        assert!(entries.iter().any(|e| e.name == "split-right"));
    }

    #[test]
    fn entries_include_toggle_command_palette() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        assert!(entries.iter().any(|e| e.name == "toggle-command-palette"));
    }

    #[test]
    fn entries_include_change_profile() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        assert!(entries.iter().any(|e| e.name == "change-profile"));
    }

    #[test]
    fn entries_include_pass_through_next_key() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        assert!(entries.iter().any(|e| e.name == "pass-through-next-key"));
    }

    #[test]
    fn entries_include_rearrangement_and_retile_actions() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let entries = build_palette_entries(&bindings);
        for action in [
            "swap-pane-left",
            "swap-pane-right",
            "toggle-split-orientation",
            "equalize-tab",
            "retile-grid",
            "retile-main-left",
            "retile-main-top",
        ] {
            assert!(entries.iter().any(|e| e.name == action), "missing {action}");
        }
    }

    #[test]
    fn entries_have_keybinding_from_tmux_bindings() {
        let bindings = wtd_core::global_settings::tmux_bindings();
        let entries = build_palette_entries(&bindings);
        let split_right = entries.iter().find(|e| e.name == "split-right").unwrap();
        assert_eq!(split_right.keybinding, Some("Alt+Shift+D".to_string()));
    }

    #[test]
    fn entries_have_chord_keybinding() {
        let bindings = wtd_core::global_settings::tmux_bindings();
        let entries = build_palette_entries(&bindings);
        let zoom = entries.iter().find(|e| e.name == "zoom-pane").unwrap();
        assert_eq!(zoom.keybinding, Some("Ctrl+B, z".to_string()));
    }
}
