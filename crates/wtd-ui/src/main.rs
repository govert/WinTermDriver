//! `wtd-ui` — WinTermDriver UI process.
//!
//! When a workspace name is provided (via `--workspace` or `WTD_WORKSPACE`
//! env var), connects to `wtd-host` via IPC. Otherwise runs in standalone
//! demo mode with hardcoded content.

use std::collections::HashMap;

use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_core::logging::init_stderr_logging;
use wtd_core::LogLevel;
use wtd_pty::ScreenBuffer;
use wtd_ui::command_palette::{CommandPalette, PaletteResult};
use wtd_ui::host_bridge::{HostBridge, HostCommand, HostEvent};
use wtd_ui::input::KeyName;
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
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
    for i in 0..args.len() {
        if args[i] == "--workspace" || args[i] == "-w" {
            return args.get(i + 1).cloned();
        }
    }
    std::env::var("WTD_WORKSPACE").ok()
}

/// Session mapping: pane ID → session ID (from host).
struct PaneSession {
    session_id: String,
}

fn run(workspace_name: Option<String>) -> anyhow::Result<()> {
    let cols: u16 = 80;
    let rows: u16 = 24;

    // Create a layout tree with a multi-pane split (demo mode) or single pane.
    let mut layout_tree = LayoutTree::new();
    let mut screens: HashMap<PaneId, ScreenBuffer> = HashMap::new();
    let pane_sessions: HashMap<PaneId, PaneSession> = HashMap::new();

    // Host bridge (None in demo mode).
    let bridge: Option<HostBridge> = workspace_name.as_ref().map(|name| {
        HostBridge::connect(name.clone())
    });

    // In demo mode, set up hardcoded panes.
    if bridge.is_none() {
        let p1 = layout_tree.focus();
        let _p2 = layout_tree.split_right(p1.clone()).unwrap();
        let _p3 = layout_tree.split_down(p1.clone()).unwrap();
        for pane_id in layout_tree.panes() {
            let mut screen = ScreenBuffer::new(cols, rows, 1000);
            feed_demo_content(&mut screen, &pane_id);
            screens.insert(pane_id, screen);
        }
    } else {
        // In connected mode, start with one pane; state will arrive via HostEvent::Connected.
        let p1 = layout_tree.focus();
        screens.insert(p1, ScreenBuffer::new(cols, rows, 1000));
    }

    // Create window.
    let title = workspace_name.as_deref().unwrap_or("WinTermDriver");
    let hwnd = window::create_terminal_window(title, 1000, 600)?;

    // Create the renderer.
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config)?;

    let (cell_w, cell_h) = renderer.cell_size();

    // Create the tab strip.
    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    tab_strip.add_tab("main".to_string());
    tab_strip.set_active(0);

    // Create the status bar.
    let mut status_bar = StatusBar::new(renderer.dw_factory())?;
    if let Some(ref name) = workspace_name {
        status_bar.set_workspace_name(name.clone());
    } else {
        status_bar.set_workspace_name("demo".to_string());
    }

    // Create the command palette with default keybindings.
    let bindings = wtd_core::global_settings::default_bindings();
    let mut command_palette = CommandPalette::new(renderer.dw_factory(), &bindings)?;

    let mut window_width: f32 = 1000.0;
    let mut window_height: f32 = 600.0;
    tab_strip.layout(window_width);
    status_bar.layout(window_width);

    // Create the pane layout manager.
    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    let content_height = window_height - tab_strip.height() - status_bar.height();
    let content_rows = (content_height / cell_h) as u16;
    let content_cols = (window_width / cell_w) as u16;
    pane_layout.update(
        &layout_tree,
        0.0,
        tab_strip.height(),
        content_cols,
        content_rows,
    );

    // Set initial window title.
    let win_title = tab_strip.window_title(title);
    window::set_window_title(hwnd, &win_title);

    // Track whether we're connected to host.
    let mut connected = false;

    // Initial paint.
    paint_all(
        &renderer,
        &tab_strip,
        &pane_layout,
        &layout_tree,
        &screens,
        &status_bar,
        &command_palette,
        window_width,
        window_height,
    )?;

    // Message loop.
    loop {
        window::pump_pending_messages();

        let mut needs_paint = false;

        // ── Drain host events ────────────────────────────────────
        if let Some(ref bridge) = bridge {
            while let Some(event) = bridge.try_recv() {
                match event {
                    HostEvent::Connected { .. } => {
                        connected = true;
                        status_bar.set_session_status(SessionStatus::Running);
                        tracing::info!("attached to workspace");
                        needs_paint = true;
                    }
                    HostEvent::SessionOutput { session_id, data } => {
                        // Feed VT bytes to the screen buffer of the matching pane.
                        if let Some(pane_id) = find_pane_for_session(&pane_sessions, &session_id) {
                            if let Some(screen) = screens.get_mut(&pane_id) {
                                screen.advance(&data);
                                needs_paint = true;
                            }
                        }
                    }
                    HostEvent::SessionStateChanged {
                        session_id,
                        new_state,
                        exit_code,
                    } => {
                        tracing::info!(
                            session_id = %session_id,
                            new_state = %new_state,
                            exit_code = ?exit_code,
                            "session state changed"
                        );
                        // Update status bar if this is the focused pane's session.
                        let focused = layout_tree.focus();
                        if pane_sessions
                            .get(&focused)
                            .map_or(false, |ps| ps.session_id == session_id)
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
                        needs_paint = true;
                    }
                    HostEvent::TitleChanged { session_id, title } => {
                        tracing::debug!(session_id = %session_id, title = %title, "session title changed");
                        // Update window title if it's the focused pane.
                        let focused = layout_tree.focus();
                        if pane_sessions
                            .get(&focused)
                            .map_or(false, |ps| ps.session_id == session_id)
                        {
                            let win_title = format!(
                                "{} — {}",
                                workspace_name.as_deref().unwrap_or("WinTermDriver"),
                                title
                            );
                            window::set_window_title(hwnd, &win_title);
                        }
                        needs_paint = true;
                    }
                    HostEvent::LayoutChanged { .. } => {
                        // Layout changes from host — would rebuild layout tree.
                        // Full implementation deferred to downstream beads.
                        tracing::debug!("layout changed notification received");
                        needs_paint = true;
                    }
                    HostEvent::WorkspaceStateChanged {
                        workspace,
                        new_state,
                    } => {
                        tracing::info!(workspace = %workspace, new_state = %new_state, "workspace state changed");
                    }
                    HostEvent::Error { message } => {
                        tracing::error!(message = %message, "host error");
                    }
                    HostEvent::Disconnected { reason } => {
                        tracing::warn!(reason = %reason, "disconnected from host");
                        connected = false;
                        status_bar.set_session_status(SessionStatus::Failed {
                            error: reason,
                        });
                        needs_paint = true;
                    }
                }
            }
        }

        // ── Handle resize ────────────────────────────────────────
        if let Some((w, h)) = window::take_resize() {
            if w > 0 && h > 0 {
                let _ = renderer.resize(w, h);
                window_width = w as f32;
                window_height = h as f32;
                tab_strip.layout(window_width);
                status_bar.layout(window_width);

                let content_height = window_height - tab_strip.height() - status_bar.height();
                let content_rows = (content_height / cell_h) as u16;
                let content_cols = (window_width / cell_w) as u16;
                pane_layout.update(
                    &layout_tree,
                    0.0,
                    tab_strip.height(),
                    content_cols,
                    content_rows,
                );

                // Notify host of pane resize (send for each pane).
                if let Some(ref bridge) = bridge {
                    if connected {
                        for pane_id in layout_tree.panes() {
                            if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
                                let pane_cols = (rect.width / cell_w) as u16;
                                let pane_rows = (rect.height / cell_h) as u16;
                                if pane_cols > 0 && pane_rows > 0 {
                                    bridge.send_resize(
                                        format!("{}", pane_id.0),
                                        pane_cols,
                                        pane_rows,
                                    );
                                }
                            }
                        }
                    }
                }

                needs_paint = true;
            }
        }

        // ── Process keyboard events ──────────────────────────────
        for event in window::drain_key_events() {
            // When the command palette is visible, it consumes all keyboard input.
            if command_palette.is_visible() {
                match command_palette.on_key_event(&event) {
                    PaletteResult::Dismissed => {
                        needs_paint = true;
                    }
                    PaletteResult::Action(action_ref) => {
                        let action_name = match &action_ref {
                            wtd_core::workspace::ActionReference::Simple(n) => n.as_str(),
                            wtd_core::workspace::ActionReference::WithArgs { action, .. } => {
                                action.as_str()
                            }
                        };
                        // Handle toggle-command-palette locally (palette already hidden).
                        if action_name == "toggle-command-palette" {
                            // Already dismissed by on_key_event, nothing more to do.
                        } else if let Some(ref bridge) = bridge {
                            if connected {
                                let focused = layout_tree.focus();
                                bridge.send_action(
                                    action_name.to_string(),
                                    Some(format!("{}", focused.0)),
                                    serde_json::Value::Null,
                                );
                            }
                        }
                        needs_paint = true;
                    }
                    PaletteResult::Consumed => {
                        needs_paint = true;
                    }
                }
                continue;
            }

            // Check for Ctrl+Shift+Space to toggle the command palette.
            if event.key == KeyName::Space
                && event.modifiers.ctrl()
                && event.modifiers.shift()
            {
                command_palette.toggle();
                needs_paint = true;
                continue;
            }

            // Normal keyboard handling — send raw bytes to focused session.
            if let Some(ref bridge) = bridge {
                if connected {
                    let bytes = wtd_ui::input::key_event_to_bytes(&event);
                    if !bytes.is_empty() {
                        let focused = layout_tree.focus();
                        if let Some(ps) = pane_sessions.get(&focused) {
                            bridge.send_input(ps.session_id.clone(), bytes);
                        }
                    }
                }
            }
        }

        // ── Process mouse events ─────────────────────────────────
        for event in window::drain_mouse_events() {
            // When the command palette is visible, clicks dismiss or select.
            if command_palette.is_visible() {
                if matches!(event.kind, MouseEventKind::LeftDown) {
                    if let Some(result) =
                        command_palette.on_click(event.x, event.y, window_width, window_height)
                    {
                        if let PaletteResult::Action(ref action_ref) = result {
                            let action_name = match action_ref {
                                wtd_core::workspace::ActionReference::Simple(n) => n.as_str(),
                                wtd_core::workspace::ActionReference::WithArgs {
                                    action, ..
                                } => action.as_str(),
                            };
                            if action_name != "toggle-command-palette" {
                                if let Some(ref bridge) = bridge {
                                    if connected {
                                        let focused = layout_tree.focus();
                                        bridge.send_action(
                                            action_name.to_string(),
                                            Some(format!("{}", focused.0)),
                                            serde_json::Value::Null,
                                        );
                                    }
                                }
                            }
                        }
                        needs_paint = true;
                    }
                }
                continue;
            }

            // Tab strip area.
            if event.y < tab_strip.height() {
                let action = match event.kind {
                    MouseEventKind::LeftDown => tab_strip.on_mouse_down(event.x, event.y),
                    MouseEventKind::LeftUp => tab_strip.on_mouse_up(event.x, event.y),
                    MouseEventKind::Move => tab_strip.on_mouse_move(event.x, event.y),
                    _ => None,
                };

                if let Some(action) = action {
                    match action {
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
                            tab_strip.layout(window_width);
                        }
                        TabAction::Close(_) => {
                            tab_strip.layout(window_width);
                        }
                        TabAction::Reorder { .. } => {
                            tab_strip.layout(window_width);
                        }
                        TabAction::SwitchTo(_) => {}
                    }
                    let win_title = tab_strip.window_title(
                        workspace_name.as_deref().unwrap_or("WinTermDriver"),
                    );
                    window::set_window_title(hwnd, &win_title);
                }
            } else if event.y < window_height - status_bar.height() {
                // Pane layout area.
                let action = match event.kind {
                    MouseEventKind::LeftDown => pane_layout.on_mouse_down(event.x, event.y),
                    MouseEventKind::LeftUp => pane_layout.on_mouse_up(event.x, event.y),
                    MouseEventKind::Move => pane_layout.on_mouse_move(event.x, event.y),
                    _ => None,
                };

                if let Some(action) = action {
                    match action {
                        PaneLayoutAction::FocusPane(pane_id) => {
                            let _ = layout_tree.set_focus(pane_id.clone());
                            // Update status bar pane path.
                            status_bar.set_pane_path(format!("{}", pane_id.0));
                        }
                        PaneLayoutAction::Resize {
                            pane_id,
                            direction,
                            cells,
                        } => {
                            let content_height =
                                window_height - tab_strip.height() - status_bar.height();
                            let content_rows = (content_height / cell_h) as u16;
                            let content_cols = (window_width / cell_w) as u16;
                            let total =
                                wtd_core::layout::Rect::new(0, 0, content_cols, content_rows);
                            let _ = layout_tree.resize_pane(pane_id, direction, cells, total);
                            pane_layout.update(
                                &layout_tree,
                                0.0,
                                tab_strip.height(),
                                content_cols,
                                content_rows,
                            );
                        }
                    }
                }
            }

            needs_paint = true;
        }

        if window::take_needs_paint() {
            needs_paint = true;
        }

        if needs_paint {
            paint_all(
                &renderer,
                &tab_strip,
                &pane_layout,
                &layout_tree,
                &screens,
                &status_bar,
                &command_palette,
                window_width,
                window_height,
            )?;
        }

        // Sleep briefly to avoid busy-looping.
        std::thread::sleep(std::time::Duration::from_millis(16));

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
) -> anyhow::Result<()> {
    renderer.begin_draw();
    renderer.clear_background();

    // Tab strip.
    let tab_result = tab_strip.paint(renderer.render_target());

    // Pane content: render each pane's screen buffer clipped to its viewport.
    for pane_id in layout_tree.panes() {
        if let (Some(rect), Some(screen)) = (
            pane_layout.pane_pixel_rect(&pane_id),
            screens.get(&pane_id),
        ) {
            renderer
                .paint_pane_viewport(screen, rect.x, rect.y, rect.width, rect.height, None)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }

    // Pane borders, splitters, and focus indicator.
    let focused = layout_tree.focus();
    let layout_result = pane_layout.paint(renderer.render_target(), &focused);

    // Status bar at the bottom.
    let status_result =
        status_bar.paint(renderer.render_target(), window_height - status_bar.height());

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
fn find_pane_for_session(
    pane_sessions: &HashMap<PaneId, PaneSession>,
    session_id: &str,
) -> Option<PaneId> {
    pane_sessions
        .iter()
        .find(|(_, ps)| ps.session_id == session_id)
        .map(|(id, _)| id.clone())
}

/// Feed VT content that identifies the pane (demo mode only).
fn feed_demo_content(screen: &mut ScreenBuffer, pane_id: &PaneId) {
    let mut vt = Vec::new();

    // Pane header.
    vt.extend_from_slice(
        format!("\x1b[1;44;37m Pane {} \x1b[0m\r\n", pane_id).as_bytes(),
    );
    vt.extend_from_slice(b"\r\n");

    // Colored content per pane.
    match pane_id.0 {
        1 => {
            vt.extend_from_slice(
                b"  \x1b[32muser@host\x1b[0m:\x1b[34m~/projects\x1b[0m$ ls -la\r\n",
            );
            vt.extend_from_slice(
                b"  drwxr-xr-x  2 user user 4096 Mar 28 \x1b[1;34msrc\x1b[0m\r\n",
            );
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
