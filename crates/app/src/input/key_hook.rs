//! Low-level keyboard and mouse hooks: while this computer steers another one, keys, clicks and
//! the wheel go only there (Ctrl+C must copy on the other computer, a scroll must not scroll a
//! window here).
//!
//! Swallowed input never reaches `GetAsyncKeyState`, so this module also keeps the key and button
//! state that the capture loop, the hotkeys and the panic key read through [`key_down`].

#[cfg(any(windows, test))]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

#[cfg(any(windows, test))]
const KEYS: usize = 256;

/// Physical state of every key, as the hook saw it (injected keys are left out).
#[cfg(any(windows, test))]
static HELD: [AtomicBool; KEYS] = [const { AtomicBool::new(false) }; KEYS];
/// Pressed since the capture loop last looked: a tap shorter than one poll still gets sent.
#[cfg(any(windows, test))]
static TAPPED: [AtomicBool; KEYS] = [const { AtomicBool::new(false) }; KEYS];
/// The key's press reached Windows here, so its release has to reach it too (no stuck keys).
#[cfg(any(windows, test))]
static PASSED: [AtomicBool; KEYS] = [const { AtomicBool::new(false) }; KEYS];
#[cfg(any(windows, test))]
static HOOK_ON: AtomicBool = AtomicBool::new(false);
/// The mouse hook is in: mouse buttons are read from [`HELD`] too.
#[cfg(any(windows, test))]
static MOUSE_HOOK_ON: AtomicBool = AtomicBool::new(false);
/// Key repeats (a held key) while steering, not yet sent: Windows repeats only physical keys,
/// so the other computer gets them as extra presses.
#[cfg(any(windows, test))]
static REPEATS: [AtomicU32; KEYS] = [const { AtomicU32::new(0) }; KEYS];
/// Wheel turned while steering, in Windows wheel units (120 = one notch), not yet sent.
#[cfg(any(windows, test))]
static WHEEL_V: AtomicI32 = AtomicI32::new(0);
#[cfg(any(windows, test))]
static WHEEL_H: AtomicI32 = AtomicI32::new(0);
/// Events of this computer's own mouse so far (moves and clicks, injected ones left out).
static PHYSICAL_MOUSE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Counter of the physical mouse's events: someone is using this computer's own mouse when it
/// goes up while another computer steers this one.
pub fn physical_mouse_events() -> u32 {
    PHYSICAL_MOUSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// What the hook does with one physical key event; `true` = keep it from this computer.
#[cfg(any(windows, test))]
fn on_key(vk: usize, down: bool, steering: bool) -> bool {
    let vk = vk & 0xFF;
    let was_down = HELD[vk].swap(down, Ordering::SeqCst);
    if steering && down && was_down {
        REPEATS[vk].fetch_add(1, Ordering::SeqCst);
    }
    if down {
        TAPPED[vk].store(true, Ordering::SeqCst);
    }
    let swallow = steering && !(!down && PASSED[vk].load(Ordering::SeqCst));
    if !swallow {
        PASSED[vk].store(down, Ordering::SeqCst);
    }
    swallow
}

/// Left/right variants of the generic modifier codes Windows also answers for.
#[cfg(any(windows, test))]
fn variants(vk: i32) -> &'static [usize] {
    match vk {
        0x10 => &[0xA0, 0xA1],
        0x11 => &[0xA2, 0xA3],
        0x12 => &[0xA4, 0xA5],
        _ => &[],
    }
}

#[cfg(any(windows, test))]
fn held(vk: i32) -> bool {
    let generic = variants(vk);
    if generic.is_empty() {
        HELD[(vk as usize) & 0xFF].load(Ordering::SeqCst)
    } else {
        generic.iter().any(|&v| HELD[v].load(Ordering::SeqCst))
    }
}

/// The wheel while steering: `true` = keep it from this computer (it goes to the other one).
#[cfg(any(windows, test))]
fn on_wheel(delta: i32, horizontal: bool, steering: bool) -> bool {
    if !steering {
        return false;
    }
    if horizontal { &WHEEL_H } else { &WHEEL_V }.fetch_add(delta, Ordering::SeqCst);
    true
}

/// Wheel turned since the last call, in notches `(horizontal, vertical)`; forward = positive.
#[cfg(any(windows, test))]
pub fn take_wheel() -> (f32, f32) {
    let h = WHEEL_H.swap(0, Ordering::SeqCst);
    let v = WHEEL_V.swap(0, Ordering::SeqCst);
    (h as f32 / 120.0, v as f32 / 120.0)
}

/// Repeats of a held key since the last call (at most a few per poll).
#[cfg(any(windows, test))]
pub fn take_repeats(vk: u32) -> u32 {
    REPEATS[(vk as usize) & 0xFF].swap(0, Ordering::SeqCst).min(8)
}

/// Is this key (or mouse button, VK 1..6) held? Without the hook: `GetAsyncKeyState`.
pub fn key_down(vk: i32) -> bool {
    #[cfg(windows)]
    {
        let button = (0x01..=0x06).contains(&vk);
        let hooked = if button { MOUSE_HOOK_ON.load(Ordering::SeqCst) } else { HOOK_ON.load(Ordering::SeqCst) };
        if hooked {
            return held(vk);
        }
        use ::windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = vk;
        false
    }
}

/// [`key_down`], or pressed since the last call (for the capture loop's poll).
#[cfg(any(windows, test))]
pub fn take_down(vk: i32) -> bool {
    let tapped = TAPPED[(vk as usize) & 0xFF].swap(false, Ordering::SeqCst);
    key_down(vk) || (HOOK_ON.load(Ordering::SeqCst) && tapped)
}

/// Forget taps, repeats and wheel from before a capture started (they were for this computer).
#[cfg(any(windows, test))]
pub fn clear_taps() {
    for t in &TAPPED {
        t.store(false, Ordering::SeqCst);
    }
    for r in &REPEATS {
        r.store(0, Ordering::SeqCst);
    }
    WHEEL_V.store(0, Ordering::SeqCst);
    WHEEL_H.store(0, Ordering::SeqCst);
}

#[cfg(windows)]
pub use win::spawn;

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
        WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
        WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
        WM_XBUTTONDOWN, WM_XBUTTONUP,
    };
    use std::sync::{Arc, OnceLock};
    use std::thread;

    static STEERING: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    /// Counts the physical mouse's events and, while steering, keeps its clicks and wheel from
    /// this computer (they go to the other one). Movement always passes: the capture reads it.
    unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if ms.flags & LLMHF_INJECTED == 0 {
                PHYSICAL_MOUSE.fetch_add(1, Ordering::Relaxed);
                let steering = STEERING.get().is_some_and(|s| s.load(Ordering::SeqCst));
                let wheel = ((ms.mouseData >> 16) as u16) as i16 as i32;
                let xbutton = if (ms.mouseData >> 16) as u16 == 2 { 0x06 } else { 0x05 };
                let swallow = match wparam.0 as u32 {
                    WM_LBUTTONDOWN => on_key(0x01, true, steering),
                    WM_LBUTTONUP => on_key(0x01, false, steering),
                    WM_RBUTTONDOWN => on_key(0x02, true, steering),
                    WM_RBUTTONUP => on_key(0x02, false, steering),
                    WM_MBUTTONDOWN => on_key(0x04, true, steering),
                    WM_MBUTTONUP => on_key(0x04, false, steering),
                    WM_XBUTTONDOWN => on_key(xbutton, true, steering),
                    WM_XBUTTONUP => on_key(xbutton, false, steering),
                    WM_MOUSEWHEEL => on_wheel(wheel, false, steering),
                    WM_MOUSEHWHEEL => on_wheel(wheel, true, steering),
                    _ => false,
                };
                if swallow {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if kb.flags.0 & LLKHF_INJECTED.0 == 0 {
                let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let steering = STEERING.get().is_some_and(|s| s.load(Ordering::SeqCst));
                if on_key(kb.vkCode as usize, down, steering) {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Install the hook on its own thread (it needs a message loop). `steering` is the
    /// capture flag: while it is set, physical keys stay away from this computer.
    pub fn spawn(steering: Arc<AtomicBool>) {
        if STEERING.set(steering).is_err() {
            return;
        }
        let _ = thread::Builder::new().name("inputshare-keys".into()).spawn(|| unsafe {
            let module = GetModuleHandleW(None).ok();
            let module = module.map(HINSTANCE::from).unwrap_or_default();
            let installed = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0);
            if installed.is_err() {
                tracing::warn!("keyboard hook not installed: keys also reach this computer while steering");
                return;
            }
            HOOK_ON.store(true, Ordering::SeqCst);
            if SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0).is_err() {
                tracing::warn!("mouse hook not installed: no wheel while steering, and moving this computer's mouse will not take control back");
            } else {
                MOUSE_HOOK_ON.store(true, Ordering::SeqCst);
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            HOOK_ON.store(false, Ordering::SeqCst);
            MOUSE_HOOK_ON.store(false, Ordering::SeqCst);
        });
    }
}

// Drives the decision function only; no hook is installed and no key state is read.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_pressed_while_steering_stay_away_and_a_release_of_an_earlier_press_passes() {
        // 'C' typed here: passes
        assert!(!on_key(0x43, true, false));
        assert!(!on_key(0x43, false, false));
        // Ctrl held here, then steering starts and Ctrl+C is pressed
        assert!(!on_key(0xA2, true, false));
        assert!(on_key(0x43, true, true), "C goes only to the other computer");
        assert!(on_key(0x43, false, true));
        assert!(!on_key(0xA2, false, true), "this computer saw Ctrl go down, so it sees it go up");
        assert!(on_key(0xA2, true, true));
        assert!(on_key(0xA2, false, true));
    }

    #[test]
    fn generic_modifiers_read_either_side_and_taps_are_latched() {
        on_key(0xA3, true, false);
        assert!(held(0x11), "right Ctrl counts as Ctrl");
        on_key(0xA3, false, false);
        assert!(!held(0x11));
        clear_taps();
        on_key(0x44, true, true);
        on_key(0x44, false, true);
        assert!(!held(0x44));
        assert!(TAPPED[0x44].load(Ordering::SeqCst), "a quick tap is remembered for the next poll");
        clear_taps();
        assert!(!TAPPED[0x44].load(Ordering::SeqCst));
    }

    #[test]
    fn a_held_key_repeats_only_while_steering() {
        on_key(0x45, true, true);
        on_key(0x45, true, true);
        on_key(0x45, true, true);
        assert_eq!(take_repeats(0x45), 2, "two repeats after the first press");
        assert_eq!(take_repeats(0x45), 0);
        on_key(0x45, false, true);
        on_key(0x46, true, false);
        on_key(0x46, true, false);
        assert_eq!(take_repeats(0x46), 0, "typing here repeats through Windows itself");
        on_key(0x46, false, false);
    }

    #[test]
    fn the_wheel_goes_over_only_while_steering() {
        assert!(!on_wheel(120, false, false), "scrolls here");
        assert!(on_wheel(120, false, true));
        assert!(on_wheel(-240, true, true));
        assert!(on_wheel(60, false, true));
        let (h, v) = take_wheel();
        assert_eq!((h, v), (-2.0, 1.5));
        assert_eq!(take_wheel(), (0.0, 0.0));
    }
}
