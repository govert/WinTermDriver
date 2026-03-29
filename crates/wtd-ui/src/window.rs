//! Win32 window creation, message pump, and event handling for the terminal UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::input::{current_modifiers, vk_to_char, vk_to_key_name, KeyEvent};

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

// ── Mouse events ─────────────────────────────────────────────────────────────

/// A mouse event captured from the window proc.
#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub x: f32,
    pub y: f32,
}

/// Kind of mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Down,
    Up,
    Move,
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

// ── Window procedure ─────────────────────────────────────────────────────────

fn extract_mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
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
        WM_LBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::Down,
                x,
                y,
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::Up,
                x,
                y,
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = extract_mouse_pos(lparam);
            mouse_queue().lock().unwrap().push(MouseEvent {
                kind: MouseEventKind::Move,
                x,
                y,
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
