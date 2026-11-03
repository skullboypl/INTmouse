//! Inject received input events via SendInput (Windows).

use inputshare_protocol::{Edge, InputEvent, Key, MouseButton};

#[cfg(windows)]
use super::keymap::key_to_vk;

pub fn apply_event(ev: &InputEvent) {
    #[cfg(windows)]
    {
        win::apply(ev);
    }
    #[cfg(not(windows))]
    {
        let _ = ev;
    }
}

pub fn apply_scroll(dx: f32, dy: f32, speed_v: f32, inv_v: bool, speed_h: f32, inv_h: bool) {
    let (dx, dy) = scale_scroll(dx, dy, speed_v, inv_v, speed_h, inv_h);
    apply_event(&InputEvent::Scroll { dx, dy });
}

/// Pure part of [`apply_scroll`]: slider 0..1 -> 0.25..2.0 multiplier, then optional invert.
fn scale_scroll(
    dx: f32,
    dy: f32,
    speed_v: f32,
    inv_v: bool,
    speed_h: f32,
    inv_h: bool,
) -> (f32, f32) {
    let mut dy = dy * (0.25 + speed_v.clamp(0.0, 1.0) * 1.75);
    let mut dx = dx * (0.25 + speed_h.clamp(0.0, 1.0) * 1.75);
    if inv_v {
        dy = -dy;
    }
    if inv_h {
        dx = -dx;
    }
    (dx, dy)
}

/// Scroll deltas below this (in notches) are treated as noise and dropped.
#[cfg(any(windows, test))]
const SCROLL_DEADZONE: f32 = 0.01;

/// Wheel units per notch in Win32 (`WHEEL_DELTA`).
#[cfg(any(windows, test))]
const WHEEL_DELTA: f32 = 120.0;

/// Pure part of `place_on_edge`: where the cursor lands on the virtual desktop
/// `(x, y, w, h)` when entering along `edge` at normalized `entry` (0..1).
#[cfg(any(windows, test))]
fn edge_entry_pos(edge: Edge, entry: f32, virt: (i32, i32, i32, i32)) -> (i32, i32) {
    let entry = entry.clamp(0.0, 1.0);
    let (vx, vy, vw, vh) = virt;
    match edge {
        Edge::Left => (vx + 2, vy + ((vh as f32) * entry) as i32),
        Edge::Right => (vx + vw - 3, vy + ((vh as f32) * entry) as i32),
        Edge::Top => (vx + ((vw as f32) * entry) as i32, vy + 2),
        Edge::Bottom => (vx + ((vw as f32) * entry) as i32, vy + vh - 3),
    }
}

/// Pure part of the Scroll injection: `(vertical, horizontal)` wheel units, `0` = send nothing.
#[cfg(any(windows, test))]
fn scroll_wheel_units(dx: f32, dy: f32) -> (i32, i32) {
    let units = |d: f32| {
        if d.abs() >= SCROLL_DEADZONE {
            (d * WHEEL_DELTA).round() as i32
        } else {
            0
        }
    };
    (units(dy), units(dx))
}

pub fn place_on_edge(edge: Edge, entry: f32) {
    #[cfg(windows)]
    {
        win::place_on_edge(edge, entry);
    }
    #[cfg(not(windows))]
    {
        let _ = (edge, entry);
    }
}

pub fn apply_lock_state(caps: bool, num: bool, scroll: bool) {
    #[cfg(windows)]
    {
        win::set_lock(0x14, caps);
        win::set_lock(0x90, num);
        win::set_lock(0x91, scroll);
    }
    #[cfg(not(windows))]
    {
        let _ = (caps, num, scroll);
    }
}

/// Best-effort ActivateKeyboardLayout by name (partial).
pub fn try_activate_layout(name: &str) {
    #[cfg(windows)]
    {
        win::try_activate_layout(name);
    }
    #[cfg(not(windows))]
    {
        let _ = name;
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::Win32::Foundation::POINT;
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
        KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
        MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
        VIRTUAL_KEY,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
        SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
    };

    pub fn apply(ev: &InputEvent) {
        match ev {
            InputEvent::MouseMove { dx, dy } => mouse_move(*dx, *dy),
            InputEvent::MouseButton { button, pressed } => mouse_button(*button, *pressed),
            InputEvent::Scroll { dx, dy } => {
                let (v, h) = scroll_wheel_units(*dx, *dy);
                if v != 0 {
                    mouse_wheel(v, false);
                }
                if h != 0 {
                    mouse_wheel(h, true);
                }
            }
            InputEvent::Key { key, pressed } => key_event(*key, *pressed),
        }
    }

    fn mouse_move(dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn mouse_button(button: MouseButton, pressed: bool) {
        let (flags, data) = match (button, pressed) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0u32),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
        };
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn mouse_wheel(notches: i32, horizontal: bool) {
        // Windows wheel: 120 = one notch (already applied by `scroll_wheel_units`).
        let flags = if horizontal {
            MOUSEEVENTF_HWHEEL
        } else {
            MOUSEEVENTF_WHEEL
        };
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: notches as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn key_event(key: Key, pressed: bool) {
        let Some(vk) = key_to_vk(key) else {
            return;
        };
        let mut flags = if pressed {
            KEYBD_EVENT_FLAGS(0)
        } else {
            KEYEVENTF_KEYUP
        };
        // With the scan code too: programs that read it (shortcuts in browsers, games, Office)
        // see a real key, and arrows / Insert / Delete / Win keep their extended form.
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        if is_extended_vk(vk) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// Keys Windows sends with the extended flag (E0 prefix).
    fn is_extended_vk(vk: u16) -> bool {
        matches!(
            vk,
            0x21..=0x28 // PageUp, PageDown, End, Home, arrows
                | 0x2C | 0x2D | 0x2E // PrintScreen, Insert, Delete
                | 0x5B | 0x5C | 0x5D // Win, Apps
                | 0x6F // numpad /
                | 0x90 // NumLock
                | 0xA3 | 0xA5 // right Ctrl, right Alt
                | 0xA6..=0xB7 // browser, volume, media and launch keys
        )
    }

    pub fn place_on_edge(edge: Edge, entry: f32) {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);
        // Fallback primary if virtual is weird.
        let _ = (unsafe { GetSystemMetrics(SM_CXSCREEN) }, unsafe {
            GetSystemMetrics(SM_CYSCREEN)
        });
        let (x, y) = edge_entry_pos(edge, entry, (vx, vy, vw, vh));
        let _ = unsafe { SetCursorPos(x, y) };
    }

    pub fn set_lock(vk: u8, want_on: bool) {
        unsafe {
            let state = GetKeyState(vk as i32);
            let is_on = (state as u16) & 1 != 0;
            if is_on != want_on {
                keybd_event(vk, 0, KEYBD_EVENT_FLAGS(0), 0);
                keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    }

    use ::windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;

    pub fn try_activate_layout(name: &str) {
        use ::windows::core::PCWSTR;
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            ActivateKeyboardLayout, LoadKeyboardLayoutW, KLF_ACTIVATE,
        };
        // Accept HKL hex like "00000415" (Polish) or leave as-is.
        let wide: Vec<u16> = name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            if let Ok(hkl) = LoadKeyboardLayoutW(PCWSTR(wide.as_ptr()), KLF_ACTIVATE) {
                let _ = ActivateKeyboardLayout(hkl, KLF_ACTIVATE);
                tracing::debug!(name, "ActivateKeyboardLayout attempted");
            } else {
                tracing::debug!(name, "LoadKeyboardLayoutW failed (best-effort)");
            }
        }
    }

    #[allow(dead_code)]
    pub fn cursor_pos() -> (i32, i32) {
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        (pt.x, pt.y)
    }
}

// Tests cover only the pure helpers: nothing here injects input or touches the cursor.
#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn scroll_default_speed_is_near_unity() {
        // slider 0.5 -> 0.25 + 0.875 = 1.125
        let (dx, dy) = scale_scroll(1.0, 2.0, 0.5, false, 0.5, false);
        assert!(approx(dx, 1.125));
        assert!(approx(dy, 2.25));
    }

    #[test]
    fn scroll_speed_endpoints_map_to_quarter_and_two() {
        let (_, dy_min) = scale_scroll(0.0, 1.0, 0.0, false, 0.0, false);
        let (_, dy_max) = scale_scroll(0.0, 1.0, 1.0, false, 0.0, false);
        assert!(approx(dy_min, 0.25));
        assert!(approx(dy_max, 2.0));
    }

    #[test]
    fn scroll_speed_is_clamped_to_slider_range() {
        let (_, below) = scale_scroll(0.0, 1.0, -5.0, false, 0.0, false);
        let (_, above) = scale_scroll(0.0, 1.0, 9.0, false, 0.0, false);
        assert!(approx(below, 0.25));
        assert!(approx(above, 2.0));
    }

    #[test]
    fn scroll_axes_use_independent_speed_and_invert() {
        let (dx, dy) = scale_scroll(1.0, 1.0, 1.0, true, 0.0, false);
        assert!(approx(dy, -2.0), "vertical inverted at max speed, got {dy}");
        assert!(approx(dx, 0.25), "horizontal untouched at min speed, got {dx}");

        let (dx, dy) = scale_scroll(1.0, 1.0, 0.0, false, 1.0, true);
        assert!(approx(dy, 0.25));
        assert!(approx(dx, -2.0));
    }

    #[test]
    fn scroll_invert_negates_both_directions() {
        let (_, dy) = scale_scroll(0.0, -1.0, 1.0, true, 0.0, false);
        assert!(approx(dy, 2.0), "inverting a negative delta flips it positive");
    }

    #[test]
    fn scroll_zero_stays_zero() {
        let (dx, dy) = scale_scroll(0.0, 0.0, 1.0, true, 1.0, true);
        assert_eq!(dx, 0.0);
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn wheel_units_are_120_per_notch() {
        assert_eq!(scroll_wheel_units(0.0, 1.0), (120, 0));
        assert_eq!(scroll_wheel_units(0.0, -1.0), (-120, 0));
        assert_eq!(scroll_wheel_units(1.0, 0.0), (0, 120));
        assert_eq!(scroll_wheel_units(-2.0, 3.0), (360, -240));
    }

    #[test]
    fn wheel_units_round_to_nearest() {
        // 0.5 notch = 60 units; 0.0125 -> 1.5 -> rounds away from zero to 2.
        assert_eq!(scroll_wheel_units(0.0, 0.5), (60, 0));
        assert_eq!(scroll_wheel_units(0.0, 0.0125), (2, 0));
    }

    #[test]
    fn wheel_units_drop_tiny_deltas() {
        assert_eq!(scroll_wheel_units(0.009, -0.009), (0, 0));
        assert_eq!(scroll_wheel_units(0.0, 0.0), (0, 0));
        // At exactly the deadzone the delta passes (>=) and rounds to 1 unit.
        assert_eq!(scroll_wheel_units(0.0, 0.01), (1, 0));
    }

    #[test]
    fn scaled_then_quantized_scroll_matches_end_to_end() {
        // Sender sends 1 notch at slider 0.5: receiver injects round(1.125 * 120) = 135.
        let (dx, dy) = scale_scroll(0.0, 1.0, 0.5, false, 0.5, false);
        assert_eq!(scroll_wheel_units(dx, dy), (135, 0));
    }

    #[test]
    fn edge_entry_left_and_right_use_virtual_desktop_height() {
        let virt = (0, 0, 1920, 1080);
        assert_eq!(edge_entry_pos(Edge::Left, 0.0, virt), (2, 0));
        assert_eq!(edge_entry_pos(Edge::Left, 0.5, virt), (2, 540));
        assert_eq!(edge_entry_pos(Edge::Right, 0.5, virt), (1917, 540));
    }

    #[test]
    fn edge_entry_top_and_bottom_use_virtual_desktop_width() {
        let virt = (0, 0, 1920, 1080);
        assert_eq!(edge_entry_pos(Edge::Top, 0.25, virt), (480, 2));
        assert_eq!(edge_entry_pos(Edge::Bottom, 0.25, virt), (480, 1077));
    }

    #[test]
    fn edge_entry_respects_negative_virtual_origin() {
        // Secondary monitor left of and above the primary: origin is negative.
        let virt = (-2560, -1080, 5120, 2520);
        assert_eq!(edge_entry_pos(Edge::Left, 0.0, virt), (-2558, -1080));
        assert_eq!(edge_entry_pos(Edge::Right, 1.0, virt), (2557, 1440));
        assert_eq!(edge_entry_pos(Edge::Top, 0.5, virt), (0, -1078));
        assert_eq!(edge_entry_pos(Edge::Bottom, 0.5, virt), (0, 1437));
    }

    #[test]
    fn edge_entry_is_clamped_to_0_1() {
        let virt = (0, 0, 1000, 1000);
        assert_eq!(edge_entry_pos(Edge::Left, -3.0, virt), (2, 0));
        assert_eq!(edge_entry_pos(Edge::Left, 7.0, virt), (2, 1000));
        assert_eq!(edge_entry_pos(Edge::Top, 9.0, virt), (1000, 2));
    }
}
