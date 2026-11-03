//! Global enable/disable + quick-jump hotkeys (GetAsyncKeyState poll).

use inputshare_core::{MonitorLayout, MonitorTile};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Closes the program on the spot, whatever state it is in (trapped cursor, hung window). Not
/// configurable and not switchable off: it is the way out when everything else fails.
pub const EMERGENCY_QUIT: &str = "Ctrl+Alt+Shift+Q";

/// Parsed combo: modifiers + one key VK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
}

impl KeyCombo {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut win = false;
        let mut vk: Option<u32> = None;
        for part in s.split(|c: char| c == '+' || c == '-') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            match p.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "strg" => ctrl = true,
                "alt" | "menu" | "option" => alt = true,
                "shift" => shift = true,
                "win" | "windows" | "meta" | "super" | "cmd" => win = true,
                other => {
                    if vk.is_some() {
                        return None;
                    }
                    vk = Some(vk_from_token(other)?);
                }
            }
        }
        Some(Self {
            ctrl,
            alt,
            shift,
            win,
            vk: vk?,
        })
    }

    #[cfg(windows)]
    pub fn is_down(&self) -> bool {
        use crate::input::key_hook::key_down;
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        };
        // Through the keyboard hook's state: while steering another computer the keys are
        // kept from Windows here, and Windows' own key state no longer sees them.
        if !key_down(self.vk as i32) {
            return false;
        }
        let ctrl_ok = !self.ctrl || key_down(VK_CONTROL.0 as i32);
        let alt_ok = !self.alt || key_down(VK_MENU.0 as i32);
        let shift_ok = !self.shift || key_down(VK_SHIFT.0 as i32);
        let win_ok = !self.win || key_down(VK_LWIN.0 as i32) || key_down(VK_RWIN.0 as i32);
        ctrl_ok && alt_ok && shift_ok && win_ok
    }

    #[cfg(not(windows))]
    pub fn is_down(&self) -> bool {
        let _ = self;
        false
    }
}

fn vk_from_token(tok: &str) -> Option<u32> {
    let t = tok.trim().to_ascii_lowercase();
    match t.as_str() {
        "esc" | "escape" => Some(0x1B),
        "tab" => Some(0x09),
        "space" | "spacebar" => Some(0x20),
        "enter" | "return" => Some(0x0D),
        "backspace" | "bksp" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "left" => Some(0x25),
        "up" => Some(0x26),
        "right" => Some(0x27),
        "down" => Some(0x28),
        "pause" => Some(0x13),
        "f1" => Some(0x70),
        "f2" => Some(0x71),
        "f3" => Some(0x72),
        "f4" => Some(0x73),
        "f5" => Some(0x74),
        "f6" => Some(0x75),
        "f7" => Some(0x76),
        "f8" => Some(0x77),
        "f9" => Some(0x78),
        "f10" => Some(0x79),
        "f11" => Some(0x7A),
        "f12" => Some(0x7B),
        _ => {
            let chars: Vec<char> = t.chars().collect();
            if chars.len() == 1 {
                let c = chars[0];
                if c.is_ascii_alphabetic() {
                    return Some(c.to_ascii_uppercase() as u32);
                }
                if c.is_ascii_digit() {
                    return Some(c as u32);
                }
            }
            None
        }
    }
}

#[derive(Clone)]
pub struct HotkeyControl {
    /// Enable/disable combo always polled when set.
    enable_hotkey: Arc<Mutex<String>>,
    jump_hotkey: Arc<Mutex<String>>,
    layout: Arc<Mutex<MonitorLayout>>,
    /// Quick-jump only when sharing is on.
    sharing: Arc<AtomicBool>,
    /// GUI reads: flip sharing.
    pub toggle_sharing: Arc<AtomicBool>,
    /// Set after jump (optional status); jump itself runs on the hotkey thread.
    pub quick_jump: Arc<AtomicBool>,
}

impl HotkeyControl {
    pub fn new(
        enable: &str,
        jump: &str,
        layout: MonitorLayout,
        sharing: bool,
    ) -> Self {
        Self {
            enable_hotkey: Arc::new(Mutex::new(enable.to_string())),
            jump_hotkey: Arc::new(Mutex::new(jump.to_string())),
            layout: Arc::new(Mutex::new(layout)),
            sharing: Arc::new(AtomicBool::new(sharing)),
            toggle_sharing: Arc::new(AtomicBool::new(false)),
            quick_jump: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_enable_hotkey(&self, s: &str) {
        if let Ok(mut g) = self.enable_hotkey.lock() {
            *g = s.to_string();
        }
    }

    pub fn set_jump_hotkey(&self, s: &str) {
        if let Ok(mut g) = self.jump_hotkey.lock() {
            *g = s.to_string();
        }
    }

    pub fn set_layout(&self, layout: MonitorLayout) {
        if let Ok(mut g) = self.layout.lock() {
            *g = layout;
        }
    }

    pub fn set_sharing(&self, on: bool) {
        self.sharing.store(on, Ordering::Relaxed);
    }

    pub fn take_toggle_sharing(&self) -> bool {
        self.toggle_sharing
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }

    pub fn take_quick_jump(&self) -> bool {
        self.quick_jump
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }
}

pub fn spawn(ctrl: HotkeyControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-hotkeys".into())
        .spawn(move || run_loop(ctrl))
        .expect("spawn hotkeys thread")
}

fn run_loop(ctrl: HotkeyControl) {
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

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::Win32::Foundation::POINT;
    use ::windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

    pub fn run(ctrl: HotkeyControl) {
        let mut enable_was = false;
        let mut jump_was = false;
        let mut quit_was = false;
        let quit = KeyCombo::parse(EMERGENCY_QUIT);

        loop {
            thread::sleep(Duration::from_millis(15));

            if let Some(combo) = quit {
                let down = combo.is_down();
                if down && !quit_was {
                    tracing::warn!("Skrót awaryjny {EMERGENCY_QUIT}: zamykam program");
                    std::process::exit(0);
                }
                quit_was = down;
            }

            let enable_s = ctrl
                .enable_hotkey
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();
            let jump_s = ctrl
                .jump_hotkey
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();

            // Enable/disable — always (even when sharing off).
            if let Some(combo) = KeyCombo::parse(&enable_s) {
                let down = combo.is_down();
                if down && !enable_was {
                    ctrl.toggle_sharing.store(true, Ordering::SeqCst);
                    wait_release(&combo);
                    enable_was = false;
                    jump_was = false;
                    continue;
                }
                enable_was = down;
            } else {
                enable_was = false;
            }

            // Quick jump — only while sharing.
            if ctrl.sharing.load(Ordering::Relaxed) {
                if let Some(combo) = KeyCombo::parse(&jump_s) {
                    let down = combo.is_down();
                    if down && !jump_was {
                        if do_quick_jump(&ctrl) {
                            ctrl.quick_jump.store(true, Ordering::SeqCst);
                        }
                        wait_release(&combo);
                        jump_was = false;
                        enable_was = false;
                        continue;
                    }
                    jump_was = down;
                } else {
                    jump_was = false;
                }
            } else {
                jump_was = false;
            }
        }
    }

    fn wait_release(combo: &KeyCombo) {
        while combo.is_down() {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn do_quick_jump(ctrl: &HotkeyControl) -> bool {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return false;
        }
        let layout = match ctrl.layout.lock() {
            Ok(g) => g.clone(),
            Err(_) => return false,
        };
        let monitors = local_monitors_in_order(&layout);
        let Some((cx, cy)) = quick_jump_target(&monitors, pt.x, pt.y) else {
            return false;
        };
        unsafe { SetCursorPos(cx, cy) }.is_ok()
    }
}

/// This PC's monitors (OS-synced tiles, or all tiles when none is synced) in
/// top-to-bottom, left-to-right, then label order — the QuickJump cycle order.
#[cfg(any(windows, test))]
fn local_monitors_in_order(layout: &MonitorLayout) -> Vec<MonitorTile> {
    let mut local: Vec<_> = layout
        .monitors
        .iter()
        .filter(|m| m.system_synced)
        .cloned()
        .collect();
    if local.is_empty() {
        local = layout.monitors.clone();
    }
    local.sort_by(|a, b| {
        a.y.cmp(&b.y)
            .then(a.x.cmp(&b.x))
            .then(a.label.cmp(&b.label))
    });
    local
}

#[cfg(any(windows, test))]
fn point_in(m: &MonitorTile, x: i32, y: i32) -> bool {
    x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + m.height as i32
}

/// QuickJump destination: center of the monitor after the one under `(px, py)` in `monitors`
/// (cycling; cursor on no monitor counts as the first). `None` when there are no monitors.
#[cfg(any(windows, test))]
fn quick_jump_target(monitors: &[MonitorTile], px: i32, py: i32) -> Option<(i32, i32)> {
    if monitors.is_empty() {
        return None;
    }
    let cur_idx = monitors
        .iter()
        .position(|m| point_in(m, px, py))
        .unwrap_or(0);
    let next = &monitors[(cur_idx + 1) % monitors.len()];
    Some((
        next.x + (next.width as i32) / 2,
        next.y + (next.height as i32) / 2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ctrl_shift_a() {
        let c = KeyCombo::parse("Ctrl+Shift+A").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.win);
        assert_eq!(c.vk, b'A' as u32);
    }

    #[test]
    fn parse_alt_win_f12() {
        let c = KeyCombo::parse("Alt+Win+F12").unwrap();
        assert!(c.alt && c.win && !c.ctrl);
        assert_eq!(c.vk, 0x7B);
    }

    // ---- KeyCombo::parse (pure; `is_down` is never called here) ----

    #[test]
    fn parse_is_case_and_whitespace_insensitive() {
        let c = KeyCombo::parse("  cTrL + shift + q ").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.win);
        assert_eq!(c.vk, b'Q' as u32);
    }

    #[test]
    fn parse_accepts_dash_separator_and_modifier_aliases() {
        let c = KeyCombo::parse("Strg-Menu-Cmd-F1").unwrap();
        assert!(c.ctrl && c.alt && c.win && !c.shift);
        assert_eq!(c.vk, 0x70);

        let c = KeyCombo::parse("control+option+meta+Delete").unwrap();
        assert!(c.ctrl && c.alt && c.win);
        assert_eq!(c.vk, 0x2E);

        assert!(KeyCombo::parse("Windows+Super+A").unwrap().win);
    }

    #[test]
    fn parse_bare_key_has_no_modifiers() {
        let c = KeyCombo::parse("Pause").unwrap();
        assert!(!c.ctrl && !c.alt && !c.shift && !c.win);
        assert_eq!(c.vk, 0x13);
    }

    #[test]
    fn parse_default_hotkeys_from_config() {
        let d = inputshare_core::SwitchingConfig::default();
        let enable = KeyCombo::parse(&d.enable_disable_hotkey).unwrap();
        assert_eq!(enable, KeyCombo { ctrl: true, alt: false, shift: true, win: false, vk: b'A' as u32 });
        let jump = KeyCombo::parse(&d.quick_jump_hotkey).unwrap();
        assert_eq!(jump, KeyCombo { ctrl: true, alt: false, shift: true, win: false, vk: b'Q' as u32 });
    }

    #[test]
    fn parse_rejects_empty_and_modifier_only() {
        assert_eq!(KeyCombo::parse(""), None);
        assert_eq!(KeyCombo::parse("   "), None);
        assert_eq!(KeyCombo::parse("Ctrl+Shift"), None, "no key part");
        assert_eq!(KeyCombo::parse("+"), None);
        assert_eq!(KeyCombo::parse("-"), None);
    }

    #[test]
    fn parse_rejects_two_main_keys_and_unknown_tokens() {
        assert_eq!(KeyCombo::parse("A+B"), None);
        assert_eq!(KeyCombo::parse("Ctrl+F1+F2"), None);
        assert_eq!(KeyCombo::parse("Ctrl+Banana"), None);
        assert_eq!(KeyCombo::parse("Ctrl+F13"), None);
        assert_eq!(KeyCombo::parse("Ctrl+"), None);
    }

    #[test]
    fn parse_ignores_empty_segments() {
        let c = KeyCombo::parse("Ctrl++A").unwrap();
        assert!(c.ctrl);
        assert_eq!(c.vk, b'A' as u32);
    }

    #[test]
    fn parse_duplicate_modifier_is_harmless() {
        let c = KeyCombo::parse("Ctrl+Ctrl+Z").unwrap();
        assert!(c.ctrl && !c.shift);
        assert_eq!(c.vk, b'Z' as u32);
    }

    // ---- vk_from_token ----

    #[test]
    fn token_named_keys() {
        let cases: [(&str, u32); 22] = [
            ("esc", 0x1B),
            ("Escape", 0x1B),
            ("tab", 0x09),
            ("space", 0x20),
            ("spacebar", 0x20),
            ("enter", 0x0D),
            ("return", 0x0D),
            ("backspace", 0x08),
            ("bksp", 0x08),
            ("delete", 0x2E),
            ("del", 0x2E),
            ("insert", 0x2D),
            ("ins", 0x2D),
            ("home", 0x24),
            ("end", 0x23),
            ("pageup", 0x21),
            ("pgup", 0x21),
            ("pagedown", 0x22),
            ("pgdn", 0x22),
            ("pause", 0x13),
            ("left", 0x25),
            ("right", 0x27),
        ];
        for (tok, vk) in cases {
            assert_eq!(vk_from_token(tok), Some(vk), "{tok}");
        }
        assert_eq!(vk_from_token("up"), Some(0x26));
        assert_eq!(vk_from_token("down"), Some(0x28));
    }

    #[test]
    fn token_function_keys_f1_to_f12_are_contiguous() {
        for n in 1u32..=12 {
            assert_eq!(vk_from_token(&format!("f{n}")), Some(0x6F + n), "f{n}");
        }
        assert_eq!(vk_from_token("f0"), None);
        assert_eq!(vk_from_token("f13"), None);
    }

    #[test]
    fn token_letters_and_digits() {
        assert_eq!(vk_from_token("a"), Some(0x41));
        assert_eq!(vk_from_token("Z"), Some(0x5A));
        assert_eq!(vk_from_token("0"), Some(0x30));
        assert_eq!(vk_from_token("9"), Some(0x39));
    }

    #[test]
    fn token_rejects_symbols_multichar_and_non_ascii() {
        assert_eq!(vk_from_token(""), None);
        assert_eq!(vk_from_token("ab"), None);
        assert_eq!(vk_from_token("!"), None);
        assert_eq!(vk_from_token(";"), None);
        assert_eq!(vk_from_token("ż"), None);
        assert_eq!(vk_from_token("10"), None);
    }

    // ---- QuickJump cycle ----

    fn tile(id: &str, label: &str, x: i32, y: i32, w: u32, h: u32, synced: bool) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: "PC".into(),
            label: label.into(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: synced,
        }
    }

    /// User's desk: A above B, D-B-C row (D lower).
    fn desk() -> MonitorLayout {
        MonitorLayout {
            monitors: vec![
                tile("c", "C", 2560, 0, 2560, 1440, true),
                tile("a", "A", 320, -1080, 1920, 1080, true),
                tile("d", "D", -2560, 400, 2560, 1440, true),
                tile("b", "B", 0, 0, 2560, 1440, true),
            ],
        }
    }

    #[test]
    fn jump_order_is_top_to_bottom_then_left_to_right() {
        let ids: Vec<_> = local_monitors_in_order(&desk()).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
    }

    #[test]
    fn jump_order_ties_break_on_x_then_label() {
        let layout = MonitorLayout {
            monitors: vec![
                tile("z", "Z", 0, 0, 100, 100, true),
                tile("y", "Y", 0, 0, 100, 100, true),
                tile("r", "R", 100, 0, 100, 100, true),
            ],
        };
        let ids: Vec<_> = local_monitors_in_order(&layout).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["y", "z", "r"]);
    }

    #[test]
    fn jump_skips_remote_tiles_but_falls_back_when_none_synced() {
        let mut layout = desk();
        layout.monitors.push(tile("remote", "R", 5120, 0, 1920, 1080, false));
        let ids: Vec<_> = local_monitors_in_order(&layout).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);

        let unsynced = MonitorLayout {
            monitors: vec![
                tile("m2", "B", 100, 0, 100, 100, false),
                tile("m1", "A", 0, 0, 100, 100, false),
            ],
        };
        let ids: Vec<_> = local_monitors_in_order(&unsynced).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["m1", "m2"]);
    }

    #[test]
    fn jump_goes_to_center_of_next_monitor() {
        let order = local_monitors_in_order(&desk());
        // Cursor on A -> center of B (0,0,2560x1440).
        assert_eq!(quick_jump_target(&order, 1000, -500), Some((1280, 720)));
        // Cursor on B -> center of C.
        assert_eq!(quick_jump_target(&order, 100, 100), Some((2560 + 1280, 720)));
        // Cursor on C -> center of D (-2560,400).
        assert_eq!(quick_jump_target(&order, 3000, 100), Some((-1280, 400 + 720)));
    }

    #[test]
    fn jump_wraps_from_last_to_first() {
        let order = local_monitors_in_order(&desk());
        // Cursor on D (last) -> back to A (320,-1080,1920x1080): center (1280, -540).
        assert_eq!(quick_jump_target(&order, -1000, 500), Some((320 + 960, -1080 + 540)));
    }

    #[test]
    fn jump_from_off_screen_cursor_counts_as_first_monitor() {
        let order = local_monitors_in_order(&desk());
        // Not on any monitor -> index 0 (A) -> next is B.
        assert_eq!(quick_jump_target(&order, 99_999, 99_999), Some((1280, 720)));
    }

    #[test]
    fn jump_with_single_monitor_targets_its_own_center() {
        let only = vec![tile("a", "A", 0, 0, 1920, 1080, true)];
        assert_eq!(quick_jump_target(&only, 10, 10), Some((960, 540)));
    }

    #[test]
    fn jump_with_no_monitors_does_nothing() {
        assert_eq!(quick_jump_target(&[], 0, 0), None);
    }

    #[test]
    fn jump_uses_integer_half_for_odd_sizes() {
        let only = vec![tile("a", "A", 10, 20, 101, 51, true)];
        assert_eq!(quick_jump_target(&only, 10, 20), Some((10 + 50, 20 + 25)));
    }

    #[test]
    fn point_in_is_half_open() {
        let m = tile("a", "A", 0, 0, 100, 100, true);
        assert!(point_in(&m, 0, 0));
        assert!(point_in(&m, 99, 99));
        assert!(!point_in(&m, 100, 50));
        assert!(!point_in(&m, 50, 100));
        assert!(!point_in(&m, -1, 0));
    }

    // ---- HotkeyControl flags ----

    #[test]
    fn toggle_and_jump_flags_are_one_shot() {
        let c = HotkeyControl::new("Ctrl+Shift+A", "Ctrl+Shift+Q", MonitorLayout::default(), true);
        assert!(!c.take_toggle_sharing());
        assert!(!c.take_quick_jump());

        c.toggle_sharing.store(true, Ordering::SeqCst);
        c.quick_jump.store(true, Ordering::SeqCst);
        assert!(c.take_toggle_sharing());
        assert!(!c.take_toggle_sharing());
        assert!(c.take_quick_jump());
        assert!(!c.take_quick_jump());
    }

    #[test]
    fn the_emergency_quit_combo_parses_and_needs_three_modifiers() {
        let c = KeyCombo::parse(EMERGENCY_QUIT).expect("parses");
        assert!(c.ctrl && c.alt && c.shift && !c.win);
        assert_eq!(c.vk, 'Q' as u32);
    }

    #[test]
    fn hotkey_control_setters_update_shared_state() {
        let c = HotkeyControl::new("Ctrl+A", "Ctrl+Q", MonitorLayout::default(), false);
        let shared = c.clone();
        assert!(!shared.sharing.load(Ordering::Relaxed));
        c.set_sharing(true);
        c.set_enable_hotkey("Alt+F9");
        c.set_jump_hotkey("Alt+F10");
        c.set_layout(desk());
        assert!(shared.sharing.load(Ordering::Relaxed));
        assert_eq!(*shared.enable_hotkey.lock().unwrap(), "Alt+F9");
        assert_eq!(*shared.jump_hotkey.lock().unwrap(), "Alt+F10");
        assert_eq!(shared.layout.lock().unwrap().monitors.len(), 4);
    }
}
