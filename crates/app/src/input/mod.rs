//! Windows capture / emulate / clipboard helpers used by the net hub + wrap edge.

mod clipboard;
pub mod clipboard_full;
mod cursor_draw;
mod cursor_shield;
pub mod drag_drop;
mod emulate;
pub mod key_hook;
mod keymap;

pub use clipboard::{get_clipboard_text, set_clipboard_text, simulate_paste};
pub use emulate::{apply_event, apply_lock_state, apply_scroll, place_on_edge, try_activate_layout};

use inputshare_protocol::{Edge, InputEvent, MouseButton};
use inputshare_core::HitMeter;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Shared flags between wrap edge, capture loop, and net hub.
#[derive(Clone)]
pub struct CaptureControl {
    /// We are sending input to a remote peer.
    pub capturing: Arc<AtomicBool>,
    /// Remote peer is injecting into us.
    pub being_controlled: Arc<AtomicBool>,
    /// Leave requested (hotkey / panic / peer Leave).
    pub leave_requested: Arc<AtomicBool>,
    pub mouse_speed: Arc<Mutex<f32>>,
    /// Update rate of the cursor on the computer being steered (not this mouse's USB rate).
    pub mouse_hz: Arc<AtomicU32>,
    pub output_scroll_v: Arc<Mutex<(f32, bool)>>,
    pub output_scroll_h: Arc<Mutex<(f32, bool)>>,
    /// Target peer hostname while capturing (for status).
    pub target_host: Arc<Mutex<Option<String>>>,
    /// Last injected cursor pos (for remote cursor overlay).
    pub inject_pos: Arc<Mutex<(i32, i32)>>,
    pub draw_cursor: Arc<AtomicBool>,
    pub large_cursor: Arc<AtomicBool>,
    /// Virtual cursor on the computer being steered: it gives the way back (see `RemoteSpace`).
    pub remote_space: Arc<Mutex<Option<inputshare_core::RemoteSpace>>>,
    /// When input from a steering peer last arrived.
    pub controlled_at: Arc<Mutex<Option<Instant>>>,
    /// Where the cursor is parked while steering (the cursor shield sits there).
    pub shield_at: Arc<Mutex<Option<(i32, i32)>>>,
    /// Coming back needs a hit at least this fast (px/s); 0 = any push.
    pub fast_hit_speed: Arc<AtomicU32>,
    /// A program on the computer being steered holds its cursor (a game): no way back by the edge.
    pub remote_cursor_locked: Arc<AtomicBool>,
}

/// A peer that stays silent this long no longer counts as steering this computer.
const CONTROLLED_IDLE: Duration = Duration::from_secs(300);

impl CaptureControl {
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            being_controlled: Arc::new(AtomicBool::new(false)),
            leave_requested: Arc::new(AtomicBool::new(false)),
            mouse_speed: Arc::new(Mutex::new(0.5)),
            mouse_hz: Arc::new(AtomicU32::new(1000)),
            output_scroll_v: Arc::new(Mutex::new((0.5, false))),
            output_scroll_h: Arc::new(Mutex::new((0.5, false))),
            target_host: Arc::new(Mutex::new(None)),
            inject_pos: Arc::new(Mutex::new((0, 0))),
            draw_cursor: Arc::new(AtomicBool::new(false)),
            large_cursor: Arc::new(AtomicBool::new(false)),
            remote_space: Arc::new(Mutex::new(None)),
            controlled_at: Arc::new(Mutex::new(None)),
            shield_at: Arc::new(Mutex::new(None)),
            fast_hit_speed: Arc::new(AtomicU32::new(0)),
            remote_cursor_locked: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A peer is steering this computer (called for every packet that arrives from it).
    pub fn touch_controlled(&self) {
        self.being_controlled.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.controlled_at.lock() {
            *g = Some(Instant::now());
        }
    }

    /// Another computer steers this one right now. While true this computer must not hand control
    /// onward: two computers doing that to each other make the cursor fly in a loop.
    pub fn is_controlled(&self) -> bool {
        self.being_controlled.load(Ordering::SeqCst)
            && self
                .controlled_at
                .lock()
                .ok()
                .and_then(|g| *g)
                .is_some_and(|t| t.elapsed() < CONTROLLED_IDLE)
    }

    pub fn begin_capture(&self, host: &str) {
        if let Ok(mut g) = self.target_host.lock() {
            *g = Some(host.to_string());
        }
        self.leave_requested.store(false, Ordering::SeqCst);
        self.remote_cursor_locked.store(false, Ordering::SeqCst);
        self.capturing.store(true, Ordering::SeqCst);
    }

    pub fn end_capture(&self) {
        self.capturing.store(false, Ordering::SeqCst);
        self.remote_cursor_locked.store(false, Ordering::SeqCst);
        if let Ok(mut g) = self.shield_at.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.remote_space.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.target_host.lock() {
            *g = None;
        }
    }

    pub fn request_leave(&self) {
        self.leave_requested.store(true, Ordering::SeqCst);
    }
}

impl Default for CaptureControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Channel-ish: capture thread pushes batches here; hub drains and sends UDP.
#[derive(Clone, Default)]
pub struct CaptureOutbox {
    events: Arc<Mutex<Vec<InputEvent>>>,
    enter: Arc<Mutex<Option<(String, Edge, f32)>>>,
    /// Id of the remote display the pending enter goes to (for the way back).
    enter_tile: Arc<Mutex<Option<String>>>,
    leave: Arc<AtomicBool>,
    /// Wakes the network loop the moment there is something to send. Sleeping instead made
    /// motion wait for the Windows timer (often 15.6 ms), so the cursor there floated and lagged.
    wake: Arc<tokio::sync::Notify>,
}

impl CaptureOutbox {
    pub fn push_events(&self, batch: Vec<InputEvent>) {
        if batch.is_empty() {
            return;
        }
        if let Ok(mut g) = self.events.lock() {
            g.extend(batch);
        }
        self.wake.notify_one();
    }

    /// Until something is pushed (events, enter, leave) or `at_most` has passed.
    pub async fn wait(&self, at_most: std::time::Duration) {
        let _ = tokio::time::timeout(at_most, self.wake.notified()).await;
    }

    pub fn take_events(&self) -> Vec<InputEvent> {
        self.events.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
    }

    pub fn request_enter(&self, host: String, edge: Edge, entry: f32) {
        if let Ok(mut g) = self.enter.lock() {
            *g = Some((host, edge, entry));
        }
        self.wake.notify_one();
    }

    pub fn set_enter_tile(&self, tile_id: String) {
        if let Ok(mut g) = self.enter_tile.lock() {
            *g = Some(tile_id);
        }
    }

    pub fn take_enter_tile(&self) -> Option<String> {
        self.enter_tile.lock().ok().and_then(|mut g| g.take())
    }

    pub fn take_enter(&self) -> Option<(String, Edge, f32)> {
        self.enter.lock().ok().and_then(|mut g| g.take())
    }

    pub fn signal_leave(&self) {
        self.leave.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    pub fn take_leave(&self) -> bool {
        self.leave.swap(false, Ordering::SeqCst)
    }
}

/// Spawn poll-based capture while `capturing` is true.
pub fn spawn_capture(ctrl: CaptureControl, outbox: CaptureOutbox) -> JoinHandle<()> {
    #[cfg(windows)]
    key_hook::spawn(ctrl.capturing.clone());
    thread::Builder::new()
        .name("inputshare-capture".into())
        .spawn(move || capture_loop(ctrl, outbox))
        .expect("spawn capture")
}

/// Spawn the window that hides the host cursor while steering another computer.
pub fn spawn_cursor_shield(ctrl: CaptureControl) -> JoinHandle<()> {
    cursor_shield::spawn(ctrl)
}

/// Spawn remote cursor overlay (no-op if draw disabled).
pub fn spawn_cursor_overlay(ctrl: CaptureControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-cursor".into())
        .spawn(move || cursor_draw::run(ctrl))
        .expect("spawn cursor overlay")
}

fn capture_loop(ctrl: CaptureControl, outbox: CaptureOutbox) {
    #[cfg(windows)]
    {
        win_capture::run(ctrl, outbox);
    }
    #[cfg(not(windows))]
    {
        let _ = (ctrl, outbox);
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

/// After entering, pushing back through the edge is ignored for this long (the jitter of the push
/// that carried the cursor over must not bounce it straight back).
#[cfg(any(windows, test))]
const LEAVE_GRACE: Duration = Duration::from_millis(150);

/// Feed one sent mouse delta to the virtual cursor on the other computer. `Some` = control returns
/// to this computer: back through the edge the user entered by (`Some(None)`: the cursor goes back
/// where it was), or through another edge that leads here (`Some(Some(point))`: it lands there).
/// `hits` holds the recent sent motion, for the fast-hit rule.
#[cfg(any(windows, test))]
fn virtual_cursor_left(
    ctrl: &CaptureControl,
    dx: i32,
    dy: i32,
    since_enter: Duration,
    hits: &HitMeter,
) -> Option<Option<(i32, i32)>> {
    // A game there holds its cursor: the mouse aims, the cursor does not travel to the edge.
    if ctrl.remote_cursor_locked.load(Ordering::Relaxed) {
        return None;
    }
    let min_speed = ctrl.fast_hit_speed.load(Ordering::Relaxed);
    let mut g = ctrl.remote_space.lock().ok()?;
    let space = g.as_mut()?;
    let grace_over = since_enter >= LEAVE_GRACE;
    // The speed counts toward the edge being crossed, whichever way back that is.
    space
        .advance_with(dx, dy, |toward| grace_over && hits.fast_enough(toward, min_speed))
        .then_some(space.landing)
}

/// How far (px, per axis) the cursor may drift from the capture anchor before it is pulled back.
#[cfg(test)]
const ANCHOR_DRIFT_PX: i32 = 40;

/// Map the 0..1 mouse-speed slider (clamped to 0.05..2.0) to a pointer multiplier
/// (0.5 default ≈ 1.125).
#[cfg(any(windows, test))]
fn pointer_speed_multiplier(speed: f32) -> f32 {
    0.25 + speed.clamp(0.05, 2.0) * 1.75
}

/// Relative delta between two cursor samples, scaled and rounded per tick
/// (no remainder carry-over). `None` when the rounded delta is zero.
#[cfg(test)]
fn scaled_mouse_delta(last: (i32, i32), cur: (i32, i32), mul: f32) -> Option<(i32, i32)> {
    let dx = ((cur.0 - last.0) as f32 * mul).round() as i32;
    let dy = ((cur.1 - last.1) as f32 * mul).round() as i32;
    if dx != 0 || dy != 0 {
        Some((dx, dy))
    } else {
        None
    }
}

/// Scale a raw device delta, keeping the fractional remainder so a slow move at a high rate is not
/// rounded away to nothing.
#[cfg(any(windows, test))]
fn scale_raw_delta(dx: i32, dy: i32, mul: f32, carry: &mut (f32, f32)) -> Option<(i32, i32)> {
    carry.0 += dx as f32 * mul;
    carry.1 += dy as f32 * mul;
    let ox = carry.0.trunc() as i32;
    let oy = carry.1.trunc() as i32;
    carry.0 -= ox as f32;
    carry.1 -= oy as f32;
    if ox != 0 || oy != 0 {
        Some((ox, oy))
    } else {
        None
    }
}

/// True when the cursor strayed more than [`ANCHOR_DRIFT_PX`] from the anchor on either axis.
#[cfg(test)]
fn drifted_from_anchor(cur: (i32, i32), anchor: (i32, i32)) -> bool {
    (cur.0 - anchor.0).abs() > ANCHOR_DRIFT_PX || (cur.1 - anchor.1).abs() > ANCHOR_DRIFT_PX
}

/// Index into the polled button VK list `[VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]`.
#[cfg(any(windows, test))]
fn mouse_button_for_index(i: usize) -> MouseButton {
    match i {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Back,
        _ => MouseButton::Forward,
    }
}

/// A mouse button polled while steering. A button already held when control went over (a file
/// drag begun on this computer) belongs to this computer: neither its press nor its release is
/// sent, so the other one shows no selection rectangle.
#[cfg(any(windows, test))]
#[derive(Debug, Default, Clone, Copy)]
struct PolledButton {
    down: bool,
    held_from_here: bool,
}

#[cfg(any(windows, test))]
impl PolledButton {
    /// Control just went over with the button `down`.
    fn arm(&mut self, down: bool) {
        self.down = down;
        self.held_from_here = down;
    }

    /// The button is `down` now: `Some(pressed)` when that is to be sent.
    fn poll(&mut self, down: bool) -> Option<bool> {
        if down == self.down {
            return None;
        }
        self.down = down;
        if std::mem::take(&mut self.held_from_here) {
            return None;
        }
        Some(down)
    }
}

/// Highest sample rate Windows reports for a connected mouse, when it reports one.
pub fn detected_mouse_sample_hz() -> Option<u32> {
    #[cfg(windows)]
    {
        win_capture::detected_mouse_sample_hz()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod win_capture {
    use super::*;
    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use ::windows::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
    };
    use ::windows::Win32::UI::Input::{
        GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices,
        HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST,
        RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICEINFO, RID_DEVICE_INFO, RID_INPUT,
        RIM_TYPEMOUSE,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GetCursorPos, GetSystemMetrics, PeekMessageW,
        RegisterClassW, SetCursorPos, HWND_MESSAGE, MSG, PM_REMOVE, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_INPUT, WNDCLASSW,
    };

    const CLASS: &str = "INTmouseRawMouse\0";
    /// Polled mouse buttons, in [`mouse_button_for_index`] order.
    const BUTTON_VKS: [i32; 5] = [0x01, 0x02, 0x04, 0x05, 0x06];

    pub fn run(ctrl: CaptureControl, outbox: CaptureOutbox) {
        let mut raw = RawMouse::open();
        let mut timer = TickTimer::open();
        let mut key_prev = [false; 256];
        let mut buttons = [PolledButton::default(); 5];
        let mut clip_anchor: Option<(i32, i32)> = None;
        let mut use_raw = false;
        let mut entered_at: Option<Instant> = None;
        let mut carry = (0.0_f32, 0.0_f32);
        // Sent motion summed up, so the hit meter can measure the speed of a push back.
        let mut travel = (0_i32, 0_i32);
        let mut hits = HitMeter::default();
        // Where the cursor was when the capture started: it goes back there on leaving.
        let mut home: Option<(i32, i32)> = None;
        // Since when raw input stays silent although the cursor moves (raw registration lost).
        let mut raw_silent_since: Option<Instant> = None;
        let mut last_keys = Instant::now();
        let mut reported_max: Option<u32> = None;
        let mut reported_at = Instant::now() - Duration::from_secs(30);

        loop {
            if reported_at.elapsed() >= Duration::from_secs(5) {
                reported_max = detected_mouse_sample_hz();
                reported_at = Instant::now();
            }
            let hz = inputshare_core::clamp_mouse_hz(
                ctrl.mouse_hz.load(Ordering::Relaxed),
                reported_max,
            );
            let capturing = ctrl.capturing.load(Ordering::SeqCst);
            // Arm before the wait, so the first period already collects device motion instead of
            // letting Windows clamp the cursor against the edge.
            if capturing && clip_anchor.is_none() {
                let mut pt = POINT::default();
                if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                    home = Some((pt.x, pt.y));
                    key_hook::clear_taps();
                    for (b, vk) in buttons.iter_mut().zip(BUTTON_VKS) {
                        b.arm(key_hook::key_down(vk));
                    }
                    raw_silent_since = None;
                    use_raw = raw.as_mut().is_some_and(|r| r.set_capture(true));
                    if use_raw {
                        clip_anchor = Some((pt.x, pt.y));
                    } else {
                        let parked = interior_point(pt.x, pt.y);
                        clip_anchor = Some(parked);
                        let _ = unsafe { SetCursorPos(parked.0, parked.1) };
                        tracing::warn!(
                            "raw mouse unavailable; cursor is parked inside the screen so motion still crosses"
                        );
                    }
                    entered_at = Some(Instant::now());
                    carry = (0.0, 0.0);
                }
            }
            if capturing {
                wait_tick(timer.as_mut(), inputshare_core::mouse_tick_period(hz));
            } else {
                thread::sleep(Duration::from_millis(8));
            }
            if let Some(raw) = raw.as_mut() {
                if !capturing {
                    raw.set_capture(false);
                    let _ = raw.drain();
                }
            }

            if ctrl.leave_requested.load(Ordering::SeqCst) && capturing {
                outbox.signal_leave();
                ctrl.end_capture();
                if let Some((hx, hy)) = home.take() {
                    let _ = unsafe { SetCursorPos(hx, hy) };
                }
                if let Some(raw) = raw.as_mut() {
                    raw.set_capture(false);
                }
                clip_anchor = None;
                use_raw = false;
                entered_at = None;
                carry = (0.0, 0.0);
                hits.clear();
                continue;
            }

            if !capturing {
                clip_anchor = None;
                use_raw = false;
                entered_at = None;
                carry = (0.0, 0.0);
                hits.clear();
                continue;
            }

            let speed = ctrl.mouse_speed.lock().ok().map(|g| *g).unwrap_or(0.5);
            let speed_mul = pointer_speed_multiplier(speed);

            let (rdx, rdy) = if use_raw {
                let raw_delta = raw.as_mut().map(|r| r.drain()).unwrap_or((0, 0));
                let moved = clip_anchor.map(cursor_delta_from).unwrap_or((0, 0));
                if raw_delta != (0, 0) || moved == (0, 0) {
                    if raw_delta != (0, 0) {
                        raw_silent_since = None;
                    }
                    raw_delta
                } else if raw_silent_since.get_or_insert_with(Instant::now).elapsed() >= RAW_SILENT_LIMIT {
                    // The mouse clearly moves but raw input says nothing (its registration was
                    // lost): read the cursor instead, parked away from the edges.
                    tracing::warn!("raw mouse input is silent while the cursor moves; using the cursor position");
                    if let Some(r) = raw.as_mut() {
                        r.set_capture(false);
                    }
                    use_raw = false;
                    let anchor = clip_anchor.unwrap_or_default();
                    let parked = interior_point(anchor.0, anchor.1);
                    clip_anchor = Some(parked);
                    let _ = unsafe { SetCursorPos(parked.0, parked.1) };
                    moved
                } else {
                    (0, 0)
                }
            } else if let Some(anchor) = clip_anchor {
                cursor_delta_from(anchor)
            } else {
                (0, 0)
            };

            let mut batch = Vec::new();
            if let Some((dx, dy)) = scale_raw_delta(rdx, rdy, speed_mul, &mut carry) {
                let since_enter = entered_at.get_or_insert_with(Instant::now).elapsed();
                travel = (travel.0.wrapping_add(dx), travel.1.wrapping_add(dy));
                hits.record(Instant::now(), travel.0, travel.1);
                if let Some(landing) = virtual_cursor_left(&ctrl, dx, dy, since_enter, &hits) {
                    // The leave block above ends the capture and puts the cursor back where it
                    // was, or where the edge it went out through leads on this computer.
                    if landing.is_some() {
                        home = landing;
                    }
                    ctrl.leave_requested.store(true, Ordering::SeqCst);
                    continue;
                }
                batch.push(InputEvent::MouseMove { dx, dy });
            }

            if let Some((ax, ay)) = clip_anchor {
                let _ = unsafe { SetCursorPos(ax, ay) };
            }
            if let Ok(mut g) = ctrl.shield_at.lock() {
                *g = clip_anchor;
            }

            let poll_keys = inputshare_core::mouse_tick_period(hz) >= Duration::from_millis(4)
                || last_keys.elapsed() >= Duration::from_millis(4);
            if poll_keys {
                last_keys = Instant::now();
                for (i, vk) in BUTTON_VKS.into_iter().enumerate() {
                    // Through the mouse hook: it keeps clicks from this computer while steering.
                    let down = key_hook::take_down(vk);
                    if let Some(pressed) = buttons[i].poll(down) {
                        batch.push(InputEvent::MouseButton {
                            button: mouse_button_for_index(i),
                            pressed,
                        });
                    }
                }
                for vk in 0x08u32..=0xFE {
                    // Generic Shift/Ctrl/Alt: the left/right codes below already carry them.
                    if matches!(vk, 0x01 | 0x02 | 0x04 | 0x05 | 0x06 | 0x10 | 0x11 | 0x12) {
                        continue;
                    }
                    let down = key_hook::take_down(vk as i32);
                    let idx = vk as usize;
                    if down != key_prev[idx] {
                        if let Some(key) = keymap::vk_to_key(vk) {
                            batch.push(InputEvent::Key { key, pressed: down });
                        }
                        key_prev[idx] = down;
                    }
                    // A held key repeats on the other computer as it would here.
                    let repeats = key_hook::take_repeats(vk);
                    if down && repeats > 0 {
                        if let Some(key) = keymap::vk_to_key(vk) {
                            for _ in 0..repeats {
                                batch.push(InputEvent::Key { key, pressed: true });
                            }
                        }
                    }
                }
                let (wheel_h, wheel_v) = key_hook::take_wheel();
                if wheel_h != 0.0 || wheel_v != 0.0 {
                    batch.push(InputEvent::Scroll { dx: wheel_h, dy: wheel_v });
                }
            }

            outbox.push_events(batch);
        }
    }

    /// How long raw input may stay silent while the cursor moves before the capture reads the
    /// cursor instead.
    const RAW_SILENT_LIMIT: Duration = Duration::from_millis(150);

    /// A point far enough from every screen edge that `GetCursorPos` still sees motion in both
    /// directions. Used only when raw input could not be opened.
    fn interior_point(x: i32, y: i32) -> (i32, i32) {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        let margin = 80;
        let ax = x.clamp(vx + margin, (vx + vw - margin).max(vx + margin));
        let ay = y.clamp(vy + margin, (vy + vh - margin).max(vy + margin));
        (ax, ay)
    }

    fn cursor_delta_from(anchor: (i32, i32)) -> (i32, i32) {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return (0, 0);
        }
        (pt.x - anchor.0, pt.y - anchor.1)
    }

    fn wait_tick(timer: Option<&mut TickTimer>, period: Duration) {
        if let Some(timer) = timer {
            if timer.wait(period) {
                return;
            }
        }
        if period >= Duration::from_millis(1) {
            thread::sleep(period);
        } else {
            let start = Instant::now();
            while start.elapsed() < period {
                thread::yield_now();
            }
        }
    }

    struct TickTimer {
        handle: HANDLE,
    }

    impl TickTimer {
        fn open() -> Option<Self> {
            let handle = unsafe {
                CreateWaitableTimerExW(
                    None,
                    PCWSTR::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )
                .ok()?
            };
            Some(Self { handle })
        }

        fn wait(&self, period: Duration) -> bool {
            let ticks = (period.as_nanos() / 100).max(1) as i64;
            let due = -ticks;
            unsafe {
                if SetWaitableTimer(self.handle, &due, 0, None, None, false).is_err() {
                    return false;
                }
                WaitForSingleObject(self.handle, 1000);
            }
            true
        }
    }

    struct RawMouse {
        hwnd: HWND,
        on: bool,
    }

    impl RawMouse {
        fn open() -> Option<Self> {
            unsafe {
                let hinst = GetModuleHandleW(None).ok()?;
                let class_name: Vec<u16> = CLASS.encode_utf16().collect();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(raw_wnd_proc),
                    hInstance: hinst.into(),
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(class_name.as_ptr()),
                    PCWSTR(class_name.as_ptr()),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    hinst,
                    None,
                )
                .ok()?;
                Some(Self { hwnd, on: false })
            }
        }

        fn set_capture(&mut self, on: bool) -> bool {
            if self.on == on {
                return true;
            }
            let device = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: if on { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
                hwndTarget: if on { self.hwnd } else { HWND::default() },
            };
            let ok = unsafe {
                RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
                    .is_ok()
            };
            if ok {
                self.on = on;
            } else if on {
                tracing::warn!("RegisterRawInputDevices failed");
            }
            ok
        }

        fn drain(&mut self) -> (i32, i32) {
            let mut dx = 0i32;
            let mut dy = 0i32;
            let mut msg = MSG::default();
            let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
            unsafe {
                while PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message != WM_INPUT {
                        let _ = DefWindowProcW(msg.hwnd, msg.message, msg.wParam, msg.lParam);
                        continue;
                    }
                    let h = HRAWINPUT(msg.lParam.0 as *mut core::ffi::c_void);
                    let mut size = 0u32;
                    if GetRawInputData(h, RID_INPUT, None, &mut size, header) == u32::MAX || size == 0
                    {
                        continue;
                    }
                    let mut storage = vec![0u64; (size as usize).div_ceil(8)];
                    let mut got = size;
                    if GetRawInputData(
                        h,
                        RID_INPUT,
                        Some(storage.as_mut_ptr() as *mut _),
                        &mut got,
                        header,
                    ) == u32::MAX
                    {
                        continue;
                    }
                    let raw = &*(storage.as_ptr() as *const RAWINPUT);
                    if raw.header.dwType != RIM_TYPEMOUSE.0 {
                        continue;
                    }
                    let mouse = raw.data.mouse;
                    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 {
                        continue;
                    }
                    dx = dx.saturating_add(mouse.lLastX);
                    dy = dy.saturating_add(mouse.lLastY);
                }
            }
            (dx, dy)
        }
    }

    unsafe extern "system" fn raw_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    pub fn detected_mouse_sample_hz() -> Option<u32> {
        unsafe {
            let mut count = 0u32;
            let stride = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
            if GetRawInputDeviceList(None, &mut count, stride) == u32::MAX || count == 0 {
                return None;
            }
            let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
            let got = GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, stride);
            if got == u32::MAX {
                return None;
            }
            list.truncate(got as usize);
            let mut max_hz = 0u32;
            for dev in list {
                if dev.dwType != RIM_TYPEMOUSE {
                    continue;
                }
                let mut info = RID_DEVICE_INFO {
                    cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32,
                    ..RID_DEVICE_INFO::default()
                };
                let mut size = info.cbSize;
                if GetRawInputDeviceInfoW(
                    dev.hDevice,
                    RIDI_DEVICEINFO,
                    Some(&mut info as *mut _ as *mut _),
                    &mut size,
                ) == u32::MAX
                {
                    continue;
                }
                let rate = info.Anonymous.mouse.dwSampleRate;
                if rate > max_hz {
                    max_hz = rate;
                }
            }
            if max_hz >= 125 {
                Some(max_hz)
            } else {
                None
            }
        }
    }
}

// Tests touch only in-memory state: no capture thread is spawned, no cursor/input API is called.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_steers_this_computer_until_it_leaves_or_goes_quiet() {
        let c = CaptureControl::new();
        assert!(!c.is_controlled());
        c.touch_controlled();
        assert!(c.is_controlled());
        c.being_controlled.store(false, Ordering::SeqCst);
        assert!(!c.is_controlled(), "Leave clears it");
        c.touch_controlled();
        *c.controlled_at.lock().unwrap() = Instant::now().checked_sub(CONTROLLED_IDLE + Duration::from_secs(1));
        assert!(!c.is_controlled(), "a peer that vanished without Leave stops counting");
    }

    #[test]
    fn pushing_back_through_the_entry_edge_asks_to_leave_but_not_during_the_grace_time() {
        use inputshare_core::{MonitorLayout, MonitorTile, RemoteSpace};
        let tile = MonitorTile {
            id: "PC2:A".into(),
            host: "PC2".into(),
            label: "A".into(),
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
            primary: false,
            system_synced: false,
        };
        let layout = MonitorLayout { monitors: vec![tile] };
        let c = CaptureControl::new();
        // no space known: behaves as before, never leaves by itself
        let none = HitMeter::default();
        assert!(virtual_cursor_left(&c, -500, 0, Duration::from_secs(5), &none).is_none());
        *c.remote_space.lock().unwrap() = RemoteSpace::enter(&layout, "PC2", None, Edge::Left, 0.5);
        assert!(virtual_cursor_left(&c, -5, 0, Duration::from_millis(10), &none).is_none(), "inside the grace time");
        assert_eq!(
            virtual_cursor_left(&c, -5, 0, Duration::from_millis(400), &none),
            Some(None),
            "after it, back out of the left edge, to where the cursor was"
        );
        c.end_capture();
        assert!(c.remote_space.lock().unwrap().is_none(), "ending the capture forgets the space");
    }

    #[test]
    fn with_fast_hit_only_a_quick_push_back_leaves_and_a_held_cursor_never_does() {
        use inputshare_core::{MonitorLayout, MonitorTile, RemoteSpace};
        let tile = MonitorTile {
            id: "PC2:A".into(),
            host: "PC2".into(),
            label: "A".into(),
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
            primary: false,
            system_synced: false,
        };
        let layout = MonitorLayout { monitors: vec![tile] };
        let c = CaptureControl::new();
        c.fast_hit_speed.store(1500, Ordering::SeqCst);
        *c.remote_space.lock().unwrap() = RemoteSpace::enter(&layout, "PC2", None, Edge::Left, 0.5);
        let t0 = Instant::now();
        let after = Duration::from_millis(400);

        // slow: 10 px in 50 ms = 200 px/s
        let mut slow = HitMeter::default();
        slow.record(t0, 0, 0);
        slow.record(t0 + Duration::from_millis(50), -10, 0);
        assert!(virtual_cursor_left(&c, -10, 0, after, &slow).is_none(), "a slow slide stops at the edge");

        // fast: 200 px in 50 ms = 4000 px/s
        let mut fast = HitMeter::default();
        fast.record(t0, 0, 0);
        fast.record(t0 + Duration::from_millis(50), -200, 0);
        c.remote_cursor_locked.store(true, Ordering::SeqCst);
        assert!(virtual_cursor_left(&c, -200, 0, after, &fast).is_none(), "a game holds the cursor there");
        c.remote_cursor_locked.store(false, Ordering::SeqCst);
        assert!(virtual_cursor_left(&c, -200, 0, after, &fast).is_some(), "a flick goes back");
    }

    #[test]
    fn speed_multiplier_default_slider() {
        let m = pointer_speed_multiplier(0.5);
        assert!((m - 1.125).abs() < 1e-6, "got {m}");
    }

    #[test]
    fn speed_multiplier_is_clamped_to_005_2() {
        let lo = pointer_speed_multiplier(0.0);
        let lo_edge = pointer_speed_multiplier(0.05);
        assert!((lo - lo_edge).abs() < 1e-6, "0.0 clamps up to 0.05");
        assert!((lo - (0.25 + 0.05 * 1.75)).abs() < 1e-6);

        let hi = pointer_speed_multiplier(99.0);
        assert!((hi - (0.25 + 2.0 * 1.75)).abs() < 1e-6, "got {hi}");
    }

    #[test]
    fn mouse_delta_none_when_cursor_did_not_move() {
        assert_eq!(scaled_mouse_delta((10, 10), (10, 10), 1.0), None);
    }

    #[test]
    fn mouse_delta_is_relative_and_signed() {
        assert_eq!(scaled_mouse_delta((100, 100), (103, 96), 1.0), Some((3, -4)));
        assert_eq!(scaled_mouse_delta((-5, 0), (-8, 0), 1.0), Some((-3, 0)));
    }

    #[test]
    fn mouse_delta_scales_with_multiplier() {
        assert_eq!(scaled_mouse_delta((0, 0), (10, -10), 2.0), Some((20, -20)));
        assert_eq!(scaled_mouse_delta((0, 0), (10, 4), 0.5), Some((5, 2)));
    }

    #[test]
    fn raw_delta_keeps_the_fraction_so_slow_motion_is_not_lost() {
        let mut carry = (0.0, 0.0);
        assert_eq!(scale_raw_delta(1, 0, 0.4, &mut carry), None);
        assert_eq!(scale_raw_delta(1, 0, 0.4, &mut carry), None);
        assert_eq!(scale_raw_delta(1, 0, 0.4, &mut carry), Some((1, 0)));
        // 0.2 remains, so one pixel back is not a full pixel yet.
        assert_eq!(scale_raw_delta(-1, 2, 1.0, &mut carry), Some((0, 2)));
        assert_eq!(scale_raw_delta(-1, 0, 1.0, &mut carry), Some((-1, 0)));
    }

    #[test]
    fn mouse_delta_rounds_per_tick_without_carrying_remainder() {
        // 1px at 0.4x rounds to 0 and is dropped: the remainder is not accumulated.
        assert_eq!(scaled_mouse_delta((0, 0), (1, 0), 0.4), None);
        // 1px at 0.6x rounds up to 1 (growth, not loss).
        assert_eq!(scaled_mouse_delta((0, 0), (1, 0), 0.6), Some((1, 0)));
        // One axis can vanish while the other survives.
        assert_eq!(scaled_mouse_delta((0, 0), (1, 5), 0.4), Some((0, 2)));
    }

    #[test]
    fn anchor_drift_is_strictly_more_than_40px_on_either_axis() {
        let a = (500, 500);
        assert!(!drifted_from_anchor((500, 500), a));
        assert!(!drifted_from_anchor((540, 460), a), "exactly 40 is still ok");
        assert!(drifted_from_anchor((541, 500), a));
        assert!(drifted_from_anchor((500, 459), a));
        assert!(drifted_from_anchor((100, 100), a));
    }

    #[test]
    fn a_button_held_when_control_goes_over_is_never_sent() {
        let mut b = PolledButton::default();
        b.arm(true);
        assert_eq!(b.poll(true), None, "still held: the drag on this computer goes on");
        assert_eq!(b.poll(false), None, "its release stays here too");
        assert_eq!(b.poll(true), Some(true), "a new click goes over");
        assert_eq!(b.poll(false), Some(false));

        let mut free = PolledButton::default();
        free.arm(false);
        assert_eq!(free.poll(false), None);
        assert_eq!(free.poll(true), Some(true));
    }

    #[test]
    fn button_index_matches_polled_vk_order() {
        assert_eq!(mouse_button_for_index(0), MouseButton::Left);
        assert_eq!(mouse_button_for_index(1), MouseButton::Right);
        assert_eq!(mouse_button_for_index(2), MouseButton::Middle);
        assert_eq!(mouse_button_for_index(3), MouseButton::Back);
        assert_eq!(mouse_button_for_index(4), MouseButton::Forward);
    }

    #[test]
    fn capture_control_defaults() {
        let c = CaptureControl::new();
        assert!(!c.capturing.load(Ordering::SeqCst));
        assert!(!c.being_controlled.load(Ordering::SeqCst));
        assert!(!c.leave_requested.load(Ordering::SeqCst));
        assert!(!c.draw_cursor.load(Ordering::SeqCst));
        assert!(!c.large_cursor.load(Ordering::SeqCst));
        assert_eq!(*c.mouse_speed.lock().unwrap(), 0.5);
        assert_eq!(c.mouse_hz.load(Ordering::SeqCst), 1000);
        assert_eq!(*c.output_scroll_v.lock().unwrap(), (0.5, false));
        assert_eq!(*c.output_scroll_h.lock().unwrap(), (0.5, false));
        assert_eq!(*c.target_host.lock().unwrap(), None);
        assert_eq!(*c.inject_pos.lock().unwrap(), (0, 0));
    }

    #[test]
    fn begin_capture_sets_target_and_clears_pending_leave() {
        let c = CaptureControl::default();
        c.request_leave();
        assert!(c.leave_requested.load(Ordering::SeqCst));

        c.begin_capture("LAPTOP");
        assert!(c.capturing.load(Ordering::SeqCst));
        assert!(!c.leave_requested.load(Ordering::SeqCst));
        assert_eq!(c.target_host.lock().unwrap().as_deref(), Some("LAPTOP"));
    }

    #[test]
    fn end_capture_clears_target_but_not_leave_flag() {
        let c = CaptureControl::new();
        c.begin_capture("LAPTOP");
        c.request_leave();
        c.end_capture();
        assert!(!c.capturing.load(Ordering::SeqCst));
        assert_eq!(*c.target_host.lock().unwrap(), None);
        assert!(c.leave_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn capture_control_clones_share_state() {
        let a = CaptureControl::new();
        let b = a.clone();
        a.begin_capture("X");
        assert!(b.capturing.load(Ordering::SeqCst));
        b.end_capture();
        assert!(!a.capturing.load(Ordering::SeqCst));
    }

    #[test]
    fn outbox_events_are_drained_in_order_once() {
        let o = CaptureOutbox::default();
        assert!(o.take_events().is_empty());

        o.push_events(vec![InputEvent::MouseMove { dx: 1, dy: 2 }]);
        o.push_events(vec![]); // empty batch is ignored
        o.push_events(vec![
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            InputEvent::Scroll { dx: 0.0, dy: 1.0 },
        ]);

        let got = o.take_events();
        assert_eq!(got.len(), 3);
        assert!(matches!(got[0], InputEvent::MouseMove { dx: 1, dy: 2 }));
        assert!(matches!(
            got[1],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }
        ));
        assert!(matches!(got[2], InputEvent::Scroll { .. }));
        assert!(o.take_events().is_empty(), "second drain is empty");
    }

    #[test]
    fn outbox_enter_request_is_taken_once_and_latest_wins() {
        let o = CaptureOutbox::default();
        assert!(o.take_enter().is_none());

        o.request_enter("A".into(), Edge::Right, 0.25);
        o.request_enter("B".into(), Edge::Left, 0.75);
        let (host, edge, entry) = o.take_enter().expect("enter pending");
        assert_eq!(host, "B");
        assert_eq!(edge, Edge::Left);
        assert_eq!(entry, 0.75);
        assert!(o.take_enter().is_none());
    }

    #[test]
    fn outbox_leave_signal_is_edge_triggered() {
        let o = CaptureOutbox::default();
        assert!(!o.take_leave());
        o.signal_leave();
        o.signal_leave();
        assert!(o.take_leave());
        assert!(!o.take_leave());
    }

    #[test]
    fn outbox_clones_share_queues() {
        let a = CaptureOutbox::default();
        let b = a.clone();
        a.push_events(vec![InputEvent::MouseMove { dx: 5, dy: 5 }]);
        assert_eq!(b.take_events().len(), 1);
        assert!(a.take_events().is_empty());
    }

    #[tokio::test]
    async fn the_network_loop_wakes_at_once_when_motion_is_pushed() {
        let outbox = CaptureOutbox::default();
        let o2 = outbox.clone();
        let started = Instant::now();
        let waiter = tokio::spawn(async move { o2.wait(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        outbox.push_events(vec![InputEvent::MouseMove { dx: 1, dy: 0 }]);
        waiter.await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "woken by the push, not the timeout");
        assert_eq!(outbox.take_events().len(), 1);
        // a push with nobody waiting is kept for the next wait
        outbox.push_events(vec![InputEvent::MouseMove { dx: 2, dy: 0 }]);
        let t = Instant::now();
        outbox.wait(Duration::from_secs(5)).await;
        assert!(t.elapsed() < Duration::from_millis(500));
    }
}

