//! Windows foreground / input helpers for switching gates.
//! Non-Windows builds get inert stubs so the crate still compiles.

/// Corner dead-zone size used with `prevent_corners` (pixels).
pub const CORNER_PX: i32 = 16;

/// True when the cursor is within `px` of any corner of the monitor rect.
pub fn in_corner(mon_x: i32, mon_y: i32, mon_w: i32, mon_h: i32, cx: i32, cy: i32, px: i32) -> bool {
    if mon_w <= 0 || mon_h <= 0 || px <= 0 {
        return false;
    }
    let right = mon_x + mon_w - 1;
    let bottom = mon_y + mon_h - 1;
    let near_left = (cx - mon_x).abs() <= px;
    let near_right = (cx - right).abs() <= px;
    let near_top = (cy - mon_y).abs() <= px;
    let near_bottom = (cy - bottom).abs() <= px;
    (near_left || near_right) && (near_top || near_bottom)
}

/// A single modifier key family from the `require_modifier` setting.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
    /// Left or right Windows key.
    Win,
}

/// Parse a `require_modifier` value. `None` means "never satisfiable" (unknown token / empty).
///
/// Accepts one name (`Ctrl`, `Strg`, `Alt`, `Menu`, `Shift`, `Win`/`Meta`/`Super`) or a
/// combination separated by `+`, `-` or whitespace (`Ctrl+Shift`); all parts must be held.
#[cfg(any(windows, test))]
fn parse_modifier_spec(name: &str) -> Option<Vec<Modifier>> {
    let n = name.trim().to_ascii_lowercase();
    let single = match n.as_str() {
        "ctrl" | "control" | "strg" => Some(Modifier::Ctrl),
        "alt" | "menu" => Some(Modifier::Alt),
        "shift" => Some(Modifier::Shift),
        "win" | "windows" | "meta" | "super" => Some(Modifier::Win),
        _ => None,
    };
    if let Some(m) = single {
        return Some(vec![m]);
    }
    // Allow "Ctrl+Shift"-style: any listed token must be held.
    if n.contains('+') || n.contains('-') || n.contains(' ') {
        let parts: Vec<&str> = n
            .split(|c: char| c == '+' || c == '-' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return None;
        }
        // Each part is resolved on its own (nested combos are not possible: separators are gone).
        return parts
            .iter()
            .map(|p| match parse_modifier_spec(p).as_deref() {
                Some([m]) => Some(*m),
                _ => None,
            })
            .collect();
    }
    None
}

/// Plain rectangle (same convention as Win32 `RECT`: right/bottom exclusive).
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rc {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `outer` is covered by `inner` with a 2px tolerance on every side.
#[cfg(any(windows, test))]
fn covers_with_tolerance(inner: Rc, outer: Rc) -> bool {
    inner.left <= outer.left + 2
        && inner.top <= outer.top + 2
        && inner.right >= outer.right - 2
        && inner.bottom >= outer.bottom - 2
}

/// Fullscreen decision from window/monitor geometry:
/// the window covers the whole monitor, or it is a topmost window filling the work area
/// (borderless fullscreen).
#[cfg(any(windows, test))]
fn is_fullscreen_rects(window: Rc, monitor: Rc, work: Rc, topmost: bool) -> bool {
    covers_with_tolerance(window, monitor) || (topmost && covers_with_tolerance(window, work))
}

/// The cursor clip rect is smaller than the whole desktop: some program confined the cursor.
#[cfg(any(windows, test))]
fn clip_is_confined(clip: Rc, desktop: Rc) -> bool {
    clip.left > desktop.left || clip.top > desktop.top || clip.right < desktop.right || clip.bottom < desktop.bottom
}

/// A program holds the cursor, as games do with a crosshair: it is hidden (while a mouse is
/// present: with no mouse Windows hides it on its own) or confined to part of the desktop.
#[cfg(any(windows, test))]
fn is_cursor_locked(hidden: bool, mouse_present: bool, clip: Rc, desktop: Rc) -> bool {
    (hidden && mouse_present) || clip_is_confined(clip, desktop)
}

/// File name (`game.exe`) from a full image path; `None` when it is empty.
#[cfg(any(windows, test))]
fn exe_name_from_path(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Parse a modifier name (Ctrl/Alt/Shift/Win) and check if it is currently held.
pub fn modifier_held(name: &str) -> bool {
    #[cfg(windows)]
    {
        win::modifier_held(name)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        false
    }
}

pub fn foreground_fullscreen() -> bool {
    #[cfg(windows)]
    {
        win::foreground_fullscreen()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn foreground_maximized() -> bool {
    #[cfg(windows)]
    {
        win::foreground_maximized()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// A program (a game) holds the cursor: hidden or clipped.
pub fn cursor_locked() -> bool {
    #[cfg(windows)]
    {
        win::cursor_locked()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn foreground_process_name() -> Option<String> {
    #[cfg(windows)]
    {
        win::foreground_process_name()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod win {
    use super::{
        exe_name_from_path, is_cursor_locked, is_fullscreen_rects, parse_modifier_spec, Modifier, Rc,
    };
    use ::windows::core::PWSTR;
    use ::windows::Win32::Foundation::{CloseHandle, MAX_PATH, RECT};
    use ::windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use ::windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetClipCursor, GetCursorInfo, GetForegroundWindow, GetSystemMetrics, GetWindowLongW,
        GetWindowRect, GetWindowThreadProcessId, IsIconic, IsZoomed, CURSORINFO, CURSOR_SHOWING,
        GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_MOUSEPRESENT, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, WS_EX_TOPMOST,
    };

    pub fn modifier_held(name: &str) -> bool {
        let Some(mods) = parse_modifier_spec(name) else {
            return false;
        };
        mods.iter().all(|m| {
            let vks: &[i32] = match m {
                Modifier::Ctrl => &[VK_CONTROL.0 as i32],
                Modifier::Alt => &[VK_MENU.0 as i32],
                Modifier::Shift => &[VK_SHIFT.0 as i32],
                Modifier::Win => &[VK_LWIN.0 as i32, VK_RWIN.0 as i32],
            };
            vks.iter().any(|&vk| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0)
        })
    }

    pub fn foreground_fullscreen() -> bool {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return false;
        }
        if unsafe { IsIconic(hwnd) }.as_bool() {
            return false;
        }

        let mut wr = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
            return false;
        }

        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            return false;
        }

        let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        let topmost = (ex & WS_EX_TOPMOST.0) != 0;
        // Full monitor cover (small tolerance for borders / exclusive mode), or the
        // borderless-fullscreen fallback: topmost window filling the work area.
        is_fullscreen_rects(to_rc(wr), to_rc(mi.rcMonitor), to_rc(mi.rcWork), topmost)
    }

    fn to_rc(r: RECT) -> Rc {
        Rc {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }

    pub fn cursor_locked() -> bool {
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        let hidden = unsafe { GetCursorInfo(&mut ci) }.is_ok() && (ci.flags.0 & CURSOR_SHOWING.0) == 0;
        let mouse_present = unsafe { GetSystemMetrics(SM_MOUSEPRESENT) } != 0;
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let desktop = Rc { left: x, top: y, right: x + w, bottom: y + h };
        let mut clip = RECT::default();
        let clip = if unsafe { GetClipCursor(&mut clip) }.is_ok() { to_rc(clip) } else { desktop };
        is_cursor_locked(hidden, mouse_present, clip, desktop)
    }

    pub fn foreground_maximized() -> bool {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return false;
        }
        unsafe { IsZoomed(hwnd) }.as_bool()
    }

    pub fn foreground_process_name() -> Option<String> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return None;
        }

        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buf = vec![0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        };
        let _ = unsafe { CloseHandle(handle) };
        if ok.is_err() || size == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        exe_name_from_path(&path)
    }
}

// Pure logic only: nothing here queries the foreground window or the keyboard state.
#[cfg(test)]
mod tests {
    use super::*;

    const fn rc(left: i32, top: i32, right: i32, bottom: i32) -> Rc {
        Rc {
            left,
            top,
            right,
            bottom,
        }
    }

    // ---- in_corner ----

    // Monitor 1920x1080 at origin: right = 1919, bottom = 1079.
    fn corner(cx: i32, cy: i32) -> bool {
        in_corner(0, 0, 1920, 1080, cx, cy, CORNER_PX)
    }

    #[test]
    fn corner_hits_all_four_corners() {
        assert!(corner(0, 0));
        assert!(corner(1919, 0));
        assert!(corner(0, 1079));
        assert!(corner(1919, 1079));
    }

    #[test]
    fn corner_zone_is_inclusive_at_px() {
        assert!(corner(CORNER_PX, CORNER_PX));
        assert!(!corner(CORNER_PX + 1, CORNER_PX), "x just outside the zone");
        assert!(!corner(CORNER_PX, CORNER_PX + 1), "y just outside the zone");
        assert!(corner(1919 - CORNER_PX, 1079 - CORNER_PX));
        assert!(!corner(1919 - CORNER_PX - 1, 1079 - CORNER_PX));
    }

    #[test]
    fn edge_midpoints_are_not_corners() {
        assert!(!corner(960, 0), "middle of top edge");
        assert!(!corner(0, 540), "middle of left edge");
        assert!(!corner(1919, 540));
        assert!(!corner(960, 1079));
        assert!(!corner(960, 540), "screen center");
    }

    #[test]
    fn corner_needs_both_axes_near() {
        // Near left edge but far from top and bottom.
        assert!(!corner(0, 500));
        // Near top edge but far from left and right.
        assert!(!corner(500, 0));
    }

    #[test]
    fn corner_respects_monitor_offset_including_negative() {
        // Secondary monitor left of primary: x in -2560..0.
        let at = |cx, cy| in_corner(-2560, 400, 2560, 1440, cx, cy, CORNER_PX);
        assert!(at(-2560, 400), "top-left of D");
        assert!(at(-1, 400 + 1439), "bottom-right of D");
        assert!(!at(-1280, 400), "middle of D's top edge");
        assert!(!at(-1280, 400 + 720));
    }

    #[test]
    fn corner_disabled_for_degenerate_inputs() {
        assert!(!in_corner(0, 0, 0, 1080, 0, 0, CORNER_PX), "zero width");
        assert!(!in_corner(0, 0, 1920, 0, 0, 0, CORNER_PX), "zero height");
        assert!(!in_corner(0, 0, -5, 1080, 0, 0, CORNER_PX), "negative width");
        assert!(!in_corner(0, 0, 1920, 1080, 0, 0, 0), "zero px zone");
        assert!(!in_corner(0, 0, 1920, 1080, 0, 0, -3), "negative px zone");
    }

    #[test]
    fn corner_works_on_tiny_monitor() {
        // 10x10 monitor with a 16px zone: everything is "in a corner".
        assert!(in_corner(0, 0, 10, 10, 5, 5, CORNER_PX));
    }

    // ---- modifier parsing ----

    use Modifier::*;

    #[test]
    fn single_modifier_names_and_aliases() {
        for n in ["ctrl", "Ctrl", "CONTROL", "Strg"] {
            assert_eq!(parse_modifier_spec(n), Some(vec![Ctrl]), "{n}");
        }
        for n in ["alt", "Menu"] {
            assert_eq!(parse_modifier_spec(n), Some(vec![Alt]), "{n}");
        }
        assert_eq!(parse_modifier_spec("Shift"), Some(vec![Shift]));
        for n in ["win", "Windows", "META", "super"] {
            assert_eq!(parse_modifier_spec(n), Some(vec![Win]), "{n}");
        }
    }

    #[test]
    fn modifier_name_is_trimmed() {
        assert_eq!(parse_modifier_spec("  ctrl \t"), Some(vec![Ctrl]));
    }

    #[test]
    fn modifier_combos_split_on_plus_dash_and_space() {
        assert_eq!(parse_modifier_spec("Ctrl+Shift"), Some(vec![Ctrl, Shift]));
        assert_eq!(parse_modifier_spec("ctrl-alt"), Some(vec![Ctrl, Alt]));
        assert_eq!(parse_modifier_spec("Alt Win"), Some(vec![Alt, Win]));
        assert_eq!(
            parse_modifier_spec("ctrl + shift + alt"),
            Some(vec![Ctrl, Shift, Alt])
        );
    }

    #[test]
    fn modifier_combo_ignores_empty_parts() {
        assert_eq!(parse_modifier_spec("ctrl++shift"), Some(vec![Ctrl, Shift]));
        assert_eq!(parse_modifier_spec("ctrl-"), Some(vec![Ctrl]));
    }

    #[test]
    fn unknown_or_empty_modifier_is_never_satisfied() {
        assert_eq!(parse_modifier_spec(""), None);
        assert_eq!(parse_modifier_spec("   "), None);
        assert_eq!(parse_modifier_spec("hyper"), None);
        assert_eq!(parse_modifier_spec("+"), None);
        assert_eq!(parse_modifier_spec(" - "), None);
    }

    #[test]
    fn combo_with_one_unknown_part_is_rejected_entirely() {
        assert_eq!(parse_modifier_spec("ctrl+banana"), None);
        assert_eq!(parse_modifier_spec("banana shift"), None);
    }

    #[test]
    fn tab_is_not_a_combo_separator() {
        // Only ' ', '+' and '-' trigger the combo path (kept as in the original check).
        assert_eq!(parse_modifier_spec("ctrl\tshift"), None);
    }

    // ---- fullscreen / maximized geometry ----

    const MON: Rc = rc(0, 0, 1920, 1080);
    const WORK: Rc = rc(0, 0, 1920, 1040); // taskbar 40px at the bottom

    #[test]
    fn exact_monitor_cover_is_fullscreen() {
        assert!(is_fullscreen_rects(MON, MON, WORK, false));
    }

    #[test]
    fn slightly_bigger_window_is_fullscreen() {
        // Typical borderless/exclusive overscan.
        assert!(is_fullscreen_rects(rc(-8, -8, 1928, 1088), MON, WORK, false));
    }

    #[test]
    fn two_px_tolerance_on_each_side() {
        assert!(is_fullscreen_rects(rc(2, 2, 1918, 1078), MON, WORK, false));
        assert!(!is_fullscreen_rects(rc(3, 0, 1920, 1080), MON, WORK, false));
        assert!(!is_fullscreen_rects(rc(0, 3, 1920, 1080), MON, WORK, false));
        assert!(!is_fullscreen_rects(rc(0, 0, 1917, 1080), MON, WORK, false));
        assert!(!is_fullscreen_rects(rc(0, 0, 1920, 1077), MON, WORK, false));
    }

    #[test]
    fn maximized_window_over_work_area_is_not_fullscreen_unless_topmost() {
        let maximized = rc(-8, -8, 1928, 1048);
        assert!(!is_fullscreen_rects(maximized, MON, WORK, false));
        // Topmost window filling the work area counts (borderless fullscreen fallback).
        assert!(is_fullscreen_rects(maximized, MON, WORK, true));
    }

    #[test]
    fn small_topmost_window_is_not_fullscreen() {
        assert!(!is_fullscreen_rects(rc(100, 100, 800, 600), MON, WORK, true));
    }

    #[test]
    fn half_screen_snapped_window_is_not_fullscreen() {
        assert!(!is_fullscreen_rects(rc(0, 0, 960, 1040), MON, WORK, false));
        assert!(!is_fullscreen_rects(rc(0, 0, 960, 1040), MON, WORK, true));
    }

    #[test]
    fn fullscreen_on_secondary_monitor_with_negative_origin() {
        let mon = rc(-2560, 400, 0, 1840);
        let work = rc(-2560, 400, 0, 1800);
        assert!(is_fullscreen_rects(mon, mon, work, false));
        // A window that is fullscreen on the *primary* does not cover the secondary.
        assert!(!is_fullscreen_rects(MON, mon, work, true));
    }

    // ---- process names ----

    #[test]
    fn exe_name_from_windows_path() {
        assert_eq!(
            exe_name_from_path(r"C:\Games\Steam\game.exe").as_deref(),
            Some("game.exe")
        );
    }

    #[test]
    fn exe_name_handles_forward_slashes_and_bare_names() {
        assert_eq!(exe_name_from_path("C:/a/b/vlc.exe").as_deref(), Some("vlc.exe"));
        assert_eq!(exe_name_from_path("notepad.exe").as_deref(), Some("notepad.exe"));
    }

    #[test]
    fn exe_name_empty_when_path_ends_with_separator_or_is_empty() {
        assert_eq!(exe_name_from_path(""), None);
        assert_eq!(exe_name_from_path(r"C:\dir\"), None);
    }

    #[test]
    fn exe_name_keeps_spaces_and_unicode() {
        assert_eq!(
            exe_name_from_path(r"D:\Program Files\Gra żółta\Mój Program.exe").as_deref(),
            Some("Mój Program.exe")
        );
    }

    // ---- cursor lock ----

    #[test]
    fn a_clip_inside_the_desktop_or_a_hidden_cursor_is_a_lock() {
        let desktop = rc(-1920, 0, 2560, 1440);
        assert!(!is_cursor_locked(false, true, desktop, desktop), "free cursor");
        assert!(is_cursor_locked(false, true, rc(0, 0, 2560, 1440), desktop), "clipped to one monitor");
        assert!(is_cursor_locked(false, true, rc(1270, 710, 1290, 730), desktop), "held in the middle");
        assert!(is_cursor_locked(true, true, desktop, desktop), "hidden by a game");
        assert!(!is_cursor_locked(true, false, desktop, desktop), "no mouse: Windows hides it itself");
    }
}
