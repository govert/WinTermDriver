//! Win32 window creation, message pump, and event handling for the terminal UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::input::{current_modifiers, vk_to_key_name, KeyEvent, Modifiers};

// ── Paint / resize signals (atomics) ─────────────────────────────────────────

/// Signals that a `WM_PAINT` was received and the window needs repainting.
static NEEDS_PAINT: AtomicBool = AtomicBool::new(true);

/// Signals that the window was resized. The new dimensions are stored in
/// `RESIZE_WIDTH` / `RESIZE_HEIGHT`.
static RESIZED: AtomicBool = AtomicBool::new(false);
static RESIZE_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static RESIZE_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WINDOW_MINIMIZED: AtomicBool = AtomicBool::new(false);

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

fn clear_resize_signal() {
    RESIZED.store(false, Ordering::Relaxed);
    RESIZE_WIDTH.store(0, Ordering::Relaxed);
    RESIZE_HEIGHT.store(0, Ordering::Relaxed);
}

fn should_record_resize(width: u32, height: u32, minimized: bool) -> bool {
    !minimized && width > 0 && height > 0
}

fn record_resize(width: u32, height: u32) {
    if !should_record_resize(width, height, WINDOW_MINIMIZED.load(Ordering::Relaxed)) {
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
    if is_minimized(hwnd) {
        WINDOW_MINIMIZED.store(true, Ordering::Relaxed);
        clear_resize_signal();
        return false;
    }

    if let Some((width, height)) = client_size(hwnd) {
        record_resize(width, height);
        true
    } else {
        false
    }
}

fn resize_frame_thickness() -> (i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER),
            GetSystemMetrics(SM_CYSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER),
        )
    }
}

fn apply_maximized_client_insets(rect: &mut RECT, frame_x: i32, frame_y: i32) {
    if rect.right - rect.left > frame_x * 2 {
        rect.left += frame_x;
        rect.right -= frame_x;
    }
    if rect.bottom - rect.top > frame_y {
        rect.bottom -= frame_y;
    }
}

fn apply_maximized_bounds(minmax: &mut MINMAXINFO, monitor_rect: RECT, work_rect: RECT) {
    minmax.ptMaxPosition.x = work_rect.left - monitor_rect.left;
    minmax.ptMaxPosition.y = work_rect.top - monitor_rect.top;
    minmax.ptMaxSize.x = work_rect.right - work_rect.left;
    minmax.ptMaxSize.y = work_rect.bottom - work_rect.top;
    minmax.ptMaxTrackSize = minmax.ptMaxSize;
}

unsafe fn update_maximized_bounds(hwnd: HWND, minmax: &mut MINMAXINFO) {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if monitor.0.is_null() {
        return;
    }

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info as *mut _).as_bool() {
        apply_maximized_bounds(minmax, info.rcMonitor, info.rcWork);
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

fn lock_mouse_queue() -> std::sync::MutexGuard<'static, Vec<MouseEvent>> {
    match mouse_queue().lock() {
        Ok(queue) => queue,
        Err(poisoned) => {
            tracing::error!("mouse event queue mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

/// Drain all pending mouse events from the queue.
pub fn drain_mouse_events() -> Vec<MouseEvent> {
    let mut queue = lock_mouse_queue();
    std::mem::take(&mut *queue)
}

// ── Keyboard events ──────────────────────────────────────────────────────────

/// Input events captured from the window proc.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// Non-text keyboard input such as navigation keys, function keys, or
    /// keys that may participate in bindings/chords.
    Key(KeyEvent),
    /// Committed text input from the OS text-input path (WM_CHAR/WM_SYSCHAR).
    Text(String),
}

static INPUT_EVENTS: OnceLock<Mutex<Vec<InputEvent>>> = OnceLock::new();
static PENDING_HIGH_SURROGATE: OnceLock<Mutex<Option<u16>>> = OnceLock::new();

fn input_queue() -> &'static Mutex<Vec<InputEvent>> {
    INPUT_EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_input_queue() -> std::sync::MutexGuard<'static, Vec<InputEvent>> {
    match input_queue().lock() {
        Ok(queue) => queue,
        Err(poisoned) => {
            tracing::error!("input event queue mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

fn pending_high_surrogate() -> &'static Mutex<Option<u16>> {
    PENDING_HIGH_SURROGATE.get_or_init(|| Mutex::new(None))
}

fn lock_pending_high_surrogate() -> std::sync::MutexGuard<'static, Option<u16>> {
    match pending_high_surrogate().lock() {
        Ok(pending) => pending,
        Err(poisoned) => {
            tracing::error!("pending surrogate mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

/// Drain all pending keyboard/text input events from the queue.
pub fn drain_input_events() -> Vec<InputEvent> {
    let mut queue = lock_input_queue();
    std::mem::take(&mut *queue)
}

/// Build a `KeyEvent` from a Win32 WM_KEYDOWN / WM_SYSKEYDOWN message and
/// push it to the input queue. Modifier-only keys (Shift, Ctrl, Alt) are
/// ignored.
fn push_key_event(wparam: WPARAM, _lparam: LPARAM) {
    let vk = (wparam.0 & 0xFFFF) as u16;

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

        lock_input_queue().push(InputEvent::Key(KeyEvent {
            key,
            modifiers,
            character: None,
        }));
    }
}

fn push_text_event(wparam: WPARAM) {
    let code_unit = (wparam.0 & 0xFFFF) as u16;

    if (0xD800..=0xDBFF).contains(&code_unit) {
        *lock_pending_high_surrogate() = Some(code_unit);
        return;
    }

    let mut pending = lock_pending_high_surrogate();
    let text = if (0xDC00..=0xDFFF).contains(&code_unit) {
        if let Some(high) = pending.take() {
            let mut buf = [0u16; 2];
            buf[0] = high;
            buf[1] = code_unit;
            String::from_utf16(&buf).ok()
        } else {
            None
        }
    } else {
        pending.take();
        char::from_u32(code_unit as u32).map(|ch| ch.to_string())
    };
    drop(pending);

    if let Some(text) = text {
        if !text.chars().all(char::is_control) {
            lock_input_queue().push(InputEvent::Text(text));
        }
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

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
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

    let (frame_x, frame_y) = resize_frame_thickness();
    let border_x = frame_x.max(6);
    let border_y = frame_y.max(6);

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
        WM_GETMINMAXINFO => {
            if let Some(minmax) = (lparam.0 as *mut MINMAXINFO).as_mut() {
                update_maximized_bounds(hwnd, minmax);
            }
            LRESULT(0)
        }
        WM_NCCALCSIZE => {
            if wparam.0 != 0 {
                if let Some(params) = (lparam.0 as *mut NCCALCSIZE_PARAMS).as_mut() {
                    if IsZoomed(hwnd).as_bool() {
                        let (frame_x, frame_y) = resize_frame_thickness();
                        apply_maximized_client_insets(&mut params.rgrc[0], frame_x, frame_y);
                    }
                }
            }
            LRESULT(0)
        }
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
            if wparam.0 == SIZE_MINIMIZED as usize {
                WINDOW_MINIMIZED.store(true, Ordering::Relaxed);
                clear_resize_signal();
                return LRESULT(0);
            }

            WINDOW_MINIMIZED.store(false, Ordering::Relaxed);
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
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::LeftDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::LeftDoubleDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::LeftUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::RightDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::RightUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MBUTTONDOWN => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::MiddleDown,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
                kind: MouseEventKind::MiddleUp,
                x,
                y,
                modifiers: current_modifiers(),
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = extract_mouse_pos(lparam);
            lock_mouse_queue().push(MouseEvent {
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
            lock_mouse_queue().push(MouseEvent {
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
        WM_CHAR => {
            push_text_event(wparam);
            LRESULT(0)
        }
        WM_SYSCHAR => {
            push_text_event(wparam);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_resize_state() {
        WINDOW_MINIMIZED.store(false, Ordering::Relaxed);
        NEEDS_PAINT.store(false, Ordering::Relaxed);
        clear_resize_signal();
    }

    fn reset_input_queue() {
        lock_input_queue().clear();
        *lock_pending_high_surrogate() = None;
    }

    #[test]
    fn should_record_resize_ignores_zero_or_minimized_sizes() {
        assert!(!should_record_resize(0, 600, false));
        assert!(!should_record_resize(800, 0, false));
        assert!(!should_record_resize(800, 600, true));
        assert!(should_record_resize(800, 600, false));
    }

    #[test]
    fn record_resize_drops_updates_while_minimized() {
        reset_resize_state();
        WINDOW_MINIMIZED.store(true, Ordering::Relaxed);

        record_resize(800, 600);

        assert_eq!(take_resize(), None);
    }

    #[test]
    fn record_resize_queues_updates_when_restored() {
        reset_resize_state();

        record_resize(800, 600);

        assert_eq!(take_resize(), Some((800, 600)));
    }

    #[test]
    fn push_text_event_enqueues_printable_text() {
        reset_input_queue();

        push_text_event(WPARAM('é' as usize));

        let events = drain_input_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Text(text) => assert_eq!(text, "é"),
            other => panic!("expected text event, got {other:?}"),
        }
    }

    #[test]
    fn push_text_event_ignores_control_characters() {
        reset_input_queue();

        push_text_event(WPARAM('\r' as usize));
        push_text_event(WPARAM('\t' as usize));

        assert!(drain_input_events().is_empty());
    }

    #[test]
    fn push_text_event_combines_surrogate_pairs() {
        reset_input_queue();

        push_text_event(WPARAM(0xD842));
        push_text_event(WPARAM(0xDFB7));

        let events = drain_input_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Text(text) => assert_eq!(text, "𠮷"),
            other => panic!("expected text event, got {other:?}"),
        }
    }

    #[test]
    fn push_text_event_ignores_orphan_low_surrogate() {
        reset_input_queue();

        push_text_event(WPARAM(0xDFB7));

        assert!(drain_input_events().is_empty());
    }

    #[test]
    fn push_key_event_does_not_synthesize_printable_character() {
        reset_input_queue();

        push_key_event(WPARAM(0x41), LPARAM(0)); // 'A'

        let events = drain_input_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Key(event) => {
                assert!(matches!(event.key, crate::input::KeyName::Char('A')));
                assert_eq!(event.character, None);
            }
            other => panic!("expected key event, got {other:?}"),
        }
    }

    #[test]
    fn apply_maximized_client_insets_preserves_resize_margins() {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };

        apply_maximized_client_insets(&mut rect, 12, 10);

        assert_eq!(rect.left, 12);
        assert_eq!(rect.right, 1908);
        assert_eq!(rect.top, 0);
        assert_eq!(rect.bottom, 1070);
    }

    #[test]
    fn apply_maximized_bounds_uses_monitor_work_area() {
        let mut minmax = MINMAXINFO::default();
        let monitor = RECT {
            left: 100,
            top: 50,
            right: 2020,
            bottom: 1130,
        };
        let work = RECT {
            left: 100,
            top: 50,
            right: 2020,
            bottom: 1090,
        };

        apply_maximized_bounds(&mut minmax, monitor, work);

        assert_eq!(minmax.ptMaxPosition.x, 0);
        assert_eq!(minmax.ptMaxPosition.y, 0);
        assert_eq!(minmax.ptMaxSize.x, 1920);
        assert_eq!(minmax.ptMaxSize.y, 1040);
        assert_eq!(minmax.ptMaxTrackSize.x, 1920);
        assert_eq!(minmax.ptMaxTrackSize.y, 1040);
    }
}
