//! Win32 window creation, message pump, and event handling for the terminal UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::input::{current_modifiers, vk_to_char, vk_to_key_name, KeyEvent, Modifiers};

// ── Paint / resize signals (atomics) ─────────────────────────────────────────

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

fn record_resize(width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    RESIZE_WIDTH.store(width, Ordering::Relaxed);
    RESIZE_HEIGHT.store(height, Ordering::Relaxed);
    RESIZED.store(true, Ordering::Relaxed);
    NEEDS_PAINT.store(true, Ordering::Relaxed);
}

/// Return the current client area size in pixels.
pub fn client_size(hwnd: HWND) -> Option<(u32, u32)> {
    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_ok() {
            let width = (client.right - client.left).max(0) as u32;
            let height = (client.bottom - client.top).max(0) as u32;
            if width > 0 && height > 0 {
                Some((width, height))
            } else {
                None
            }
        } else {
            None
        }
    }
}

fn record_resize_from_client(hwnd: HWND) -> bool {
    if let Some((width, height)) = client_size(hwnd) {
        record_resize(width, height);
        true
    } else {
        false
    }
}

// ── Mouse events ─────────────────────────────────────────────────────────────

/// A mouse event captured from the window proc.
#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub x: f32,
    pub y: f32,
    pub modifiers: Modifiers,
}

/// Kind of mouse event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventKind {
    /// Left button pressed.
    LeftDown,
    /// Left button double-clicked.
    LeftDoubleDown,
    /// Left button released.
    LeftUp,
    /// Right button pressed.
    RightDown,
    /// Right button released.
    RightUp,
    /// Middle button pressed.
    MiddleDown,
    /// Middle button released.
    MiddleUp,
    /// Mouse moved (any button state).
    Move,
    /// Scroll wheel rotated. Positive = up, negative = down. Value is delta in
    /// multiples of `WHEEL_DELTA` (120).
    Wheel(i16),
}

static MOUSE_EVENTS: OnceLock<Mutex<Vec<MouseEvent>>> = OnceLock::new();

fn mouse_queue() -> &'static Mutex<Vec<MouseEvent>> {
    MOUSE_EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drain all pending mouse events from the queue.
pub fn drain_mouse_events() -> Vec<MouseEvent> {
    let mut queue = mouse_queue().lock().unwrap();
    std::mem::take(&mut *queue)
}

// ── Keyboard events ──────────────────────────────────────────────────────────

static KEY_EVENTS: OnceLock<Mutex<Vec<KeyEvent>>> = OnceLock::new();

fn key_queue() -> &'static Mutex<Vec<KeyEvent>> {
    KEY_EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drain all pending keyboard events from the queue.
pub fn drain_key_events() -> Vec<KeyEvent> {
    let mut queue = key_queue().lock().unwrap();
    std::mem::take(&mut *queue)
}

/// Build a `KeyEvent` from a Win32 WM_KEYDOWN / WM_SYSKEYDOWN message and
/// push it to the event queue. Modifier-only keys (Shift, Ctrl, Alt) are
/// ignored.
fn push_key_event(wparam: WPARAM, lparam: LPARAM) {
    let vk = (wparam.0 & 0xFFFF) as u16;
    let scan_code = ((lparam.0 >> 16) & 0xFF) as u16;

    // Ignore modifier-only keys
    match vk {
        0x10 | 0x11 | 0x12 | // VK_SHIFT, VK_CONTROL, VK_MENU
        0xA0 | 0xA1 |        // VK_LSHIFT, VK_RSHIFT
        0xA2 | 0xA3 |        // VK_LCONTROL, VK_RCONTROL
        0xA4 | 0xA5 => return, // VK_LMENU, VK_RMENU
        _ => {}
    }

    if let Some(key) = vk_to_key_name(vk) {
        let modifiers = current_modifiers();
        let character = vk_to_char(vk, scan_code);

        key_queue().lock().unwrap().push(KeyEvent {
            key,
            modifiers,
            character,
        });
    }
}

// ── Window management ────────────────────────────────────────────────────────

/// Update the window title text.
pub fn set_window_title(hwnd: HWND, title: &str) {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

/// Begin a standard window drag from client-area chrome.
pub fn begin_window_drag(hwnd: HWND) {
    unsafe {
        let _ = SendMessageW(
            hwnd,
            WM_NCLBUTTONDOWN,
            WPARAM(HTCAPTION as usize),
            LPARAM(0),
        );
    }
}

pub fn minimize_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
}

pub fn toggle_maximize_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(
            hwnd,
            if IsZoomed(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_MAXIMIZE
            },
        );
    }
}

pub fn is_maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
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
            style: CS_DBLCLKS,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        let style = WINDOW_STYLE(
            WS_POPUP.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0 | WS_SYSMENU.0,
        );

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR(title_wide.as_ptr()),
            style,
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

/// Show the window once initial layout and host sizing are ready.
pub fn show_terminal_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
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

// ── Window procedure ─────────────────────────────────────────────────────────

fn extract_mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

fn client_pos_from_screen(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let mut point = POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    let _ = unsafe { ScreenToClient(hwnd, &mut point) };
    (point.x as f32, point.y as f32)
}

unsafe fn resize_hit_test(hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    if IsZoomed(hwnd).as_bool() {
        return None;
    }

    let mut window_rect = RECT::default();
    if GetWindowRect(hwnd, &mut window_rect).is_err() {
        return None;
    }

    let border_x = (GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)).max(6);
    let border_y = (GetSystemMetrics(SM_CYSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)).max(6);

    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

    let left = x < window_rect.left + border_x;
    let right = x >= window_rect.right - border_x;
    let top = y < window_rect.top + border_y;
    let bottom = y >= window_rect.bottom - border_y;

    let hit = match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, _, _, _) => HTLEFT,
        (_, true, _, _) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => return None,
    };

    Some(LRESULT(hit as isize))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE => LRESULT(0),
        WM_NCHITTEST => {
            if let Some(hit) = resize_hit_test(hwnd, lparam) {
                return hit;
            }
            LRESULT(HTCLIENT as isize)
        }
        WM_PAINT => {
            NEEDS_PAINT.store(true, Ordering::Relaxed);
            // Validate the window region so WM_PAINT stops repeating.
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SIZE => {
            let mut width = (lparam.0 & 0xFFFF) as u32;
            let mut height = ((lparam.0 >> 16) & 0xFFFF) as u32;

            if width == 0 || height == 0 {
                let mut client = RECT::default();
                if GetClientRect(hwnd, &mut client).is_ok() {
                    let measured_w = client.right - client.left;
                    let measured_h = client.bottom - client.top;
                    width = measured_w as u32;
                    height = measured_h as u32;
                }
            }

            if width == 0 || height == 0 {
                return LRESULT(0);
            }

            record_resize(width, height);
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            let _ = record_resize_from_client(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_EXITSIZEMOVE => {
            let _ = record_resize_from_client(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if lparam.0 != 0 {
                let suggested = lparam.0 as *const RECT;
                if let Some(rect) = suggested.as_ref() {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            let _ = record_resize_from_client(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::LeftDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::LeftDoubleDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::LeftUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::RightDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::RightUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::MiddleDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::MiddleUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::Move,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Wheel delta is in the high word of wparam (signed).
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            let (x, y) = client_pos_from_screen(hwnd, lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::Wheel(delta),
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            push_key_event(wparam, lparam);
            LRESULT(0)
        }
        WM_SYSKEYDOWN => {
            // Alt+F4 → let Windows handle (WM_CLOSE → WM_DESTROY)
            let vk = (wparam.0 & 0xFFFF) as u16;
            if vk == 0x73 {
                // VK_F4
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            push_key_event(wparam, lparam);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
