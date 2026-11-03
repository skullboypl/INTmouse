//! Tray icon ownership and the tray-side behaviour that must not depend on a visible window.
//!
//! Two Windows facts shape this module:
//! * A *hidden* window gets no `WM_PAINT`, so eframe stops calling `update()` and the
//!   hotkeys, panic key and tray clicks (all applied from `update()`) go dead. The window
//!   is therefore *minimized* to the tray, never hidden, and frames keep coming.
//! * "Zamknij" must work even so, so the menu handler (UI thread, inside the message pump)
//!   quits by itself instead of waiting for a frame.

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, MutexGuard};
use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;
use tray_icon::menu::{MenuEvent, MenuId};
use tray_icon::TrayIcon;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, GetWindowTextW, SetTimer};

use super::icon;

pub const WINDOW_TITLE: &str = "INTmouse";

static PENDING: Mutex<Vec<MenuId>> = Mutex::new(Vec::new());
static UI_THREAD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    static SHOWN_SHARING: Cell<Option<bool>> = const { Cell::new(None) };
}

pub fn tooltip(sharing: bool) -> &'static str {
    if sharing {
        "INTmouse: udostępnianie włączone"
    } else {
        "INTmouse: udostępnianie wyłączone"
    }
}

/// Takes ownership of the tray icon (already showing `sharing`) and routes menu clicks.
/// Must be called on the UI thread.
pub fn install(ctx: egui::Context, tray: TrayIcon, quit: MenuId, sharing: bool) {
    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    ICON.with(|i| *i.borrow_mut() = Some(tray));
    SHOWN_SHARING.with(|s| s.set(Some(sharing)));
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        if ev.id == quit {
            quit_app();
            return;
        }
        lock_pending().push(ev.id);
        ctx.request_repaint();
    }));
}

/// Menu commands received since the last call, for `update()` to apply.
pub fn drain() -> Vec<MenuId> {
    std::mem::take(&mut *lock_pending())
}

/// Switches the tray icon between the green (on) and red (off) bubble.
pub fn set_sharing(sharing: bool) {
    if SHOWN_SHARING.with(|s| s.replace(Some(sharing))) == Some(sharing) {
        return;
    }
    ICON.with(|i| {
        if let Some(tray) = i.borrow().as_ref() {
            let _ = tray.set_icon(Some(icon::tray_icon(sharing)));
            let _ = tray.set_tooltip(Some(tooltip(sharing)));
        }
    });
}

/// Removes the notification-area icon. UI thread only.
pub fn remove_icon() {
    ICON.with(|i| {
        i.borrow_mut().take();
    });
}

/// Shows or removes the window's taskbar button. While the app lives in the tray the
/// window is minimized and has no button; opening it from the tray brings the button back.
pub fn set_taskbar_button(visible: bool) {
    let Some(hwnd) = main_window() else {
        return;
    };
    unsafe {
        let Ok(list) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        if list.HrInit().is_err() {
            return;
        }
        let _ = if visible {
            list.AddTab(hwnd)
        } else {
            list.DeleteTab(hwnd)
        };
    }
}

fn quit_app() {
    tracing::info!("Tray: zamykanie aplikacji");
    let on_ui_thread = unsafe { GetCurrentThreadId() } == UI_THREAD.load(Ordering::Relaxed);
    // Dropping the icon inside its own menu callback would free the menu that is still in
    // use, so the cleanup runs from a timer once the callback has returned.
    let timer = if on_ui_thread {
        unsafe { SetTimer(HWND::default(), 0, 10, Some(quit_timer)) }
    } else {
        0
    };
    if timer == 0 {
        std::process::exit(0);
    }
}

unsafe extern "system" fn quit_timer(_: HWND, _: u32, _: usize, _: u32) {
    remove_icon();
    std::process::exit(0);
}

fn main_window() -> Option<HWND> {
    let mut found = HWND::default();
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(match_title),
            LPARAM(&mut found as *mut HWND as isize),
        );
    }
    (!found.0.is_null()).then_some(found)
}

unsafe extern "system" fn match_title(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut buf = [0u16; 32];
    let len = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
    if String::from_utf16_lossy(&buf[..len]) == WINDOW_TITLE {
        *(lparam.0 as *mut HWND) = hwnd;
        return BOOL(0);
    }
    BOOL(1)
}

fn lock_pending() -> MutexGuard<'static, Vec<MenuId>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}
