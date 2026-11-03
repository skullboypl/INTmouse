//! While this computer steers another one, a small, almost invisible window sits under the parked
//! cursor. Over it the cursor is hidden and clicks land on it instead of on the program below, so
//! the host shows no frozen cursor and no stray clicks. The window belongs to this process: if the
//! program dies the window goes with it and the cursor can never stay hidden (unlike swapping the
//! system cursors, which would outlive a killed process).

use super::CaptureControl;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Side of the shield square around the parked cursor.
const SIZE: i32 = 96;

/// Top-left and size of the shield for a parked cursor at `at`.
pub fn shield_rect(at: (i32, i32)) -> (i32, i32, i32, i32) {
    (at.0 - SIZE / 2, at.1 - SIZE / 2, SIZE, SIZE)
}

pub fn spawn(ctrl: CaptureControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-cursor-shield".into())
        .spawn(move || {
            #[cfg(windows)]
            win::run(ctrl);
            #[cfg(not(windows))]
            {
                let _ = ctrl;
            }
        })
        .expect("spawn cursor shield")
}

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
    use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW, SetCursor,
        SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, HCURSOR, HWND_TOPMOST,
        LWA_ALPHA, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_MOUSEACTIVATE, WM_SETCURSOR,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    const CLASS: &str = "INTmouseCursorShield\0";
    /// `WM_MOUSEACTIVATE` answer: do not activate, and throw the click away.
    const MA_NOACTIVATEANDEAT: isize = 4;

    pub fn run(ctrl: CaptureControl) {
        let Some(hwnd) = create() else {
            tracing::warn!("cursor shield window unavailable; the host cursor stays visible while steering");
            return;
        };
        // A file drag carried over the edge ends on the shield: it takes the files for the other computer.
        let _drop_target = super::super::drag_drop::register_shield_target(hwnd, ctrl.clone());
        let mut shown: Option<(i32, i32)> = None;
        loop {
            pump();
            let want = if ctrl.capturing.load(Ordering::SeqCst) {
                ctrl.shield_at.lock().ok().and_then(|g| *g)
            } else {
                None
            };
            if want != shown {
                unsafe {
                    match want {
                        Some(at) => {
                            let (x, y, w, h) = shield_rect(at);
                            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                        }
                        None => {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                }
                shown = want;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn create() -> Option<HWND> {
        unsafe {
            let hinst = GetModuleHandleW(None).ok()?;
            let class: Vec<u16> = CLASS.encode_utf16().collect();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                // No class cursor: over this window the pointer is not drawn.
                hCursor: HCURSOR::default(),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR(class.as_ptr()),
                WS_POPUP,
                0,
                0,
                SIZE,
                SIZE,
                None,
                None,
                hinst,
                None,
            )
            .ok()?;
            // Alpha 1 of 255: invisible to the eye, still hit-tested (alpha 0 would let clicks through).
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
            Some(hwnd)
        }
    }

    fn pump() {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_SETCURSOR => {
                SetCursor(HCURSOR::default());
                LRESULT(1)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATEANDEAT),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shield_is_centred_on_the_parked_cursor() {
        assert_eq!(shield_rect((1000, 500)), (952, 452, 96, 96));
        // at the very edge of the screen half of it is off-screen, the cursor is still inside
        let (x, y, w, h) = shield_rect((0, 0));
        assert!(x < 0 && y < 0 && x + w > 0 && y + h > 0);
    }
}
