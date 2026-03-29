//! `wtd-ui` — WinTermDriver UI process.
//!
//! Demonstrates the tab strip and terminal rendering pipeline: creates a Win32
//! window with a tab strip at the top, feeds VT sequences into a ScreenBuffer,
//! and renders everything using Direct2D + DirectWrite.

use wtd_pty::ScreenBuffer;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::tab_strip::{TabAction, TabStrip};
use wtd_ui::window::{self, MouseEventKind};

fn main() {
    eprintln!("wtd-ui: tab strip + rendering prototype");

    if let Err(e) = run() {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cols: u16 = 80;
    let rows: u16 = 24;

    // Create a screen buffer and feed VT content into it.
    let mut screen = ScreenBuffer::new(cols, rows, 1000);
    feed_demo_content(&mut screen);

    // Create window.
    let hwnd = window::create_terminal_window("WinTermDriver", 1000, 600)?;

    // Create the renderer.
    let config = RendererConfig::default();
    let mut renderer = TerminalRenderer::new(hwnd, &config)?;

    let (cell_w, cell_h) = renderer.cell_size();
    eprintln!(
        "cell size: {:.1}x{:.1}px, grid: {}x{}",
        cell_w, cell_h, cols, rows
    );

    // Create the tab strip with demo tabs.
    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    tab_strip.add_tab("main".to_string());
    tab_strip.add_tab("build".to_string());
    tab_strip.add_tab("logs".to_string());
    tab_strip.set_active(0);

    let mut window_width: f32 = 1000.0;
    tab_strip.layout(window_width);

    // Set initial window title.
    let title = tab_strip.window_title("WinTermDriver");
    window::set_window_title(hwnd, &title);

    // Initial paint.
    paint_all(&renderer, &tab_strip, &screen)?;

    // Message loop with repaint on WM_PAINT / WM_SIZE / mouse events.
    loop {
        window::pump_pending_messages();

        let mut needs_paint = false;

        // Handle resize.
        if let Some((w, h)) = window::take_resize() {
            if w > 0 && h > 0 {
                let _ = renderer.resize(w, h);
                window_width = w as f32;
                tab_strip.layout(window_width);
                needs_paint = true;
            }
        }

        // Process mouse events.
        for event in window::drain_mouse_events() {
            let action = match event.kind {
                MouseEventKind::Down => tab_strip.on_mouse_down(event.x, event.y),
                MouseEventKind::Up => tab_strip.on_mouse_up(event.x, event.y),
                MouseEventKind::Move => tab_strip.on_mouse_move(event.x, event.y),
            };

            if let Some(action) = action {
                match action {
                    TabAction::WindowClose => {
                        unsafe {
                            let _ =
                                windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
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

            // Any mouse activity may change hover state, so repaint.
            needs_paint = true;
        }

        if window::take_needs_paint() {
            needs_paint = true;
        }

        if needs_paint {
            paint_all(&renderer, &tab_strip, &screen)?;
        }

        // Sleep briefly to avoid busy-looping (prototype only — a real UI
        // would use MsgWaitForMultipleObjects or similar).
        std::thread::sleep(std::time::Duration::from_millis(16));

        // Check if the window was closed.
        if !is_window_valid(hwnd) {
            break;
        }
    }

    Ok(())
}

fn paint_all(
    renderer: &TerminalRenderer,
    tab_strip: &TabStrip,
    screen: &ScreenBuffer,
) -> anyhow::Result<()> {
    renderer.begin_draw();
    renderer.clear_background();
    let tab_result = tab_strip.paint(renderer.render_target());
    let screen_result = renderer.paint_screen(screen, tab_strip.height());
    let end_result = renderer.end_draw();
    tab_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    screen_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    end_result.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn is_window_valid(hwnd: windows::Win32::Foundation::HWND) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() }
}

/// Feed a variety of VT sequences to demonstrate rendering capabilities.
fn feed_demo_content(screen: &mut ScreenBuffer) {
    let mut vt = Vec::new();

    // Line 1: Title — bold white on blue
    vt.extend_from_slice(b"\x1b[1;44;37m WinTermDriver Rendering Prototype \x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");

    // Line 3: Standard ANSI colors (foreground)
    vt.extend_from_slice(b"  Standard colors: ");
    for i in 0..8u8 {
        let code = 30 + i;
        vt.extend_from_slice(format!("\x1b[{code}m\u{2588}\u{2588}").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");

    // Line 4: Bright ANSI colors (foreground)
    vt.extend_from_slice(b"  Bright colors:   ");
    for i in 0..8u8 {
        let code = 90 + i;
        vt.extend_from_slice(format!("\x1b[{code}m\u{2588}\u{2588}").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");

    // Line 5: Background colors
    vt.extend_from_slice(b"  Backgrounds:     ");
    for i in 0..8u8 {
        let code = 40 + i;
        vt.extend_from_slice(format!("\x1b[{code}m  ").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");

    // Line 6: Bright backgrounds
    vt.extend_from_slice(b"  Bright BG:       ");
    for i in 0..8u8 {
        let code = 100 + i;
        vt.extend_from_slice(format!("\x1b[{code}m  ").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");

    // Line 8: Text attributes
    vt.extend_from_slice(b"  \x1b[1mBold\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[2mDim\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[3mItalic\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[4mUnderline\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[7mInverse\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[9mStrikethrough\x1b[0m\r\n");

    // Line 9: Combined attributes
    vt.extend_from_slice(b"  \x1b[1;3mBold+Italic\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[1;4mBold+Underline\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[1;3;4mBold+Italic+UL\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");

    // Line 11: 256-color samples
    vt.extend_from_slice(b"  256-color: ");
    for i in (16..52u8).step_by(1) {
        vt.extend_from_slice(format!("\x1b[38;5;{i}m\u{2588}").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");

    // Line 12: RGB truecolor gradient
    vt.extend_from_slice(b"  Truecolor: ");
    for i in (0..=255u16).step_by(8) {
        let r = i.min(255) as u8;
        let g = 0u8;
        let b = (255 - i.min(255)) as u8;
        vt.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m\u{2588}").as_bytes());
    }
    vt.extend_from_slice(b"\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");

    // Line 14: Colored text samples
    vt.extend_from_slice(b"  \x1b[31mRed text\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[32mGreen text\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[33mYellow text\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[34mBlue text\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[35mMagenta text\x1b[0m\r\n");

    // Line 15: Bold colored text
    vt.extend_from_slice(b"  \x1b[1;31mBold Red\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[1;32mBold Green\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[1;33mBold Yellow\x1b[0m  ");
    vt.extend_from_slice(b"\x1b[1;34mBold Blue\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");

    // Line 17: Prompt-like output
    vt.extend_from_slice(b"  \x1b[32muser@host\x1b[0m:\x1b[34m~/projects\x1b[0m$ ls -la\r\n");
    vt.extend_from_slice(
        b"  drwxr-xr-x  2 user user 4096 Mar 28 10:00 \x1b[1;34msrc\x1b[0m\r\n",
    );
    vt.extend_from_slice(
        b"  -rw-r--r--  1 user user  256 Mar 28 10:00 \x1b[0mCargo.toml\x1b[0m\r\n",
    );
    vt.extend_from_slice(
        b"  -rwxr-xr-x  1 user user 8192 Mar 28 10:00 \x1b[1;32mtarget\x1b[0m\r\n",
    );

    screen.advance(&vt);
}
