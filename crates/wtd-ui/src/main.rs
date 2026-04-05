//! `wtd-ui` — WinTermDriver UI process.
//!
//! When a workspace name is provided (via `--workspace` or `WTD_WORKSPACE`
//! env var), connects to `wtd-host` via IPC. Otherwise runs in standalone
//! demo mode with hardcoded content.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_core::logging::init_stderr_logging;
use wtd_core::workspace::PaneNode;
use wtd_core::LogLevel;
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
use wtd_ui::snapshot::{rebuild_from_snapshot, SnapshotRebuild, SnapshotTab};
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::{TabAction, TabStrip};
use wtd_ui::window::{self, MouseEventKind};

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

fn pane_cell_size_for_rect(rect: &PixelRect, cell_w: f32, cell_h: f32) -> (u16, u16) {
    (
        clamp_cells(rect.width / cell_w),
        clamp_cells(rect.height / cell_h),
    )
}

fn send_active_pane_sizes(
    bridge: Option<&HostBridge>,
    connected: bool,
    pane_layout: &PaneLayout,
    layout_tree: &wtd_core::layout::LayoutTree,
    cell_w: f32,
    cell_h: f32,
) {
    if !connected {
        return;
    }
    let Some(bridge) = bridge else {
        return;
    };

    for pane_id in layout_tree.panes() {
        if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
            let (cols, rows) = pane_cell_size_for_rect(&rect, cell_w, cell_h);
            bridge.send_resize(format!("{}", pane_id.0), cols, rows);
        }
    }
}

fn pane_sizes_for_layout(
    pane_layout: &PaneLayout,
    layout_tree: &LayoutTree,
    cell_w: f32,
    cell_h: f32,
) -> Vec<(PaneId, u16, u16)> {
    let mut sizes = Vec::new();
    for pane_id in layout_tree.panes() {
        let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) else {
            continue;
        };

        let (cols, rows) = pane_cell_size_for_rect(&rect, cell_w, cell_h);
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

fn send_ui_action(bridge: &HostBridge, layout_tree: &LayoutTree, action_name: &str) {
    let focused = layout_tree.focus();
    bridge.send_action(
        action_name.to_string(),
        Some(format!("{}", focused.0)),
        serde_json::Value::Null,
    );
    bridge.refresh_workspace();
}

fn send_workspace_action(bridge: &HostBridge, action: &str, args: serde_json::Value) {
    bridge.send_action(action.to_string(), None, args);
    bridge.refresh_workspace();
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
    mouse_handler: &MouseHandler,
) -> bool {
    let name = action_name(action_ref);
    let args = match action_ref {
        wtd_core::workspace::ActionReference::WithArgs { args, .. } => args.clone(),
        _ => None,
    };

    match name {
        "toggle-command-palette" => {
            command_palette.toggle();
            true
        }
        "next-tab" => {
            let count = tab_strip.tab_count();
            if count > 0 {
                let next = (tab_strip.active_index() + 1) % count;
                tab_strip.set_active(next);
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "next-tab", serde_json::Value::Null);
                    }
                }
            }
            true
        }
        "prev-tab" => {
            let count = tab_strip.tab_count();
            if count > 0 {
                let prev = if tab_strip.active_index() == 0 {
                    count - 1
                } else {
                    tab_strip.active_index() - 1
                };
                tab_strip.set_active(prev);
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "prev-tab", serde_json::Value::Null);
                    }
                }
            }
            true
        }
        "goto-tab" => {
            if let Some(ref a) = args {
                if let Some(idx_str) = a.get("index") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < tab_strip.tab_count() {
                            tab_strip.set_active(idx);
                            if let Some(bridge) = bridge {
                                if connected {
                                    send_workspace_action(
                                        bridge,
                                        "goto-tab",
                                        serde_json::json!({ "index": idx }),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            true
        }
        "new-tab" => {
            let tab_name = format!("tab-{}", tab_strip.tab_count() + 1);
            tab_strip.add_tab(tab_name);
            tab_strip.set_active(tab_strip.tab_count() - 1);
            // Also tell host to create a tab with a session.
            if let Some(bridge) = bridge {
                if connected {
                    send_workspace_action(bridge, "new-tab", serde_json::json!({}));
                }
            }
            true
        }
        "close-tab" => {
            if tab_strip.tab_count() > 1 {
                let idx = tab_strip.active_index();
                tab_strip.close_tab(idx);
                // Also tell host to close the tab.
                if let Some(bridge) = bridge {
                    if connected {
                        send_workspace_action(bridge, "close-tab", serde_json::json!({}));
                    }
                }
            }
            true
        }
        "copy" => {
            let focused = active_tab.layout_tree.focus();
            if let Some(sel) = mouse_handler.selection(&focused) {
                if let Some(screen) = active_tab.screens.get(&focused) {
                    let text = wtd_ui::clipboard::extract_selection_text(screen, &sel);
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
                    send_ui_action(bridge, &active_tab.layout_tree, name);
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
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config)?;

    let (cell_w, cell_h) = renderer.cell_size();
    let pane_viewport_insets = PaneViewportInsets::from_env();

    // Create the tab strip.
    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    if bridge.is_none() {
        tab_strip.add_tab("main".to_string());
    } else {
        tab_strip.add_tab("loading".to_string());
    }
    tab_strip.set_active(0);

    // Create the status bar.
    let mut status_bar = StatusBar::new(renderer.dw_factory())?;
    if let Some(ref name) = workspace_name {
        status_bar.set_workspace_name(name.clone());
    } else {
        status_bar.set_workspace_name("demo".to_string());
    }

    // Create the command palette and input state machine.
    let bindings = wtd_core::global_settings::default_bindings();
    let input_classifier = InputClassifier::from_bindings(&bindings)?;
    let mut command_palette = CommandPalette::new(renderer.dw_factory(), &bindings)?;
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
    let win_title = tab_strip.window_title(title);
    window::set_window_title(hwnd, &win_title);

    // Track whether we're connected to host.
    let mut connected = false;
    let mut window_shown = bridge.is_none();
    let mut awaiting_startup_frame = bridge.is_some();
    let mut startup_refresh_pending = bridge.is_some();
    let mut paint_scheduler = PaintScheduler::new();
    let mut delayed_show_deadline = bridge
        .as_ref()
        .map(|_| Instant::now() + Duration::from_millis(400));

    if window_shown {
        window::show_terminal_window(hwnd);
        paint_all(
            &renderer,
            &tab_strip,
            &pane_layout,
            &tabs[active_tab_index].layout_tree,
            &tabs[active_tab_index].screens,
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
        let mut should_close_window = false;

        // ── Drain host events ────────────────────────────────────
        if let Some(ref bridge) = bridge {
            while let Some(event) = bridge.try_recv() {
                match event {
                    HostEvent::Connected { state } => {
                        connected = true;
                        tracing::info!("attached to workspace");
                        delayed_show_deadline = Some(Instant::now() + Duration::from_millis(250));
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
                            status_bar.set_workspace_name(workspace_name);
                            status_bar.set_session_status(SessionStatus::Running);

                            tab_strip = TabStrip::new(renderer.dw_factory())?;
                            for name in tab_names {
                                tab_strip.add_tab(name);
                            }
                            tab_strip.set_active(active_tab_index);
                            tab_strip.layout(window_width);

                            let mut startup_sizes_match = false;
                            if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                                let focused = active_tab.layout_tree.focus();
                                if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                    status_bar.set_pane_path(ps.pane_path.clone());
                                } else {
                                    status_bar.set_pane_path(format!("{}", focused.0));
                                }
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
                                );
                                startup_sizes_match =
                                    pane_sessions_match_sizes(active_tab, &pane_sizes);
                                sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
                                refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
                                send_active_pane_sizes(
                                    Some(bridge),
                                    connected,
                                    &pane_layout,
                                    &active_tab.layout_tree,
                                    cell_w,
                                    cell_h,
                                );
                            }
                            if awaiting_startup_frame {
                                if startup_sizes_match {
                                    awaiting_startup_frame = false;
                                    startup_refresh_pending = false;
                                } else if startup_refresh_pending {
                                    bridge.refresh_workspace();
                                    startup_refresh_pending = false;
                                }
                            }
                        }
                        force_immediate_paint = true;
                        needs_paint = true;
                    }
                    HostEvent::SessionOutput { session_id, data } => {
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
                    HostEvent::TitleChanged { session_id, title } => {
                        tracing::debug!(session_id = %session_id, title = %title, "session title changed");
                        if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                            let focused = active_tab.layout_tree.focus();
                            if active_tab
                                .pane_sessions
                                .get(&focused)
                                .is_some_and(|ps| ps.session_id == session_id)
                            {
                                window::set_window_title(hwnd, &format!("{title} — {title}"));
                            }
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

                        let (content_cols, content_rows) = content_dims(
                            window_width,
                            window_height,
                            &tab_strip,
                            &status_bar,
                            cell_w,
                            cell_h,
                        );

                        if target_tab == active_tab_index {
                            if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
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
                                );
                                sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
                                refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
                                send_active_pane_sizes(
                                    Some(bridge),
                                    connected,
                                    &pane_layout,
                                    &active_tab.layout_tree,
                                    cell_w,
                                    cell_h,
                                );

                                let focused = active_tab.layout_tree.focus();
                                if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                    status_bar.set_pane_path(ps.pane_path.clone());
                                } else {
                                    status_bar.set_pane_path(format!("{}", focused.0));
                                }
                            }
                        }

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
            if w > 0 && h > 0 {
                let _ = renderer.resize(w, h);
                window_width = w as f32;
                window_height = h as f32;
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
                    );
                    sync_screen_buffers_to_sizes(active_tab, &pane_sizes);
                }
                send_active_pane_sizes(
                    bridge.as_ref(),
                    connected,
                    &pane_layout,
                    &tabs[active_tab_index].layout_tree,
                    cell_w,
                    cell_h,
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
                                &mouse_handler,
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
                                &mouse_handler,
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
                            &mouse_handler,
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
                                if let Some(ps) = active_tab.pane_sessions.get(&focused) {
                                    bridge.send_input(ps.session_id.clone(), bytes);
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
                                &mouse_handler,
                            );
                        }
                    }
                    force_immediate_paint = true;
                    needs_paint = true;
                }
                continue;
            }

            // Normal mode — delegate to MouseHandler.
            let focused = match active_tab_ref(&tabs, active_tab_index) {
                Some(tab) => tab.layout_tree.focus(),
                None => continue,
            };
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
                cell_w,
                cell_h,
                pane_viewport_insets.horizontal_cells,
                pane_viewport_insets.vertical_cells,
            );

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
                    }
                    MouseOutput::SelectionChanged(_pane_id, _selection) => {
                        // Selection state is tracked inside MouseHandler.
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
                                &active_tab.layout_tree,
                                cell_w,
                                cell_h,
                            );
                        }
                    }
                    MouseOutput::PaneResize(PaneLayoutAction::FocusPane(pane_id)) => {
                        if let Some(active_tab) = active_tab_mut(&mut tabs, active_tab_index) {
                            let _ = active_tab.layout_tree.set_focus(pane_id);
                        }
                    }
                    MouseOutput::SendToSession(pane_id, bytes) => {
                        if let Some(ref bridge) = bridge {
                            if connected {
                                let ps = active_tab_ref(&tabs, active_tab_index)
                                    .and_then(|tab| tab.pane_sessions.get(&pane_id));
                                if let Some(ps) = ps {
                                    bridge.send_input(ps.session_id.clone(), bytes);
                                }
                            }
                        }
                    }
                    MouseOutput::ScrollPane(_pane_id, _delta) => {
                        // Scrollback view adjustment — tracked in MouseHandler.
                    }
                    MouseOutput::PasteClipboard(pane_id) => {
                        if let Ok(text) = wtd_ui::clipboard::read_from_clipboard() {
                            if !text.is_empty() {
                                let bytes = wtd_ui::clipboard::prepare_paste(&text, false);
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        let ps = active_tab_ref(&tabs, active_tab_index)
                                            .and_then(|tab| tab.pane_sessions.get(&pane_id));
                                        if let Some(ps) = ps {
                                            bridge.send_input(ps.session_id.clone(), bytes);
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
                                if let Some(active_tab) = active_tab_ref(&tabs, active_tab_index) {
                                    pane_layout.update(
                                        &active_tab.layout_tree,
                                        0.0,
                                        tab_strip.height(),
                                        content_cols,
                                        content_rows,
                                    );
                                    refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
                                }
                                tab_strip.layout(window_width);
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        send_workspace_action(
                                            bridge,
                                            "new-tab",
                                            serde_json::json!({}),
                                        );
                                    }
                                }
                            }
                            TabAction::Close(_) => {
                                if tabs.len() > 1 {
                                    let result = tab_strip.close_tab(active_tab_index);
                                    if matches!(result, TabAction::Close(_)) && !tabs.is_empty() {
                                        tabs.remove(active_tab_index);
                                        active_tab_index = tab_strip.active_index();
                                        if active_tab_index >= tabs.len() {
                                            active_tab_index = tabs.len().saturating_sub(1);
                                        }
                                    }
                                    if let Some(ref bridge) = bridge {
                                        if connected {
                                            send_workspace_action(
                                                bridge,
                                                "close-tab",
                                                serde_json::json!({}),
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
                                    tab_strip.set_active(target_tab);
                                    active_tab_index = target_tab;
                                    let (pane_sizes, focused, pane_path) = if let Some(active_tab) =
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
                                        refresh_mouse_modes(&mut mouse_modes, &active_tab.screens);
                                    }
                                    if let Some(ref bridge) = bridge {
                                        if connected {
                                            send_workspace_action(
                                                bridge,
                                                "goto-tab",
                                                serde_json::json!({"index": target_tab}),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        let win_title = tab_strip
                            .window_title(workspace_name.as_deref().unwrap_or("WinTermDriver"));
                        window::set_window_title(hwnd, &win_title);
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
            } else {
                paint_scheduler.request_immediate();
            }
        }

        if !window_shown {
            let should_show = !awaiting_startup_frame
                || delayed_show_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if should_show {
                window::show_terminal_window(hwnd);
                window::request_repaint(hwnd);
                window_shown = true;
                paint_scheduler.request_immediate();
            }
        }

        if window_shown && paint_scheduler.should_paint_now(Instant::now()) {
            let active_tab = if tabs.is_empty() {
                continue;
            } else {
                &tabs[active_tab_index]
            };
            paint_all(
                &renderer,
                &tab_strip,
                &pane_layout,
                &active_tab.layout_tree,
                &active_tab.screens,
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
    layout_tree: &LayoutTree,
    screens: &HashMap<PaneId, ScreenBuffer>,
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
    for pane_id in layout_tree.panes() {
        if let (Some(rect), Some(screen)) =
            (pane_layout.pane_pixel_rect(&pane_id), screens.get(&pane_id))
        {
            let content_rect = pane_content_rect(rect, cell_w, cell_h, pane_viewport_insets);
            renderer
                .paint_pane_viewport(
                    screen,
                    content_rect.x,
                    content_rect.y,
                    content_rect.width,
                    content_rect.height,
                    None,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }

    // Pane borders, splitters, and focus indicator.
    let focused = layout_tree.focus();
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
}
