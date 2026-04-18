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
use wtd_ipc::message::ProgressInfo;
use wtd_pty::MouseMode;
use wtd_pty::ScreenBuffer;
use wtd_ui::command_palette::{CommandPalette, PaletteResult};
use wtd_ui::host_bridge::{HostBridge, HostCommand, HostEvent};
use wtd_ui::input::{InputAction, InputClassifier, KeyEvent};
use wtd_ui::mouse_handler::{MouseHandler, MouseOutput};
use wtd_ui::paint_scheduler::PaintScheduler;
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction, PixelRect};
use wtd_ui::prefix_state::{PrefixOutput, PrefixStateMachine};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::snapshot::{rebuild_from_snapshot, PaneSession, SnapshotRebuild, SnapshotTab};
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::{TabAction, TabStrip};
use wtd_ui::window::{self, MouseEventKind};

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
        refresh_mouse_modes(mouse_modes, &active_tab.screens);
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
    let mut rebuilt_status_bar = StatusBar::new(renderer.dw_factory())?;
    rebuilt_status_bar.set_pane_path(pane_path);
    rebuilt_status_bar.set_session_status(session_status);
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
    screens: &HashMap<PaneId, ScreenBuffer>,
) {
    mouse_modes.clear();
    for (pane_id, screen) in screens {
        mouse_modes.insert(pane_id.clone(), screen.mouse_mode());
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

fn pane_is_on_alternate(tab: &SnapshotTab, pane_id: &PaneId) -> bool {
    tab.screens
        .get(pane_id)
        .map(ScreenBuffer::on_alternate)
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

/// Route an action locally or to the host.
///
/// Returns `true` if the action was handled locally.
fn dispatch_action(
    action_ref: &wtd_core::workspace::ActionReference,
    command_palette: &mut CommandPalette,
    tab_strip: &mut TabStrip,
    active_tab: &SnapshotTab,
    bridge: Option<&HostBridge>,
    connected: bool,
    mouse_handler: &mut MouseHandler,
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
        "toggle-command-palette" => {
            command_palette.toggle();
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
                    let bytes = wtd_ui::clipboard::prepare_paste(&text, false);
                    if let Some(bridge) = bridge {
                        if connected {
                            let focused = active_tab.layout_tree.focus();
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
    let mut mouse_modes: HashMap<PaneId, MouseMode> = HashMap::new();
    if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
        refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
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
                                    refresh_mouse_modes(&mut mouse_modes, &tab.screens);
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
                    HostEvent::LayoutChanged { tab, layout, .. } => {
                        if tabs.is_empty() {
                            continue;
                        }

                        let target_tab =
                            tab_index_for_host_name(&tab_strip, &tab, active_tab_index);
                        if target_tab.is_none() {
                            continue;
                        }
                        let target_tab = target_tab.unwrap();

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

        // ── Process keyboard events ──────────────────────────────
        for event in window::drain_key_events() {
            // When the command palette is visible, it consumes all keyboard input.
            if command_palette.is_visible() {
                if let Some(bound_name) = bound_action_name(prefix_sm.classifier(), &event) {
                    if command_palette.has_action(&bound_name) {
                        let simple_ref = wtd_core::workspace::ActionReference::Simple(bound_name);
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            dispatch_action(
                                &simple_ref,
                                &mut command_palette,
                                &mut tab_strip,
                                active_tab,
                                bridge.as_ref(),
                                connected,
                                &mut mouse_handler,
                            );
                        }
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
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            dispatch_action(
                                &action_ref,
                                &mut command_palette,
                                &mut tab_strip,
                                active_tab,
                                bridge.as_ref(),
                                connected,
                                &mut mouse_handler,
                            );
                        }
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

            // Normal mode — run through prefix state machine (§21.3).
            let output = prefix_sm.process(&event);
            match output {
                PrefixOutput::DispatchAction(action_ref) => {
                    if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                        dispatch_action(
                            &action_ref,
                            &mut command_palette,
                            &mut tab_strip,
                            active_tab,
                            bridge.as_ref(),
                            connected,
                            &mut mouse_handler,
                        );
                    }
                    force_immediate_paint = true;
                    needs_paint = true;
                }
                PrefixOutput::SendToSession(bytes) => {
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
                }
                PrefixOutput::Consumed => {
                    force_immediate_paint = true;
                    needs_paint = true;
                }
            }
        }

        // Check prefix timeout (§21.3).
        if prefix_sm.check_timeout() {
            status_bar.set_prefix_active(false);
            force_immediate_paint = true;
            needs_paint = true;
        }
        // Update status bar prefix indicator.
        status_bar.set_prefix_active(prefix_sm.is_prefix_active());

        // ── Process mouse events ─────────────────────────────────
        for event in window::drain_mouse_events() {
            if let MouseEventKind::Wheel(delta) = event.kind {
                if event.modifiers.ctrl() {
                    let target_pane = active_tab_ref(&tabs, active_tab_index).and_then(|tab| {
                        pane_at_point(&pane_layout, event.x, event.y)
                            .or_else(|| Some(tab.layout_tree.focus()))
                            .filter(|pane_id| pane_is_on_alternate(tab, pane_id))
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
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            dispatch_action(
                                action_ref,
                                &mut command_palette,
                                &mut tab_strip,
                                active_tab,
                                bridge.as_ref(),
                                connected,
                                &mut mouse_handler,
                            );
                        }
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
                                    if reset_live_view {
                                        force_immediate_paint = true;
                                        needs_paint = true;
                                    }
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
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    MouseOutput::PasteClipboard(pane_id) => {
                        if let Ok(text) = wtd_ui::clipboard::read_from_clipboard() {
                            if !text.is_empty() {
                                let bytes = wtd_ui::clipboard::prepare_paste(&text, false);
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        if let Some(active_tab) =
                                            active_tab_ref(&tabs, active_tab_index)
                                        {
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
                                        refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
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
}
