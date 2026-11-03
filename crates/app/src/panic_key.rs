//! Global panic key — disables sharing / local wrap when triggered.

use inputshare_core::PanicMode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct PanicControl {
    pub enabled: Arc<AtomicBool>,
    /// Fired once when panic triggers — GUI clears sharing.
    pub triggered: Arc<AtomicBool>,
    mode_hold: Arc<AtomicBool>,
    /// VK code (default Escape = 0x1B)
    vk: Arc<std::sync::atomic::AtomicU32>,
}

impl PanicControl {
    pub fn new(enabled: bool, mode: PanicMode, key_name: &str) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            triggered: Arc::new(AtomicBool::new(false)),
            mode_hold: Arc::new(AtomicBool::new(matches!(mode, PanicMode::Hold))),
            vk: Arc::new(std::sync::atomic::AtomicU32::new(vk_from_name(key_name))),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn set_mode(&self, mode: PanicMode) {
        self.mode_hold
            .store(matches!(mode, PanicMode::Hold), Ordering::Relaxed);
    }

    pub fn set_key(&self, key_name: &str) {
        self.vk
            .store(vk_from_name(key_name), Ordering::Relaxed);
    }

    pub fn take_triggered(&self) -> bool {
        self.triggered
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }
}

fn vk_from_name(name: &str) -> u32 {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "escape" | "esc" => 0x1B,
        "pause" => 0x13,
        "f12" => 0x7B,
        "f9" => 0x78,
        _ => 0x1B,
    }
}

pub fn spawn(ctrl: PanicControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-panic".into())
        .spawn(move || run_loop(ctrl))
        .expect("spawn panic thread")
}

fn run_loop(ctrl: PanicControl) {
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

/// Hold mode: key must stay down this long.
#[cfg(any(windows, test))]
const HOLD_MS: u64 = 450;
/// Double-press mode: max gap between two presses.
#[cfg(any(windows, test))]
const DOUBLE_PRESS_MS: u64 = 400;

/// Panic-key state machine. Pure: the caller feeds the key state and the current time,
/// so it can be driven deterministically (the poll loop passes `Instant::now()`).
#[cfg(any(windows, test))]
#[derive(Debug, Default)]
struct PanicMachine {
    was_down: bool,
    hold_since: Option<Instant>,
    last_tap: Option<Instant>,
}

#[cfg(any(windows, test))]
impl PanicMachine {
    /// Called every poll while the panic key is disabled.
    fn reset_disabled(&mut self) {
        self.was_down = false;
        self.hold_since = None;
    }

    /// One poll. `down` = panic key currently pressed. Returns true when panic fires.
    fn step(&mut self, down: bool, hold_mode: bool, now: Instant) -> bool {
        if hold_mode {
            if down {
                let since = *self.hold_since.get_or_insert(now);
                if now.duration_since(since) >= Duration::from_millis(HOLD_MS) {
                    self.hold_since = None;
                    return true;
                }
            } else {
                self.hold_since = None;
            }
            false
        } else {
            // Double-press
            let mut fired = false;
            if down && !self.was_down {
                if let Some(prev) = self.last_tap {
                    if now.duration_since(prev) < Duration::from_millis(DOUBLE_PRESS_MS) {
                        fired = true;
                        self.last_tap = None;
                    } else {
                        self.last_tap = Some(now);
                    }
                } else {
                    self.last_tap = Some(now);
                }
            }
            self.was_down = down;
            fired
        }
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    // The keyboard hook's state: Esc pressed while steering never reaches Windows here.
    use crate::input::key_hook::key_down;

    pub fn run(ctrl: PanicControl) {
        let mut machine = PanicMachine::default();

        loop {
            thread::sleep(Duration::from_millis(15));
            if !ctrl.enabled.load(Ordering::Relaxed) {
                machine.reset_disabled();
                continue;
            }

            let vk = ctrl.vk.load(Ordering::Relaxed) as i32;
            let down = key_down(vk);
            let hold_mode = ctrl.mode_hold.load(Ordering::Relaxed);

            if machine.step(down, hold_mode, Instant::now()) {
                ctrl.triggered.store(true, Ordering::SeqCst);
                if hold_mode {
                    // Wait for release to avoid retrigger
                    while key_down(vk) {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
    }
}

// The state machine is driven with a fake clock (base Instant + offsets); no key state is read.
#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ---- key name -> VK ----

    #[test]
    fn key_names_map_to_vk() {
        assert_eq!(vk_from_name("escape"), 0x1B);
        assert_eq!(vk_from_name("esc"), 0x1B);
        assert_eq!(vk_from_name("pause"), 0x13);
        assert_eq!(vk_from_name("f12"), 0x7B);
        assert_eq!(vk_from_name("f9"), 0x78);
    }

    #[test]
    fn key_names_are_case_and_whitespace_insensitive() {
        assert_eq!(vk_from_name("  PAUSE "), 0x13);
        assert_eq!(vk_from_name("F12"), 0x7B);
        assert_eq!(vk_from_name("Esc"), 0x1B);
    }

    #[test]
    fn unknown_key_name_falls_back_to_escape() {
        assert_eq!(vk_from_name(""), 0x1B);
        assert_eq!(vk_from_name("f1"), 0x1B);
        assert_eq!(vk_from_name("banana"), 0x1B);
    }

    // ---- PanicControl flags ----

    #[test]
    fn control_initial_state_and_mode() {
        let c = PanicControl::new(true, PanicMode::Hold, "pause");
        assert!(c.enabled.load(Ordering::Relaxed));
        assert!(!c.triggered.load(Ordering::Relaxed));
        assert!(c.mode_hold.load(Ordering::Relaxed));
        assert_eq!(c.vk.load(Ordering::Relaxed), 0x13);

        let c = PanicControl::new(false, PanicMode::DoublePress, "f12");
        assert!(!c.enabled.load(Ordering::Relaxed));
        assert!(!c.mode_hold.load(Ordering::Relaxed));
        assert_eq!(c.vk.load(Ordering::Relaxed), 0x7B);
    }

    #[test]
    fn control_setters_update_shared_state() {
        let c = PanicControl::new(true, PanicMode::Hold, "esc");
        let shared = c.clone();
        c.set_enabled(false);
        c.set_mode(PanicMode::DoublePress);
        c.set_key("f9");
        assert!(!shared.enabled.load(Ordering::Relaxed));
        assert!(!shared.mode_hold.load(Ordering::Relaxed));
        assert_eq!(shared.vk.load(Ordering::Relaxed), 0x78);
        c.set_mode(PanicMode::Hold);
        assert!(shared.mode_hold.load(Ordering::Relaxed));
    }

    #[test]
    fn take_triggered_is_one_shot() {
        let c = PanicControl::new(true, PanicMode::Hold, "esc");
        assert!(!c.take_triggered());
        c.triggered.store(true, Ordering::SeqCst);
        assert!(c.take_triggered());
        assert!(!c.take_triggered(), "flag cleared after first take");
    }

    // ---- hold mode ----

    #[test]
    fn hold_fires_after_450ms_of_continuous_press() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        assert!(!m.step(true, true, t0));
        assert!(!m.step(true, true, t0 + ms(200)));
        assert!(!m.step(true, true, t0 + ms(449)));
        assert!(m.step(true, true, t0 + ms(450)), "exactly 450ms fires (>=)");
    }

    #[test]
    fn hold_released_early_does_not_fire_and_resets_timer() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        assert!(!m.step(true, true, t0));
        assert!(!m.step(true, true, t0 + ms(300)));
        assert!(!m.step(false, true, t0 + ms(330))); // released
        // New press starts counting from scratch.
        assert!(!m.step(true, true, t0 + ms(360)));
        assert!(!m.step(true, true, t0 + ms(360 + 449)));
        assert!(m.step(true, true, t0 + ms(360 + 450)));
    }

    #[test]
    fn hold_never_fires_without_press() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        for i in 0..100 {
            assert!(!m.step(false, true, t0 + ms(i * 15)));
        }
    }

    #[test]
    fn hold_restarts_after_firing() {
        // After firing the timer is cleared; a still-held key starts a new 450ms window.
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, true, t0);
        assert!(m.step(true, true, t0 + ms(450)));
        assert!(!m.step(true, true, t0 + ms(465)));
        assert!(!m.step(true, true, t0 + ms(465 + 449)));
        assert!(m.step(true, true, t0 + ms(465 + 450)));
    }

    #[test]
    fn hold_mode_ignores_double_press_state() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        // Two quick taps in hold mode must not fire.
        assert!(!m.step(true, true, t0));
        assert!(!m.step(false, true, t0 + ms(50)));
        assert!(!m.step(true, true, t0 + ms(100)));
        assert!(!m.step(false, true, t0 + ms(150)));
    }

    // ---- double-press mode ----

    #[test]
    fn double_press_within_400ms_fires_on_second_press() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        assert!(!m.step(true, false, t0)); // 1st press
        assert!(!m.step(false, false, t0 + ms(60)));
        assert!(m.step(true, false, t0 + ms(200)), "2nd press inside window");
    }

    #[test]
    fn double_press_window_is_strictly_below_400ms() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, false, t0);
        m.step(false, false, t0 + ms(50));
        assert!(m.step(true, false, t0 + ms(399)));

        let mut m = PanicMachine::default();
        m.step(true, false, t0);
        m.step(false, false, t0 + ms(50));
        assert!(!m.step(true, false, t0 + ms(400)), "exactly 400ms is too slow");
    }

    #[test]
    fn slow_second_press_becomes_new_first_press() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, false, t0);
        m.step(false, false, t0 + ms(50));
        assert!(!m.step(true, false, t0 + ms(1000))); // too late: restarts the sequence
        m.step(false, false, t0 + ms(1050));
        assert!(m.step(true, false, t0 + ms(1200)), "within 400ms of the new first press");
    }

    #[test]
    fn held_key_counts_as_one_press() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        // Key stays down across many polls: only the rising edge counts.
        for i in 0..20 {
            assert!(!m.step(true, false, t0 + ms(i * 15)));
        }
    }

    #[test]
    fn double_press_resets_after_firing() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, false, t0);
        m.step(false, false, t0 + ms(30));
        assert!(m.step(true, false, t0 + ms(100)));
        m.step(false, false, t0 + ms(130));
        // A third press right after must not fire: it is a fresh first press.
        assert!(!m.step(true, false, t0 + ms(200)));
        m.step(false, false, t0 + ms(230));
        assert!(m.step(true, false, t0 + ms(300)));
    }

    #[test]
    fn double_press_triple_tap_fires_once_per_pair() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        let mut fires = 0;
        for t in [0u64, 100, 200, 300, 400, 500] {
            if m.step(true, false, t0 + ms(t)) {
                fires += 1;
            }
            m.step(false, false, t0 + ms(t + 40));
        }
        assert_eq!(fires, 3, "presses pair up: (0,100) (200,300) (400,500)");
    }

    // ---- enable / disable ----

    #[test]
    fn disabling_clears_hold_and_edge_state() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, true, t0);
        m.step(true, true, t0 + ms(300));
        m.reset_disabled();
        // Hold counting restarts from the first poll after re-enable.
        assert!(!m.step(true, true, t0 + ms(1000)));
        assert!(!m.step(true, true, t0 + ms(1449)));
        assert!(m.step(true, true, t0 + ms(1450)));
    }

    #[test]
    fn disabling_forgets_key_down_edge_but_keeps_last_tap() {
        let t0 = Instant::now();
        let mut m = PanicMachine::default();
        m.step(true, false, t0); // first press recorded, key still down
        m.reset_disabled(); // was_down = false (last_tap is intentionally kept)
        // Still-held key now looks like a fresh rising edge -> counts as 2nd press if in time.
        assert!(m.step(true, false, t0 + ms(100)));
    }
}
