# ADR-001: Rendering Technology Selection

**Status:** Accepted
**Date:** 2026-03-29
**Spec reference:** SS7.9, SS24.1

## Context

WinTermDriver requires a rendering technology to display terminal content (character grids with attributes), tab strips, pane splitters, and status bars. The spec (SS7.9) identifies three candidates in recommended evaluation order:

1. **wezterm's rendering components** (Rust-native, GPU/OpenGL)
2. **Win32 + DirectWrite** (native Windows, Direct2D/DWrite)
3. **WebView2 + xterm.js** (Chromium-based, JavaScript terminal)

This document records the time-boxed evaluation (SS37) with benchmarks, analysis, and a go/no-go for each candidate.

## Evaluation Criteria

| Criterion | Weight | Description |
|-----------|--------|-------------|
| Embeddability | High | Can the renderer be embedded in our custom Win32 window/tab/pane framework? |
| Latency | High | Frame render time for a 120x40 terminal grid with realistic color runs |
| Memory | Medium | Working set and commit charge for the rendering subsystem |
| Build complexity | Medium | Dependency count, compile time, build system requirements |
| Windows integration | Medium | Fit with Win32 window management, DPI scaling, IME, accessibility |
| Implementation effort | Medium | Lines of code and complexity to reach feature parity |

---

## Candidate 1: wezterm Components

### Assessment

wezterm is a Rust terminal emulator using OpenGL for GPU-accelerated rendering. The evaluation question is whether its renderer can be extracted and embedded in a custom framework.

**Crate analysis:**
- `termwiz` (published) -- Terminal utilities, surface abstraction, escape sequence parsing. This is a *TUI toolkit* for rendering to terminals, not a GPU renderer. Not applicable to our use case.
- `wezterm-term` (published) -- Terminal emulator model (equivalent to our `ScreenBuffer`). We already have this implemented in `wtd-pty::screen`.
- `wezterm-font` (published) -- Font loading and shaping with HarfBuzz. Potentially reusable but tightly coupled to wezterm's rendering pipeline.
- `wezterm-gui` (NOT published) -- The actual GPU renderer. Lives in the main wezterm repository, not available as a standalone crate. Uses `glium`/OpenGL with a custom glyph atlas and render pipeline deeply integrated with wezterm's window management (`window` crate).

**Key finding:** The GPU rendering code is not extractable. `wezterm-gui` depends on wezterm's custom window management layer, configuration system, and font infrastructure. Extracting the renderer would require forking a substantial portion of the wezterm codebase (~15,000+ lines of tightly coupled rendering code). This defeats the "code reuse" advantage and creates an ongoing maintenance burden tracking upstream changes.

**Alternative: use wezterm as a library via `wezterm-term`:** We already have a VT screen buffer (`wtd-pty::screen`). Using `wezterm-term` would mean replacing our implementation with theirs, but this doesn't help with the rendering question -- we'd still need a separate renderer to display the buffer contents in a window.

### Verdict: NO-GO

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Embeddability | Fail | GPU renderer not published as standalone crate |
| Build complexity | Poor | Would require forking wezterm-gui + transitive deps (glium, OpenGL) |
| Windows integration | Fair | OpenGL on Windows (not native DX path); no built-in DPI/IME support |
| Implementation effort | High | Fork maintenance + integration work exceeds building from scratch |

---

## Candidate 2: Win32 + DirectWrite

### Assessment

Uses `windows-rs` bindings (already in the project at v0.58) for `ID2D1Factory`/`ID2D1RenderTarget` (Direct2D) and `IDWriteFactory`/`IDWriteTextFormat` (DirectWrite). This is the same approach used by Windows Terminal.

### Benchmark Results

Benchmark executed on the development machine with `--release` optimizations. Grid: 120x40, font: Cascadia Mono 14pt, 500 frames per test after 50-frame warmup.

**Startup:**

| Phase | Time |
|-------|------|
| Window creation | 105ms |
| D2D/DWrite factory | 0.3ms |
| Render target + fonts | 265ms |
| **Total startup** | **371ms** |

**Rendering performance:**

| Mode | DrawText calls/frame | Avg frame | p95 frame | FPS |
|------|---------------------|-----------|-----------|-----|
| Per-row, uniform color | 40 | 2.1ms | 2.6ms | 481 |
| Run-based, ~5 runs/row (200 total) | 200 | 2.4ms | 3.0ms | 417 |
| Run-based, ~15 runs/row (600 total) | 600 | 4.4ms | 5.3ms | 226 |
| Per-cell, uniform color | 4,800 | 24.6ms | 26.3ms | 41 |
| Per-cell, 16-color cycle | 4,800 | 25.6ms | 27.8ms | 39 |

**Memory:**

| Metric | Value |
|--------|-------|
| Working set | 42 MB |
| Peak working set | 42 MB |
| Commit charge | 58 MB |

### Analysis

- **Realistic terminal rendering uses run-based batching** -- consecutive cells with the same attributes are grouped into a single DrawText call. A typical terminal screen has 100-300 color runs.
- At 200 runs (typical): **2.4ms/frame, 417 FPS** -- 7x headroom above 60 FPS target.
- At 600 runs (heavy coloring like syntax-highlighted code): **4.4ms/frame, 226 FPS** -- still 3.7x above target.
- Color switching cost is negligible (~4% overhead with pre-created brush palette).
- Startup is dominated by font loading (265ms), which is one-time. Subsequent window creation in the same process reuses cached factories.
- Memory footprint is modest at 42 MB working set, comparable to a simple Win32 application.
- **Cell size measurement** via `IDWriteTextLayout::GetMetrics` provides exact monospace cell dimensions (8.2x16.3px at 14pt), enabling precise grid layout.

**Optimization headroom:** The benchmark uses basic `DrawText` calls. Further optimizations available:
- `DrawGlyphRun` with a glyph atlas (Windows Terminal approach) -- eliminates per-call text shaping overhead
- `ID2D1DeviceContext` (Direct2D 1.1) with hardware-accelerated composition
- Dirty-region tracking -- only redraw changed rows/cells
- These optimizations could reduce frame times to sub-1ms for typical updates.

### Verdict: GO (Recommended)

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Embeddability | Excellent | Direct2D render targets attach to any HWND; full control over rendering pipeline |
| Latency | Excellent | 2-5ms/frame for realistic content, 200+ FPS |
| Memory | Good | 42 MB working set |
| Build complexity | Good | Uses existing `windows` 0.58 dep; only adds feature flags, no new crates |
| Windows integration | Excellent | Native DPI scaling, DirectComposition, IME rect support, accessibility via UIA |
| Implementation effort | Medium | ~2,000-3,000 LoC for the renderer; well-documented APIs; Windows Terminal as reference |

---

## Candidate 3: WebView2 + xterm.js

### Assessment

Embeds a Chromium-based WebView2 control (ships with Windows 11, installable on Windows 10 via Edge) and uses xterm.js for terminal rendering. Communication between Rust host and JS renderer via `postMessage` / `ExecuteScript`.

**No hands-on benchmark was conducted** for this candidate due to the significant setup overhead (WebView2 COM initialization, xterm.js bundling). Assessment is based on published WebView2 performance data and xterm.js project benchmarks.

### Published Performance Data

| Metric | Value | Source |
|--------|-------|--------|
| WebView2 environment creation | 200-500ms | Microsoft WebView2 docs, community benchmarks |
| First contentful paint | 300-800ms | Depends on HTML complexity and xterm.js initialization |
| Rust-to-JS round-trip (postMessage) | 0.1-1ms | WebView2 interop benchmarks |
| xterm.js render (120x40 grid) | 2-8ms | xterm.js WebGL renderer benchmarks (Canvas fallback: 5-15ms) |
| Memory per WebView2 instance | 80-150 MB | Chromium process model (browser + renderer + GPU processes) |

**Effective frame pipeline:**
1. Rust host receives PTY bytes (~0ms)
2. Process into screen buffer (~0.1ms)
3. Serialize delta to JSON (~0.1ms)
4. PostMessage to WebView2 (~0.5ms)
5. JS deserialize + xterm.js write (~1ms)
6. xterm.js render (~5ms with WebGL, ~10ms Canvas)
7. **Total: ~7-12ms per update** (best case with WebGL renderer)

### Analysis

- **Memory:** 80-150 MB for a single WebView2 instance is 2-3x the DirectWrite approach. Multiple panes would share some Chromium processes, but memory scales poorly.
- **Latency:** The IPC pipeline (Rust -> JSON -> postMessage -> JS -> render) adds ~5ms of non-rendering overhead on top of xterm.js's own render time. Total is 7-12ms best case -- still above 60 FPS but with much less headroom.
- **Build complexity:** Requires bundling xterm.js (and addons), setting up WebView2 COM initialization, handling the async creation lifecycle, and managing the Rust/JS boundary. Adds `webview2-com` or `wry` crate dependency.
- **Runtime dependency:** WebView2 runtime (~100 MB) must be present. Ships with Windows 11 and recent Windows 10 updates, but may need a bootstrap installer for older systems.
- **Debugging:** Two-language debugging (Rust + JS DevTools) adds development friction.
- **Advantages:** Fastest path to a visually functional terminal. xterm.js is battle-tested, handles selection, scrollback, ligatures, and complex Unicode rendering out of the box. Good for rapid prototyping.

### Verdict: NO-GO for primary renderer

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Embeddability | Good | WebView2 control embeds in HWND; xterm.js handles terminal rendering |
| Latency | Acceptable | 7-12ms/frame including IPC overhead |
| Memory | Poor | 80-150 MB per WebView2 instance |
| Build complexity | Fair | WebView2 COM + xterm.js bundling + JS build pipeline |
| Windows integration | Fair | WebView2 handles DPI; IME/accessibility depend on xterm.js addons |
| Implementation effort | Low | xterm.js provides turnkey terminal rendering |

While this approach offers the fastest path to a working prototype, the memory overhead, IPC latency layer, and dual-language complexity make it unsuitable as the primary rendering technology for a native Windows terminal workspace manager.

---

## Decision

**Win32 + DirectWrite** is selected as the rendering technology for WinTermDriver.

### Rationale

1. **Performance:** 2-5ms/frame for realistic terminal content (200-400+ FPS), with significant optimization headroom via glyph atlas and dirty-region tracking. No IPC overhead between the renderer and application logic.

2. **Native integration:** Direct2D/DirectWrite are the Windows-native rendering APIs. They provide built-in DPI scaling (`ID2D1RenderTarget::GetDpi`), IME candidate window positioning, and UIA accessibility hooks. Windows Terminal uses the same technology stack.

3. **Zero new dependencies:** The project already uses `windows` 0.58. DirectWrite/Direct2D only require additional feature flags -- no new crates, no runtime installers, no JS build toolchain.

4. **Memory efficiency:** 42 MB working set vs 80-150 MB for WebView2. This matters for a workspace manager that may host many simultaneous terminal panes.

5. **Single-language stack:** All rendering code is Rust, debuggable in a single toolchain, with no serialization boundary between the application and renderer.

### Implementation Path

The renderer implementation should proceed in phases:

1. **Phase 1 (Prototype):** Basic `DrawText`-based renderer with run batching. Monospace grid, single font, 16 ANSI colors. This is sufficient for M2 milestone visual verification.

2. **Phase 2 (Optimization):** Glyph atlas with `DrawGlyphRun`, dirty-region tracking, hardware-accelerated composition via `IDCompositionDevice`. Target sub-1ms frame times for incremental updates.

3. **Phase 3 (Polish):** Extended color support (256-color, RGB), bold/italic/underline attribute rendering, cursor styles, selection highlighting, ligature support via DirectWrite font features.

### Windows Features Required

The following `windows` 0.58 feature flags are needed (in addition to existing workspace features):

```
Foundation_Numerics
Win32_Graphics_Direct2D
Win32_Graphics_Direct2D_Common
Win32_Graphics_DirectWrite
Win32_Graphics_Dxgi_Common
Win32_Graphics_Gdi
Win32_UI_WindowsAndMessaging
```

### Benchmark Code

The benchmark is preserved in `crates/eval-renderer/examples/bench_directwrite.rs` for reproducibility and future regression testing.
