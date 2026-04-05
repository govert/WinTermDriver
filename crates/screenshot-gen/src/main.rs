//! Generate documentation screenshots for the WinTermDriver README, or render
//! a live workspace snapshot to PNG by attaching to `wtd-host`.

use std::collections::HashMap;
use std::mem::size_of;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use image::{ImageBuffer, RgbImage};

use wtd_core::global_settings::default_bindings;
use wtd_core::ids::PaneId;
use wtd_core::layout::LayoutTree;
use wtd_ipc::message::{AttachWorkspace, AttachWorkspaceResult, ErrorResponse, MessagePayload};
use wtd_ipc::Envelope;
use wtd_pty::ScreenBuffer;
use wtd_ui::command_palette::CommandPalette;
use wtd_ui::host_client::UiIpcClient;
use wtd_ui::input::{KeyEvent, KeyName, Modifiers};
use wtd_ui::pane_layout::PaneLayout;
use wtd_ui::renderer::{RendererConfig, TerminalRenderer};
use wtd_ui::snapshot::rebuild_from_snapshot;
use wtd_ui::status_bar::{SessionStatus, StatusBar};
use wtd_ui::tab_strip::TabStrip;
use wtd_ui::window;

use windows::core::Interface;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const DOC_WINDOW_WIDTH: i32 = 1200;
const DOC_WINDOW_HEIGHT: i32 = 700;
const LIVE_WINDOW_WIDTH: i32 = 1600;
const LIVE_WINDOW_HEIGHT: i32 = 1000;

fn main() -> anyhow::Result<()> {
    match parse_args()? {
        CaptureMode::Docs { output_dir } => run_docs_capture(&output_dir),
        CaptureMode::Workspace {
            workspace,
            output,
            width,
            height,
        } => run_workspace_capture(&workspace, &output, width, height),
    }
}

enum CaptureMode {
    Docs {
        output_dir: PathBuf,
    },
    Workspace {
        workspace: String,
        output: PathBuf,
        width: i32,
        height: i32,
    },
}

fn parse_args() -> anyhow::Result<CaptureMode> {
    let mut workspace = None;
    let mut output = None;
    let mut output_dir = None;
    let mut width = LIVE_WINDOW_WIDTH;
    let mut height = LIVE_WINDOW_HEIGHT;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => workspace = Some(next_arg(&mut args, "--workspace")?),
            "--output" => output = Some(PathBuf::from(next_arg(&mut args, "--output")?)),
            "--output-dir" => {
                output_dir = Some(PathBuf::from(next_arg(&mut args, "--output-dir")?))
            }
            "--width" => {
                width = next_arg(&mut args, "--width")?
                    .parse()
                    .context("invalid --width")?
            }
            "--height" => {
                height = next_arg(&mut args, "--height")?
                    .parse()
                    .context("invalid --height")?
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unrecognized argument: {other}"),
        }
    }

    if let Some(workspace) = workspace {
        let output =
            output.unwrap_or_else(|| PathBuf::from(format!("docs/images/{workspace}-live.png")));
        return Ok(CaptureMode::Workspace {
            workspace,
            output,
            width,
            height,
        });
    }

    Ok(CaptureMode::Docs {
        output_dir: output_dir.unwrap_or_else(|| PathBuf::from("docs/images")),
    })
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("missing value for {flag}"))
}

fn print_help() {
    println!(
        "screenshot-gen [--output-dir <dir>]\n\
         screenshot-gen --workspace <name> [--output <png>] [--width <px>] [--height <px>]"
    );
}

fn run_docs_capture(output_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(output_dir)?;

    let hwnd =
        window::create_terminal_window("dev — WinTermDriver", DOC_WINDOW_WIDTH, DOC_WINDOW_HEIGHT)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    window::pump_pending_messages();

    let (client_w, client_h) = client_size(hwnd)?;
    let w = client_w as f32;
    let h = client_h as f32;

    let config = RendererConfig {
        software_rendering: true,
        ..RendererConfig::default()
    };
    let renderer = TerminalRenderer::new(hwnd, &config)?;
    let (cell_w, cell_h) = renderer.cell_size();

    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    tab_strip.add_tab("backend".into());
    tab_strip.add_tab("ops".into());
    tab_strip.add_tab("logs".into());
    tab_strip.set_active(0);
    tab_strip.layout(w);

    let mut status_bar = StatusBar::new(renderer.dw_factory())?;
    status_bar.set_workspace_name("dev".into());
    status_bar.set_pane_path("backend/editor".into());
    status_bar.set_session_status(SessionStatus::Running);
    status_bar.layout(w);

    let bindings = default_bindings();
    let mut palette = CommandPalette::new(renderer.dw_factory(), &bindings)?;

    let mut tree = LayoutTree::new();
    let p1 = tree.focus();
    let p2 = tree.split_right(p1.clone()).context("split_right")?;
    let p3 = tree.split_down(p2.clone()).context("split_down")?;

    let content_height = h - tab_strip.height() - status_bar.height();
    let content_rows = (content_height / cell_h).max(1.0) as u16;
    let content_cols = (w / cell_w).max(1.0) as u16;

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(&tree, 0.0, tab_strip.height(), content_cols, content_rows);

    let mut screens: HashMap<PaneId, ScreenBuffer> = HashMap::new();
    for pane_id in tree.panes() {
        let mut screen = ScreenBuffer::new(content_cols, content_rows, 1000);
        feed_pane_content(&mut screen, &pane_id);
        screens.insert(pane_id, screen);
    }

    println!("Generating workspace-overview.png ...");
    render_and_capture(
        &renderer,
        client_w,
        client_h,
        &output_dir.join("workspace-overview.png"),
        |rt| {
            paint_scene(
                &renderer,
                rt,
                &tab_strip,
                &pane_layout,
                &tree,
                &screens,
                &status_bar,
                &palette,
                w,
                h,
                None,
            )
        },
    )?;

    println!("Generating command-palette.png ...");
    palette.show();
    inject_text(&mut palette, "split");
    render_and_capture(
        &renderer,
        client_w,
        client_h,
        &output_dir.join("command-palette.png"),
        |rt| {
            paint_scene(
                &renderer,
                rt,
                &tab_strip,
                &pane_layout,
                &tree,
                &screens,
                &status_bar,
                &palette,
                w,
                h,
                None,
            )
        },
    )?;
    palette.hide();

    println!("Generating prefix-chord.png ...");
    status_bar.set_prefix_active(true);
    status_bar.set_prefix_label("Ctrl+B".into());
    render_and_capture(
        &renderer,
        client_w,
        client_h,
        &output_dir.join("prefix-chord.png"),
        |rt| {
            paint_scene(
                &renderer,
                rt,
                &tab_strip,
                &pane_layout,
                &tree,
                &screens,
                &status_bar,
                &palette,
                w,
                h,
                None,
            )
        },
    )?;
    status_bar.set_prefix_active(false);

    println!("Generating failed-pane.png ...");
    status_bar.set_session_status(SessionStatus::Failed {
        error: "executable not found".into(),
    });
    status_bar.set_pane_path("ops/deploy".into());
    render_and_capture(
        &renderer,
        client_w,
        client_h,
        &output_dir.join("failed-pane.png"),
        |rt| {
            paint_scene(
                &renderer,
                rt,
                &tab_strip,
                &pane_layout,
                &tree,
                &screens,
                &status_bar,
                &palette,
                w,
                h,
                Some(&p3),
            )
        },
    )?;

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    window::pump_pending_messages();

    println!("All screenshots saved to {}", output_dir.display());
    Ok(())
}

fn run_workspace_capture(
    workspace: &str,
    output: &Path,
    width: i32,
    height: i32,
) -> anyhow::Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let attached = attach_workspace_state(workspace)?;

    let hwnd =
        window::create_terminal_window(&format!("{workspace} — WinTermDriver"), width, height)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    window::pump_pending_messages();

    let (client_w, client_h) = client_size(hwnd)?;
    let w = client_w as f32;
    let h = client_h as f32;

    let config = RendererConfig {
        software_rendering: true,
        ..RendererConfig::default()
    };
    let renderer = TerminalRenderer::new(hwnd, &config)?;
    let (cell_w, cell_h) = renderer.cell_size();

    let tab_names = attached.state["tabs"]
        .as_array()
        .map(|tabs| {
            tabs.iter()
                .map(|tab| tab["name"].as_str().unwrap_or("tab").to_string())
                .collect::<Vec<_>>()
        })
        .filter(|tabs| !tabs.is_empty())
        .unwrap_or_else(|| vec!["main".to_string()]);

    let mut tab_strip = TabStrip::new(renderer.dw_factory())?;
    for tab_name in &tab_names {
        tab_strip.add_tab(tab_name.clone());
    }
    tab_strip.set_active(0);
    tab_strip.layout(w);

    let mut status_bar = StatusBar::new(renderer.dw_factory())?;

    let bindings = default_bindings();
    let palette = CommandPalette::new(renderer.dw_factory(), &bindings)?;

    let content_height = h - tab_strip.height() - status_bar.height();
    let content_rows = (content_height / cell_h).max(1.0) as u16;
    let content_cols = (w / cell_w).max(1.0) as u16;

    let rebuilt = rebuild_from_snapshot(&attached.state, content_cols, content_rows)
        .ok_or_else(|| anyhow!("failed to rebuild layout from attach snapshot"))?;

    let active_tab = rebuilt
        .tabs
        .get(rebuilt.active_tab_index)
        .ok_or_else(|| anyhow!("attach snapshot did not contain the active tab"))?;

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    pane_layout.update(
        &active_tab.layout_tree,
        0.0,
        tab_strip.height(),
        content_cols,
        content_rows,
    );

    status_bar.set_workspace_name(rebuilt.workspace_name.clone());
    if let Some(focused) = active_tab
        .pane_sessions
        .get(&active_tab.layout_tree.focus())
    {
        status_bar.set_pane_path(focused.pane_path.clone());
    } else {
        status_bar.set_pane_path(format!(
            "{}/{}",
            rebuilt.workspace_name, rebuilt.tab_names[0]
        ));
    }
    status_bar.set_session_status(SessionStatus::Running);
    status_bar.layout(w);
    tab_strip.set_active(rebuilt.active_tab_index);
    tab_strip.layout(w);

    render_and_capture(&renderer, client_w, client_h, output, |rt| {
        paint_scene(
            &renderer,
            rt,
            &tab_strip,
            &pane_layout,
            &active_tab.layout_tree,
            &active_tab.screens,
            &status_bar,
            &palette,
            w,
            h,
            None,
        )
    })?;

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    window::pump_pending_messages();

    println!("Saved live workspace screenshot to {}", output.display());
    Ok(())
}

fn attach_workspace_state(workspace: &str) -> anyhow::Result<AttachWorkspaceResult> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async move {
        let mut client = UiIpcClient::connect_and_handshake()
            .await
            .context("failed to connect to wtd-host")?;
        let request = Envelope::new(
            "screenshot-attach-1",
            &AttachWorkspace {
                workspace: workspace.to_string(),
            },
        );
        let response = client
            .request(&request)
            .await
            .context("attach request failed")?;

        if response.msg_type == AttachWorkspaceResult::TYPE_NAME {
            return response
                .extract_payload()
                .context("failed to decode AttachWorkspaceResult");
        }

        if response.msg_type == ErrorResponse::TYPE_NAME {
            let err: ErrorResponse = response
                .extract_payload()
                .context("failed to decode ErrorResponse")?;
            bail!(
                "host rejected attach for workspace `{workspace}`: {}",
                err.message
            );
        }

        bail!(
            "expected {} or {} while attaching workspace `{}`, but got {}",
            AttachWorkspaceResult::TYPE_NAME,
            ErrorResponse::TYPE_NAME,
            workspace,
            response.msg_type
        )
    })
}

fn render_and_capture(
    renderer: &TerminalRenderer,
    width: i32,
    height: i32,
    path: &Path,
    paint_fn: impl FnOnce(&ID2D1RenderTarget) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    renderer.begin_draw();
    renderer.clear_background();

    paint_fn(renderer.render_target())?;

    let pixels = capture_via_gdi_interop(renderer.render_target(), width, height)?;

    renderer.end_draw()?;

    save_bgr_as_png(&pixels, width as u32, height as u32, path)?;
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!(
        "  Saved {} ({} bytes, {}x{})",
        path.display(),
        file_size,
        width,
        height
    );
    Ok(())
}

fn capture_via_gdi_interop(
    rt: &ID2D1RenderTarget,
    width: i32,
    height: i32,
) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let gdi_rt: ID2D1GdiInteropRenderTarget = rt.cast().context(
            "Failed to cast to ID2D1GdiInteropRenderTarget. \
             Ensure software_rendering is enabled.",
        )?;

        let hdc_rt = gdi_rt
            .GetDC(D2D1_DC_INITIALIZE_MODE_COPY)
            .context("GetDC from D2D render target")?;

        let hdc_mem = CreateCompatibleDC(hdc_rt);
        let hbm = CreateCompatibleBitmap(hdc_rt, width, height);
        let old = SelectObject(hdc_mem, hbm);

        BitBlt(hdc_mem, 0, 0, width, height, hdc_rt, 0, 0, SRCCOPY)
            .context("BitBlt from D2D DC")?;

        SelectObject(hdc_mem, old);

        gdi_rt
            .ReleaseDC(None)
            .context("ReleaseDC on D2D render target")?;

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        GetDIBits(
            hdc_mem,
            hbm,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);

        Ok(pixels)
    }
}

fn paint_scene(
    renderer: &TerminalRenderer,
    _rt: &ID2D1RenderTarget,
    tab_strip: &TabStrip,
    pane_layout: &PaneLayout,
    tree: &LayoutTree,
    screens: &HashMap<PaneId, ScreenBuffer>,
    status_bar: &StatusBar,
    palette: &CommandPalette,
    w: f32,
    h: f32,
    failed_pane: Option<&PaneId>,
) -> anyhow::Result<()> {
    let _ = tab_strip.paint(renderer.render_target());

    for pane_id in tree.panes() {
        if let Some(rect) = pane_layout.pane_pixel_rect(&pane_id) {
            if failed_pane.map_or(false, |fp| &pane_id == fp) {
                let msg = wtd_ui::renderer::failed_pane_message("executable not found: deploy.sh");
                renderer.paint_failed_pane(&msg, rect.x, rect.y, rect.width, rect.height)?;
            } else if let Some(screen) = screens.get(&pane_id) {
                renderer.paint_pane_viewport(
                    screen,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    None,
                )?;
            }
        }
    }

    let focused = tree.focus();
    let _ = pane_layout.paint(renderer.render_target(), &focused);
    let _ = status_bar.paint(renderer.render_target(), h - status_bar.height());
    let _ = palette.paint(renderer.render_target(), w, h);

    Ok(())
}

fn client_size(hwnd: HWND) -> anyhow::Result<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect)? };
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

fn save_bgr_as_png(pixels: &[u8], width: u32, height: u32, path: &Path) -> anyhow::Result<()> {
    let img: RgbImage = ImageBuffer::from_fn(width, height, |x, y| {
        let idx = ((y * width + x) * 4) as usize;
        image::Rgb([pixels[idx + 2], pixels[idx + 1], pixels[idx]])
    });
    img.save(path).context("failed to save PNG")?;
    Ok(())
}

fn inject_text(palette: &mut CommandPalette, text: &str) {
    for ch in text.chars() {
        let key = if ch.is_ascii_alphabetic() {
            KeyName::Char(ch.to_ascii_uppercase())
        } else {
            KeyName::Space
        };
        let event = KeyEvent {
            key,
            modifiers: Modifiers::NONE,
            character: Some(ch),
        };
        let _ = palette.on_key_event(&event);
    }
}

fn feed_pane_content(screen: &mut ScreenBuffer, pane_id: &PaneId) {
    let vt: Vec<u8> = match pane_id.0 {
        1 => build_editor_pane_content(),
        2 => build_server_pane_content(),
        _ => build_test_pane_content(),
    };
    screen.advance(&vt);
}

fn build_editor_pane_content() -> Vec<u8> {
    let mut vt = Vec::new();
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m git status\r\n");
    vt.extend_from_slice(b"On branch \x1b[32mmain\x1b[0m\r\n");
    vt.extend_from_slice(b"Your branch is up to date with '\x1b[31morigin/main\x1b[0m'.\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"Changes not staged for commit:\r\n");
    vt.extend_from_slice(b"  (use \"git add <file>...\" to update)\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"        \x1b[31mmodified:   src/lib.rs\x1b[0m\r\n");
    vt.extend_from_slice(b"        \x1b[31mmodified:   src/main.rs\x1b[0m\r\n");
    vt.extend_from_slice(b"        \x1b[31mmodified:   Cargo.toml\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"Untracked files:\r\n");
    vt.extend_from_slice(b"  (use \"git add <file>...\" to include)\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"        \x1b[31m.wtd/dev.yaml\x1b[0m\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"no changes added to commit\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m ls\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"    Directory: C:\\src\\app\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"Mode         LastWriteTime    Length Name\r\n");
    vt.extend_from_slice(b"----         -------------    ------ ----\r\n");
    vt.extend_from_slice(b"d----   3/29/2026  10:15          \x1b[1;34msrc\x1b[0m\r\n");
    vt.extend_from_slice(b"d----   3/29/2026  10:15          \x1b[1;34m.wtd\x1b[0m\r\n");
    vt.extend_from_slice(b"-a---   3/29/2026  10:15    1256 Cargo.toml\r\n");
    vt.extend_from_slice(b"-a---   3/29/2026  10:15     384 README.md\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m ");
    vt
}

fn build_server_pane_content() -> Vec<u8> {
    let mut vt = Vec::new();
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m cargo build\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-core v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-ipc v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-pty v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-host v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-ui v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-cli v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m    Finished\x1b[0m `dev` target(s) in 8.34s\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m cargo run --bin wtd-host\r\n");
    vt.extend_from_slice(b"\x1b[32m    Finished\x1b[0m `dev` target(s) in 0.12s\r\n");
    vt.extend_from_slice(b"\x1b[32m     Running\x1b[0m `target\\debug\\wtd-host.exe`\r\n");
    vt.extend_from_slice(
        b"\x1b[34m2026-03-29T10:15:32Z\x1b[0m \x1b[32m INFO\x1b[0m wtd_host: Host started\r\n",
    );
    vt.extend_from_slice(
        b"\x1b[34m2026-03-29T10:15:32Z\x1b[0m \x1b[32m INFO\x1b[0m wtd_host: Listening\r\n",
    );
    vt.extend_from_slice(
        b"\x1b[34m2026-03-29T10:15:33Z\x1b[0m \x1b[32m INFO\x1b[0m wtd_host: Client connected\r\n",
    );
    vt.extend_from_slice(
        b"\x1b[34m2026-03-29T10:15:34Z\x1b[0m \x1b[32m INFO\x1b[0m wtd_host: Workspace opened\r\n",
    );
    vt
}

fn build_test_pane_content() -> Vec<u8> {
    let mut vt = Vec::new();
    vt.extend_from_slice(b"\x1b[36mPS C:\\src\\app>\x1b[0m cargo test\r\n");
    vt.extend_from_slice(b"\x1b[32m   Compiling\x1b[0m wtd-core v0.1.0\r\n");
    vt.extend_from_slice(b"\x1b[32m    Finished\x1b[0m `test` target(s) in 4.21s\r\n");
    vt.extend_from_slice(b"\x1b[32m     Running\x1b[0m unittests src/lib.rs\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"running 47 tests\r\n");
    vt.extend_from_slice(b"test core::workspace::test_parse_minimal ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test core::workspace::test_parse_full ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test core::layout::test_split_right ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test core::layout::test_split_down ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test core::layout::test_close_pane ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test pty::screen::test_advance_basic ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test pty::screen::test_cursor_movement ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test pty::screen::test_ansi_colors ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test ipc::framing::test_encode_decode ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"test host::session::test_start_stop ... \x1b[32mok\x1b[0m\r\n");
    vt.extend_from_slice(b"...\r\n");
    vt.extend_from_slice(b"\r\n");
    vt.extend_from_slice(b"test result: \x1b[32mok\x1b[0m. 47 passed; 0 failed; 0 ignored\r\n");
    vt
}
