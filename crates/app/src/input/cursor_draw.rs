//! Simple layered HWND circle following injected cursor while being controlled.

use super::CaptureControl;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

pub fn run(ctrl: CaptureControl) {
    #[cfg(windows)]
    {
        win::run(ctrl);
    }
    #[cfg(not(windows))]
    {
        let _ = ctrl;
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

/// Marker diameter in px: normal / "large cursor" option.
#[cfg(any(windows, test))]
const MARKER_SIZE: i32 = 20;
#[cfg(any(windows, test))]
const MARKER_SIZE_LARGE: i32 = 36;

/// Square marker window centered on the injected cursor position: `(left, top, size)`.
#[cfg(any(windows, test))]
fn marker_rect(center: (i32, i32), large: bool) -> (i32, i32, i32) {
    let size = if large { MARKER_SIZE_LARGE } else { MARKER_SIZE };
    (center.0 - size / 2, center.1 - size / 2, size)
}

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use ::windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, PAINTSTRUCT,
    };
    use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, LoadCursorW, MoveWindow, PeekMessageW,
        PostQuitMessage, RegisterClassW, SetLayeredWindowAttributes, ShowWindow, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, LWA_COLORKEY, MSG, PM_REMOVE, SW_HIDE,
        SW_SHOWNA, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    const CLASS: &str = "InputShareRemoteCursor\0";

    pub fn run(ctrl: CaptureControl) {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class_name: Vec<u16> = CLASS.encode_utf16().collect();
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(class_name.as_ptr()),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                24,
                24,
                None,
                None,
                hinst,
                None,
            );
            let Ok(hwnd) = hwnd else {
                tracing::warn!("remote cursor HWND create failed");
                loop {
                    thread::sleep(Duration::from_secs(3600));
                }
            };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_COLORKEY);

            let mut visible = false;
            loop {
                // Drain messages without blocking forever.
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_DESTROY {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                let want = ctrl.being_controlled.load(Ordering::Relaxed)
                    && ctrl.draw_cursor.load(Ordering::Relaxed);
                let large = ctrl.large_cursor.load(Ordering::Relaxed);

                if want {
                    let (x, y) = ctrl
                        .inject_pos
                        .lock()
                        .map(|g| *g)
                        .unwrap_or((0, 0));
                    let (left, top, size) = marker_rect((x, y), large);
                    let _ = MoveWindow(hwnd, left, top, size, size, true);
                    if !visible {
                        let _ = ShowWindow(hwnd, SW_SHOWNA);
                        visible = true;
                    }
                } else if visible {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    visible = false;
                }

                thread::sleep(Duration::from_millis(16));
            }
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = ::windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
                // Magenta key color cleared; draw black circle.
                let brush = CreateSolidBrush(COLORREF(0x000000));
                let _ = Ellipse(hdc, 1, 1, rc.right - 1, rc.bottom - 1);
                let _ = DeleteObject(brush);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// Pure geometry only: no overlay window is created.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_centered_on_cursor() {
        assert_eq!(marker_rect((100, 200), false), (90, 190, 20));
        assert_eq!(marker_rect((100, 200), true), (82, 182, 36));
    }

    #[test]
    fn large_marker_is_bigger() {
        let (_, _, normal) = marker_rect((0, 0), false);
        let (_, _, large) = marker_rect((0, 0), true);
        assert!(large > normal);
    }

    #[test]
    fn marker_handles_negative_monitor_coordinates() {
        assert_eq!(marker_rect((-1, -1), false), (-11, -11, 20));
        assert_eq!(marker_rect((0, 0), true), (-18, -18, 36));
    }
}
