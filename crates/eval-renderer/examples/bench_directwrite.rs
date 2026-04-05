//! Win32 + DirectWrite terminal rendering benchmark.
//!
//! Creates a window, sets up Direct2D + DirectWrite, and renders a terminal-like
//! character grid (120×40). Measures startup time, per-frame rendering in three
//! modes (per-row uniform, per-cell uniform, per-cell colored), and memory usage.

use std::mem;
use std::time::Instant;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::*;

const COLS: usize = 120;
const ROWS: usize = 40;
const FONT_SIZE: f32 = 14.0;
const NUM_FRAMES: u32 = 500;
const WARMUP: u32 = 50;

// Standard 16-color ANSI palette as (r, g, b) floats
const ANSI_COLORS: [(f32, f32, f32); 16] = [
    (0.0, 0.0, 0.0),
    (0.67, 0.0, 0.0),
    (0.0, 0.67, 0.0),
    (0.67, 0.33, 0.0),
    (0.0, 0.0, 0.67),
    (0.67, 0.0, 0.67),
    (0.0, 0.67, 0.67),
    (0.75, 0.75, 0.75),
    (0.33, 0.33, 0.33),
    (1.0, 0.33, 0.33),
    (0.33, 1.0, 0.33),
    (1.0, 1.0, 0.33),
    (0.33, 0.33, 1.0),
    (1.0, 0.33, 1.0),
    (0.33, 1.0, 1.0),
    (1.0, 1.0, 1.0),
];

const BG_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.1,
    g: 0.1,
    b: 0.15,
    a: 1.0,
};
const FG_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.9,
    g: 0.9,
    b: 0.9,
    a: 1.0,
};

struct Bench {
    rt: ID2D1RenderTarget,
    tf: IDWriteTextFormat,
    fg: ID2D1SolidColorBrush,
    palette: Vec<ID2D1SolidColorBrush>,
    cell_w: f32,
    cell_h: f32,
}

fn main() -> Result<()> {
    println!("=== Win32 + DirectWrite Terminal Rendering Benchmark ===");
    println!(
        "Grid: {}x{} | Font: {}pt | Frames: {}\n",
        COLS, ROWS, FONT_SIZE, NUM_FRAMES
    );

    // --- Phase 1: Startup ---
    let t_total = Instant::now();

    let t0 = Instant::now();
    let hwnd = create_window()?;
    let window_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let d2d: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
    let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let factory_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let t2 = Instant::now();
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect)?;
    }
    let size = D2D_SIZE_U {
        width: (rect.right - rect.left) as u32,
        height: (rect.bottom - rect.top) as u32,
    };

    let rt_props = D2D1_RENDER_TARGET_PROPERTIES::default();
    let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
        hwnd,
        pixelSize: size,
        presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
    };
    let hwnd_rt = unsafe { d2d.CreateHwndRenderTarget(&rt_props, &hwnd_props)? };
    let rt: ID2D1RenderTarget = hwnd_rt.cast()?;

    let tf = unsafe {
        dw.CreateTextFormat(
            w!("Cascadia Mono"),
            None,
            DWRITE_FONT_WEIGHT_REGULAR,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            FONT_SIZE,
            w!("en-us"),
        )?
    };
    let resource_ms = t2.elapsed().as_secs_f64() * 1000.0;

    // Measure cell dimensions using a text layout for "M"
    let (cell_w, cell_h) = measure_cell(&dw, &tf)?;

    // Create brushes
    let fg = unsafe { rt.CreateSolidColorBrush(&FG_COLOR, None)? };
    let mut palette = Vec::with_capacity(16);
    for &(r, g, b) in &ANSI_COLORS {
        let color = D2D1_COLOR_F { r, g, b, a: 1.0 };
        palette.push(unsafe { rt.CreateSolidColorBrush(&color, None)? });
    }

    let startup_ms = t_total.elapsed().as_secs_f64() * 1000.0;
    println!("Startup:");
    println!("  Window creation:   {:.2}ms", window_ms);
    println!("  Factory creation:  {:.2}ms", factory_ms);
    println!("  Resources + fonts: {:.2}ms", resource_ms);
    println!("  Total startup:     {:.2}ms", startup_ms);
    println!("  Cell size:         {:.1}x{:.1}px", cell_w, cell_h);

    // Show window
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    pump_messages();

    let bench = Bench {
        rt,
        tf,
        fg,
        palette,
        cell_w,
        cell_h,
    };

    // Generate content: cycling printable ASCII per cell
    let content: Vec<Vec<char>> = (0..ROWS)
        .map(|r| {
            (0..COLS)
                .map(|c| {
                    let idx = r * COLS + c;
                    char::from(b'!' + (idx % 94) as u8)
                })
                .collect()
        })
        .collect();

    // --- Benchmark 1: Per-row, uniform color ---
    println!("\n--- Benchmark 1: Per-row, uniform color ---");
    let times1 = run_benchmark(|b, cont| render_per_row_uniform(b, cont), &bench, &content)?;
    print_stats(&times1);

    pump_messages();

    // --- Benchmark 2: Per-cell, uniform color ---
    println!("\n--- Benchmark 2: Per-cell, uniform color ---");
    let times2 = run_benchmark(|b, cont| render_per_cell_uniform(b, cont), &bench, &content)?;
    print_stats(&times2);

    pump_messages();

    // --- Benchmark 3: Per-cell, cycling 16 ANSI colors ---
    println!("\n--- Benchmark 3: Per-cell, 16-color cycle ---");
    let times3 = run_benchmark(|b, cont| render_per_cell_colored(b, cont), &bench, &content)?;
    print_stats(&times3);

    pump_messages();

    // --- Benchmark 4: Run-based, ~5 color runs per row (realistic terminal) ---
    println!("\n--- Benchmark 4: Run-based, ~5 runs/row (200 total) ---");
    let times4 = run_benchmark(|b, cont| render_run_based(b, cont, 5), &bench, &content)?;
    print_stats(&times4);

    pump_messages();

    // --- Benchmark 5: Run-based, ~15 color runs per row (heavy coloring) ---
    println!("\n--- Benchmark 5: Run-based, ~15 runs/row (600 total) ---");
    let times5 = run_benchmark(|b, cont| render_run_based(b, cont, 15), &bench, &content)?;
    print_stats(&times5);

    // --- Memory ---
    println!("\n--- Memory ---");
    print_memory();

    println!("\n=== Benchmark complete ===");
    Ok(())
}

fn run_benchmark(
    render_fn: impl Fn(&Bench, &[Vec<char>]) -> Result<()>,
    bench: &Bench,
    content: &[Vec<char>],
) -> Result<Vec<f64>> {
    // Warmup
    for _ in 0..WARMUP {
        render_fn(bench, content)?;
    }

    let mut times = Vec::with_capacity(NUM_FRAMES as usize);
    for _ in 0..NUM_FRAMES {
        let t = Instant::now();
        render_fn(bench, content)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(times)
}

fn render_per_row_uniform(b: &Bench, content: &[Vec<char>]) -> Result<()> {
    unsafe {
        b.rt.BeginDraw();
        b.rt.Clear(Some(&BG_COLOR));

        for (row, row_chars) in content.iter().enumerate() {
            let text: String = row_chars.iter().collect();
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let rect = D2D_RECT_F {
                left: 0.0,
                top: row as f32 * b.cell_h,
                right: COLS as f32 * b.cell_w,
                bottom: (row + 1) as f32 * b.cell_h,
            };
            b.rt.DrawText(
                &utf16,
                &b.tf,
                &rect,
                &b.fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        b.rt.EndDraw(None, None)?;
    }
    Ok(())
}

fn render_per_cell_uniform(b: &Bench, content: &[Vec<char>]) -> Result<()> {
    unsafe {
        b.rt.BeginDraw();
        b.rt.Clear(Some(&BG_COLOR));

        let mut buf = [0u16; 2];
        for (row, row_chars) in content.iter().enumerate() {
            let y = row as f32 * b.cell_h;
            for (col, &ch) in row_chars.iter().enumerate() {
                let len = ch.encode_utf16(&mut buf).len();
                let rect = D2D_RECT_F {
                    left: col as f32 * b.cell_w,
                    top: y,
                    right: (col + 1) as f32 * b.cell_w,
                    bottom: y + b.cell_h,
                };
                b.rt.DrawText(
                    &buf[..len],
                    &b.tf,
                    &rect,
                    &b.fg,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        b.rt.EndDraw(None, None)?;
    }
    Ok(())
}

fn render_per_cell_colored(b: &Bench, content: &[Vec<char>]) -> Result<()> {
    unsafe {
        b.rt.BeginDraw();
        b.rt.Clear(Some(&BG_COLOR));

        let mut buf = [0u16; 2];
        for (row, row_chars) in content.iter().enumerate() {
            let y = row as f32 * b.cell_h;
            for (col, &ch) in row_chars.iter().enumerate() {
                let len = ch.encode_utf16(&mut buf).len();
                let color_idx = (row * COLS + col) % 16;
                let rect = D2D_RECT_F {
                    left: col as f32 * b.cell_w,
                    top: y,
                    right: (col + 1) as f32 * b.cell_w,
                    bottom: y + b.cell_h,
                };
                b.rt.DrawText(
                    &buf[..len],
                    &b.tf,
                    &rect,
                    &b.palette[color_idx],
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        b.rt.EndDraw(None, None)?;
    }
    Ok(())
}

fn render_run_based(b: &Bench, content: &[Vec<char>], runs_per_row: usize) -> Result<()> {
    let run_len = COLS / runs_per_row;
    unsafe {
        b.rt.BeginDraw();
        b.rt.Clear(Some(&BG_COLOR));

        for (row, row_chars) in content.iter().enumerate() {
            let y = row as f32 * b.cell_h;
            let mut col = 0;
            let mut run_idx = 0;
            while col < COLS {
                let end = (col + run_len).min(COLS);
                let run_text: String = row_chars[col..end].iter().collect();
                let utf16: Vec<u16> = run_text.encode_utf16().collect();
                let color_idx = (row * runs_per_row + run_idx) % 16;
                let rect = D2D_RECT_F {
                    left: col as f32 * b.cell_w,
                    top: y,
                    right: end as f32 * b.cell_w,
                    bottom: y + b.cell_h,
                };
                b.rt.DrawText(
                    &utf16,
                    &b.tf,
                    &rect,
                    &b.palette[color_idx],
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                col = end;
                run_idx += 1;
            }
        }

        b.rt.EndDraw(None, None)?;
    }
    Ok(())
}

fn measure_cell(dw: &IDWriteFactory, tf: &IDWriteTextFormat) -> Result<(f32, f32)> {
    let text: Vec<u16> = "M".encode_utf16().collect();
    let layout = unsafe { dw.CreateTextLayout(&text, tf, 1000.0, 1000.0)? };
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics)? };
    Ok((metrics.width, metrics.height))
}

fn print_stats(times: &[f64]) {
    let mut sorted = times.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let n = sorted.len();
    let avg = sorted.iter().sum::<f64>() / n as f64;
    let p50 = sorted[n / 2];
    let p95 = sorted[(n as f64 * 0.95) as usize];
    let p99 = sorted[(n as f64 * 0.99) as usize];
    let min = sorted[0];
    let max = sorted[n - 1];
    let total_s = times.iter().sum::<f64>() / 1000.0;
    let fps = n as f64 / total_s;

    println!("  Frames: {} | Total: {:.1}ms", n, total_s * 1000.0);
    println!("  FPS:    {:.0}", fps);
    println!(
        "  Frame:  avg={:.3}ms  p50={:.3}ms  p95={:.3}ms  p99={:.3}ms",
        avg, p50, p95, p99
    );
    println!("          min={:.3}ms  max={:.3}ms", min, max);
}

fn print_memory() {
    unsafe {
        let process = GetCurrentProcess();
        let mut counters: PROCESS_MEMORY_COUNTERS = mem::zeroed();
        counters.cb = mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(
            process,
            &mut counters,
            mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .as_bool()
        {
            println!(
                "  Working set:      {:.1} MB",
                counters.WorkingSetSize as f64 / (1024.0 * 1024.0)
            );
            println!(
                "  Peak working set: {:.1} MB",
                counters.PeakWorkingSetSize as f64 / (1024.0 * 1024.0)
            );
            println!(
                "  Commit charge:    {:.1} MB",
                counters.PagefileUsage as f64 / (1024.0 * 1024.0)
            );
        } else {
            println!("  (Failed to read process memory info)");
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let hinstance: HINSTANCE = instance.into();
        let class_name = w!("DWriteBench");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        // Size for 120x40 grid at ~8x16 cell size + window chrome
        let width = 1100;
        let height = 720;

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("DirectWrite Benchmark"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(&hinstance),
            None,
        )?;

        Ok(hwnd)
    }
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
