//! `wtd-ui` — WinTermDriver UI process.
//!
//! Demonstrates the tab strip, pane layout, and terminal rendering pipeline:
//! creates a Win32 window with a tab strip at the top, a multi-pane split
//! layout, and renders everything using Direct2D + DirectWrite.

use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_pty::ScreenBuffer;
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction};
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::tab_strip::{TabAction, TabStrip};
use wtd_ui::window::{self, MouseEventKind};

fn main() {
    eprintln!("wtd-ui: tab strip + pane layout + rendering prototype");

    if let Err(e) = run() {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cols: u16 = 80;
    let rows: u16 = 24;

    // Create a layout tree with a multi-pane split.
    let mut layout_tree = LayoutTree::new();
    let p1 = layout_tree.focus();
    let _p2 = layout_tree.split_right(p1.clone()).unwrap();
    let _p3 = layout_tree.split_down(p1.clone()).unwrap();

    // Create screen buffers for each pane.
    let mut screens: std::collections::HashMap<PaneId, ScreenBuffer> =
        std::collections::HashMap::new();
    for pane_id in layout_tree.panes() {
        let mut screen = ScreenBuffer::new(cols, rows, 1000);
        feed_pane_content(&mut screen, &pane_id);
        screens.insert(pane_id, screen);
    }

    // Create window.
    let hwnd = window::create_terminal_window("WinTermDriver", 1000, 600)?;

    // Create the renderer.
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config)?;

    let (cell_w, cell_h) = renderer.cell_size();
    eprintln!(
        "cell size: {:.1}x{:.1}px, grid: {}x{}, panes: {}",
        cell_w,
        cell_h,
        cols,
        rows,
        layout_tree.pane_count()
    );

    // Create the tab strip with demo tabs.
    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    tab_strip.add_tab("main".to_string());
    tab_strip.add_tab("build".to_string());
    tab_strip.add_tab("logs".to_string());
    tab_strip.set_active(0);

    let mut window_width: f32 = 1000.0;
    let mut window_height: f32 = 600.0;
    tab_strip.layout(window_width);

    // Create the pane layout manager.
    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    let content_rows = ((window_height - tab_strip.height()) / cell_h) as u16;
    let content_cols = (window_width / cell_w) as u16;
    pane_layout.update(
        &layout_tree,
        0.0,
        tab_strip.height(),
        content_cols,
        content_rows,
    );

    // Set initial window title.
    let title = tab_strip.window_title("WinTermDriver");
    window::set_window_title(hwnd, &title);

    // Initial paint.
    paint_all(&renderer, &tab_strip, &pane_layout, &layout_tree, &screens)?;

    // Message loop with repaint on WM_PAINT / WM_SIZE / mouse events.
    loop {
        window::pump_pending_messages();

        let mut needs_paint = false;

        // Handle resize.
        if let Some((w, h)) = window::take_resize() {
            if w > 0 && h > 0 {
                let _ = renderer.resize(w, h);
                window_width = w as f32;
                window_height = h as f32;
                tab_strip.layout(window_width);

                let content_rows = ((window_height - tab_strip.height()) / cell_h) as u16;
                let content_cols = (window_width / cell_w) as u16;
                pane_layout.update(
                    &layout_tree,
                    0.0,
                    tab_strip.height(),
                    content_cols,
                    content_rows,
                );
                needs_paint = true;
            }
        }

        // Process mouse events.
        for event in window::drain_mouse_events() {
            // First try the tab strip if event is in the tab strip area.
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
                    let title = tab_strip.window_title("WinTermDriver");
                    window::set_window_title(hwnd, &title);
                }
            } else {
                // Pane layout area — handle splitter drag and pane focus.
                let action = match event.kind {
                    MouseEventKind::LeftDown => pane_layout.on_mouse_down(event.x, event.y),
                    MouseEventKind::LeftUp => pane_layout.on_mouse_up(event.x, event.y),
                    MouseEventKind::Move => pane_layout.on_mouse_move(event.x, event.y),
                    _ => None,
                };

                if let Some(action) = action {
                    match action {
                        PaneLayoutAction::FocusPane(pane_id) => {
                            let _ = layout_tree.set_focus(pane_id);
                        }
                        PaneLayoutAction::Resize {
                            pane_id,
                            direction,
                            cells,
                        } => {
                            let content_rows =
                                ((window_height - tab_strip.height()) / cell_h) as u16;
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
            paint_all(&renderer, &tab_strip, &pane_layout, &layout_tree, &screens)?;
        }

        // Sleep briefly to avoid busy-looping (prototype only).
        std::thread::sleep(std::time::Duration::from_millis(16));

        if !is_window_valid(hwnd) {
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
    screens: &std::collections::HashMap<PaneId, ScreenBuffer>,
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

    let end_result = renderer.end_draw();
    tab_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    layout_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    end_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn is_window_valid(hwnd: windows::Win32::Foundation::HWND) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() }
}

/// Feed VT content that identifies the pane.
fn feed_pane_content(screen: &mut ScreenBuffer, pane_id: &PaneId) {
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
