//! `wtd-ui` — WinTermDriver UI process.
//!
//! When a workspace name is provided (via `--workspace` or `WTD_WORKSPACE`
//! env var), connects to `wtd-host` via IPC. Otherwise runs in standalone
//! demo mode with hardcoded content.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenuEx, MF_ENABLED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
};
use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_core::logging::init_stderr_logging;
use wtd_core::workspace::PaneNode;
use wtd_core::LogLevel;
use wtd_ipc::message::{AttentionState, ProgressInfo};
use wtd_pty::MouseMode;
use wtd_pty::ScreenBuffer;
use wtd_ui::command_palette::{CommandPalette, PaletteResult};
use wtd_ui::host_bridge::{HostBridge, HostCommand, HostEvent};
use wtd_ui::input::{
    key_event_to_bytes, InputAction, InputClassifier, KeyEvent, KeyName, Modifiers,
};
use wtd_ui::mouse_handler::{MouseHandler, MouseOutput};
use wtd_ui::paint_scheduler::PaintScheduler;
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction, PixelRect};
use wtd_ui::prefix_state::{PrefixOutput, PrefixStateMachine};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer, TextSelection};
use wtd_ui::snapshot::{rebuild_from_snapshot, PaneSession, SnapshotRebuild, SnapshotTab};
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::{TabAction, TabStrip};
use wtd_ui::window::{self, InputEvent, MouseEventKind};

const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const FONT_SIZE_STEP: f32 = 1.0;

fn main() {
    // §31.1: UI logs to stderr.
    init_stderr_logging(&LogLevel::default());

    let workspace_name = parse_workspace_arg();

    if let Some(ref name) = workspace_name {
        tracing::info!(workspace = %name, "connecting to workspace");
    } else {
        tracing::info!("running in demo mode (no workspace specified)");
    }

    if let Err(e) = run(workspace_name) {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

/// Parse workspace name from `--workspace <name>` args or `WTD_WORKSPACE` env.
fn parse_workspace_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    parse_workspace_from_args(&args)
}

fn parse_workspace_from_args(args: &[String]) -> Option<String> {
    let mut positional: Option<String> = None;

    let mut i = 1usize;
    while i < args.len() {
        let arg = &args[i];
        if (arg == "--workspace" || arg == "-w") && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }

        if arg.starts_with("--workspace=") {
            return Some(arg.trim_start_matches("--workspace=").to_string());
        }
        if arg.starts_with("-w=") {
            return Some(arg.trim_start_matches("-w=").to_string());
        }

        if !arg.starts_with('-') && positional.is_none() {
            positional = Some(arg.clone());
        }

        i += 1;
    }

    positional.or_else(|| std::env::var("WTD_WORKSPACE").ok())
}

fn tab_index_for_host_name(
    tab_strip: &TabStrip,
    host_tab: &str,
    active_tab_index: usize,
) -> Option<usize> {
    if tab_strip.tab_count() == 0 {
        return None;
    }

    if let Ok(index) = host_tab.parse::<usize>() {
        if index < tab_strip.tab_count() {
            return Some(index);
        }
    }

    if let Some(index) = tab_strip.tabs().iter().position(|tab| tab.name == host_tab) {
        return Some(index);
    }

    if active_tab_index < tab_strip.tab_count() {
        Some(active_tab_index)
    } else {
        None
    }
}

fn apply_tab_layout(tab: &mut SnapshotTab, cols: u16, rows: u16, pane_node: &PaneNode) {
    let (layout_tree, _mappings) = LayoutTree::from_pane_node(pane_node);

    let old_panes: HashSet<PaneId> = tab.layout_tree.panes().into_iter().collect();
    let new_panes: HashSet<PaneId> = layout_tree.panes().into_iter().collect();

    for removed in old_panes.difference(&new_panes) {
        tab.screens.remove(removed);
        tab.pane_sessions.remove(removed);
    }

    for added in new_panes.difference(&old_panes) {
        tab.screens
            .entry(added.clone())
            .or_insert_with(|| ScreenBuffer::new(cols, rows, 1000));
    }

    tab.layout_tree = layout_tree;
}

fn refresh_active_tab_ui(
    tabs: &mut Vec<SnapshotTab>,
    active_tab_index: usize,
    pane_layout: &mut PaneLayout,
    tab_strip: &TabStrip,
    status_bar: &mut StatusBar,
    mouse_modes: &mut HashMap<PaneId, MouseMode>,
    sgr_mouse_modes: &mut HashMap<PaneId, bool>,
    window_width: f32,
    window_height: f32,
    cell_w: f32,
    cell_h: f32,
    pane_viewport_insets: PaneViewportInsets,
) {
    if tabs.is_empty() || active_tab_index >= tabs.len() {
        return;
    }

    let (content_cols, content_rows) = content_dims(
        window_width,
        window_height,
        tab_strip,
        status_bar,
        cell_w,
        cell_h,
    );

    let (pane_sizes, focused, pane_path) =
        if let Some(active_tab) = active_tab_ref(tabs, active_tab_index) {
            pane_layout.update(
                &active_tab.layout_tree,
                0.0,
                tab_strip.height(),
                content_cols,
                content_rows,
            );
            let pane_sizes = pane_sizes_for_layout(
                pane_layout,
                &active_tab.layout_tree,
                cell_w,
                cell_h,
                pane_viewport_insets,
            );
            let focused = active_tab.layout_tree.focus();
            let pane_path = active_tab
                .pane_sessions
                .get(&focused)
                .map(|ps| ps.pane_path.clone());
            (pane_sizes, Some(focused), pane_path)
        } else {
            return;
        };

    if let Some(active_tab) = active_tab_mut(tabs, active_tab_index) {
        sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
    }
    if let Some(path) = pane_path {
        status_bar.set_pane_path(path);
    } else if let Some(focused) = focused {
        status_bar.set_pane_path(format!("{}", focused.0));
    }
    if let Some(active_tab) = active_tab_ref(tabs, active_tab_index) {
        let (attention_state, attention_message) = focused_pane_attention(active_tab);
        status_bar.set_attention(attention_state, attention_message);
        refresh_mouse_modes(mouse_modes, sgr_mouse_modes, &active_tab.screens);
    }
}

fn pane_cell_size_for_rect(rect: &PixelRect, cell_w: f32, cell_h: f32) -> (u16, u16) {
    (
        clamp_cells(rect.width / cell_w),
        clamp_cells(rect.height / cell_h),
    )
}

fn adjusted_font_size(current: f32, wheel_delta: i16) -> f32 {
    let notches = wheel_delta as i32 / 120;
    if notches == 0 {
        return current;
    }
    (current + notches as f32 * FONT_SIZE_STEP).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

fn should_coalesce_primary_screen_output(data: &[u8]) -> bool {
    let esc_count = data.iter().filter(|&&byte| byte == 0x1B).count();
    let has_clear = data.windows(4).any(|window| window == b"\x1B[2J");
    let has_home = data.windows(3).any(|window| window == b"\x1B[H");

    has_clear || (has_home && esc_count >= 4) || esc_count >= 8
}

fn prepare_pane_for_live_input(
    mouse_handler: &mut MouseHandler,
    tab: &SnapshotTab,
    pane_id: &PaneId,
    clear_selection: bool,
) -> bool {
    let Some(screen) = tab.screens.get(pane_id) else {
        return false;
    };

    if screen.on_alternate() {
        return false;
    }

    let had_scrollback = mouse_handler.scroll_offset(pane_id) != 0;
    let had_selection = clear_selection && mouse_handler.selection(pane_id).is_some();

    if had_scrollback {
        mouse_handler.reset_scroll(pane_id);
    }
    if had_selection {
        mouse_handler.clear_selection(pane_id);
    }

    had_scrollback || had_selection
}

fn rebuild_renderer_resources(
    hwnd: HWND,
    config: &RendererConfig,
    bindings: &wtd_core::workspace::BindingsDefinition,
    renderer: &mut TerminalRenderer,
    tab_strip: &mut TabStrip,
    status_bar: &mut StatusBar,
    command_palette: &mut CommandPalette,
    window_width: f32,
) -> anyhow::Result<(f32, f32)> {
    *renderer = TerminalRenderer::new(hwnd, config)?;
    let (cell_w, cell_h) = renderer.cell_size();

    let tabs = tab_strip.tabs().to_vec();
    let active_index = tab_strip.active_index();
    let window_maximized = window::is_maximized(hwnd);
    let mut rebuilt_tab_strip = TabStrip::new(renderer.dw_factory())?;
    for (index, tab) in tabs.iter().enumerate() {
        rebuilt_tab_strip.add_tab(tab.name.clone());
        rebuilt_tab_strip.set_progress(index, tab.progress.clone());
    }
    if !tabs.is_empty() {
        rebuilt_tab_strip.set_active(active_index.min(tabs.len().saturating_sub(1)));
    }
    rebuilt_tab_strip.set_window_maximized(window_maximized);
    rebuilt_tab_strip.layout(window_width);
    *tab_strip = rebuilt_tab_strip;

    let pane_path = status_bar.pane_path().to_string();
    let session_status = status_bar.session_status().clone();
    let attention_state = status_bar.attention_state();
    let mut rebuilt_status_bar = StatusBar::new(renderer.dw_factory())?;
    rebuilt_status_bar.set_pane_path(pane_path);
    rebuilt_status_bar.set_session_status(session_status);
    rebuilt_status_bar.set_attention(attention_state, None);
    rebuilt_status_bar.layout(window_width);
    *status_bar = rebuilt_status_bar;

    let palette_was_visible = command_palette.is_visible();
    *command_palette = CommandPalette::new(
        renderer.dw_factory(),
        bindings,
        command_palette.profile_entries().to_vec(),
    )?;
    if palette_was_visible {
        command_palette.show();
    }

    Ok((cell_w, cell_h))
}

fn pane_cell_size_for_viewport(
    rect: &PixelRect,
    cell_w: f32,
    cell_h: f32,
    insets: PaneViewportInsets,
) -> (u16, u16) {
    let content_rect = pane_content_rect(*rect, cell_w, cell_h, insets);
    pane_cell_size_for_rect(&content_rect, cell_w, cell_h)
}

fn pane_host_target<'a>(tab: &'a SnapshotTab, pane_id: &PaneId) -> Option<&'a str> {
    tab.pane_sessions
        .get(pane_id)
        .map(|pane_session| pane_session.pane_path.as_str())
}

fn pane_short_name(pane_path: &str) -> &str {
    pane_path.rsplit('/').next().unwrap_or(pane_path)
}

fn focused_pane_title(tab: &SnapshotTab) -> Option<&str> {
    let focused = tab.layout_tree.focus();
    tab.pane_sessions
        .get(&focused)
        .and_then(|pane_session| pane_session.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
}

fn focused_pane_attention(tab: &SnapshotTab) -> (AttentionState, Option<String>) {
    let focused = tab.layout_tree.focus();
    tab.pane_sessions
        .get(&focused)
        .map(|pane_session| {
            (
                pane_session.attention,
                pane_session.attention_message.clone(),
            )
        })
        .unwrap_or((AttentionState::Active, None))
}

fn pane_is_unread_attention(pane_session: &PaneSession) -> bool {
    matches!(
        pane_session.attention,
        AttentionState::NeedsAttention | AttentionState::Error
    )
}

fn focused_pane_matches_host_id(tab: &SnapshotTab, host_pane_id: &str) -> bool {
    let focused = tab.layout_tree.focus();
    tab.pane_sessions
        .get(&focused)
        .and_then(|pane_session| pane_session.host_pane_id.as_deref())
        == Some(host_pane_id)
}

fn focus_aware_attention_state(state: AttentionState, target_is_focused: bool) -> AttentionState {
    if target_is_focused && state == AttentionState::NeedsAttention {
        AttentionState::Active
    } else {
        state
    }
}

fn attention_count(tabs: &[SnapshotTab]) -> usize {
    tabs.iter()
        .flat_map(|tab| tab.pane_sessions.values())
        .filter(|pane_session| pane_is_unread_attention(pane_session))
        .count()
}

fn notification_center_label(tabs: &[SnapshotTab]) -> String {
    let mut items = Vec::new();
    for tab in tabs {
        for pane_id in tab.layout_tree.panes() {
            let Some(pane_session) = tab.pane_sessions.get(&pane_id) else {
                continue;
            };
            if !pane_is_unread_attention(pane_session) {
                continue;
            }
            let label = match pane_session.attention_message.as_deref() {
                Some(message) if !message.trim().is_empty() => {
                    format!("{}: {}", pane_session.pane_path, message.trim())
                }
                _ => pane_session.pane_path.clone(),
            };
            items.push(label);
        }
    }

    if items.is_empty() {
        "Notifications: none".to_string()
    } else {
        format!("Notifications: {}", items.join(" | "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneListFilter {
    All,
    Attention,
    Status,
    DriverProfile,
    Cwd,
    Branch,
}

fn pane_metadata_summary(tabs: &[SnapshotTab], filter: PaneListFilter) -> String {
    let mut items = Vec::new();
    for tab in tabs {
        for pane_id in tab.layout_tree.panes() {
            let Some(pane_session) = tab.pane_sessions.get(&pane_id) else {
                continue;
            };
            let has_status = pane_session.phase.is_some()
                || pane_session.status_text.is_some()
                || pane_session.queue_pending.is_some()
                || pane_session.health_state.is_some();
            let include = match filter {
                PaneListFilter::All => true,
                PaneListFilter::Attention => pane_is_unread_attention(pane_session),
                PaneListFilter::Status => has_status,
                PaneListFilter::DriverProfile => pane_session.driver_profile.is_some(),
                PaneListFilter::Cwd => pane_session.cwd.is_some(),
                PaneListFilter::Branch => pane_session.branch.is_some(),
            };
            if !include {
                continue;
            }
            let sort_key = match filter {
                PaneListFilter::All => pane_session.pane_path.clone(),
                PaneListFilter::Attention => {
                    format!("{:?}:{}", pane_session.attention, pane_session.pane_path)
                }
                PaneListFilter::Status => format!(
                    "{}:{}:{}",
                    pane_session.phase.as_deref().unwrap_or_default(),
                    pane_session.status_text.as_deref().unwrap_or_default(),
                    pane_session.pane_path
                ),
                PaneListFilter::DriverProfile => format!(
                    "{}:{}",
                    pane_session.driver_profile.as_deref().unwrap_or_default(),
                    pane_session.pane_path
                ),
                PaneListFilter::Cwd => format!(
                    "{}:{}",
                    pane_session.cwd.as_deref().unwrap_or_default(),
                    pane_session.pane_path
                ),
                PaneListFilter::Branch => format!(
                    "{}:{}",
                    pane_session.branch.as_deref().unwrap_or_default(),
                    pane_session.pane_path
                ),
            };
            let mut parts = vec![pane_session.pane_path.clone()];
            if pane_is_unread_attention(pane_session) {
                parts.push(format!("{:?}", pane_session.attention).to_lowercase());
            }
            if let Some(phase) = pane_session.phase.as_deref() {
                parts.push(format!("phase={phase}"));
            }
            if let Some(status) = pane_session.status_text.as_deref() {
                parts.push(status.to_string());
            }
            if let Some(queue) = pane_session.queue_pending {
                parts.push(format!("queue={queue}"));
            }
            if let Some(health) = pane_session.health_state.as_deref() {
                parts.push(format!("health={health}"));
            }
            if let Some(source) = pane_session.source.as_deref() {
                parts.push(format!("source={source}"));
            }
            if let Some(driver_profile) = pane_session.driver_profile.as_deref() {
                parts.push(format!("driver={driver_profile}"));
            }
            if let Some(cwd) = pane_session.cwd.as_deref() {
                parts.push(format!("cwd={cwd}"));
            }
            if let Some(branch) = pane_session.branch.as_deref() {
                parts.push(format!("branch={branch}"));
            }
            items.push((sort_key, parts.join(" ")));
        }
    }
    if items.is_empty() {
        "Panes: none".to_string()
    } else {
        items.sort_by(|left, right| left.0.cmp(&right.0));
        format!(
            "Panes: {}",
            items
                .into_iter()
                .map(|(_, item)| item)
                .collect::<Vec<_>>()
                .join(" | ")
        )
    }
}

fn next_attention_target(
    tabs: &[SnapshotTab],
    active_tab_index: usize,
    forward: bool,
) -> Option<(usize, PaneId)> {
    let current_order = tabs
        .get(active_tab_index)
        .map(|tab| tab.layout_tree.focus())
        .and_then(|focused| {
            let mut order = 0usize;
            for (tab_index, tab) in tabs.iter().enumerate() {
                for pane_id in tab.layout_tree.panes() {
                    if tab_index == active_tab_index && pane_id == focused {
                        return Some(order);
                    }
                    order += 1;
                }
            }
            None
        })
        .unwrap_or(0);

    let mut attention = Vec::new();
    let mut order = 0usize;
    for (tab_index, tab) in tabs.iter().enumerate() {
        for pane_id in tab.layout_tree.panes() {
            if tab
                .pane_sessions
                .get(&pane_id)
                .is_some_and(pane_is_unread_attention)
            {
                attention.push((order, tab_index, pane_id));
            }
            order += 1;
        }
    }

    if attention.is_empty() {
        return None;
    }

    let selected = if forward {
        attention
            .iter()
            .find(|(order, _, _)| *order > current_order)
            .or_else(|| attention.first())?
    } else {
        attention
            .iter()
            .rev()
            .find(|(order, _, _)| *order < current_order)
            .or_else(|| attention.last())?
    };

    Some((selected.1, selected.2.clone()))
}

fn focus_attention_target(
    tabs: &mut [SnapshotTab],
    tab_strip: &mut TabStrip,
    active_tab_index: &mut usize,
    target: (usize, PaneId),
) {
    let (tab_index, pane_id) = target;
    if let Some(tab) = tabs.get_mut(tab_index) {
        let _ = tab.layout_tree.set_focus(pane_id);
        *active_tab_index = tab_index;
        tab_strip.set_active(tab_index);
    }
}

fn clear_focused_attention(
    tabs: &mut [SnapshotTab],
    active_tab_index: usize,
    bridge: Option<&HostBridge>,
    status_bar: &mut StatusBar,
) -> bool {
    let Some(tab) = tabs.get_mut(active_tab_index) else {
        return false;
    };
    let focused = tab.layout_tree.focus();
    let Some(pane_session) = tab.pane_sessions.get_mut(&focused) else {
        return false;
    };
    pane_session.attention = AttentionState::Active;
    pane_session.attention_message = None;
    status_bar.set_attention(AttentionState::Active, None);
    if let Some(bridge) = bridge {
        bridge.clear_attention(pane_session.pane_path.clone());
    }
    true
}

fn compose_window_title(
    workspace_name: &str,
    tab_strip: &TabStrip,
    active_tab: Option<&SnapshotTab>,
) -> String {
    let mut title = tab_strip.window_title(workspace_name);
    if let Some(pane_title) = active_tab.and_then(focused_pane_title) {
        title.push_str(" — ");
        title.push_str(pane_title);
    }
    title
}

fn refresh_window_title(
    hwnd: windows::Win32::Foundation::HWND,
    workspace_name: &str,
    tab_strip: &TabStrip,
    active_tab: Option<&SnapshotTab>,
) {
    let win_title = compose_window_title(workspace_name, tab_strip, active_tab);
    window::set_window_title(hwnd, &win_title);
}

fn pane_overlay_label(pane_session: &PaneSession) -> String {
    pane_short_name(&pane_session.pane_path).to_string()
}

fn send_active_pane_sizes(
    bridge: Option<&HostBridge>,
    connected: bool,
    pane_layout: &PaneLayout,
    tab: &SnapshotTab,
    cell_w: f32,
    cell_h: f32,
    pane_viewport_insets: PaneViewportInsets,
) {
    if !connected {
        return;
    }
    let Some(bridge) = bridge else {
        return;
    };

    for pane_id in tab.layout_tree.panes() {
        if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
            let Some(target) = pane_host_target(tab, &pane_id) else {
                continue;
            };
            let (cols, rows) =
                pane_cell_size_for_viewport(&rect, cell_w, cell_h, pane_viewport_insets);
            bridge.send_resize(target.to_string(), cols, rows);
        }
    }
}

fn pane_sizes_for_layout(
    pane_layout: &PaneLayout,
    layout_tree: &LayoutTree,
    cell_w: f32,
    cell_h: f32,
    pane_viewport_insets: PaneViewportInsets,
) -> Vec<(PaneId, u16, u16)> {
    let mut sizes = Vec::new();
    for pane_id in layout_tree.panes() {
        let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) else {
            continue;
        };

        let (cols, rows) = pane_cell_size_for_viewport(&rect, cell_w, cell_h, pane_viewport_insets);
        sizes.push((pane_id, cols, rows));
    }
    sizes
}

fn sync_screen_buffers_to_sizes(tab: &mut SnapshotTab, sizes: &[(PaneId, u16, u16)]) {
    for (pane_id, cols, rows) in sizes {
        match tab.screens.get_mut(&pane_id) {
            Some(screen) => {
                if screen.cols() as u16 != *cols || screen.rows() as u16 != *rows {
                    screen.resize(*cols, *rows);
                }
            }
            None => {
                tab.screens
                    .insert(pane_id.clone(), ScreenBuffer::new(*cols, *rows, 1000));
            }
        }
    }
}

fn pane_sessions_match_sizes(tab: &SnapshotTab, sizes: &[(PaneId, u16, u16)]) -> bool {
    for (pane_id, cols, rows) in sizes {
        let Some(pane_session) = tab.pane_sessions.get(pane_id) else {
            continue;
        };
        let Some((session_cols, session_rows)) = pane_session.session_size else {
            return false;
        };
        if session_cols != *cols || session_rows != *rows {
            return false;
        }
    }
    true
}

fn tab_progress(tab: &SnapshotTab) -> Option<ProgressInfo> {
    let focused = tab.layout_tree.focus();
    tab.pane_sessions
        .get(&focused)
        .and_then(|pane_session| pane_session.progress.clone())
}

fn sync_tab_progresses(tab_strip: &mut TabStrip, tabs: &[SnapshotTab]) {
    for (index, tab) in tabs.iter().enumerate() {
        tab_strip.set_progress(index, tab_progress(tab));
    }
}

fn action_name(action: &wtd_core::workspace::ActionReference) -> &str {
    match action {
        wtd_core::workspace::ActionReference::Simple(name) => name.as_str(),
        wtd_core::workspace::ActionReference::WithArgs { action, .. } => action.as_str(),
        wtd_core::workspace::ActionReference::Removed => "",
    }
}

fn workspace_state_closes_ui(new_state: &str) -> bool {
    matches!(
        new_state.to_ascii_lowercase().as_str(),
        "closing" | "closed" | "stopping" | "stopped" | "exited" | "terminating" | "terminated"
    )
}

fn bound_action_name(classifier: &InputClassifier, event: &KeyEvent) -> Option<String> {
    match classifier.classify(event, false) {
        InputAction::SingleStrokeBinding(action) => {
            let name = action_name(&action);
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        }
        _ => None,
    }
}

fn text_input_bytes(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

const PASS_THROUGH_NEXT_KEY_LABEL: &str = "SEND KEY";

#[derive(Debug, Default)]
struct PassThroughNextKeyState {
    armed: bool,
}

impl PassThroughNextKeyState {
    fn arm(&mut self) {
        self.armed = true;
    }

    fn is_armed(&self) -> bool {
        self.armed
    }

    fn badge_label(&self) -> &'static str {
        PASS_THROUGH_NEXT_KEY_LABEL
    }

    fn process_key(&mut self, event: &KeyEvent) -> Option<Vec<u8>> {
        if !self.armed {
            return None;
        }

        let bytes = key_event_to_bytes(event);
        if !bytes.is_empty() {
            self.armed = false;
        }
        Some(bytes)
    }

    fn process_text(&mut self, text: &str) -> Option<Vec<u8>> {
        if !self.armed || text.is_empty() {
            return None;
        }

        self.armed = false;
        Some(text_input_bytes(text))
    }
}

const TAB_MENU_NEW_TAB: u32 = 1;
const TAB_MENU_RENAME_TAB: u32 = 2;
const TAB_MENU_CLOSE_TAB: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabContextMenuAction {
    NewTab,
    RenameTab,
    CloseTab,
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn action_args_to_value(args: &Option<HashMap<String, String>>) -> serde_json::Value {
    args.as_ref()
        .map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null)
}

fn paste_bytes_for_pane(active_tab: &SnapshotTab, pane_id: &PaneId, text: &str) -> Vec<u8> {
    let bracketed_paste_active = active_tab
        .screens
        .get(pane_id)
        .map(|screen| screen.bracketed_paste())
        .unwrap_or(false);
    wtd_ui::clipboard::prepare_paste(text, bracketed_paste_active)
}

fn send_ui_action(
    bridge: &HostBridge,
    tab: &SnapshotTab,
    action_name: &str,
    args: serde_json::Value,
) {
    let focused = tab.layout_tree.focus();
    let target = pane_host_target(tab, &focused)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}", focused.0));
    bridge.send_action(action_name.to_string(), Some(target), args);
    bridge.refresh_workspace();
}

fn send_workspace_action(bridge: &HostBridge, action: &str, args: serde_json::Value) {
    bridge.send_action(action.to_string(), None, args);
    bridge.refresh_workspace();
}

fn wtd_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WTD_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        format!(r"{}\AppData\Roaming", home)
    });
    PathBuf::from(appdata).join("WinTermDriver")
}

fn settings_path() -> PathBuf {
    wtd_data_dir().join("settings.yaml")
}

fn profile_kind(profile_type: &wtd_core::workspace::ProfileType) -> &'static str {
    match profile_type {
        wtd_core::workspace::ProfileType::Powershell => "PowerShell",
        wtd_core::workspace::ProfileType::Cmd => "cmd",
        wtd_core::workspace::ProfileType::Wsl => "WSL",
        wtd_core::workspace::ProfileType::Ssh => "SSH",
        wtd_core::workspace::ProfileType::Custom => "custom",
    }
}

fn builtin_profile_entries() -> Vec<wtd_ui::command_palette::PaletteEntry> {
    vec![
        wtd_ui::command_palette::PaletteEntry {
            name: "powershell".to_string(),
            description: "Built-in PowerShell profile".to_string(),
            keybinding: None,
        },
        wtd_ui::command_palette::PaletteEntry {
            name: "cmd".to_string(),
            description: "Built-in Command Prompt profile".to_string(),
            keybinding: None,
        },
        wtd_ui::command_palette::PaletteEntry {
            name: "wsl".to_string(),
            description: "Built-in Windows Subsystem for Linux profile".to_string(),
            keybinding: None,
        },
        wtd_ui::command_palette::PaletteEntry {
            name: "ssh".to_string(),
            description: "Built-in SSH profile".to_string(),
            keybinding: None,
        },
    ]
}

fn insert_profile_entry(
    entries: &mut Vec<wtd_ui::command_palette::PaletteEntry>,
    seen: &mut HashSet<String>,
    name: &str,
    description: String,
) {
    if seen.insert(name.to_string()) {
        entries.push(wtd_ui::command_palette::PaletteEntry {
            name: name.to_string(),
            description,
            keybinding: None,
        });
    }
}

fn load_workspace_profile_entries(
    workspace_name: Option<&str>,
) -> Vec<wtd_ui::command_palette::PaletteEntry> {
    let mut entries = builtin_profile_entries();
    let mut seen: HashSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    let mut preferred_profile: Option<String> = None;

    if let Ok(settings) = wtd_core::load_global_settings(&settings_path()) {
        preferred_profile = Some(settings.default_profile.clone());
        let mut global_profiles: Vec<_> = settings.profiles.into_iter().collect();
        global_profiles.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, profile) in global_profiles {
            insert_profile_entry(
                &mut entries,
                &mut seen,
                &name,
                format!("Global {} profile", profile_kind(&profile.profile_type)),
            );
        }
    }

    if let Some(workspace_name) = workspace_name {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(found) = wtd_core::find_workspace(workspace_name, None, &cwd) {
                if let Ok(content) = fs::read_to_string(&found.path) {
                    let file_name = found
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(workspace_name);
                    if let Ok(workspace) = wtd_core::load_workspace_definition(file_name, &content)
                    {
                        if let Some(profile) = workspace
                            .defaults
                            .as_ref()
                            .and_then(|defaults| defaults.profile.as_ref())
                        {
                            preferred_profile = Some(profile.clone());
                        }
                        if let Some(mut workspace_profiles) = workspace.profiles {
                            let mut workspace_profiles: Vec<_> =
                                workspace_profiles.drain().collect();
                            workspace_profiles.sort_by(|a, b| a.0.cmp(&b.0));
                            for (name, profile) in workspace_profiles {
                                insert_profile_entry(
                                    &mut entries,
                                    &mut seen,
                                    &name,
                                    format!(
                                        "Workspace {} profile",
                                        profile_kind(&profile.profile_type)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(profile) = preferred_profile {
        if let Some(index) = entries.iter().position(|entry| entry.name == profile) {
            if index > 0 {
                let entry = entries.remove(index);
                entries.insert(0, entry);
            }
        }
    }

    entries
}

fn should_prompt_for_profile(
    action_name: &str,
    args: &Option<HashMap<String, String>>,
    connected: bool,
    bridge_present: bool,
) -> bool {
    bridge_present
        && connected
        && args.is_none()
        && matches!(
            action_name,
            "new-tab" | "split-right" | "split-down" | "change-profile"
        )
}

fn show_profile_selector_for_action(
    command_palette: &mut CommandPalette,
    action_name: &str,
) -> bool {
    match action_name {
        "new-tab" => {
            command_palette.show_profile_selector(
                "new-tab",
                "Create a new tab with profile",
                "Select a profile or type one manually",
            );
            true
        }
        "split-right" => {
            command_palette.show_profile_selector(
                "split-right",
                "Split right using profile",
                "Select a profile or type one manually",
            );
            true
        }
        "split-down" => {
            command_palette.show_profile_selector(
                "split-down",
                "Split down using profile",
                "Select a profile or type one manually",
            );
            true
        }
        "change-profile" => {
            command_palette.show_profile_selector(
                "change-profile",
                "Change focused pane profile",
                "Select a profile or type one manually",
            );
            true
        }
        _ => false,
    }
}

fn focused_pane_name(tab: &SnapshotTab) -> String {
    let focused = tab.layout_tree.focus();
    tab.pane_sessions
        .get(&focused)
        .and_then(|session| session.pane_path.rsplit('/').next().map(str::to_owned))
        .unwrap_or_else(|| format!("{}", focused.0))
}

fn show_tab_context_menu(
    hwnd: HWND,
    x: f32,
    y: f32,
    can_close: bool,
) -> Option<TabContextMenuAction> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let new_tab = wide_null("New Tab");
        let rename_tab = wide_null("Rename Tab");
        let close_tab = wide_null("Close Tab");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            TAB_MENU_NEW_TAB as usize,
            PCWSTR(new_tab.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            TAB_MENU_RENAME_TAB as usize,
            PCWSTR(rename_tab.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let close_flags = if can_close {
            MF_STRING | MF_ENABLED
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(
            menu,
            close_flags,
            TAB_MENU_CLOSE_TAB as usize,
            PCWSTR(close_tab.as_ptr()),
        );

        let mut point = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        let _ = ClientToScreen(hwnd, &mut point);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenuEx(
            menu,
            (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0,
            point.x,
            point.y,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match command.0 as u32 {
            TAB_MENU_NEW_TAB => Some(TabContextMenuAction::NewTab),
            TAB_MENU_RENAME_TAB => Some(TabContextMenuAction::RenameTab),
            TAB_MENU_CLOSE_TAB if can_close => Some(TabContextMenuAction::CloseTab),
            _ => None,
        }
    }
}

fn clamp_cells(value: f32) -> u16 {
    (value.max(1.0).floor() as u16).max(1)
}

#[derive(Debug, Clone, Copy)]
struct PaneViewportInsets {
    horizontal_cells: f32,
    vertical_cells: f32,
}

impl PaneViewportInsets {
    fn from_env() -> Self {
        let uniform = std::env::var("WTD_PANE_MARGIN_CELLS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok());
        let horizontal_cells = std::env::var("WTD_PANE_MARGIN_X_CELLS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .or(uniform)
            .unwrap_or(0.5)
            .max(0.0);
        let vertical_cells = std::env::var("WTD_PANE_MARGIN_Y_CELLS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .or(uniform)
            .unwrap_or(0.5)
            .max(0.0);

        Self {
            horizontal_cells,
            vertical_cells,
        }
    }
}

fn pane_content_rect(
    rect: PixelRect,
    cell_w: f32,
    cell_h: f32,
    insets: PaneViewportInsets,
) -> PixelRect {
    let desired_inset_x = cell_w * insets.horizontal_cells;
    let desired_inset_y = cell_h * insets.vertical_cells;
    let max_inset_x = ((rect.width - cell_w).max(0.0)) * 0.5;
    let max_inset_y = ((rect.height - cell_h).max(0.0)) * 0.5;
    let inset_x = desired_inset_x.min(max_inset_x);
    let inset_y = desired_inset_y.min(max_inset_y);

    PixelRect::new(
        rect.x + inset_x,
        rect.y + inset_y,
        (rect.width - inset_x * 2.0).max(cell_w.min(rect.width)),
        (rect.height - inset_y * 2.0).max(cell_h.min(rect.height)),
    )
}

const SCROLLBAR_HOVER_WIDTH: f32 = 14.0;
const SCROLLBAR_THICK_WIDTH: f32 = 10.0;
const SCROLLBAR_RIGHT_INSET: f32 = 2.0;
const SCROLLBAR_MIN_THUMB: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScrollbarMetrics {
    track: PixelRect,
    thumb: PixelRect,
    max_scroll: i32,
}

#[derive(Debug, Clone)]
struct ScrollbarDrag {
    pane_id: PaneId,
    grab_y: f32,
    thumb_top_at_start: f32,
    metrics_at_start: ScrollbarMetrics,
}

#[derive(Debug, Default)]
struct ScrollbarInteractionState {
    hovered_pane: Option<PaneId>,
    drag: Option<ScrollbarDrag>,
}

fn scrollbar_metrics(
    content_rect: PixelRect,
    scrollback_rows: usize,
    screen_rows: usize,
    visible_rows: usize,
    scrollback_offset: i32,
) -> Option<ScrollbarMetrics> {
    if scrollback_rows == 0 || visible_rows == 0 || content_rect.height <= 0.0 {
        return None;
    }
    let total_rows = scrollback_rows + screen_rows;
    let max_scroll = scrollback_rows as i32;
    if max_scroll <= 0 {
        return None;
    }

    let track = PixelRect::new(
        content_rect.x + content_rect.width - SCROLLBAR_RIGHT_INSET - SCROLLBAR_THICK_WIDTH,
        content_rect.y,
        SCROLLBAR_THICK_WIDTH.min(content_rect.width.max(0.0)),
        content_rect.height,
    );
    let thumb_height = (track.height * visible_rows as f32 / total_rows as f32)
        .clamp(SCROLLBAR_MIN_THUMB.min(track.height), track.height);
    let travel = (track.height - thumb_height).max(0.0);
    let offset = scrollback_offset.clamp(0, max_scroll);
    let progress = (max_scroll - offset) as f32 / max_scroll as f32;
    let thumb_top = track.y + travel * progress;

    Some(ScrollbarMetrics {
        track,
        thumb: PixelRect::new(track.x, thumb_top, track.width, thumb_height),
        max_scroll,
    })
}

fn scrollbar_offset_for_thumb_top(metrics: ScrollbarMetrics, thumb_top: f32) -> i32 {
    let travel = (metrics.track.height - metrics.thumb.height).max(0.0);
    if travel <= f32::EPSILON {
        return 0;
    }
    let clamped_top = thumb_top.clamp(metrics.track.y, metrics.track.y + travel);
    let progress = (clamped_top - metrics.track.y) / travel;
    ((1.0 - progress) * metrics.max_scroll as f32).round() as i32
}

fn scrollbar_hit_rect(metrics: ScrollbarMetrics) -> PixelRect {
    let width = SCROLLBAR_HOVER_WIDTH.min(metrics.track.width.max(SCROLLBAR_HOVER_WIDTH));
    PixelRect::new(
        metrics.track.x + metrics.track.width - width,
        metrics.track.y,
        width,
        metrics.track.height,
    )
}

fn rect_contains(rect: PixelRect, x: f32, y: f32) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn pane_scrollbar_metrics_at_point(
    tab: &SnapshotTab,
    pane_layout: &PaneLayout,
    mouse_handler: &MouseHandler,
    cell_w: f32,
    cell_h: f32,
    pane_viewport_insets: PaneViewportInsets,
    x: f32,
    y: f32,
) -> Option<(PaneId, ScrollbarMetrics)> {
    for pane_id in tab.layout_tree.panes() {
        let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) else {
            continue;
        };
        let content_rect = pane_content_rect(rect, cell_w, cell_h, pane_viewport_insets);
        if y < content_rect.y || y >= content_rect.y + content_rect.height {
            continue;
        }
        let screen = match tab.screens.get(&pane_id) {
            Some(screen) => screen,
            None => continue,
        };
        if screen.on_alternate() {
            continue;
        }
        let visible_rows = ((content_rect.height / cell_h).ceil() as usize).min(screen.rows());
        let metrics = match scrollbar_metrics(
            content_rect,
            screen.scrollback_len(),
            screen.rows(),
            visible_rows,
            mouse_handler.scroll_offset(&pane_id),
        ) {
            Some(metrics) => metrics,
            None => continue,
        };
        if rect_contains(scrollbar_hit_rect(metrics), x, y) {
            return Some((pane_id, metrics));
        }
    }
    None
}

fn content_dims(
    window_width: f32,
    window_height: f32,
    tab_strip: &TabStrip,
    status_bar: &StatusBar,
    cell_w: f32,
    cell_h: f32,
) -> (u16, u16) {
    let content_height = window_height - tab_strip.height() - status_bar.height();
    let content_rows = clamp_cells(content_height / cell_h);
    let content_cols = clamp_cells(window_width / cell_w);
    (content_cols, content_rows)
}

fn refresh_mouse_modes(
    mouse_modes: &mut HashMap<PaneId, MouseMode>,
    sgr_mouse_modes: &mut HashMap<PaneId, bool>,
    screens: &HashMap<PaneId, ScreenBuffer>,
) {
    mouse_modes.clear();
    sgr_mouse_modes.clear();
    for (pane_id, screen) in screens {
        mouse_modes.insert(pane_id.clone(), screen.mouse_mode());
        sgr_mouse_modes.insert(pane_id.clone(), screen.sgr_mouse());
    }
}

fn pane_at_point(pane_layout: &PaneLayout, x: f32, y: f32) -> Option<PaneId> {
    for (pane_id, rect) in pane_layout.pane_pixel_rects() {
        if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
            return Some(pane_id.clone());
        }
    }
    None
}

fn pane_has_mouse_reporting(tab: &SnapshotTab, pane_id: &PaneId) -> bool {
    tab.screens
        .get(pane_id)
        .map(|screen| screen.mouse_mode() != MouseMode::None)
        .unwrap_or(false)
}

fn active_tab_ref(tabs: &Vec<SnapshotTab>, active_tab_index: usize) -> Option<&SnapshotTab> {
    tabs.get(active_tab_index)
}

fn active_tab_mut(
    tabs: &mut Vec<SnapshotTab>,
    active_tab_index: usize,
) -> Option<&mut SnapshotTab> {
    tabs.get_mut(active_tab_index)
}

#[derive(Clone, Copy)]
enum ScrollbackNavigation {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

fn scrollback_navigation_for_action(name: &str) -> Option<ScrollbackNavigation> {
    match name {
        "scrollback-line-up" => Some(ScrollbackNavigation::LineUp),
        "scrollback-line-down" => Some(ScrollbackNavigation::LineDown),
        "scrollback-page-up" => Some(ScrollbackNavigation::PageUp),
        "scrollback-page-down" => Some(ScrollbackNavigation::PageDown),
        "scrollback-top" => Some(ScrollbackNavigation::Top),
        "scrollback-bottom" => Some(ScrollbackNavigation::Bottom),
        _ => None,
    }
}

fn navigate_focused_scrollback(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    navigation: ScrollbackNavigation,
) -> bool {
    let focused = tab.layout_tree.focus();
    let Some(screen) = tab.screens.get(&focused) else {
        return true;
    };
    if screen.on_alternate() {
        return true;
    }

    let max_scrollback = screen.scrollback_len() as i32;
    match navigation {
        ScrollbackNavigation::LineUp => mouse_handler.scroll_by(&focused, 1, max_scrollback),
        ScrollbackNavigation::LineDown => mouse_handler.scroll_by(&focused, -1, max_scrollback),
        ScrollbackNavigation::PageUp => {
            let page = screen.rows().saturating_sub(1).max(1) as i32;
            mouse_handler.scroll_by(&focused, page, max_scrollback);
        }
        ScrollbackNavigation::PageDown => {
            let page = screen.rows().saturating_sub(1).max(1) as i32;
            mouse_handler.scroll_by(&focused, -page, max_scrollback);
        }
        ScrollbackNavigation::Top => mouse_handler.scroll_to_top(&focused, max_scrollback),
        ScrollbackNavigation::Bottom => mouse_handler.reset_scroll(&focused),
    }
    true
}

#[derive(Default)]
struct KeyboardSelectionState {
    active: bool,
    pane_id: Option<PaneId>,
    move_start: bool,
}

impl KeyboardSelectionState {
    fn activate(&mut self, pane_id: PaneId) {
        self.active = true;
        self.pane_id = Some(pane_id);
        self.move_start = false;
    }

    fn deactivate(&mut self) {
        self.active = false;
        self.pane_id = None;
        self.move_start = false;
    }

    fn switch_endpoint(&mut self) {
        if self.active {
            self.move_start = !self.move_start;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FindMatch {
    row: usize,
    col: usize,
    len: usize,
}

#[derive(Default)]
struct FindState {
    pane_id: Option<PaneId>,
    query: String,
    matches: Vec<FindMatch>,
    current: usize,
}

impl FindState {
    fn clear(&mut self) {
        self.pane_id = None;
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    fn current_match(&self) -> Option<&FindMatch> {
        self.matches.get(self.current)
    }
}

fn virtual_row_text(screen: &ScreenBuffer, row: usize) -> String {
    let mut text = String::with_capacity(screen.cols());
    for col in 0..screen.cols() {
        if let Some(cell) = screen.cell_at_virtual(row, col) {
            text.push_str(cell.text.as_str());
        }
    }
    text
}

fn find_matches(screen: &ScreenBuffer, query: &str) -> Vec<FindMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let len = query.chars().count().max(1);
    let mut matches = Vec::new();
    for row in 0..screen.total_rows() {
        let haystack = virtual_row_text(screen, row).to_lowercase();
        let mut start = 0;
        while let Some(idx) = haystack[start..].find(&needle) {
            let col = start + idx;
            matches.push(FindMatch { row, col, len });
            start = col + needle.len().max(1);
        }
    }
    matches
}

fn next_find_index(current: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (current + 1) % len
    } else if current == 0 {
        len - 1
    } else {
        current - 1
    }
}

fn apply_find_match(
    screen: &ScreenBuffer,
    pane_id: &PaneId,
    mouse_handler: &mut MouseHandler,
    find_match: &FindMatch,
) {
    let max_scrollback = screen.scrollback_len();
    let base_row = find_match.row.min(max_scrollback);
    let offset = max_scrollback.saturating_sub(base_row) as i32;
    mouse_handler.set_scroll_offset(pane_id, offset, max_scrollback as i32);
    let viewport_row = find_match.row.saturating_sub(base_row);
    mouse_handler.set_selection(
        pane_id,
        Some(TextSelection {
            start_row: viewport_row,
            start_col: find_match.col,
            end_row: viewport_row,
            end_col: find_match
                .col
                .saturating_add(find_match.len.saturating_sub(1)),
        }),
    );
}

fn start_find(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    find_state: &mut FindState,
    query: &str,
    status_bar: &mut StatusBar,
) -> bool {
    let focused = tab.layout_tree.focus();
    let Some(screen) = tab.screens.get(&focused) else {
        find_state.clear();
        return true;
    };
    let matches = find_matches(screen, query);
    find_state.pane_id = Some(focused.clone());
    find_state.query = query.trim().to_string();
    find_state.matches = matches;
    find_state.current = 0;
    if let Some(find_match) = find_state.current_match() {
        apply_find_match(screen, &focused, mouse_handler, find_match);
        status_bar.set_pane_path(format!(
            "find: {} ({}/{})",
            find_state.query,
            find_state.current + 1,
            find_state.matches.len()
        ));
    } else {
        mouse_handler.clear_selection(&focused);
        status_bar.set_pane_path(format!("find: no matches for {}", find_state.query));
    }
    true
}

fn navigate_find(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    find_state: &mut FindState,
    status_bar: &mut StatusBar,
    forward: bool,
) -> bool {
    let focused = tab.layout_tree.focus();
    if find_state.pane_id.as_ref() != Some(&focused) || find_state.matches.is_empty() {
        return true;
    }
    let Some(screen) = tab.screens.get(&focused) else {
        find_state.clear();
        return true;
    };
    find_state.current = next_find_index(find_state.current, find_state.matches.len(), forward);
    if let Some(find_match) = find_state.current_match() {
        apply_find_match(screen, &focused, mouse_handler, find_match);
        status_bar.set_pane_path(format!(
            "find: {} ({}/{})",
            find_state.query,
            find_state.current + 1,
            find_state.matches.len()
        ));
    }
    true
}

fn selection_screen_bounds(screen: &ScreenBuffer) -> (usize, usize) {
    (
        screen.rows().saturating_sub(1),
        screen.cols().saturating_sub(1),
    )
}

fn clamp_selection_position(row: usize, col: usize, screen: &ScreenBuffer) -> (usize, usize) {
    let (max_row, max_col) = selection_screen_bounds(screen);
    (row.min(max_row), col.min(max_col))
}

fn activate_keyboard_mark_mode(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    keyboard_selection: &mut KeyboardSelectionState,
) -> bool {
    let focused = tab.layout_tree.focus();
    let Some(screen) = tab.screens.get(&focused) else {
        return true;
    };
    let cursor = screen.cursor();
    let (row, col) = clamp_selection_position(cursor.row, cursor.col, screen);
    if mouse_handler.selection(&focused).is_none() {
        mouse_handler.set_selection(
            &focused,
            Some(TextSelection {
                start_row: row,
                start_col: col,
                end_row: row,
                end_col: col,
            }),
        );
    }
    keyboard_selection.activate(focused);
    true
}

fn select_all_focused_pane(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    keyboard_selection: &mut KeyboardSelectionState,
) -> bool {
    let focused = tab.layout_tree.focus();
    let Some(screen) = tab.screens.get(&focused) else {
        return true;
    };
    let (max_row, max_col) = selection_screen_bounds(screen);
    mouse_handler.set_selection(
        &focused,
        Some(TextSelection {
            start_row: 0,
            start_col: 0,
            end_row: max_row,
            end_col: max_col,
        }),
    );
    keyboard_selection.activate(focused);
    true
}

fn move_keyboard_selection(
    tab: &SnapshotTab,
    mouse_handler: &mut MouseHandler,
    keyboard_selection: &mut KeyboardSelectionState,
    event: &KeyEvent,
) -> bool {
    if !keyboard_selection.active {
        return false;
    }
    let focused = tab.layout_tree.focus();
    if keyboard_selection.pane_id.as_ref() != Some(&focused) {
        keyboard_selection.deactivate();
        return false;
    }
    let Some(screen) = tab.screens.get(&focused) else {
        keyboard_selection.deactivate();
        return false;
    };
    if event.modifiers != Modifiers::NONE && event.modifiers != Modifiers::SHIFT {
        return false;
    }

    match event.key {
        KeyName::Escape | KeyName::Enter => {
            keyboard_selection.deactivate();
            true
        }
        KeyName::Left
        | KeyName::Right
        | KeyName::Up
        | KeyName::Down
        | KeyName::Home
        | KeyName::End
        | KeyName::PageUp
        | KeyName::PageDown => {
            let selection = mouse_handler.selection(&focused).unwrap_or_else(|| {
                let cursor = screen.cursor();
                let (row, col) = clamp_selection_position(cursor.row, cursor.col, screen);
                TextSelection {
                    start_row: row,
                    start_col: col,
                    end_row: row,
                    end_col: col,
                }
            });
            let (mut row, mut col) = if keyboard_selection.move_start {
                (selection.start_row, selection.start_col)
            } else {
                (selection.end_row, selection.end_col)
            };
            let (max_row, max_col) = selection_screen_bounds(screen);
            match event.key {
                KeyName::Left => col = col.saturating_sub(1),
                KeyName::Right => col = (col + 1).min(max_col),
                KeyName::Up => row = row.saturating_sub(1),
                KeyName::Down => row = (row + 1).min(max_row),
                KeyName::Home => col = 0,
                KeyName::End => col = max_col,
                KeyName::PageUp => row = 0,
                KeyName::PageDown => row = max_row,
                _ => {}
            }
            mouse_handler.set_selection(&focused, Some(selection));
            mouse_handler.set_selection_endpoint(&focused, keyboard_selection.move_start, row, col);
            true
        }
        _ => false,
    }
}

/// Route an action locally or to the host.
///
/// Returns `true` if the action was handled locally.
fn dispatch_action(
    action_ref: &wtd_core::workspace::ActionReference,
    command_palette: &mut CommandPalette,
    tab_strip: &mut TabStrip,
    tabs: &mut Vec<SnapshotTab>,
    active_tab_index: &mut usize,
    status_bar: &mut StatusBar,
    bridge: Option<&HostBridge>,
    connected: bool,
    notification_center_open: &mut bool,
    pane_metadata_list_open: &mut bool,
    pass_through_next_key: &mut PassThroughNextKeyState,
    mouse_handler: &mut MouseHandler,
    keyboard_selection: &mut KeyboardSelectionState,
    find_state: &mut FindState,
) -> bool {
    let name = action_name(action_ref);
    let args = match action_ref {
        wtd_core::workspace::ActionReference::WithArgs { args, .. } => args.clone(),
        _ => None,
    };

    if should_prompt_for_profile(name, &args, connected, bridge.is_some()) {
        return show_profile_selector_for_action(command_palette, name);
    }

    match name {
        "next-attention" => {
            if let Some(target) = next_attention_target(tabs, *active_tab_index, true) {
                focus_attention_target(tabs, tab_strip, active_tab_index, target);
                if let Some(active_tab) = active_tab_ref(tabs, *active_tab_index) {
                    let (attention_state, attention_message) = focused_pane_attention(active_tab);
                    status_bar.set_attention(attention_state, attention_message);
                }
            }
            return true;
        }
        "prev-attention" => {
            if let Some(target) = next_attention_target(tabs, *active_tab_index, false) {
                focus_attention_target(tabs, tab_strip, active_tab_index, target);
                if let Some(active_tab) = active_tab_ref(tabs, *active_tab_index) {
                    let (attention_state, attention_message) = focused_pane_attention(active_tab);
                    status_bar.set_attention(attention_state, attention_message);
                }
            }
            return true;
        }
        "clear-focused-attention" => {
            let _ = clear_focused_attention(tabs, *active_tab_index, bridge, status_bar);
            status_bar.set_attention_count(attention_count(tabs));
            return true;
        }
        "toggle-notification-center" => {
            *notification_center_open = !*notification_center_open;
            if *notification_center_open {
                status_bar.set_pane_path(notification_center_label(tabs));
            } else if let Some(active_tab) = active_tab_ref(tabs, *active_tab_index) {
                let focused = active_tab.layout_tree.focus();
                if let Some(pane_session) = active_tab.pane_sessions.get(&focused) {
                    status_bar.set_pane_path(pane_session.pane_path.clone());
                }
            }
            return true;
        }
        "toggle-pane-metadata-list" => {
            *pane_metadata_list_open = !*pane_metadata_list_open;
            if *pane_metadata_list_open {
                status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::All));
            } else if let Some(active_tab) = active_tab_ref(tabs, *active_tab_index) {
                let focused = active_tab.layout_tree.focus();
                if let Some(pane_session) = active_tab.pane_sessions.get(&focused) {
                    status_bar.set_pane_path(pane_session.pane_path.clone());
                }
            }
            return true;
        }
        "filter-pane-list-attention" => {
            *pane_metadata_list_open = true;
            status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::Attention));
            return true;
        }
        "filter-pane-list-status" => {
            *pane_metadata_list_open = true;
            status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::Status));
            return true;
        }
        "filter-pane-list-driver" => {
            *pane_metadata_list_open = true;
            status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::DriverProfile));
            return true;
        }
        "filter-pane-list-cwd" => {
            *pane_metadata_list_open = true;
            status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::Cwd));
            return true;
        }
        "filter-pane-list-branch" => {
            *pane_metadata_list_open = true;
            status_bar.set_pane_path(pane_metadata_summary(tabs, PaneListFilter::Branch));
            return true;
        }
        _ => {}
    }

    if matches!(name, "clear-buffer" | "clear-scrollback") {
        if let Some(active_tab) = active_tab_mut(tabs, *active_tab_index) {
            let focused = active_tab.layout_tree.focus();
            if let Some(screen) = active_tab.screens.get_mut(&focused) {
                if name == "clear-buffer" {
                    screen.clear_buffer();
                } else {
                    screen.clear_scrollback();
                }
            }
            mouse_handler.set_scroll_offset(&focused, 0, 0);
            mouse_handler.clear_selection(&focused);
            keyboard_selection.deactivate();
            if let Some(bridge) = bridge {
                if connected {
                    send_ui_action(bridge, active_tab, name, action_args_to_value(&args));
                }
            }
        }
        return true;
    }

    let Some(active_tab) = active_tab_ref(tabs, *active_tab_index) else {
        return false;
    };

    if let Some(navigation) = scrollback_navigation_for_action(name) {
        return navigate_focused_scrollback(active_tab, mouse_handler, navigation);
    }

    match name {
        "mark-mode" => activate_keyboard_mark_mode(active_tab, mouse_handler, keyboard_selection),
        "select-all" => select_all_focused_pane(active_tab, mouse_handler, keyboard_selection),
        "switch-selection-endpoint" => {
            keyboard_selection.switch_endpoint();
            true
        }
        "find" => {
            if args.is_none() {
                command_palette.show_prompt("find", "Find in focused pane", "Search text", "");
            } else if let Some(ref a) = args {
                let query = a.get("name").map(String::as_str).unwrap_or_default();
                start_find(active_tab, mouse_handler, find_state, query, status_bar);
            }
            true
        }
        "find-next" => navigate_find(active_tab, mouse_handler, find_state, status_bar, true),
        "find-prev" => navigate_find(active_tab, mouse_handler, find_state, status_bar, false),
        "toggle-command-palette" => {
            command_palette.toggle();
            true
        }
        "pass-through-next-key" => {
            pass_through_next_key.arm();
            true
        }
        "next-tab" => {
            let count = tab_strip.tab_count();
            if count > 0 {
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "next-tab", serde_json::Value::Null);
                    } else {
                        let next = (tab_strip.active_index() + 1) % count;
                        tab_strip.set_active(next);
                    }
                } else {
                    let next = (tab_strip.active_index() + 1) % count;
                    tab_strip.set_active(next);
                }
            }
            true
        }
        "prev-tab" => {
            let count = tab_strip.tab_count();
            if count > 0 {
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "prev-tab", serde_json::Value::Null);
                    } else {
                        let prev = if tab_strip.active_index() == 0 {
                            count - 1
                        } else {
                            tab_strip.active_index() - 1
                        };
                        tab_strip.set_active(prev);
                    }
                } else {
                    let prev = if tab_strip.active_index() == 0 {
                        count - 1
                    } else {
                        tab_strip.active_index() - 1
                    };
                    tab_strip.set_active(prev);
                }
            }
            true
        }
        "goto-tab" => {
            if let Some(ref a) = args {
                if let Some(idx_str) = a.get("index") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < tab_strip.tab_count() {
                            if let Some(bridge) = bridge {
                                if connected {
                                    send_workspace_action(
                                        bridge,
                                        "goto-tab",
                                        serde_json::json!({ "index": idx }),
                                    );
                                } else {
                                    tab_strip.set_active(idx);
                                }
                            } else {
                                tab_strip.set_active(idx);
                            }
                        }
                    }
                }
            }
            true
        }
        "new-tab" => {
            if let Some(bridge) = bridge {
                if connected {
                    send_workspace_action(bridge, "new-tab", action_args_to_value(&args));
                } else {
                    let tab_name = format!("tab-{}", tab_strip.tab_count() + 1);
                    tab_strip.add_tab(tab_name);
                    tab_strip.set_active(tab_strip.tab_count() - 1);
                }
            } else {
                let tab_name = format!("tab-{}", tab_strip.tab_count() + 1);
                tab_strip.add_tab(tab_name);
                tab_strip.set_active(tab_strip.tab_count() - 1);
            }
            true
        }
        "close-tab" => {
            if tab_strip.tab_count() > 1 {
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "close-tab", serde_json::json!({}));
                    } else {
                        let idx = tab_strip.active_index();
                        tab_strip.close_tab(idx);
                    }
                } else {
                    let idx = tab_strip.active_index();
                    tab_strip.close_tab(idx);
                }
            }
            true
        }
        "rename-tab" => {
            if args.is_none() {
                let initial = tab_strip
                    .active_tab()
                    .map(|tab| tab.name.clone())
                    .unwrap_or_default();
                command_palette.show_prompt(
                    "rename-tab",
                    "Rename the active tab",
                    "Enter a new tab name",
                    initial,
                );
            } else if let Some(bridge) = bridge {
                if connected {
                    send_workspace_action(bridge, "rename-tab", action_args_to_value(&args));
                }
            }
            true
        }
        "rename-pane" => {
            if args.is_none() {
                command_palette.show_prompt(
                    "rename-pane",
                    "Rename the focused pane",
                    "Enter a new pane name",
                    focused_pane_name(active_tab),
                );
            } else if let Some(bridge) = bridge {
                if connected {
                    send_ui_action(
                        bridge,
                        active_tab,
                        "rename-pane",
                        action_args_to_value(&args),
                    );
                }
            }
            true
        }
        "copy" => {
            let focused = active_tab.layout_tree.focus();
            if let Some(sel) = mouse_handler.selection(&focused) {
                if let Some(screen) = active_tab.screens.get(&focused) {
                    let scrollback_offset = mouse_handler.scroll_offset(&focused).max(0) as usize;
                    let text = wtd_ui::clipboard::extract_selection_text_at_offset(
                        screen,
                        &sel,
                        scrollback_offset,
                    );
                    if !text.is_empty() {
                        let _ = wtd_ui::clipboard::copy_to_clipboard(&text);
                    }
                }
            }
            true
        }
        "paste" => {
            if let Ok(text) = wtd_ui::clipboard::read_from_clipboard() {
                if !text.is_empty() {
                    if let Some(bridge) = bridge {
                        if connected {
                            let focused = active_tab.layout_tree.focus();
                            let bytes = paste_bytes_for_pane(active_tab, &focused, &text);
                            let _ = prepare_pane_for_live_input(
                                mouse_handler,
                                active_tab,
                                &focused,
                                true,
                            );
                            if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                bridge.send_input(ps.session_id.clone(), bytes);
                            }
                        }
                    }
                }
            }
            true
        }
        // All other actions go to host.
        _ => {
            if let Some(bridge) = bridge {
                if connected {
                    send_ui_action(bridge, active_tab, name, action_args_to_value(&args));
                }
            }
            false
        }
    }
}

fn run(workspace_name: Option<String>) -> anyhow::Result<()> {
    let cols: u16 = 80;
    let rows: u16 = 24;
    let title = workspace_name.as_deref().unwrap_or("WinTermDriver");

    let bridge: Option<HostBridge> = workspace_name
        .as_ref()
        .map(|name| HostBridge::connect(name.clone()));
    let mut tabs: Vec<SnapshotTab> = Vec::new();
    let mut active_tab_index = 0usize;
    let mut notification_center_open = false;
    let mut pane_metadata_list_open = false;

    if bridge.is_none() {
        let mut layout_tree = LayoutTree::new();
        let p1 = layout_tree.focus();
        let _ = layout_tree.split_right(p1.clone()).unwrap();
        let _ = layout_tree.split_down(p1.clone()).unwrap();
        let mut screens = HashMap::new();
        for pane_id in layout_tree.panes() {
            let mut screen = ScreenBuffer::new(cols, rows, 1000);
            feed_demo_content(&mut screen, &pane_id);
            screens.insert(pane_id, screen);
        }
        tabs.push(SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens,
        });
    } else {
        let layout_tree = LayoutTree::new();
        let p1 = layout_tree.focus();
        let mut screens = HashMap::new();
        screens.insert(p1, ScreenBuffer::new(cols, rows, 1000));
        tabs.push(SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens,
        });
    }

    // Create window.
    let hwnd = window::create_terminal_window(title, 1000, 600)?;

    // Create the renderer.
    let mut config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config)?;

    let (mut cell_w, mut cell_h) = renderer.cell_size();
    let pane_viewport_insets = PaneViewportInsets::from_env();

    // Create the tab strip.
    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    if bridge.is_none() {
        tab_strip.add_tab("main".to_string());
    } else {
        tab_strip.add_tab("loading".to_string());
    }
    tab_strip.set_active(0);
    tab_strip.set_window_maximized(window::is_maximized(hwnd));

    // Create the status bar.
    let mut status_bar = StatusBar::new(renderer.dw_factory())?;
    if let Some(ref name) = workspace_name {
        status_bar.set_workspace_name(name.clone());
    } else {
        status_bar.set_workspace_name("demo".to_string());
    }
    tab_strip.set_workspace_name(status_bar.workspace_name().to_string());

    // Create the command palette and input state machine.
    let bindings = wtd_core::global_settings::default_bindings();
    let input_classifier = InputClassifier::from_bindings(&bindings)?;
    let profile_entries = load_workspace_profile_entries(workspace_name.as_deref());
    let mut command_palette =
        CommandPalette::new(renderer.dw_factory(), &bindings, profile_entries)?;
    let mut prefix_sm = PrefixStateMachine::new(input_classifier);
    let mut pass_through_next_key = PassThroughNextKeyState::default();
    status_bar.set_prefix_label(prefix_sm.prefix_label().to_string());

    let (initial_window_width, initial_window_height) =
        window::client_size(hwnd).unwrap_or((1000, 600));
    let mut window_width: f32 = initial_window_width as f32;
    let mut window_height: f32 = initial_window_height as f32;
    tab_strip.layout(window_width);
    status_bar.layout(window_width);

    // Create the pane layout manager.
    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    let (content_cols, content_rows) = content_dims(
        window_width,
        window_height,
        &tab_strip,
        &status_bar,
        cell_w,
        cell_h,
    );
    pane_layout.update(
        &tabs[active_tab_index].layout_tree,
        0.0,
        tab_strip.height(),
        content_cols,
        content_rows,
    );

    // Mouse handler for selection, scrollback, focus, and paste.
    let mut mouse_handler = MouseHandler::new();
    let mut scrollbar_interaction = ScrollbarInteractionState::default();
    let mut keyboard_selection = KeyboardSelectionState::default();
    let mut find_state = FindState::default();
    let mut mouse_modes: HashMap<PaneId, MouseMode> = HashMap::new();
    let mut sgr_mouse_modes: HashMap<PaneId, bool> = HashMap::new();
    if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
        refresh_mouse_modes(&mut mouse_modes, &mut sgr_mouse_modes, &active_tab.screens);
    }

    // Set initial window title.
    refresh_window_title(
        hwnd,
        title,
        &tab_strip,
        active_tab_ref(&tabs, active_tab_index),
    );

    // Track whether we're connected to host.
    let mut connected = false;
    let mut window_shown = bridge.is_none();
    let mut awaiting_startup_frame = bridge.is_some();
    let mut startup_refresh_pending = bridge.is_some();
    let mut paint_scheduler = PaintScheduler::new();
    let mut startup_present_deadline = None;
    let mut delayed_show_deadline = bridge
        .as_ref()
        .map(|_| Instant::now() + Duration::from_millis(400));

    if window_shown {
        window::show_terminal_window(hwnd);
        paint_all(
            &renderer,
            &tab_strip,
            &pane_layout,
            &tabs[active_tab_index],
            &mouse_handler,
            &scrollbar_interaction,
            &status_bar,
            &command_palette,
            window_width,
            window_height,
            cell_w,
            cell_h,
            pane_viewport_insets,
        )?;
        paint_scheduler.complete_paint();
    }

    // Message loop.
    loop {
        window::pump_pending_messages();

        let mut needs_paint = false;
        let mut force_immediate_paint = false;
        let mut saw_visible_alt_screen_output = false;
        let mut saw_visible_primary_screen_tui_output = false;
        let mut should_close_window = false;

        // ── Drain host events ────────────────────────────────────
        if let Some(ref bridge) = bridge {
            while let Some(event) = bridge.try_recv() {
                match event {
                    HostEvent::Connected { state } => {
                        connected = true;
                        tracing::info!("attached to workspace");
                        delayed_show_deadline = Some(Instant::now() + Duration::from_millis(120));
                        startup_present_deadline = None;
                        let (content_cols, content_rows) = content_dims(
                            window_width,
                            window_height,
                            &tab_strip,
                            &status_bar,
                            cell_w,
                            cell_h,
                        );
                        if let Some(SnapshotRebuild {
                            workspace_name,
                            tab_names,
                            active_tab_index: rebuilt_active,
                            tabs: rebuilt_tabs,
                        }) = rebuild_from_snapshot(&state, content_cols, content_rows)
                        {
                            active_tab_index =
                                rebuilt_active.min(rebuilt_tabs.len().saturating_sub(1));
                            tabs = rebuilt_tabs;
                            status_bar.set_workspace_name(workspace_name.clone());
                            status_bar.set_session_status(SessionStatus::Running);
                            status_bar.set_attention_count(attention_count(&tabs));

                            tab_strip = TabStrip::new(renderer.dw_factory())?;
                            tab_strip.set_workspace_name(workspace_name.clone());
                            tab_strip.set_window_maximized(window::is_maximized(hwnd));
                            for name in tab_names {
                                tab_strip.add_tab(name);
                            }
                            tab_strip.set_active(active_tab_index);
                            sync_tab_progresses(&mut tab_strip, &tabs);
                            tab_strip.layout(window_width);
                            refresh_window_title(
                                hwnd,
                                &workspace_name,
                                &tab_strip,
                                active_tab_ref(&tabs, active_tab_index),
                            );

                            let mut startup_sizes_match = false;
                            refresh_active_tab_ui(
                                &mut tabs,
                                active_tab_index,
                                &mut pane_layout,
                                &tab_strip,
                                &mut status_bar,
                                &mut mouse_modes,
                                &mut sgr_mouse_modes,
                                window_width,
                                window_height,
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                            );
                            if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                let pane_sizes = pane_sizes_for_layout(
                                    &pane_layout,
                                    &active_tab.layout_tree,
                                    cell_w,
                                    cell_h,
                                    pane_viewport_insets,
                                );
                                startup_sizes_match =
                                    pane_sessions_match_sizes(active_tab, &pane_sizes);
                                send_active_pane_sizes(
                                    Some(bridge),
                                    connected,
                                    &pane_layout,
                                    active_tab,
                                    cell_w,
                                    cell_h,
                                    pane_viewport_insets,
                                );
                            }
                            if awaiting_startup_frame {
                                if startup_sizes_match {
                                    awaiting_startup_frame = false;
                                    startup_refresh_pending = false;
                                    startup_present_deadline = None;
                                } else if startup_refresh_pending {
                                    bridge.refresh_workspace();
                                    startup_refresh_pending = false;
                                }
                            }
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::SessionOutput {
                        session_id, data, ..
                    } => {
                        // Feed VT bytes to the screen buffer of the matching pane.
                        if let Some((tab_index, pane_id)) =
                            find_pane_for_session(&tabs, &session_id)
                        {
                            if let Some(tab) = tabs.get_mut(tab_index) {
                                let visible_screen_on_alternate = if let Some(screen) =
                                    tab.screens.get_mut(&pane_id)
                                {
                                    screen.advance(&data);
                                    (tab_index == active_tab_index).then(|| screen.on_alternate())
                                } else {
                                    None
                                };

                                if let Some(on_alternate) = visible_screen_on_alternate {
                                    refresh_mouse_modes(
                                        &mut mouse_modes,
                                        &mut sgr_mouse_modes,
                                        &tab.screens,
                                    );
                                    if on_alternate {
                                        saw_visible_alt_screen_output = true;
                                    } else if should_coalesce_primary_screen_output(&data) {
                                        saw_visible_primary_screen_tui_output = true;
                                    } else {
                                        force_immediate_paint = true;
                                    }
                                    needs_paint = true;
                                }
                            }
                        }
                    }
                    HostEvent::SessionStateChanged {
                        session_id,
                        new_state,
                        exit_code,
                        ..
                    } => {
                        if let Some((tab_index, pane_id)) =
                            find_pane_for_session(&tabs, &session_id)
                        {
                            if let Some(tab) = tabs.get_mut(tab_index) {
                                if let Some(pane_session) = tab.pane_sessions.get_mut(&pane_id) {
                                    pane_session.health_state = Some(new_state.clone());
                                }
                            }
                        }
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            let focused = active_tab.layout_tree.focus();
                            if active_tab
                                .pane_sessions
                                .get(&focused)
                                .is_some_and(|ps| ps.session_id == session_id)
                            {
                                let status = match new_state.as_str() {
                                    "running" => SessionStatus::Running,
                                    "exited" => SessionStatus::Exited {
                                        exit_code: exit_code.unwrap_or(0) as u32,
                                    },
                                    "failed" => SessionStatus::Failed {
                                        error: "session failed".into(),
                                    },
                                    "restarting" => SessionStatus::Restarting { attempt: 1 },
                                    _ => SessionStatus::Creating,
                                };
                                status_bar.set_session_status(status);
                            }
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::TitleChanged {
                        session_id, title, ..
                    } => {
                        tracing::debug!(session_id = %session_id, title = %title, "session title changed");
                        if let Some((tab_index, pane_id)) =
                            find_pane_for_session(&tabs, &session_id)
                        {
                            if let Some(tab) = tabs.get_mut(tab_index) {
                                if let Some(pane_session) = tab.pane_sessions.get_mut(&pane_id) {
                                    pane_session.title = Some(title.trim().to_string())
                                        .filter(|value| !value.is_empty());
                                }
                            }
                        }
                        refresh_window_title(
                            hwnd,
                            workspace_name.as_deref().unwrap_or("WinTermDriver"),
                            &tab_strip,
                            active_tab_ref(&tabs, active_tab_index),
                        );
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::ProgressChanged {
                        session_id,
                        progress,
                        ..
                    } => {
                        tracing::debug!(session_id = %session_id, "session progress changed");
                        if let Some((tab_index, pane_id)) =
                            find_pane_for_session(&tabs, &session_id)
                        {
                            if let Some(tab) = tabs.get_mut(tab_index) {
                                if let Some(pane_session) = tab.pane_sessions.get_mut(&pane_id) {
                                    pane_session.progress = progress;
                                }
                            }
                            sync_tab_progresses(&mut tab_strip, &tabs);
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::AttentionChanged {
                        pane_id,
                        state,
                        message,
                        ..
                    } => {
                        if let Some(host_pane_id) = pane_id {
                            let target_is_focused = active_tab_ref(&tabs, active_tab_index)
                                .is_some_and(|tab| {
                                    focused_pane_matches_host_id(tab, &host_pane_id)
                                });
                            let effective_state =
                                focus_aware_attention_state(state, target_is_focused);
                            for (tab_index, tab) in tabs.iter_mut().enumerate() {
                                for (ui_pane_id, pane_session) in tab.pane_sessions.iter_mut() {
                                    if pane_session.host_pane_id.as_deref()
                                        == Some(host_pane_id.as_str())
                                    {
                                        pane_session.attention = effective_state;
                                        pane_session.attention_message = message.clone();
                                        if tab_index == active_tab_index
                                            && *ui_pane_id == tab.layout_tree.focus()
                                        {
                                            status_bar
                                                .set_attention(effective_state, message.clone());
                                        }
                                    }
                                }
                            }
                        } else {
                            status_bar.set_attention(state, message);
                        }
                        status_bar.set_attention_count(attention_count(&tabs));
                        if notification_center_open {
                            status_bar.set_pane_path(notification_center_label(&tabs));
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::LayoutChanged { tab, layout, .. } => {
                        if tabs.is_empty() {
                            continue;
                        }

                        let Some(target_tab) =
                            tab_index_for_host_name(&tab_strip, &tab, active_tab_index)
                        else {
                            continue;
                        };

                        let pane_node = match serde_json::from_value::<PaneNode>(layout) {
                            Ok(node) => node,
                            Err(_) => continue,
                        };

                        if let Some(tab_state) = tabs.get_mut(target_tab) {
                            apply_tab_layout(tab_state, cols, rows, &pane_node);
                        } else {
                            continue;
                        }

                        if target_tab == active_tab_index {
                            refresh_active_tab_ui(
                                &mut tabs,
                                active_tab_index,
                                &mut pane_layout,
                                &tab_strip,
                                &mut status_bar,
                                &mut mouse_modes,
                                &mut sgr_mouse_modes,
                                window_width,
                                window_height,
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                            );
                            if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                send_active_pane_sizes(
                                    Some(bridge),
                                    connected,
                                    &pane_layout,
                                    active_tab,
                                    cell_w,
                                    cell_h,
                                    pane_viewport_insets,
                                );
                            }
                        }

                        sync_tab_progresses(&mut tab_strip, &tabs);

                        if connected {
                            bridge.refresh_workspace();
                        }

                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::WorkspaceStateChanged {
                        workspace,
                        new_state,
                    } => {
                        tracing::info!(workspace = %workspace, new_state = %new_state, "workspace state changed");
                        if workspace_name
                            .as_deref()
                            .is_none_or(|attached| attached == workspace)
                            && workspace_state_closes_ui(&new_state)
                        {
                            should_close_window = true;
                        }
                    }
                    HostEvent::Error { message } => {
                        tracing::error!(message = %message, "host error");
                    }
                    HostEvent::Disconnected { reason } => {
                        tracing::warn!(reason = %reason, "disconnected from host");
                        connected = false;
                        tracing::info!("closing UI window after host disconnect");
                        status_bar.set_session_status(SessionStatus::Failed { error: reason });
                        should_close_window = true;
                    }
                }
            }
        }

        if should_close_window {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            return Ok(());
        }

        // ── Handle resize ────────────────────────────────────────
        if let Some((w, h)) = window::take_resize() {
            if !window::is_minimized(hwnd) && w > 0 && h > 0 {
                window_width = w as f32;
                window_height = h as f32;
                if let Err(error) = renderer.resize(w, h) {
                    tracing::warn!(%error, "renderer resize failed; rebuilding render resources");
                    let (new_cell_w, new_cell_h) = rebuild_renderer_resources(
                        hwnd,
                        &config,
                        &bindings,
                        &mut renderer,
                        &mut tab_strip,
                        &mut status_bar,
                        &mut command_palette,
                        window_width,
                    )?;
                    cell_w = new_cell_w;
                    cell_h = new_cell_h;
                    pane_layout = PaneLayout::new(cell_w, cell_h);
                }
                tab_strip.set_window_maximized(window::is_maximized(hwnd));
                tab_strip.layout(window_width);
                status_bar.layout(window_width);

                let (content_cols, content_rows) = content_dims(
                    window_width,
                    window_height,
                    &tab_strip,
                    &status_bar,
                    cell_w,
                    cell_h,
                );
                pane_layout.update(
                    &tabs[active_tab_index].layout_tree,
                    0.0,
                    tab_strip.height(),
                    content_cols,
                    content_rows,
                );
                if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                    let pane_sizes = pane_sizes_for_layout(
                        &pane_layout,
                        &active_tab.layout_tree,
                        cell_w,
                        cell_h,
                        pane_viewport_insets,
                    );
                    sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
                }
                send_active_pane_sizes(
                    bridge.as_ref(),
                    connected,
                    &pane_layout,
                    &tabs[active_tab_index],
                    cell_w,
                    cell_h,
                    pane_viewport_insets,
                );
                if awaiting_startup_frame {
                    startup_refresh_pending = true;
                    if let Some(ref bridge) = bridge {
                        if connected {
                            bridge.refresh_workspace();
                            startup_refresh_pending = false;
                        }
                    }
                }

                // Notify host of pane resize (send for each pane).
                force_immediate_paint = true;
                needs_paint = true;
            }
        }

        // ── Process keyboard/text input events ───────────────────
        for input_event in window::drain_input_events() {
            match input_event {
                InputEvent::Key(event) => {
                    // When the command palette is visible, it consumes all keyboard input.
                    if command_palette.is_visible() {
                        if let Some(bound_name) = bound_action_name(prefix_sm.classifier(), &event)
                        {
                            if command_palette.has_action(&bound_name) {
                                let simple_ref =
                                    wtd_core::workspace::ActionReference::Simple(bound_name);
                                dispatch_action(
                                    &simple_ref,
                                    &mut command_palette,
                                    &mut tab_strip,
                                    &mut tabs,
                                    &mut active_tab_index,
                                    &mut status_bar,
                                    bridge.as_ref(),
                                    connected,
                                    &mut notification_center_open,
                                    &mut pane_metadata_list_open,
                                    &mut pass_through_next_key,
                                    &mut mouse_handler,
                                    &mut keyboard_selection,
                                    &mut find_state,
                                );
                                force_immediate_paint = true;
                                needs_paint = true;
                                continue;
                            }
                        }
                        match command_palette.on_key_event(&event) {
                            PaletteResult::Dismissed => {
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                            PaletteResult::Action(action_ref) => {
                                dispatch_action(
                                    &action_ref,
                                    &mut command_palette,
                                    &mut tab_strip,
                                    &mut tabs,
                                    &mut active_tab_index,
                                    &mut status_bar,
                                    bridge.as_ref(),
                                    connected,
                                    &mut notification_center_open,
                                    &mut pane_metadata_list_open,
                                    &mut pass_through_next_key,
                                    &mut mouse_handler,
                                    &mut keyboard_selection,
                                    &mut find_state,
                                );
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                            PaletteResult::Consumed => {
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                        }
                        continue;
                    }

                    if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                        if move_keyboard_selection(
                            active_tab,
                            &mut mouse_handler,
                            &mut keyboard_selection,
                            &event,
                        ) {
                            force_immediate_paint = true;
                            needs_paint = true;
                            continue;
                        }
                    }

                    if let Some(bytes) = pass_through_next_key.process_key(&event) {
                        if let Some(ref bridge) = bridge {
                            if connected && !bytes.is_empty() {
                                if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                    let focused = active_tab.layout_tree.focus();
                                    let reset_live_view = prepare_pane_for_live_input(
                                        &mut mouse_handler,
                                        active_tab,
                                        &focused,
                                        true,
                                    );
                                    if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                        bridge.send_input(ps.session_id.clone(), bytes);
                                    }
                                    if reset_live_view {
                                        force_immediate_paint = true;
                                        needs_paint = true;
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // Normal mode — run through prefix state machine (§21.3).
                    let output = prefix_sm.process(&event);
                    match output {
                        PrefixOutput::DispatchAction(action_ref) => {
                            dispatch_action(
                                &action_ref,
                                &mut command_palette,
                                &mut tab_strip,
                                &mut tabs,
                                &mut active_tab_index,
                                &mut status_bar,
                                bridge.as_ref(),
                                connected,
                                &mut notification_center_open,
                                &mut pane_metadata_list_open,
                                &mut pass_through_next_key,
                                &mut mouse_handler,
                                &mut keyboard_selection,
                                &mut find_state,
                            );
                            force_immediate_paint = true;
                            needs_paint = true;
                        }
                        PrefixOutput::SendToSession(bytes) => {
                            if let Some(ref bridge) = bridge {
                                if connected && !bytes.is_empty() {
                                    if let Some(active_tab) =
                                        active_tab_ref(&tabs, active_tab_index)
                                    {
                                        let focused = active_tab.layout_tree.focus();
                                        let reset_live_view = prepare_pane_for_live_input(
                                            &mut mouse_handler,
                                            active_tab,
                                            &focused,
                                            true,
                                        );
                                        if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                            bridge.send_input(ps.session_id.clone(), bytes);
                                        }
                                        if reset_live_view {
                                            force_immediate_paint = true;
                                            needs_paint = true;
                                        }
                                    }
                                }
                            }
                        }
                        PrefixOutput::Consumed => {
                            force_immediate_paint = true;
                            needs_paint = true;
                        }
                    }
                }
                InputEvent::Text(text) => {
                    if text.is_empty() {
                        continue;
                    }

                    if command_palette.is_visible() {
                        match command_palette.on_text_input(&text) {
                            PaletteResult::Dismissed => {
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                            PaletteResult::Action(action_ref) => {
                                dispatch_action(
                                    &action_ref,
                                    &mut command_palette,
                                    &mut tab_strip,
                                    &mut tabs,
                                    &mut active_tab_index,
                                    &mut status_bar,
                                    bridge.as_ref(),
                                    connected,
                                    &mut notification_center_open,
                                    &mut pane_metadata_list_open,
                                    &mut pass_through_next_key,
                                    &mut mouse_handler,
                                    &mut keyboard_selection,
                                    &mut find_state,
                                );
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                            PaletteResult::Consumed => {
                                force_immediate_paint = true;
                                needs_paint = true;
                            }
                        }
                        continue;
                    }

                    if let Some(bytes) = pass_through_next_key.process_text(&text) {
                        if let Some(ref bridge) = bridge {
                            if connected && !bytes.is_empty() {
                                if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                    let focused = active_tab.layout_tree.focus();
                                    let reset_live_view = prepare_pane_for_live_input(
                                        &mut mouse_handler,
                                        active_tab,
                                        &focused,
                                        true,
                                    );
                                    if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                        bridge.send_input(ps.session_id.clone(), bytes);
                                    }
                                    if reset_live_view {
                                        force_immediate_paint = true;
                                        needs_paint = true;
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    if prefix_sm.is_prefix_active() {
                        for output in prefix_sm.process_text(&text) {
                            match output {
                                PrefixOutput::DispatchAction(action_ref) => {
                                    dispatch_action(
                                        &action_ref,
                                        &mut command_palette,
                                        &mut tab_strip,
                                        &mut tabs,
                                        &mut active_tab_index,
                                        &mut status_bar,
                                        bridge.as_ref(),
                                        connected,
                                        &mut notification_center_open,
                                        &mut pane_metadata_list_open,
                                        &mut pass_through_next_key,
                                        &mut mouse_handler,
                                        &mut keyboard_selection,
                                        &mut find_state,
                                    );
                                }
                                PrefixOutput::SendToSession(bytes) => {
                                    if let Some(ref bridge) = bridge {
                                        if connected && !bytes.is_empty() {
                                            if let Some(active_tab) =
                                                active_tab_ref(&tabs, active_tab_index)
                                            {
                                                let focused = active_tab.layout_tree.focus();
                                                let reset_live_view = prepare_pane_for_live_input(
                                                    &mut mouse_handler,
                                                    active_tab,
                                                    &focused,
                                                    true,
                                                );
                                                if let Some(ps) =
                                                    active_tab.pane_sessions.get(&focused)
                                                {
                                                    bridge.send_input(ps.session_id.clone(), bytes);
                                                }
                                                let _ = reset_live_view;
                                            }
                                        }
                                    }
                                }
                                PrefixOutput::Consumed => {}
                            }
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                        continue;
                    }

                    if let Some(ref bridge) = bridge {
                        if connected {
                            if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                let focused = active_tab.layout_tree.focus();
                                let reset_live_view = prepare_pane_for_live_input(
                                    &mut mouse_handler,
                                    active_tab,
                                    &focused,
                                    true,
                                );
                                if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                    bridge
                                        .send_input(ps.session_id.clone(), text_input_bytes(&text));
                                }
                                if reset_live_view {
                                    force_immediate_paint = true;
                                    needs_paint = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Check prefix timeout (§21.3).
        if prefix_sm.check_timeout() {
            force_immediate_paint = true;
            needs_paint = true;
        }
        // Update status bar transient indicator.
        let transient_label = if pass_through_next_key.is_armed() {
            pass_through_next_key.badge_label()
        } else {
            prefix_sm.prefix_label()
        };
        status_bar.set_prefix_label(transient_label.to_string());
        status_bar
            .set_prefix_active(pass_through_next_key.is_armed() || prefix_sm.is_prefix_active());
        status_bar.set_attention_count(attention_count(&tabs));

        // ── Process mouse events ─────────────────────────────────
        for event in window::drain_mouse_events() {
            if let MouseEventKind::Wheel(delta) = event.kind {
                if event.modifiers.ctrl() {
                    let target_pane = active_tab_ref(&tabs, active_tab_index).and_then(|tab| {
                        pane_at_point(&pane_layout, event.x, event.y)
                            .or_else(|| Some(tab.layout_tree.focus()))
                            .filter(|pane_id| pane_has_mouse_reporting(tab, pane_id))
                    });
                    if target_pane.is_none() {
                        let new_font_size = adjusted_font_size(config.font_size, delta);
                        if (new_font_size - config.font_size).abs() > f32::EPSILON {
                            config.font_size = new_font_size;
                            renderer = TerminalRenderer::new(hwnd, &config)?;
                            (cell_w, cell_h) = renderer.cell_size();
                            pane_layout = PaneLayout::new(cell_w, cell_h);
                            refresh_active_tab_ui(
                                &mut tabs,
                                active_tab_index,
                                &mut pane_layout,
                                &tab_strip,
                                &mut status_bar,
                                &mut mouse_modes,
                                &mut sgr_mouse_modes,
                                window_width,
                                window_height,
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                            );
                            send_active_pane_sizes(
                                bridge.as_ref(),
                                connected,
                                &pane_layout,
                                &tabs[active_tab_index],
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                            );
                            force_immediate_paint = true;
                            needs_paint = true;
                        }
                    }
                    continue;
                }
            }

            // When the command palette is visible, clicks dismiss or select.
            if command_palette.is_visible() {
                let result = match event.kind {
                    MouseEventKind::LeftDown => {
                        command_palette.on_click(event.x, event.y, window_width, window_height)
                    }
                    MouseEventKind::Wheel(delta) => command_palette.on_wheel(
                        event.x,
                        event.y,
                        delta,
                        window_width,
                        window_height,
                    ),
                    _ => Some(PaletteResult::Consumed),
                };

                if let Some(result) = result {
                    if let PaletteResult::Action(ref action_ref) = result {
                        dispatch_action(
                            action_ref,
                            &mut command_palette,
                            &mut tab_strip,
                            &mut tabs,
                            &mut active_tab_index,
                            &mut status_bar,
                            bridge.as_ref(),
                            connected,
                            &mut notification_center_open,
                            &mut pane_metadata_list_open,
                            &mut pass_through_next_key,
                            &mut mouse_handler,
                            &mut keyboard_selection,
                            &mut find_state,
                        );
                    }
                    force_immediate_paint = true;
                    needs_paint = true;
                }
                continue;
            }

            if matches!(event.kind, MouseEventKind::RightDown) && event.y < tab_strip.height() {
                if let Some(tab_index) = tab_strip.tab_index_at(event.x, event.y) {
                    if let Some(action) =
                        show_tab_context_menu(hwnd, event.x, event.y, tab_strip.tab_count() > 1)
                    {
                        match action {
                            TabContextMenuAction::NewTab => {
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        send_workspace_action(
                                            bridge,
                                            "new-tab",
                                            serde_json::json!({}),
                                        );
                                    }
                                } else {
                                    let n = tab_strip.tab_count() + 1;
                                    tab_strip.add_tab(format!("tab-{n}"));
                                    tab_strip.set_active(tab_strip.tab_count() - 1);
                                }
                            }
                            TabContextMenuAction::RenameTab => {
                                let clicked_name = tab_strip
                                    .tabs()
                                    .get(tab_index)
                                    .map(|tab| tab.name.clone())
                                    .unwrap_or_default();
                                if let Some(ref bridge) = bridge {
                                    if connected && tab_index != active_tab_index {
                                        send_workspace_action(
                                            bridge,
                                            "goto-tab",
                                            serde_json::json!({"index": tab_index}),
                                        );
                                    }
                                } else if tab_index < tab_strip.tab_count() {
                                    tab_strip.set_active(tab_index);
                                    active_tab_index = tab_index;
                                }
                                command_palette.show_prompt(
                                    "rename-tab",
                                    "Rename the selected tab",
                                    "Enter a new tab name",
                                    clicked_name,
                                );
                            }
                            TabContextMenuAction::CloseTab => {
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        if tab_index != active_tab_index {
                                            send_workspace_action(
                                                bridge,
                                                "goto-tab",
                                                serde_json::json!({"index": tab_index}),
                                            );
                                        }
                                        send_workspace_action(
                                            bridge,
                                            "close-tab",
                                            serde_json::json!({}),
                                        );
                                    }
                                } else if tab_strip.tab_count() > 1 {
                                    let result = tab_strip.close_tab(tab_index);
                                    if matches!(result, TabAction::Close(_))
                                        && tab_index < tabs.len()
                                    {
                                        tabs.remove(tab_index);
                                        active_tab_index = tab_strip
                                            .active_index()
                                            .min(tabs.len().saturating_sub(1));
                                    }
                                }
                            }
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                        continue;
                    }
                }
            }

            if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                if let Some(drag) = scrollbar_interaction.drag.clone() {
                    match event.kind {
                        MouseEventKind::Move => {
                            let thumb_top = drag.thumb_top_at_start + (event.y - drag.grab_y);
                            let offset =
                                scrollbar_offset_for_thumb_top(drag.metrics_at_start, thumb_top);
                            mouse_handler.set_scroll_offset(
                                &drag.pane_id,
                                offset,
                                drag.metrics_at_start.max_scroll,
                            );
                            scrollbar_interaction.hovered_pane = Some(drag.pane_id);
                            force_immediate_paint = true;
                            needs_paint = true;
                            continue;
                        }
                        MouseEventKind::LeftUp => {
                            scrollbar_interaction.drag = None;
                            scrollbar_interaction.hovered_pane = pane_scrollbar_metrics_at_point(
                                active_tab,
                                &pane_layout,
                                &mouse_handler,
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                                event.x,
                                event.y,
                            )
                            .map(|(pane_id, _)| pane_id);
                            force_immediate_paint = true;
                            needs_paint = true;
                            continue;
                        }
                        _ => {}
                    }
                }

                match event.kind {
                    MouseEventKind::Move => {
                        let hovered = pane_scrollbar_metrics_at_point(
                            active_tab,
                            &pane_layout,
                            &mouse_handler,
                            cell_w,
                            cell_h,
                            pane_viewport_insets,
                            event.x,
                            event.y,
                        )
                        .map(|(pane_id, _)| pane_id);
                        if hovered != scrollbar_interaction.hovered_pane {
                            scrollbar_interaction.hovered_pane = hovered;
                            force_immediate_paint = true;
                            needs_paint = true;
                        }
                    }
                    MouseEventKind::LeftDown => {
                        if let Some((pane_id, metrics)) = pane_scrollbar_metrics_at_point(
                            active_tab,
                            &pane_layout,
                            &mouse_handler,
                            cell_w,
                            cell_h,
                            pane_viewport_insets,
                            event.x,
                            event.y,
                        ) {
                            if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                                let _ = active_tab.layout_tree.set_focus(pane_id.clone());
                            }
                            let mut drag_metrics = metrics;
                            let grab_y = if rect_contains(metrics.thumb, event.x, event.y) {
                                event.y
                            } else {
                                let thumb_top = event.y - metrics.thumb.height * 0.5;
                                let offset = scrollbar_offset_for_thumb_top(metrics, thumb_top);
                                mouse_handler.set_scroll_offset(
                                    &pane_id,
                                    offset,
                                    metrics.max_scroll,
                                );
                                if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                    if let Some((_, refreshed)) = pane_scrollbar_metrics_at_point(
                                        active_tab,
                                        &pane_layout,
                                        &mouse_handler,
                                        cell_w,
                                        cell_h,
                                        pane_viewport_insets,
                                        event.x,
                                        event.y,
                                    ) {
                                        drag_metrics = refreshed;
                                    }
                                }
                                event.y
                            };
                            scrollbar_interaction.hovered_pane = Some(pane_id.clone());
                            scrollbar_interaction.drag = Some(ScrollbarDrag {
                                pane_id,
                                grab_y,
                                thumb_top_at_start: drag_metrics.thumb.y,
                                metrics_at_start: drag_metrics,
                            });
                            force_immediate_paint = true;
                            needs_paint = true;
                            continue;
                        }
                    }
                    _ => {}
                }
            }

            // Normal mode — delegate to MouseHandler.
            let focused = match active_tab_ref(&tabs, active_tab_index) {
                Some(tab) => tab.layout_tree.focus(),
                None => continue,
            };
            let alternate_screens: HashMap<PaneId, bool> = active_tab_ref(&tabs, active_tab_index)
                .map(|tab| {
                    tab.screens
                        .iter()
                        .map(|(pane_id, screen)| (pane_id.clone(), screen.on_alternate()))
                        .collect()
                })
                .unwrap_or_default();
            let ts_height = tab_strip.height();
            let sb_height = status_bar.height();
            let (content_cols, content_rows) = content_dims(
                window_width,
                window_height,
                &tab_strip,
                &status_bar,
                cell_w,
                cell_h,
            );
            let outputs = mouse_handler.handle_event(
                &event,
                &mut tab_strip,
                &mut pane_layout,
                ts_height,
                sb_height,
                window_height,
                &focused,
                &mouse_modes,
                &sgr_mouse_modes,
                &alternate_screens,
                cell_w,
                cell_h,
                pane_viewport_insets.horizontal_cells,
                pane_viewport_insets.vertical_cells,
            );

            if matches!(event.kind, MouseEventKind::LeftDoubleDown)
                && event.y < ts_height
                && !tab_strip.hits_interactive_target(event.x, event.y)
            {
                window::toggle_maximize_window(hwnd);
                tab_strip.set_window_maximized(window::is_maximized(hwnd));
                tab_strip.layout(window_width);
                force_immediate_paint = true;
                needs_paint = true;
                continue;
            }

            if matches!(event.kind, MouseEventKind::LeftDown)
                && event.y < ts_height
                && outputs.is_empty()
                && !tab_strip.hits_interactive_target(event.x, event.y)
            {
                window::begin_window_drag(hwnd);
                continue;
            }

            for output in outputs {
                match output {
                    MouseOutput::FocusPane(pane_id) => {
                        if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                            let _ = active_tab.layout_tree.set_focus(pane_id.clone());
                            if let Some(ps) = active_tab.pane_sessions.get(&pane_id) {
                                status_bar.set_pane_path(ps.pane_path.clone());
                            } else {
                                status_bar.set_pane_path(format!("{}", pane_id.0));
                            }
                        }
                        sync_tab_progresses(&mut tab_strip, &tabs);
                        refresh_window_title(
                            hwnd,
                            workspace_name.as_deref().unwrap_or("WinTermDriver"),
                            &tab_strip,
                            active_tab_ref(&tabs, active_tab_index),
                        );
                    }
                    MouseOutput::SelectionChanged(_pane_id, _selection) => {
                        // Selection state is tracked inside MouseHandler and rendered from there.
                    }
                    MouseOutput::SelectionFinalized(pane_id, selection) => {
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            if let Some(screen) = active_tab.screens.get(&pane_id) {
                                let scrollback_offset =
                                    mouse_handler.scroll_offset(&pane_id).max(0) as usize;
                                let text = wtd_ui::clipboard::extract_selection_text_at_offset(
                                    screen,
                                    &selection,
                                    scrollback_offset,
                                );
                                if !text.is_empty() {
                                    let _ = wtd_ui::clipboard::copy_to_clipboard(&text);
                                }
                            }
                        }
                    }
                    MouseOutput::PaneResize(PaneLayoutAction::Resize {
                        pane_id,
                        direction,
                        cells,
                    }) => {
                        let (content_cols, content_rows) = content_dims(
                            window_width,
                            window_height,
                            &tab_strip,
                            &status_bar,
                            cell_w,
                            cell_h,
                        );
                        let total = wtd_core::layout::Rect::new(0, 0, content_cols, content_rows);
                        if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                            let _ = active_tab
                                .layout_tree
                                .resize_pane(pane_id, direction, cells, total);
                            pane_layout.update(
                                &active_tab.layout_tree,
                                0.0,
                                tab_strip.height(),
                                content_cols,
                                content_rows,
                            );
                            send_active_pane_sizes(
                                bridge.as_ref(),
                                connected,
                                &pane_layout,
                                active_tab,
                                cell_w,
                                cell_h,
                                pane_viewport_insets,
                            );
                        }
                    }
                    MouseOutput::PaneResize(PaneLayoutAction::FocusPane(pane_id)) => {
                        if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                            let _ = active_tab.layout_tree.set_focus(pane_id);
                        }
                        sync_tab_progresses(&mut tab_strip, &tabs);
                        refresh_window_title(
                            hwnd,
                            workspace_name.as_deref().unwrap_or("WinTermDriver"),
                            &tab_strip,
                            active_tab_ref(&tabs, active_tab_index),
                        );
                    }
                    MouseOutput::SendToSession(pane_id, bytes) => {
                        if let Some(ref bridge) = bridge {
                            if connected {
                                if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                    let reset_live_view = prepare_pane_for_live_input(
                                        &mut mouse_handler,
                                        active_tab,
                                        &pane_id,
                                        false,
                                    );
                                    if let Some(ps) = active_tab.pane_sessions.get(&pane_id) {
                                        bridge.send_input(ps.session_id.clone(), bytes);
                                    }
                                    let _ = reset_live_view;
                                }
                            }
                        }
                    }
                    MouseOutput::ScrollPane(pane_id, _delta) => {
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            if let Some(screen) = active_tab.screens.get(&pane_id) {
                                mouse_handler
                                    .clamp_scroll(&pane_id, screen.scrollback_len() as i32);
                            }
                        }
                    }
                    MouseOutput::PasteClipboard(pane_id) => {
                        if let Ok(text) = wtd_ui::clipboard::read_from_clipboard() {
                            if !text.is_empty() {
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        if let Some(active_tab) =
                                            active_tab_ref(&tabs, active_tab_index)
                                        {
                                            let bytes =
                                                paste_bytes_for_pane(active_tab, &pane_id, &text);
                                            let reset_live_view = prepare_pane_for_live_input(
                                                &mut mouse_handler,
                                                active_tab,
                                                &pane_id,
                                                true,
                                            );
                                            if let Some(ps) = active_tab.pane_sessions.get(&pane_id)
                                            {
                                                bridge.send_input(ps.session_id.clone(), bytes);
                                            }
                                            let _ = reset_live_view;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    MouseOutput::Tab(tab_action) => {
                        match tab_action {
                            TabAction::WindowClose => {
                                if let Some(ref bridge) = bridge {
                                    bridge.send(HostCommand::Disconnect);
                                }
                                unsafe {
                                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(
                                        hwnd,
                                    );
                                }
                                return Ok(());
                            }
                            TabAction::Create => {
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        send_workspace_action(
                                            bridge,
                                            "new-tab",
                                            serde_json::json!({}),
                                        );
                                    } else {
                                        let n = tab_strip.tab_count() + 1;
                                        tab_strip.add_tab(format!("tab-{n}"));
                                        tab_strip.set_active(tab_strip.tab_count() - 1);
                                        active_tab_index = tab_strip.active_index();
                                        let fresh = LayoutTree::new();
                                        let pane_id = fresh.focus();
                                        let mut screens = HashMap::new();
                                        screens
                                            .insert(pane_id, ScreenBuffer::new(cols, rows, 1000));
                                        tabs.push(SnapshotTab {
                                            layout_tree: fresh,
                                            pane_sessions: HashMap::new(),
                                            screens,
                                        });
                                        if let Some(active_tab) =
                                            active_tab_ref(&tabs, active_tab_index)
                                        {
                                            pane_layout.update(
                                                &active_tab.layout_tree,
                                                0.0,
                                                tab_strip.height(),
                                                content_cols,
                                                content_rows,
                                            );
                                            refresh_mouse_modes(
                                                &mut mouse_modes,
                                                &mut sgr_mouse_modes,
                                                &active_tab.screens,
                                            );
                                        }
                                    }
                                } else {
                                    let n = tab_strip.tab_count() + 1;
                                    tab_strip.add_tab(format!("tab-{n}"));
                                    tab_strip.set_active(tab_strip.tab_count() - 1);
                                    active_tab_index = tab_strip.active_index();
                                    let fresh = LayoutTree::new();
                                    let pane_id = fresh.focus();
                                    let mut screens = HashMap::new();
                                    screens.insert(pane_id, ScreenBuffer::new(cols, rows, 1000));
                                    tabs.push(SnapshotTab {
                                        layout_tree: fresh,
                                        pane_sessions: HashMap::new(),
                                        screens,
                                    });
                                    if let Some(active_tab) =
                                        active_tab_ref(&tabs, active_tab_index)
                                    {
                                        pane_layout.update(
                                            &active_tab.layout_tree,
                                            0.0,
                                            tab_strip.height(),
                                            content_cols,
                                            content_rows,
                                        );
                                        refresh_mouse_modes(
                                            &mut mouse_modes,
                                            &mut sgr_mouse_modes,
                                            &active_tab.screens,
                                        );
                                    }
                                }
                                tab_strip.layout(window_width);
                            }
                            TabAction::MinimizeWindow => {
                                window::minimize_window(hwnd);
                            }
                            TabAction::ToggleMaximizeWindow => {
                                window::toggle_maximize_window(hwnd);
                                tab_strip.set_window_maximized(window::is_maximized(hwnd));
                                tab_strip.layout(window_width);
                            }
                            TabAction::Close(tab_index) => {
                                if tabs.len() > 1 {
                                    if let Some(ref bridge) = bridge {
                                        if connected {
                                            if tab_index != active_tab_index {
                                                send_workspace_action(
                                                    bridge,
                                                    "goto-tab",
                                                    serde_json::json!({"index": tab_index}),
                                                );
                                            }
                                            send_workspace_action(
                                                bridge,
                                                "close-tab",
                                                serde_json::json!({}),
                                            );
                                        } else {
                                            let result = tab_strip.close_tab(tab_index);
                                            if matches!(result, TabAction::Close(_))
                                                && !tabs.is_empty()
                                            {
                                                tabs.remove(tab_index);
                                                active_tab_index = tab_strip.active_index();
                                                if active_tab_index >= tabs.len() {
                                                    active_tab_index = tabs.len().saturating_sub(1);
                                                }
                                                refresh_active_tab_ui(
                                                    &mut tabs,
                                                    active_tab_index,
                                                    &mut pane_layout,
                                                    &tab_strip,
                                                    &mut status_bar,
                                                    &mut mouse_modes,
                                                    &mut sgr_mouse_modes,
                                                    window_width,
                                                    window_height,
                                                    cell_w,
                                                    cell_h,
                                                    pane_viewport_insets,
                                                );
                                            }
                                        }
                                    } else {
                                        let result = tab_strip.close_tab(tab_index);
                                        if matches!(result, TabAction::Close(_)) && !tabs.is_empty()
                                        {
                                            tabs.remove(tab_index);
                                            active_tab_index = tab_strip.active_index();
                                            if active_tab_index >= tabs.len() {
                                                active_tab_index = tabs.len().saturating_sub(1);
                                            }
                                            refresh_active_tab_ui(
                                                &mut tabs,
                                                active_tab_index,
                                                &mut pane_layout,
                                                &tab_strip,
                                                &mut status_bar,
                                                &mut mouse_modes,
                                                &mut sgr_mouse_modes,
                                                window_width,
                                                window_height,
                                                cell_w,
                                                cell_h,
                                                pane_viewport_insets,
                                            );
                                        }
                                    }
                                }
                                tab_strip.layout(window_width);
                            }
                            TabAction::Reorder { .. } => {
                                tab_strip.layout(window_width);
                            }
                            TabAction::SwitchTo(target_tab) => {
                                if target_tab < tab_strip.tab_count() {
                                    if let Some(ref bridge) = bridge {
                                        if connected {
                                            send_workspace_action(
                                                bridge,
                                                "goto-tab",
                                                serde_json::json!({"index": target_tab}),
                                            );
                                        } else {
                                            tab_strip.set_active(target_tab);
                                            active_tab_index = target_tab;
                                            let (pane_sizes, focused, pane_path) =
                                                if let Some(active_tab) =
                                                    active_tab_ref(&tabs, active_tab_index)
                                                {
                                                    pane_layout.update(
                                                        &active_tab.layout_tree,
                                                        0.0,
                                                        tab_strip.height(),
                                                        content_cols,
                                                        content_rows,
                                                    );
                                                    let pane_sizes = pane_sizes_for_layout(
                                                        &pane_layout,
                                                        &active_tab.layout_tree,
                                                        cell_w,
                                                        cell_h,
                                                        pane_viewport_insets,
                                                    );
                                                    let focused = active_tab.layout_tree.focus();
                                                    let pane_path = active_tab
                                                        .pane_sessions
                                                        .get(&focused)
                                                        .map(|ps| ps.pane_path.clone());
                                                    (pane_sizes, Some(focused), pane_path)
                                                } else {
                                                    (Vec::new(), None, None)
                                                };
                                            if let Some(active_tab) =
                                                active_tab_mut(&mut tabs, active_tab_index)
                                            {
                                                sync_screen_buffers_to_sizes(
                                                    active_tab,
                                                    &pane_sizes,
                                                );
                                            }
                                            if let Some(path) = pane_path {
                                                status_bar.set_pane_path(path);
                                            } else if let Some(focused) = focused {
                                                status_bar.set_pane_path(format!("{}", focused.0));
                                            }
                                            if let Some(active_tab) =
                                                active_tab_ref(&tabs, active_tab_index)
                                            {
                                                refresh_mouse_modes(
                                                    &mut mouse_modes,
                                                    &mut sgr_mouse_modes,
                                                    &active_tab.screens,
                                                );
                                            }
                                        }
                                    } else {
                                        tab_strip.set_active(target_tab);
                                        active_tab_index = target_tab;
                                        let (pane_sizes, focused, pane_path) =
                                            if let Some(active_tab) =
                                                active_tab_ref(&tabs, active_tab_index)
                                            {
                                                pane_layout.update(
                                                    &active_tab.layout_tree,
                                                    0.0,
                                                    tab_strip.height(),
                                                    content_cols,
                                                    content_rows,
                                                );
                                                let pane_sizes = pane_sizes_for_layout(
                                                    &pane_layout,
                                                    &active_tab.layout_tree,
                                                    cell_w,
                                                    cell_h,
                                                    pane_viewport_insets,
                                                );
                                                let focused = active_tab.layout_tree.focus();
                                                let pane_path = active_tab
                                                    .pane_sessions
                                                    .get(&focused)
                                                    .map(|ps| ps.pane_path.clone());
                                                (pane_sizes, Some(focused), pane_path)
                                            } else {
                                                (Vec::new(), None, None)
                                            };
                                        if let Some(active_tab) =
                                            active_tab_mut(&mut tabs, active_tab_index)
                                        {
                                            sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
                                        }
                                        if let Some(path) = pane_path {
                                            status_bar.set_pane_path(path);
                                        } else if let Some(focused) = focused {
                                            status_bar.set_pane_path(format!("{}", focused.0));
                                        }
                                        if let Some(active_tab) =
                                            active_tab_ref(&tabs, active_tab_index)
                                        {
                                            refresh_mouse_modes(
                                                &mut mouse_modes,
                                                &mut sgr_mouse_modes,
                                                &active_tab.screens,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        refresh_window_title(
                            hwnd,
                            workspace_name.as_deref().unwrap_or("WinTermDriver"),
                            &tab_strip,
                            active_tab_ref(&tabs, active_tab_index),
                        );
                    }
                    MouseOutput::SetCursor(_hint) => {
                        // Cursor shape changes — could map to Win32 SetCursor.
                    }
                }
                force_immediate_paint = true;
                needs_paint = true;
            }
        }

        if window::take_needs_paint() {
            force_immediate_paint = true;
            needs_paint = true;
        }

        if needs_paint {
            if saw_visible_alt_screen_output && !force_immediate_paint {
                paint_scheduler.request_alt_screen_burst(Instant::now());
            } else if saw_visible_primary_screen_tui_output && !force_immediate_paint {
                paint_scheduler.request_primary_screen_tui_burst(Instant::now());
            } else {
                paint_scheduler.request_immediate();
            }
        }

        if !window_shown {
            let should_show = !awaiting_startup_frame
                || delayed_show_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if should_show {
                window::show_terminal_window(hwnd);
                if awaiting_startup_frame {
                    startup_present_deadline = Some(Instant::now() + Duration::from_millis(700));
                } else {
                    startup_present_deadline = None;
                }
                if let Some((w, h)) = window::client_size(hwnd) {
                    if w > 0 && h > 0 {
                        window_width = w as f32;
                        window_height = h as f32;
                        if let Err(error) = renderer.resize(w, h) {
                            tracing::warn!(
                                %error,
                                "renderer resize during window restore failed; rebuilding render resources"
                            );
                            let (new_cell_w, new_cell_h) = rebuild_renderer_resources(
                                hwnd,
                                &config,
                                &bindings,
                                &mut renderer,
                                &mut tab_strip,
                                &mut status_bar,
                                &mut command_palette,
                                window_width,
                            )?;
                            cell_w = new_cell_w;
                            cell_h = new_cell_h;
                            pane_layout = PaneLayout::new(cell_w, cell_h);
                        }
                        tab_strip.layout(window_width);
                        status_bar.layout(window_width);

                        refresh_active_tab_ui(
                            &mut tabs,
                            active_tab_index,
                            &mut pane_layout,
                            &tab_strip,
                            &mut status_bar,
                            &mut mouse_modes,
                            &mut sgr_mouse_modes,
                            window_width,
                            window_height,
                            cell_w,
                            cell_h,
                            pane_viewport_insets,
                        );
                        send_active_pane_sizes(
                            bridge.as_ref(),
                            connected,
                            &pane_layout,
                            &tabs[active_tab_index],
                            cell_w,
                            cell_h,
                            pane_viewport_insets,
                        );
                        if awaiting_startup_frame {
                            startup_refresh_pending = true;
                            if let Some(ref bridge) = bridge {
                                if connected {
                                    bridge.refresh_workspace();
                                    startup_refresh_pending = false;
                                }
                            }
                        } else {
                            startup_present_deadline = None;
                        }
                    }
                }
                window_shown = true;
                paint_scheduler.request_immediate();
            }
        }

        let startup_paint_ready = !awaiting_startup_frame
            || startup_present_deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if startup_paint_ready {
            startup_present_deadline = None;
        }

        if window_shown && startup_paint_ready && paint_scheduler.should_paint_now(Instant::now()) {
            sync_tab_progresses(&mut tab_strip, &tabs);
            let active_tab = if tabs.is_empty() {
                continue;
            } else {
                &tabs[active_tab_index]
            };
            if let Err(error) = paint_all(
                &renderer,
                &tab_strip,
                &pane_layout,
                active_tab,
                &mouse_handler,
                &scrollbar_interaction,
                &status_bar,
                &command_palette,
                window_width,
                window_height,
                cell_w,
                cell_h,
                pane_viewport_insets,
            ) {
                tracing::warn!(%error, "paint failed; rebuilding render resources and retrying");
                let (new_cell_w, new_cell_h) = rebuild_renderer_resources(
                    hwnd,
                    &config,
                    &bindings,
                    &mut renderer,
                    &mut tab_strip,
                    &mut status_bar,
                    &mut command_palette,
                    window_width,
                )?;
                cell_w = new_cell_w;
                cell_h = new_cell_h;
                pane_layout = PaneLayout::new(cell_w, cell_h);
                refresh_active_tab_ui(
                    &mut tabs,
                    active_tab_index,
                    &mut pane_layout,
                    &tab_strip,
                    &mut status_bar,
                    &mut mouse_modes,
                    &mut sgr_mouse_modes,
                    window_width,
                    window_height,
                    cell_w,
                    cell_h,
                    pane_viewport_insets,
                );
                paint_all(
                    &renderer,
                    &tab_strip,
                    &pane_layout,
                    &tabs[active_tab_index],
                    &mouse_handler,
                    &scrollbar_interaction,
                    &status_bar,
                    &command_palette,
                    window_width,
                    window_height,
                    cell_w,
                    cell_h,
                    pane_viewport_insets,
                )?;
            }
            paint_scheduler.complete_paint();
        }

        // Sleep briefly to avoid busy-looping, but wake promptly when a deferred
        // alternate-screen repaint becomes due.
        let sleep_for = paint_scheduler.sleep_interval(Duration::from_millis(16), Instant::now());
        std::thread::sleep(sleep_for);

        if !is_window_valid(hwnd) {
            if let Some(ref bridge) = bridge {
                bridge.send(HostCommand::Disconnect);
            }
            break;
        }
    }

    Ok(())
}

fn paint_all(
    renderer: &TerminalRenderer,
    tab_strip: &TabStrip,
    pane_layout: &PaneLayout,
    tab: &SnapshotTab,
    mouse_handler: &MouseHandler,
    scrollbar_interaction: &ScrollbarInteractionState,
    status_bar: &StatusBar,
    command_palette: &CommandPalette,
    window_width: f32,
    window_height: f32,
    cell_w: f32,
    cell_h: f32,
    pane_viewport_insets: PaneViewportInsets,
) -> anyhow::Result<()> {
    renderer.begin_draw();
    renderer.clear_background();

    // Tab strip.
    let tab_result = tab_strip.paint(renderer.render_target());

    // Pane content: render each pane's screen buffer clipped to its viewport.
    let focused = tab.layout_tree.focus();
    for pane_id in tab.layout_tree.panes() {
        if let (Some(rect), Some(screen)) = (
            pane_layout.pane_pixel_rect(&pane_id),
            tab.screens.get(&pane_id),
        ) {
            let content_rect = pane_content_rect(rect, cell_w, cell_h, pane_viewport_insets);
            renderer
                .paint_pane_viewport_scrolled(
                    screen,
                    content_rect.x,
                    content_rect.y,
                    content_rect.width,
                    content_rect.height,
                    mouse_handler.selection(&pane_id).as_ref(),
                    mouse_handler.scroll_offset(&pane_id).max(0) as usize,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let scroll_offset = mouse_handler.scroll_offset(&pane_id).max(0) as usize;
            let visible_rows = ((content_rect.height / cell_h).ceil() as usize).min(screen.rows());
            let scrollbar_expanded = scrollbar_interaction.hovered_pane.as_ref() == Some(&pane_id)
                || scrollbar_interaction
                    .drag
                    .as_ref()
                    .map_or(false, |drag| drag.pane_id == pane_id);
            renderer
                .paint_scrollback_scrollbar(
                    content_rect.x,
                    content_rect.y,
                    content_rect.width,
                    content_rect.height,
                    screen.scrollback_len(),
                    screen.rows(),
                    visible_rows,
                    scroll_offset,
                    scrollbar_expanded,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(pane_session) = tab.pane_sessions.get(&pane_id) {
                let overlay_label = pane_overlay_label(pane_session);
                renderer
                    .paint_pane_title_overlay(
                        &overlay_label,
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                        pane_id == focused,
                    )
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
    }

    // Pane borders, splitters, and focus indicator.
    let layout_result = pane_layout.paint(renderer.render_target(), &focused);

    // Status bar at the bottom.
    let status_result = status_bar.paint(
        renderer.render_target(),
        window_height - status_bar.height(),
    );

    // Command palette overlay (on top of everything else).
    let palette_result =
        command_palette.paint(renderer.render_target(), window_width, window_height);

    let end_result = renderer.end_draw();
    tab_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    layout_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    status_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    palette_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    end_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn is_window_valid(hwnd: windows::Win32::Foundation::HWND) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() }
}

/// Find the pane that owns the given session.
fn find_pane_for_session(tabs: &[SnapshotTab], session_id: &str) -> Option<(usize, PaneId)> {
    for (tab_index, tab) in tabs.iter().enumerate() {
        if let Some((pane_id, _)) = tab
            .pane_sessions
            .iter()
            .find(|(_, ps)| ps.session_id == session_id)
        {
            return Some((tab_index, pane_id.clone()));
        }
    }
    None
}

/// Feed VT content that identifies the pane (demo mode only).
fn feed_demo_content(screen: &mut ScreenBuffer, pane_id: &PaneId) {
    let mut vt = Vec::new();

    // Pane header.
    vt.extend_from_slice(format!("\x1b[1;44;37m Pane {} \x1b[0m\r\n", pane_id).as_bytes());
    vt.extend_from_slice(b"\r\n");

    // Colored content per pane.
    match pane_id.0 {
        1 => {
            vt.extend_from_slice(
                b"  \x1b[32muser@host\x1b[0m:\x1b[34m~/projects\x1b[0m$ ls -la\r\n",
            );
            vt.extend_from_slice(b"  drwxr-xr-x  2 user user 4096 Mar 28 \x1b[1;34msrc\x1b[0m\r\n");
            vt.extend_from_slice(
                b"  -rw-r--r--  1 user user  256 Mar 28 \x1b[0mCargo.toml\x1b[0m\r\n",
            );
        }
        2 => {
            vt.extend_from_slice(b"  \x1b[33mcargo build\x1b[0m\r\n");
            vt.extend_from_slice(b"  \x1b[32m   Compiling\x1b[0m wtd-core v0.1.0\r\n");
            vt.extend_from_slice(b"  \x1b[32m   Compiling\x1b[0m wtd-ui v0.1.0\r\n");
            vt.extend_from_slice(b"  \x1b[32m    Finished\x1b[0m dev target(s)\r\n");
        }
        _ => {
            vt.extend_from_slice(b"  \x1b[36mReady.\x1b[0m\r\n");
        }
    }

    screen.advance(&vt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_ui::input::{KeyName, Modifiers};

    #[test]
    fn default_bindings_toggle_palette_with_ctrl_shift_p() {
        let bindings = wtd_core::global_settings::default_bindings();
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let event = KeyEvent {
            key: KeyName::Char('P'),
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            character: None,
        };

        assert_eq!(
            bound_action_name(&classifier, &event).as_deref(),
            Some("toggle-command-palette")
        );
    }

    #[test]
    fn default_bindings_do_not_toggle_palette_with_ctrl_shift_space() {
        let bindings = wtd_core::global_settings::default_bindings();
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let event = KeyEvent {
            key: KeyName::Space,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            character: None,
        };

        assert_ne!(
            bound_action_name(&classifier, &event).as_deref(),
            Some("toggle-command-palette")
        );
    }

    #[test]
    fn workspace_state_closes_ui_for_terminal_states() {
        for state in [
            "closing",
            "closed",
            "stopping",
            "stopped",
            "exited",
            "terminating",
            "terminated",
            "CLOSING",
        ] {
            assert!(
                workspace_state_closes_ui(state),
                "expected terminal state '{state}' to close the UI"
            );
        }
    }

    #[test]
    fn workspace_state_keeps_ui_open_for_non_terminal_states() {
        for state in ["active", "running", "failed", "restarting", "created"] {
            assert!(
                !workspace_state_closes_ui(state),
                "expected non-terminal state '{state}' to keep the UI open"
            );
        }
    }

    #[test]
    fn parse_workspace_from_long_equals() {
        let args = vec![
            "wtd-ui".to_string(),
            "--workspace=test-workspace".to_string(),
        ];
        std::env::set_var("WTD_WORKSPACE", "ignored");
        assert_eq!(
            parse_workspace_from_args(&args),
            Some("test-workspace".to_string())
        );
    }

    #[test]
    fn parse_workspace_from_short_equals() {
        let args = vec!["wtd-ui".to_string(), "-w=test-workspace".to_string()];
        std::env::set_var("WTD_WORKSPACE", "ignored");
        assert_eq!(
            parse_workspace_from_args(&args),
            Some("test-workspace".to_string())
        );
    }

    #[test]
    fn parse_workspace_from_short_and_next() {
        let args = vec![
            "wtd-ui".to_string(),
            "-w".to_string(),
            "next-workspace".to_string(),
        ];
        assert_eq!(
            parse_workspace_from_args(&args),
            Some("next-workspace".to_string())
        );
    }

    #[test]
    fn parse_workspace_from_positional_then_env() {
        std::env::set_var("WTD_WORKSPACE", "env-workspace");
        let args = vec!["wtd-ui".to_string(), "positional-workspace".to_string()];
        assert_eq!(
            parse_workspace_from_args(&args),
            Some("positional-workspace".to_string())
        );
    }

    #[test]
    fn parse_workspace_from_env_when_none() {
        let args = vec!["wtd-ui".to_string()];
        std::env::set_var("WTD_WORKSPACE", "env-workspace");
        assert_eq!(
            parse_workspace_from_args(&args),
            Some("env-workspace".to_string())
        );
    }

    #[test]
    fn adjusted_font_size_grows_and_clamps() {
        assert_eq!(adjusted_font_size(14.0, 120), 15.0);
        assert_eq!(adjusted_font_size(MAX_FONT_SIZE, 120), MAX_FONT_SIZE);
    }

    #[test]
    fn adjusted_font_size_shrinks_and_clamps() {
        assert_eq!(adjusted_font_size(14.0, -120), 13.0);
        assert_eq!(adjusted_font_size(MIN_FONT_SIZE, -120), MIN_FONT_SIZE);
    }

    #[test]
    fn clear_heavy_primary_screen_output_is_coalesced() {
        assert!(should_coalesce_primary_screen_output(
            b"\x1B[2J\x1B[H\x1B[12;3Hhello"
        ));
    }

    #[test]
    fn paste_bytes_for_pane_wraps_when_bracketed_paste_enabled() {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let mut screen = ScreenBuffer::new(80, 24, 0);
        screen.advance(b"\x1b[?2004h");

        let tab = SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens: HashMap::from([(pane_id.clone(), screen)]),
        };

        let bytes = paste_bytes_for_pane(&tab, &pane_id, "hello");
        assert_eq!(bytes, b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn paste_bytes_for_pane_is_plain_when_bracketed_paste_disabled() {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let screen = ScreenBuffer::new(80, 24, 0);

        let tab = SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens: HashMap::from([(pane_id.clone(), screen)]),
        };

        let bytes = paste_bytes_for_pane(&tab, &pane_id, "hello");
        assert_eq!(bytes, b"hello");
    }

    fn scrollback_test_tab(rows: u16) -> (SnapshotTab, PaneId) {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let mut screen = ScreenBuffer::new(80, rows, 100);
        for line in 0..20 {
            screen.advance(format!("line {line}\r\n").as_bytes());
        }

        (
            SnapshotTab {
                layout_tree,
                pane_sessions: HashMap::new(),
                screens: HashMap::from([(pane_id.clone(), screen)]),
            },
            pane_id,
        )
    }

    #[test]
    fn scrollback_navigation_moves_line_page_and_edges() {
        let (tab, pane_id) = scrollback_test_tab(5);
        let max_scrollback = tab.screens[&pane_id].scrollback_len() as i32;
        assert!(max_scrollback > 4);
        let mut mouse_handler = MouseHandler::new();

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::LineUp
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), 1);

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::PageUp
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), 5);

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::Top
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), max_scrollback);

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::PageDown
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), max_scrollback - 4);

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::LineDown
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), max_scrollback - 5);

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::Bottom
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), 0);
    }

    #[test]
    fn scrollback_navigation_ignores_alternate_screen() {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let mut screen = ScreenBuffer::new(80, 5, 100);
        for line in 0..20 {
            screen.advance(format!("line {line}\r\n").as_bytes());
        }
        screen.advance(b"\x1b[?1049h");
        assert!(screen.on_alternate());

        let tab = SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens: HashMap::from([(pane_id.clone(), screen)]),
        };
        let mut mouse_handler = MouseHandler::new();

        assert!(navigate_focused_scrollback(
            &tab,
            &mut mouse_handler,
            ScrollbackNavigation::Top
        ));
        assert_eq!(mouse_handler.scroll_offset(&pane_id), 0);
    }

    #[test]
    fn keyboard_mark_mode_moves_endpoint_and_switches_endpoint() {
        let (tab, pane_id) = scrollback_test_tab(5);
        let mut mouse_handler = MouseHandler::new();
        let mut keyboard_selection = KeyboardSelectionState::default();

        assert!(activate_keyboard_mark_mode(
            &tab,
            &mut mouse_handler,
            &mut keyboard_selection
        ));
        assert!(keyboard_selection.active);
        assert_eq!(
            mouse_handler.selection(&pane_id),
            Some(TextSelection {
                start_row: 4,
                start_col: 0,
                end_row: 4,
                end_col: 0,
            })
        );

        let right = KeyEvent {
            key: KeyName::Right,
            modifiers: Modifiers::NONE,
            character: None,
        };
        assert!(move_keyboard_selection(
            &tab,
            &mut mouse_handler,
            &mut keyboard_selection,
            &right
        ));
        assert_eq!(
            mouse_handler.selection(&pane_id),
            Some(TextSelection {
                start_row: 4,
                start_col: 0,
                end_row: 4,
                end_col: 1,
            })
        );

        keyboard_selection.switch_endpoint();
        let up = KeyEvent {
            key: KeyName::Up,
            modifiers: Modifiers::NONE,
            character: None,
        };
        assert!(move_keyboard_selection(
            &tab,
            &mut mouse_handler,
            &mut keyboard_selection,
            &up
        ));
        assert_eq!(
            mouse_handler.selection(&pane_id),
            Some(TextSelection {
                start_row: 3,
                start_col: 0,
                end_row: 4,
                end_col: 1,
            })
        );
    }

    #[test]
    fn select_all_focused_pane_selects_viewport_and_copy_extracts_text() {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let mut screen = ScreenBuffer::new(8, 2, 10);
        screen.advance(b"alpha\r\nbeta");
        let tab = SnapshotTab {
            layout_tree,
            pane_sessions: HashMap::new(),
            screens: HashMap::from([(pane_id.clone(), screen)]),
        };
        let mut mouse_handler = MouseHandler::new();
        let mut keyboard_selection = KeyboardSelectionState::default();

        assert!(select_all_focused_pane(
            &tab,
            &mut mouse_handler,
            &mut keyboard_selection
        ));
        let selection = mouse_handler.selection(&pane_id).expect("selection");
        assert_eq!(
            selection,
            TextSelection {
                start_row: 0,
                start_col: 0,
                end_row: 1,
                end_col: 7,
            }
        );
        let text = wtd_ui::clipboard::extract_selection_text(&tab.screens[&pane_id], &selection);
        assert!(text.contains("alpha"));
        assert!(text.contains("beta"));
    }

    #[test]
    fn keyboard_selection_survives_scrollback_movement() {
        let (tab, pane_id) = scrollback_test_tab(5);
        let mut mouse_handler = MouseHandler::new();
        let mut keyboard_selection = KeyboardSelectionState::default();

        select_all_focused_pane(&tab, &mut mouse_handler, &mut keyboard_selection);
        let before = mouse_handler.selection(&pane_id);
        navigate_focused_scrollback(&tab, &mut mouse_handler, ScrollbackNavigation::PageUp);

        assert_eq!(mouse_handler.selection(&pane_id), before);
        assert!(mouse_handler.scroll_offset(&pane_id) > 0);
    }

    #[test]
    fn keyboard_mark_mode_exits_on_escape_without_clearing_selection() {
        let (tab, pane_id) = scrollback_test_tab(5);
        let mut mouse_handler = MouseHandler::new();
        let mut keyboard_selection = KeyboardSelectionState::default();
        activate_keyboard_mark_mode(&tab, &mut mouse_handler, &mut keyboard_selection);

        let escape = KeyEvent {
            key: KeyName::Escape,
            modifiers: Modifiers::NONE,
            character: None,
        };
        assert!(move_keyboard_selection(
            &tab,
            &mut mouse_handler,
            &mut keyboard_selection,
            &escape
        ));
        assert!(!keyboard_selection.active);
        assert!(mouse_handler.selection(&pane_id).is_some());
    }

    #[test]
    fn find_matches_visible_and_scrollback_rows() {
        let mut screen = ScreenBuffer::new(20, 3, 20);
        for line in ["alpha", "needle old", "middle", "needle live"] {
            screen.advance(format!("{line}\r\n").as_bytes());
        }

        let matches = find_matches(&screen, "needle");
        assert_eq!(matches.len(), 2);
        assert!(matches[0].row < matches[1].row);
        assert_eq!(find_matches(&screen, "missing"), Vec::<FindMatch>::new());
    }

    #[test]
    fn apply_find_match_scrolls_and_highlights_current_match() {
        let layout_tree = LayoutTree::new();
        let pane_id = layout_tree.focus();
        let mut screen = ScreenBuffer::new(20, 3, 20);
        for line in ["alpha", "needle old", "middle", "tail"] {
            screen.advance(format!("{line}\r\n").as_bytes());
        }
        let first = find_matches(&screen, "needle")
            .into_iter()
            .next()
            .expect("match");
        let mut mouse_handler = MouseHandler::new();

        apply_find_match(&screen, &pane_id, &mut mouse_handler, &first);

        assert!(mouse_handler.scroll_offset(&pane_id) > 0);
        assert_eq!(
            mouse_handler.selection(&pane_id),
            Some(TextSelection {
                start_row: 0,
                start_col: first.col,
                end_row: 0,
                end_col: first.col + first.len - 1,
            })
        );
    }

    #[test]
    fn next_find_index_wraps_forward_and_backward() {
        assert_eq!(next_find_index(0, 3, true), 1);
        assert_eq!(next_find_index(2, 3, true), 0);
        assert_eq!(next_find_index(0, 3, false), 2);
        assert_eq!(next_find_index(2, 3, false), 1);
        assert_eq!(next_find_index(0, 0, true), 0);
    }

    fn attention_test_tab() -> SnapshotTab {
        let mut layout_tree = LayoutTree::new();
        let first = layout_tree.focus();
        let second = layout_tree.split_right(first.clone()).unwrap();
        let third = layout_tree.split_down(first.clone()).unwrap();
        let mut pane_sessions = HashMap::new();
        for (pane_id, name, state, message) in [
            (first.clone(), "one", AttentionState::Active, None),
            (
                second.clone(),
                "two",
                AttentionState::NeedsAttention,
                Some("input requested".to_string()),
            ),
            (third.clone(), "three", AttentionState::Error, None),
        ] {
            pane_sessions.insert(
                pane_id,
                PaneSession {
                    host_pane_id: None,
                    session_id: name.to_string(),
                    pane_path: format!("dev/main/{name}"),
                    title: None,
                    session_size: None,
                    progress: None,
                    attention: state,
                    attention_message: message,
                    phase: None,
                    status_text: None,
                    queue_pending: None,
                    health_state: None,
                    source: None,
                    driver_profile: None,
                    cwd: None,
                    branch: None,
                },
            );
        }
        SnapshotTab {
            layout_tree,
            pane_sessions,
            screens: HashMap::new(),
        }
    }

    #[test]
    fn attention_helpers_count_and_summarize_unread_panes() {
        let tabs = vec![attention_test_tab()];
        assert_eq!(attention_count(&tabs), 2);
        let label = notification_center_label(&tabs);
        assert!(label.contains("dev/main/two: input requested"));
        assert!(label.contains("dev/main/three"));
    }

    #[test]
    fn focus_aware_attention_policy_suppresses_focused_needs_attention() {
        assert_eq!(
            focus_aware_attention_state(AttentionState::NeedsAttention, true),
            AttentionState::Active
        );
        assert_eq!(
            focus_aware_attention_state(AttentionState::NeedsAttention, false),
            AttentionState::NeedsAttention
        );
        assert_eq!(
            focus_aware_attention_state(AttentionState::Error, true),
            AttentionState::Error
        );

        let mut tab = attention_test_tab();
        let focused = tab.layout_tree.focus();
        tab.pane_sessions
            .get_mut(&focused)
            .expect("focused pane")
            .host_pane_id = Some("host-focused".to_string());
        assert!(focused_pane_matches_host_id(&tab, "host-focused"));
        assert!(!focused_pane_matches_host_id(&tab, "other"));
    }

    #[test]
    fn pane_metadata_summary_filters_status_and_attention() {
        let mut tab = attention_test_tab();
        let pane_id = tab
            .pane_sessions
            .iter()
            .find_map(|(pane_id, session)| {
                session
                    .pane_path
                    .ends_with("/two")
                    .then_some(pane_id.clone())
            })
            .expect("test pane must exist");
        let session = tab.pane_sessions.get_mut(&pane_id).unwrap();
        session.phase = Some("working".to_string());
        session.status_text = Some("running tests".to_string());
        session.queue_pending = Some(2);
        session.health_state = Some("running".to_string());
        session.source = Some("codex".to_string());
        session.driver_profile = Some("pi".to_string());
        session.cwd = Some("C:/Work/WinTermDriver".to_string());
        session.branch = Some("main".to_string());

        let tabs = vec![tab];
        let status = pane_metadata_summary(&tabs, PaneListFilter::Status);
        assert!(status.contains("dev/main/two"));
        assert!(status.contains("phase=working"));
        assert!(status.contains("running tests"));
        assert!(status.contains("queue=2"));
        assert!(status.contains("health=running"));
        assert!(status.contains("source=codex"));
        assert!(status.contains("driver=pi"));
        assert!(status.contains("cwd=C:/Work/WinTermDriver"));
        assert!(status.contains("branch=main"));
        assert!(!status.contains("dev/main/one"));

        let attention = pane_metadata_summary(&tabs, PaneListFilter::Attention);
        assert!(attention.contains("dev/main/two"));
        assert!(attention.contains("dev/main/three"));
        assert!(!attention.contains("dev/main/one"));

        let driver = pane_metadata_summary(&tabs, PaneListFilter::DriverProfile);
        assert!(driver.contains("driver=pi"));
        assert!(!driver.contains("dev/main/one"));

        let cwd = pane_metadata_summary(&tabs, PaneListFilter::Cwd);
        assert!(cwd.contains("cwd=C:/Work/WinTermDriver"));
        assert!(!cwd.contains("dev/main/one"));

        let branch = pane_metadata_summary(&tabs, PaneListFilter::Branch);
        assert!(branch.contains("branch=main"));
        assert!(!branch.contains("dev/main/one"));
    }

    #[test]
    fn attention_navigation_orders_and_wraps() {
        let tabs = vec![attention_test_tab()];
        let first = next_attention_target(&tabs, 0, true).expect("next attention");
        assert_eq!(first.0, 0);
        assert_eq!(
            tabs[0]
                .pane_sessions
                .get(&first.1)
                .unwrap()
                .pane_path
                .as_str(),
            "dev/main/three"
        );
        let previous = next_attention_target(&tabs, 0, false).expect("previous attention");
        assert_eq!(
            tabs[0]
                .pane_sessions
                .get(&previous.1)
                .unwrap()
                .pane_path
                .as_str(),
            "dev/main/two"
        );
    }

    #[test]
    fn text_input_bytes_preserves_altgr_style_characters_as_utf8_text() {
        for text in ["@", "{", "}", "[", "]", "\\", "~", "|"] {
            assert_eq!(text_input_bytes(text), text.as_bytes());
        }
    }

    #[test]
    fn text_input_bytes_preserves_composed_unicode_text_as_utf8() {
        for text in ["é", "ü", "ñ"] {
            assert_eq!(text_input_bytes(text), text.as_bytes());
        }
    }

    #[test]
    fn text_input_bytes_preserves_cjk_text_as_utf8() {
        for text in ["漢字", "かな", "한글"] {
            assert_eq!(text_input_bytes(text), text.as_bytes());
        }
    }

    #[test]
    fn pass_through_next_key_waits_for_text_when_key_event_has_no_bytes() {
        let mut state = PassThroughNextKeyState::default();
        state.arm();

        let event = KeyEvent {
            key: wtd_ui::input::KeyName::Char('A'),
            modifiers: wtd_ui::input::Modifiers::NONE,
            character: None,
        };

        assert_eq!(state.process_key(&event), Some(Vec::new()));
        assert!(state.is_armed());
        assert_eq!(state.process_text("a"), Some(b"a".to_vec()));
        assert!(!state.is_armed());
    }

    #[test]
    fn pass_through_next_key_sends_modified_special_key_once() {
        let mut state = PassThroughNextKeyState::default();
        state.arm();

        let event = KeyEvent {
            key: wtd_ui::input::KeyName::Right,
            modifiers: wtd_ui::input::Modifiers::ALT,
            character: None,
        };

        assert_eq!(state.process_key(&event), Some(b"\x1B[1;3C".to_vec()));
        assert!(!state.is_armed());
    }

    #[test]
    fn plain_shell_output_is_not_coalesced() {
        assert!(!should_coalesce_primary_screen_output(
            b"PS C:\\Users\\me> dir\r\n"
        ));
    }

    #[test]
    fn profile_actions_prompt_when_connected_without_args() {
        assert!(should_prompt_for_profile("new-tab", &None, true, true));
        assert!(should_prompt_for_profile("split-right", &None, true, true));
        assert!(should_prompt_for_profile("split-down", &None, true, true));
        assert!(should_prompt_for_profile(
            "change-profile",
            &None,
            true,
            true
        ));
    }

    #[test]
    fn profile_actions_do_not_prompt_with_args_or_without_host_connection() {
        let args = Some(HashMap::from([("profile".to_string(), "cmd".to_string())]));
        assert!(!should_prompt_for_profile("new-tab", &args, true, true));
        assert!(!should_prompt_for_profile("new-tab", &None, false, true));
        assert!(!should_prompt_for_profile("new-tab", &None, true, false));
        assert!(!should_prompt_for_profile("rename-pane", &None, true, true));
    }

    #[test]
    fn default_bindings_bind_pass_through_next_key_to_alt_shift_k() {
        let bindings = wtd_core::global_settings::default_bindings();
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let event = KeyEvent {
            key: wtd_ui::input::KeyName::Char('K'),
            modifiers: wtd_ui::input::Modifiers::ALT | wtd_ui::input::Modifiers::SHIFT,
            character: None,
        };

        assert_eq!(
            bound_action_name(&classifier, &event).as_deref(),
            Some("pass-through-next-key")
        );
    }

    #[test]
    fn scrollbar_thumb_represents_visible_fraction_with_minimum_size() {
        let content = PixelRect::new(10.0, 20.0, 200.0, 100.0);
        let metrics = scrollbar_metrics(content, 980, 20, 20, 0).expect("scrollbar");

        assert_eq!(metrics.max_scroll, 980);
        assert_eq!(metrics.thumb.height, SCROLLBAR_MIN_THUMB);
        assert_eq!(metrics.thumb.y, 20.0 + 100.0 - SCROLLBAR_MIN_THUMB);
        assert_eq!(
            scrollbar_offset_for_thumb_top(metrics, metrics.track.y),
            metrics.max_scroll
        );
        assert_eq!(
            scrollbar_offset_for_thumb_top(metrics, metrics.track.y + metrics.track.height),
            0
        );
    }

    #[test]
    fn scrollbar_hidden_when_buffer_fits_viewport() {
        let content = PixelRect::new(0.0, 0.0, 100.0, 80.0);
        assert!(scrollbar_metrics(content, 0, 24, 12, 0).is_none());
    }

    #[test]
    fn built_in_profile_entries_are_available_for_profile_selectors() {
        let entries = builtin_profile_entries();
        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["powershell", "cmd", "wsl", "ssh"]);
        assert!(entries
            .iter()
            .any(|entry| entry.description.contains("Command Prompt")));
    }
}
