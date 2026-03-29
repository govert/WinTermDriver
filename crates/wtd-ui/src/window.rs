//! Win32 window creation and message pump for the terminal UI.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Signals that a `WM_PAINT` was received and the window needs repainting.
static NEEDS_PAINT: AtomicBool = AtomicBool::new(true);

/// Signals that the window was resized. The new dimensions are stored in
/// `RESIZE_WIDTH` / `RESIZE_HEIGHT`.
static RESIZED: AtomicBool = AtomicBool::new(false);
static RESIZE_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static RESIZE_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Check and clear the "needs paint" flag.
pub fn take_needs_paint() -> bool {
    NEEDS_PAINT.swap(false, Ordering::Relaxed)
}

/// Check and clear the "resized" flag, returning the new dimensions if set.
pub fn take_resize() -> Option<(u32, u32)> {
    if RESIZED.swap(false, Ordering::Relaxed) {
        let w = RESIZE_WIDTH.load(Ordering::Relaxed);
        let h = RESIZE_HEIGHT.load(Ordering::Relaxed);
        Some((w, h))
    } else {
        None
    }
}

/// Request a repaint of the window.
pub fn request_repaint(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Create a top-level window for the terminal UI.
pub fn create_terminal_window(title: &str, width: i32, height: i32) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let hinstance: HINSTANCE = instance.into();
        let class_name = w!("WtdTerminal");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR(title_wide.as_ptr()),
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

        let _ = ShowWindow(hwnd, SW_SHOW);
        Ok(hwnd)
    }
}

/// Run the Win32 message pump. Returns when the window is closed.
pub fn run_message_loop() {
    unsafe {
        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if !ret.as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Pump all pending messages without blocking.
pub fn pump_pending_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
            if msg.message == WM_QUIT {
                return;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            NEEDS_PAINT.store(true, Ordering::Relaxed);
            // Validate the window region so WM_PAINT stops repeating.
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xFFFF) as u32;
            let height = ((lparam.0 >> 16) & 0xFFFF) as u32;
            RESIZE_WIDTH.store(width, Ordering::Relaxed);
            RESIZE_HEIGHT.store(height, Ordering::Relaxed);
            RESIZED.store(true, Ordering::Relaxed);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
