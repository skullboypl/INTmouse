//! Local multi-monitor cursor wrap (one PC) + edge hand-off to remote hosts.
//!
//! Axis-aware loop (not whole virtual-desktop bounce):
//! - ↑ from top monitor → bottom of the monitor under the **same X** (A → B)
//! - ← from leftmost → right of the monitor on the **same Y** (D → C)
//! - → from rightmost → left of the monitor on the **same Y** (C → D)
//!
//! When [`neighbor_across`] hits a tile whose `host` ≠ local hostname, we do **not**
//! OS-wrap — we ask the net hub to enter remote capture instead.
//!
//! Switching gates (`SwitchingConfig` / `allow_switch`) run before each wrap/enter.

use crate::input::{CaptureControl, CaptureOutbox};
use crate::win_query::{self, CORNER_PX};
use inputshare_core::{
    allow_switch, local_hostname, HitMeter, MonitorLayout, SwitchContext, SwitchingConfig,
};
use inputshare_protocol::Edge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct WrapControl {
    pub enabled: Arc<AtomicBool>,
    pub sharing: Arc<AtomicBool>,
    layout: Arc<Mutex<MonitorLayout>>,
    switching: Arc<Mutex<SwitchingConfig>>,
    /// When set, remote-edge crossings enqueue Enter on this outbox.
    net_outbox: Arc<Mutex<Option<CaptureOutbox>>>,
    /// True while we are capturing toward a remote (skip local wrap). Used until
    /// [`Self::bind_capture`] connects the real capture flags.
    remote_capturing: Arc<AtomicBool>,
    /// The capture flags. Bound at startup so "a peer steers us" is seen at once, not a
    /// frame later through the window: otherwise the steered computer grabs its own edge
    /// and the cursor on it cannot be moved.
    capture: Arc<Mutex<Option<CaptureControl>>>,
    /// Lower-case names of the computers that are linked right now. A display of a computer that is
    /// not linked is no hand-off target: the edge behaves like any other edge.
    remote_up: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl WrapControl {
    pub fn new(wrap: bool, sharing: bool, layout: MonitorLayout, switching: SwitchingConfig) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(wrap)),
            sharing: Arc::new(AtomicBool::new(sharing)),
            layout: Arc::new(Mutex::new(layout)),
            switching: Arc::new(Mutex::new(switching)),
            net_outbox: Arc::new(Mutex::new(None)),
            remote_capturing: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(Mutex::new(None)),
            remote_up: Arc::new(Mutex::new(Default::default())),
        }
    }

    pub fn set_net_outbox(&self, outbox: CaptureOutbox) {
        if let Ok(mut g) = self.net_outbox.lock() {
            *g = Some(outbox);
        }
    }

    pub fn set_remote_capturing(&self, on: bool) {
        self.remote_capturing.store(on, Ordering::Relaxed);
    }

    /// Share the hub's capture flags. After this, the edge reads them directly.
    pub fn bind_capture(&self, capture: CaptureControl) {
        if let Ok(mut g) = self.capture.lock() {
            *g = Some(capture);
        }
    }

    fn peer_steers_us(&self) -> bool {
        self.capture
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.is_controlled()))
            .unwrap_or(false)
    }

    fn we_are_capturing(&self) -> bool {
        if let Ok(g) = self.capture.lock() {
            if let Some(c) = g.as_ref() {
                return c.capturing.load(Ordering::Relaxed);
            }
        }
        self.remote_capturing.load(Ordering::Relaxed)
    }

    pub fn set_remote_hosts_up(&self, hosts: Vec<String>) {
        if let Ok(mut g) = self.remote_up.lock() {
            *g = hosts.into_iter().map(|h| h.to_lowercase()).collect();
        }
    }

    fn remote_is_up(&self, host: &str) -> bool {
        self.remote_up
            .lock()
            .map(|g| g.contains(&host.to_lowercase()))
            .unwrap_or(false)
    }

    pub fn set_wrap(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn set_sharing(&self, on: bool) {
        self.sharing.store(on, Ordering::Relaxed);
    }

    pub fn set_layout(&self, layout: MonitorLayout) {
        if let Ok(mut g) = self.layout.lock() {
            *g = layout;
        }
    }

    pub fn set_switching(&self, cfg: SwitchingConfig) {
        if let Ok(mut g) = self.switching.lock() {
            *g = cfg;
        }
    }
}

pub fn spawn(ctrl: WrapControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-local-wrap".into())
        .spawn(move || run_loop(ctrl))
        .expect("spawn wrap thread")
}

fn run_loop(ctrl: WrapControl) {
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

/// Second tap on the same blocked edge must land within this window.
#[cfg(any(windows, test))]
const DOUBLE_TAP_MS: u64 = 400;

/// Compact edge id used to remember which edge was blocked last.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeKey(u8);

#[cfg(any(windows, test))]
impl EdgeKey {
    fn from_edge(e: Edge) -> Self {
        Self(match e {
            Edge::Top => 0,
            Edge::Bottom => 1,
            Edge::Left => 2,
            Edge::Right => 3,
        })
    }
}

/// Register a blocked crossing on `edge` at `now` for the double-tap override.
///
/// Returns true when this is the second tap: same edge, within [`DOUBLE_TAP_MS`], and the
/// cursor left the edge in between (`left_edge_since_tap`). Both state fields are updated:
/// cleared on a second tap, otherwise the tap is recorded as the new first tap.
#[cfg(any(windows, test))]
fn double_tap_step(
    last_blocked_edge: &mut Option<(EdgeKey, Instant)>,
    left_edge_since_tap: &mut bool,
    edge: EdgeKey,
    now: Instant,
) -> bool {
    let is_second = matches!(
        *last_blocked_edge,
        Some((e, t))
            if e == edge
                && now.duration_since(t) <= Duration::from_millis(DOUBLE_TAP_MS)
                && *left_edge_since_tap
    );
    if is_second {
        *last_blocked_edge = None;
        *left_edge_since_tap = false;
    } else {
        *last_blocked_edge = Some((edge, now));
        *left_edge_since_tap = false;
    }
    is_second
}

/// Gate for a crossing toward a remote host (`gates_allow`): blocked taps never pass,
/// only a recognised second tap overrides.
#[cfg(any(windows, test))]
fn decide_remote_gate(
    switching: &SwitchingConfig,
    mut ctx: SwitchContext,
    edge: EdgeKey,
    last_blocked_edge: &mut Option<(EdgeKey, Instant)>,
    left_edge_since_tap: &mut bool,
    now: Instant,
) -> bool {
    let blocked_without = !allow_switch(switching, &ctx);
    if blocked_without && switching.double_tap_border_override {
        if double_tap_step(last_blocked_edge, left_edge_since_tap, edge, now) {
            ctx.double_tap_border = true;
        } else {
            return false;
        }
    }
    allow_switch(switching, &ctx)
}

/// The fast-hit rule for a hand-off: `entry_edge` is the side of the other computer's display the
/// cursor comes in by, so the push that counts goes the opposite way (entering its left side =
/// moving right, into this display's right edge).
#[cfg(any(windows, test))]
fn fast_crossing(hits: &HitMeter, entry_edge: Edge, min_speed: u32) -> bool {
    let toward = match entry_edge {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
    };
    hits.fast_enough(toward, min_speed)
}

/// Gate for the local monitor wrap (`run` loop). Same override rule as
/// [`decide_remote_gate`], but an unblocked crossing also clears the double-tap memory.
#[cfg(any(windows, test))]
fn decide_local_gate(
    switching: &SwitchingConfig,
    mut ctx: SwitchContext,
    edge: EdgeKey,
    last_blocked_edge: &mut Option<(EdgeKey, Instant)>,
    left_edge_since_tap: &mut bool,
    now: Instant,
) -> bool {
    let blocked_without = !allow_switch(switching, &ctx);
    if blocked_without && switching.double_tap_border_override {
        if double_tap_step(last_blocked_edge, left_edge_since_tap, edge, now) {
            ctx.double_tap_border = true;
        }
    } else if !blocked_without {
        *last_blocked_edge = None;
        *left_edge_since_tap = false;
    }
    allow_switch(switching, &ctx)
}

/// Which outer edge (2px band) of `m` the cursor touches; vertical edges win over horizontal.
#[cfg(any(windows, test))]
fn outer_edge_only(m: &inputshare_core::MonitorTile, cx: i32, cy: i32) -> Option<Edge> {
    if cy <= m.y + 1 {
        return Some(Edge::Top);
    }
    if cy >= m.y + m.height as i32 - 2 {
        return Some(Edge::Bottom);
    }
    if cx <= m.x + 1 {
        return Some(Edge::Left);
    }
    if cx >= m.x + m.width as i32 - 2 {
        return Some(Edge::Right);
    }
    None
}

#[cfg(any(windows, test))]
fn outer_edge_key(m: &inputshare_core::MonitorTile, cx: i32, cy: i32) -> Option<EdgeKey> {
    outer_edge_only(m, cx, cy).map(EdgeKey::from_edge)
}

/// Layout-only part of `probe_remote_edge`: cursor on the outer edge of a **local** tile whose
/// neighbour across that edge belongs to another host.
/// Returns `(host, entry_edge, entry, pull_back_pos)`.
#[cfg(test)]
fn probe_remote_edge_in(
    layout: &MonitorLayout,
    local_host: &str,
    cx: i32,
    cy: i32,
) -> Option<(String, Edge, f32, (i32, i32))> {
    probe_remote_hit_in(layout, local_host, cx, cy).map(|(host, edge, entry, pull, _)| (host, edge, entry, pull))
}

/// Like [`probe_remote_edge_in`], plus the id of the remote display that is entered.
#[cfg(test)]
fn probe_remote_hit_in(
    layout: &MonitorLayout,
    local_host: &str,
    cx: i32,
    cy: i32,
) -> Option<(String, Edge, f32, (i32, i32), String)> {
    probe_remote_crossed(layout, local_host, cx, cy, false, false)
}

/// `equalize` maps the whole source edge onto the facing display, so a taller screen
/// still hands off above and below the shorter one once the edges meet. With `wrap` (loop the
/// monitors) an edge with nothing beside it hands off to the other computer's display the loop
/// lands on.
#[cfg(any(windows, test))]
fn probe_remote_crossed(
    layout: &MonitorLayout,
    local_host: &str,
    cx: i32,
    cy: i32,
    equalize: bool,
    wrap: bool,
) -> Option<(String, Edge, f32, (i32, i32), String)> {
    let from = layout.monitors.iter().find(|m| {
        m.host.eq_ignore_ascii_case(local_host)
            && cx >= m.x
            && cx < m.x + m.width as i32
            && cy >= m.y
            && cy < m.y + m.height as i32
    })?;
    let edge = outer_edge_only(from, cx, cy)?;
    let along = match edge {
        Edge::Left | Edge::Right => {
            if from.height == 0 {
                0.5
            } else {
                ((cy - from.y) as f32 / from.height as f32).clamp(0.0, 1.0)
            }
        }
        Edge::Top | Edge::Bottom => {
            if from.width == 0 {
                0.5
            } else {
                ((cx - from.x) as f32 / from.width as f32).clamp(0.0, 1.0)
            }
        }
    };
    let hit = layout
        .neighbor_across(&from.id, edge, along)
        .or_else(|| {
            if equalize {
                layout.neighbor_beside(&from.id, edge, along)
            } else {
                None
            }
        })
        .or_else(|| if wrap { layout.wrap_across(&from.id, edge, along) } else { None })?;
    if hit.host.eq_ignore_ascii_case(local_host) {
        return None;
    }
    // Pull cursor 3px inside so it stays on local glass.
    let pull = match edge {
        Edge::Left => (from.x + 3, cy),
        Edge::Right => (from.x + from.width as i32 - 4, cy),
        Edge::Top => (cx, from.y + 3),
        Edge::Bottom => (cx, from.y + from.height as i32 - 4),
    };
    Some((hit.host, hit.entry_edge, hit.entry, pull, hit.monitor_id))
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::displays;
    use ::windows::Win32::Foundation::POINT;
    use ::windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

    pub fn run(ctrl: WrapControl) {
        let mut cooldown_until = Instant::now();
        let mut last_refresh = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);

        let mut last_blocked_edge: Option<(EdgeKey, Instant)> = None;
        let mut left_edge_since_tap = false;
        let mut on_outer_edge = false;
        let local_host = local_hostname();
        let mut hits = HitMeter::default();

        loop {
            thread::sleep(Duration::from_millis(8));
            if !ctrl.sharing.load(Ordering::Relaxed) {
                continue;
            }
            // While capturing to remote, don't local-wrap (capture owns the cursor).
            if ctrl.we_are_capturing() {
                hits.clear();
                continue;
            }
            if Instant::now() < cooldown_until {
                continue;
            }

            // Refresh local OS geometry but keep remote tiles from GUI layout.
            if last_refresh.elapsed() > Duration::from_secs(2) {
                if let Ok(sys) = displays::system_layout() {
                    if let Ok(mut g) = ctrl.layout.lock() {
                        g.merge_system_displays(sys);
                    }
                }
                last_refresh = Instant::now();
            }

            let mut pt = POINT::default();
            if unsafe { GetCursorPos(&mut pt) }.is_err() {
                continue;
            }
            hits.record(Instant::now(), pt.x, pt.y);

            let switching = ctrl
                .switching
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();

            // --- Remote edge hand-off (before local wrap) ---
            if let Some((host, edge, entry, pull_back, tile_id)) =
                probe_remote_hit(&ctrl, &local_host, pt.x, pt.y)
            {
                // Only to a computer that is linked, and never while another one steers this PC:
                // otherwise the edge would be a wall (cursor pulled back, nobody to receive it)
                // or two computers would hand control to each other in a loop.
                if ctrl.remote_is_up(&host)
                    && !ctrl.peer_steers_us()
                    && gates_allow(&ctrl, &switching, pt.x, pt.y, edge, &hits, &mut last_blocked_edge, &mut left_edge_since_tap) {
                    if let Ok(g) = ctrl.net_outbox.lock() {
                        if let Some(outbox) = g.as_ref() {
                            outbox.set_enter_tile(tile_id);
                            outbox.request_enter(host, edge, entry);
                            // Keep cursor inside local edge so OS doesn't lose it.
                            let _ = unsafe { SetCursorPos(pull_back.0, pull_back.1) };
                            hits.clear();
                            cooldown_until = Instant::now() + Duration::from_millis(200);
                            on_outer_edge = false;
                            continue;
                        }
                    }
                }
            }

            if !ctrl.enabled.load(Ordering::Relaxed) {
                continue;
            }

            let (dest, edge_key, mon_geom) = {
                let layout = match ctrl.layout.lock() {
                    Ok(g) => g,
                    Err(_) => continue,
                };
                let from = layout
                    .monitors
                    .iter()
                    .find(|m| {
                        pt.x >= m.x
                            && pt.x < m.x + m.width as i32
                            && pt.y >= m.y
                            && pt.y < m.y + m.height as i32
                    })
                    .cloned();
                // A loop onto another computer's display is a hand-off (above), never a cursor
                // move: those coordinates are not on this computer's screens.
                let dest = layout
                    .wrap_cursor_target(pt.x, pt.y, switching.equalize_display_dimensions)
                    .filter(|(_, _, hit)| hit.host.eq_ignore_ascii_case(&local_host))
                    .map(|(x, y, _)| (x, y));
                let edge_key = dest.and_then(|_| {
                    let m = from.as_ref()?;
                    outer_edge_key(m, pt.x, pt.y)
                });
                let mon_geom = from.map(|m| (m.x, m.y, m.width as i32, m.height as i32));
                (dest, edge_key, mon_geom)
            };

            let Some((nx, ny)) = dest else {
                if on_outer_edge {
                    left_edge_since_tap = true;
                }
                on_outer_edge = false;
                continue;
            };
            if (nx, ny) == (pt.x, pt.y) {
                continue;
            }

            on_outer_edge = true;
            let edge = edge_key.unwrap_or(EdgeKey(255));

            let (mx, my, mw, mh) = mon_geom.unwrap_or((pt.x, pt.y, 1, 1));
            let mod_name = switching
                .require_modifier
                .as_deref()
                .unwrap_or("")
                .to_string();

            let ctx = SwitchContext {
                fullscreen: win_query::foreground_fullscreen(),
                maximized: win_query::foreground_maximized(),
                in_corner: win_query::in_corner(mx, my, mw, mh, pt.x, pt.y, CORNER_PX),
                modifier_held: if mod_name.is_empty() {
                    true
                } else {
                    win_query::modifier_held(&mod_name)
                },
                active_process: win_query::foreground_process_name(),
                double_tap_border: false,
                sharing_enabled: ctrl.sharing.load(Ordering::Relaxed),
                // The fast-hit rule is about switching computers, not wrapping this one's monitors.
                fast_hit: true,
                cursor_locked: switching.block_when_cursor_locked && win_query::cursor_locked(),
            };

            if !decide_local_gate(
                &switching,
                ctx,
                edge,
                &mut last_blocked_edge,
                &mut left_edge_since_tap,
                Instant::now(),
            ) {
                continue;
            }

            let _ = unsafe { SetCursorPos(nx, ny) };
            hits.clear();
            cooldown_until = Instant::now() + Duration::from_millis(80);
            last_blocked_edge = None;
            left_edge_since_tap = false;
            on_outer_edge = false;
        }
    }

    fn probe_remote_hit(
        ctrl: &WrapControl,
        local_host: &str,
        cx: i32,
        cy: i32,
    ) -> Option<(String, Edge, f32, (i32, i32), String)> {
        let layout = ctrl.layout.lock().ok()?;
        let equalize = ctrl
            .switching
            .lock()
            .ok()
            .map(|g| g.equalize_display_dimensions)
            .unwrap_or(false);
        let wrap = ctrl.enabled.load(Ordering::Relaxed);
        probe_remote_crossed(&layout, local_host, cx, cy, equalize, wrap)
    }

    fn gates_allow(
        ctrl: &WrapControl,
        switching: &SwitchingConfig,
        cx: i32,
        cy: i32,
        edge: Edge,
        hits: &HitMeter,
        last_blocked_edge: &mut Option<(EdgeKey, Instant)>,
        left_edge_since_tap: &mut bool,
    ) -> bool {
        let layout = match ctrl.layout.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let from = layout.monitors.iter().find(|m| {
            cx >= m.x
                && cx < m.x + m.width as i32
                && cy >= m.y
                && cy < m.y + m.height as i32
        });
        let (mx, my, mw, mh) = from
            .map(|m| (m.x, m.y, m.width as i32, m.height as i32))
            .unwrap_or((cx, cy, 1, 1));
        let mod_name = switching.require_modifier.as_deref().unwrap_or("");
        let ctx = SwitchContext {
            fullscreen: win_query::foreground_fullscreen(),
            maximized: win_query::foreground_maximized(),
            in_corner: win_query::in_corner(mx, my, mw, mh, cx, cy, CORNER_PX),
            modifier_held: if mod_name.is_empty() {
                true
            } else {
                win_query::modifier_held(mod_name)
            },
            active_process: win_query::foreground_process_name(),
            double_tap_border: false,
            sharing_enabled: ctrl.sharing.load(Ordering::Relaxed),
            fast_hit: fast_crossing(hits, edge, switching.fast_hit_speed),
            cursor_locked: switching.block_when_cursor_locked && win_query::cursor_locked(),
        };
        decide_remote_gate(
            switching,
            ctx,
            EdgeKey::from_edge(edge),
            last_blocked_edge,
            left_edge_since_tap,
            Instant::now(),
        )
    }
}

// Tests exercise only the pure decision helpers with a synthetic layout and a fake clock:
// no cursor is read or moved, and no foreground-window / keyboard query is made.
#[cfg(test)]
mod tests {
    use super::*;
    use inputshare_core::MonitorTile;

    const LOCAL: &str = "LOCAL-PC";
    const REMOTE: &str = "REMOTE-PC";

    fn tile(id: &str, host: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: host.into(),
            label: id.to_uppercase(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: host == LOCAL,
        }
    }

    fn layout(tiles: Vec<MonitorTile>) -> MonitorLayout {
        MonitorLayout { monitors: tiles }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ---- fast hit ----

    #[test]
    fn a_flick_into_the_right_edge_counts_for_a_display_entered_by_its_left_side() {
        let t0 = Instant::now();
        let mut hits = HitMeter::default();
        hits.record(t0, 1800, 500);
        hits.record(t0 + ms(40), 1919, 500); // ~3000 px/s to the right
        assert!(fast_crossing(&hits, Edge::Left, 500), "entering its left side = moving right");
        assert!(!fast_crossing(&hits, Edge::Right, 500), "a push the other way does not count");
        let mut slow = HitMeter::default();
        slow.record(t0, 1912, 500);
        slow.record(t0 + ms(56), 1919, 500); // ~125 px/s
        assert!(!fast_crossing(&slow, Edge::Left, 500));
        assert!(fast_crossing(&slow, Edge::Left, 1), "1 px/s lets almost any push through");
    }

    // ---- outer edge detection ----

    #[test]
    fn outer_edge_two_px_band() {
        let m = tile("a", LOCAL, 100, 50, 1920, 1080);
        assert_eq!(outer_edge_only(&m, 1000, 50), Some(Edge::Top));
        assert_eq!(outer_edge_only(&m, 1000, 51), Some(Edge::Top));
        assert_eq!(outer_edge_only(&m, 1000, 52), None);
        assert_eq!(outer_edge_only(&m, 1000, 50 + 1080 - 2), Some(Edge::Bottom));
        assert_eq!(outer_edge_only(&m, 1000, 50 + 1080 - 3), None);
        assert_eq!(outer_edge_only(&m, 100, 500), Some(Edge::Left));
        assert_eq!(outer_edge_only(&m, 101, 500), Some(Edge::Left));
        assert_eq!(outer_edge_only(&m, 102, 500), None);
        assert_eq!(outer_edge_only(&m, 100 + 1920 - 2, 500), Some(Edge::Right));
        assert_eq!(outer_edge_only(&m, 100 + 1920 - 3, 500), None);
    }

    #[test]
    fn outer_edge_corner_prefers_vertical_axis() {
        let m = tile("a", LOCAL, 0, 0, 1920, 1080);
        assert_eq!(outer_edge_only(&m, 0, 0), Some(Edge::Top));
        assert_eq!(outer_edge_only(&m, 1919, 0), Some(Edge::Top));
        assert_eq!(outer_edge_only(&m, 0, 1079), Some(Edge::Bottom));
        assert_eq!(outer_edge_only(&m, 1919, 1079), Some(Edge::Bottom));
    }

    #[test]
    fn outer_edge_center_is_none() {
        let m = tile("a", LOCAL, -500, -500, 1000, 1000);
        assert_eq!(outer_edge_only(&m, 0, 0), None);
        assert_eq!(outer_edge_key(&m, 0, 0), None);
    }

    #[test]
    fn edge_keys_are_distinct_per_edge() {
        let keys = [
            EdgeKey::from_edge(Edge::Top),
            EdgeKey::from_edge(Edge::Bottom),
            EdgeKey::from_edge(Edge::Left),
            EdgeKey::from_edge(Edge::Right),
        ];
        for (i, a) in keys.iter().enumerate() {
            for (j, b) in keys.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
        let m = tile("a", LOCAL, 0, 0, 100, 100);
        assert_eq!(outer_edge_key(&m, 0, 50), Some(EdgeKey::from_edge(Edge::Left)));
    }

    // ---- remote hand-off probe ----

    fn row_local_remote() -> MonitorLayout {
        layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, 1920, 0, 1920, 1080),
        ])
    }

    #[test]
    fn a_remote_display_is_a_hand_off_target_only_while_its_computer_is_linked() {
        let c = WrapControl::new(true, true, row_local_remote(), SwitchingConfig::default());
        assert!(!c.remote_is_up(REMOTE), "nothing is linked at the start: the edge is an ordinary edge");
        c.set_remote_hosts_up(vec![REMOTE.to_uppercase()]);
        assert!(c.remote_is_up(REMOTE), "names compare without regard to case");
        c.set_remote_hosts_up(vec![]);
        assert!(!c.remote_is_up(REMOTE), "the link dropped");
    }

    #[test]
    fn a_peer_that_steers_us_is_visible_to_the_edge_immediately() {
        let c = WrapControl::new(true, true, row_local_remote(), SwitchingConfig::default());
        let cap = CaptureControl::new();
        assert!(!c.peer_steers_us());
        c.bind_capture(cap.clone());
        assert!(!c.peer_steers_us());
        cap.touch_controlled();
        assert!(c.peer_steers_us(), "the edge must know before the next window frame");
    }

    #[test]
    fn a_small_gap_still_hands_off() {
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, 1920 + 20, 0, 1920, 1080),
        ]);
        let (host, edge, _, _, id) = probe_remote_hit_in(&l, LOCAL, 1919, 540).expect("20px crack");
        assert_eq!((host.as_str(), edge, id.as_str()), (REMOTE, Edge::Left, "r1"));
        let far = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, 1920 + 400, 0, 1920, 1080),
        ]);
        assert!(probe_remote_hit_in(&far, LOCAL, 1919, 540).is_none());
    }

    #[test]
    fn the_hit_names_the_entered_display_for_the_way_back() {
        let (host, edge, _, _, tile) = probe_remote_hit_in(&row_local_remote(), LOCAL, 1919, 540).expect("hand-off");
        assert_eq!((host.as_str(), edge), (REMOTE, Edge::Left));
        assert_eq!(tile, "r1");
    }

    #[test]
    fn probe_right_edge_hits_remote_neighbor() {
        let (host, entry_edge, entry, pull) =
            probe_remote_edge_in(&row_local_remote(), LOCAL, 1919, 540).expect("hand-off");
        assert_eq!(host, REMOTE);
        assert_eq!(entry_edge, Edge::Left);
        assert!((entry - 0.5).abs() < 1e-6, "entry {entry}");
        assert_eq!(pull, (1920 - 4, 540), "pulled 3px inside the local glass");
    }

    #[test]
    fn probe_requires_cursor_on_the_edge_band() {
        let l = row_local_remote();
        assert!(probe_remote_edge_in(&l, LOCAL, 1918, 540).is_some());
        assert!(probe_remote_edge_in(&l, LOCAL, 1917, 540).is_none());
        assert!(probe_remote_edge_in(&l, LOCAL, 960, 540).is_none());
    }

    #[test]
    fn probe_host_match_is_case_insensitive() {
        let l = row_local_remote();
        assert!(probe_remote_edge_in(&l, "local-pc", 1919, 540).is_some());
        assert!(probe_remote_edge_in(&l, "Local-PC", 1919, 540).is_some());
    }

    #[test]
    fn probe_ignores_cursor_on_a_remote_tile() {
        // Cursor position inside the remote tile's edge band: only local tiles can hand off.
        let l = row_local_remote();
        assert!(probe_remote_edge_in(&l, LOCAL, 3839, 540).is_none());
        assert!(probe_remote_edge_in(&l, REMOTE, 1919, 540).is_none(), "cursor is on l1, not remote");
    }

    #[test]
    fn probe_local_neighbor_is_not_a_handoff() {
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("l2", LOCAL, 1920, 0, 1920, 1080),
        ]);
        assert!(probe_remote_edge_in(&l, LOCAL, 1919, 540).is_none());
    }

    #[test]
    fn probe_no_neighbor_is_none() {
        let l = layout(vec![tile("l1", LOCAL, 0, 0, 1920, 1080)]);
        assert!(probe_remote_edge_in(&l, LOCAL, 1919, 540).is_none());
        assert!(probe_remote_edge_in(&MonitorLayout::default(), LOCAL, 0, 0).is_none());
    }

    #[test]
    fn probe_remote_not_aligned_with_cursor_row_is_none() {
        // Remote starts below the cursor's row: nothing across the edge at y=100.
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, 1920, 500, 1920, 1080),
        ]);
        assert!(probe_remote_edge_in(&l, LOCAL, 1919, 100).is_none());
        assert!(probe_remote_edge_in(&l, LOCAL, 1919, 600).is_some());
    }

    #[test]
    fn probe_entry_is_relative_to_remote_height() {
        // Remote is taller (1440): 540px down maps to 540/1440 on the remote.
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, 1920, 0, 2560, 1440),
        ]);
        let (_, _, entry, _) = probe_remote_edge_in(&l, LOCAL, 1919, 540).unwrap();
        assert!((entry - 0.375).abs() < 1e-6, "entry {entry}");
    }

    #[test]
    fn probe_left_edge() {
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("r1", REMOTE, -1920, 0, 1920, 1080),
        ]);
        let (host, entry_edge, entry, pull) = probe_remote_edge_in(&l, LOCAL, 0, 270).unwrap();
        assert_eq!(host, REMOTE);
        assert_eq!(entry_edge, Edge::Right);
        assert!((entry - 0.25).abs() < 1e-6);
        assert_eq!(pull, (3, 270));
    }

    #[test]
    fn probe_top_and_bottom_edges() {
        let l = layout(vec![
            tile("l1", LOCAL, 0, 0, 1920, 1080),
            tile("up", REMOTE, 0, -1080, 1920, 1080),
            tile("down", "OTHER-PC", 0, 1080, 1920, 1080),
        ]);
        let (host, entry_edge, entry, pull) = probe_remote_edge_in(&l, LOCAL, 480, 0).unwrap();
        assert_eq!(host, REMOTE);
        assert_eq!(entry_edge, Edge::Bottom);
        assert!((entry - 0.25).abs() < 1e-6);
        assert_eq!(pull, (480, 3));

        let (host, entry_edge, _, pull) = probe_remote_edge_in(&l, LOCAL, 1440, 1079).unwrap();
        assert_eq!(host, "OTHER-PC");
        assert_eq!(entry_edge, Edge::Top);
        assert_eq!(pull, (1440, 1080 - 4));
    }

    #[test]
    fn probe_works_with_negative_monitor_origin() {
        let l = layout(vec![
            tile("d", LOCAL, -2560, 400, 2560, 1440),
            tile("r1", REMOTE, -5120, 400, 2560, 1440),
        ]);
        let (_, _, entry, pull) = probe_remote_edge_in(&l, LOCAL, -2560, 1120).unwrap();
        assert!((entry - 0.5).abs() < 1e-6);
        assert_eq!(pull, (-2560 + 3, 1120));
    }

    // ---- double-tap step ----

    #[test]
    fn double_tap_first_tap_is_recorded_not_accepted() {
        let t0 = Instant::now();
        let (mut last, mut left) = (None, false);
        let e = EdgeKey::from_edge(Edge::Right);
        assert!(!double_tap_step(&mut last, &mut left, e, t0));
        assert_eq!(last, Some((e, t0)));
        assert!(!left);
    }

    #[test]
    fn double_tap_requires_leaving_the_edge_between_taps() {
        let t0 = Instant::now();
        let (mut last, mut left) = (None, false);
        let e = EdgeKey::from_edge(Edge::Right);
        double_tap_step(&mut last, &mut left, e, t0);
        // Cursor stayed on the edge: second attempt is just a new first tap.
        assert!(!double_tap_step(&mut last, &mut left, e, t0 + ms(100)));
        assert_eq!(last, Some((e, t0 + ms(100))));
    }

    #[test]
    fn double_tap_second_tap_accepted_and_state_cleared() {
        let t0 = Instant::now();
        let (mut last, mut left) = (None, false);
        let e = EdgeKey::from_edge(Edge::Right);
        double_tap_step(&mut last, &mut left, e, t0);
        left = true; // caller notices the cursor left the edge
        assert!(double_tap_step(&mut last, &mut left, e, t0 + ms(200)));
        assert_eq!(last, None);
        assert!(!left);
    }

    #[test]
    fn double_tap_window_is_inclusive_400ms() {
        let t0 = Instant::now();
        let e = EdgeKey::from_edge(Edge::Top);

        let (mut last, mut left) = (None, false);
        double_tap_step(&mut last, &mut left, e, t0);
        left = true;
        assert!(double_tap_step(&mut last, &mut left, e, t0 + ms(400)));

        let (mut last, mut left) = (None, false);
        double_tap_step(&mut last, &mut left, e, t0);
        left = true;
        assert!(!double_tap_step(&mut last, &mut left, e, t0 + ms(401)));
        assert_eq!(last, Some((e, t0 + ms(401))), "late tap becomes the new first tap");
    }

    #[test]
    fn double_tap_on_a_different_edge_does_not_count() {
        let t0 = Instant::now();
        let (mut last, mut left) = (None, false);
        let right = EdgeKey::from_edge(Edge::Right);
        let left_e = EdgeKey::from_edge(Edge::Left);
        double_tap_step(&mut last, &mut left, right, t0);
        left = true;
        assert!(!double_tap_step(&mut last, &mut left, left_e, t0 + ms(100)));
        assert_eq!(last, Some((left_e, t0 + ms(100))));
    }

    // ---- gates ----

    fn open_ctx() -> SwitchContext {
        SwitchContext {
            modifier_held: true,
            sharing_enabled: true,
            ..Default::default()
        }
    }

    fn cfg_override(mut cfg: SwitchingConfig) -> SwitchingConfig {
        cfg.double_tap_border_override = true;
        cfg
    }

    struct Memory {
        last: Option<(EdgeKey, Instant)>,
        left: bool,
    }

    impl Memory {
        fn new() -> Self {
            Self {
                last: None,
                left: false,
            }
        }
    }

    fn remote_gate(cfg: &SwitchingConfig, ctx: SwitchContext, mem: &mut Memory, edge: Edge, now: Instant) -> bool {
        decide_remote_gate(cfg, ctx, EdgeKey::from_edge(edge), &mut mem.last, &mut mem.left, now)
    }

    fn local_gate(cfg: &SwitchingConfig, ctx: SwitchContext, mem: &mut Memory, edge: Edge, now: Instant) -> bool {
        decide_local_gate(cfg, ctx, EdgeKey::from_edge(edge), &mut mem.last, &mut mem.left, now)
    }

    #[test]
    fn gates_open_when_nothing_blocks() {
        let cfg = SwitchingConfig::default();
        let now = Instant::now();
        let mut m = Memory::new();
        assert!(remote_gate(&cfg, open_ctx(), &mut m, Edge::Right, now));
        assert!(local_gate(&cfg, open_ctx(), &mut m, Edge::Right, now));
    }

    #[test]
    fn gates_closed_when_sharing_is_off_even_with_override() {
        let cfg = cfg_override(SwitchingConfig::default());
        let mut ctx = open_ctx();
        ctx.sharing_enabled = false;
        ctx.double_tap_border = true;
        let now = Instant::now();
        let mut m = Memory::new();
        assert!(!remote_gate(&cfg, ctx.clone(), &mut m, Edge::Right, now));
        assert!(!local_gate(&cfg, ctx, &mut m, Edge::Right, now));
    }

    #[test]
    fn fullscreen_blocks_only_when_option_is_on() {
        let now = Instant::now();
        let mut ctx = open_ctx();
        ctx.fullscreen = true;

        let mut cfg = SwitchingConfig::default();
        assert!(remote_gate(&cfg, ctx.clone(), &mut Memory::new(), Edge::Left, now));

        cfg.prevent_fullscreen = true;
        assert!(!remote_gate(&cfg, ctx.clone(), &mut Memory::new(), Edge::Left, now));
        assert!(!local_gate(&cfg, ctx, &mut Memory::new(), Edge::Left, now));
    }

    #[test]
    fn maximized_corner_and_modifier_gates() {
        let now = Instant::now();

        let mut cfg = SwitchingConfig::default();
        cfg.prevent_maximized = true;
        let mut ctx = open_ctx();
        ctx.maximized = true;
        assert!(!remote_gate(&cfg, ctx, &mut Memory::new(), Edge::Top, now));

        let mut cfg = SwitchingConfig::default();
        cfg.prevent_corners = true;
        let mut ctx = open_ctx();
        ctx.in_corner = true;
        assert!(!remote_gate(&cfg, ctx.clone(), &mut Memory::new(), Edge::Top, now));
        ctx.in_corner = false;
        assert!(remote_gate(&cfg, ctx, &mut Memory::new(), Edge::Top, now));

        let mut cfg = SwitchingConfig::default();
        cfg.require_modifier = Some("Ctrl".into());
        let mut ctx = open_ctx();
        ctx.modifier_held = false;
        assert!(!remote_gate(&cfg, ctx.clone(), &mut Memory::new(), Edge::Top, now));
        ctx.modifier_held = true;
        assert!(remote_gate(&cfg, ctx, &mut Memory::new(), Edge::Top, now));
    }

    #[test]
    fn empty_modifier_requirement_is_ignored() {
        let mut cfg = SwitchingConfig::default();
        cfg.require_modifier = Some(String::new());
        let mut ctx = open_ctx();
        ctx.modifier_held = false;
        assert!(remote_gate(&cfg, ctx, &mut Memory::new(), Edge::Top, Instant::now()));
    }

    #[test]
    fn blocked_process_list_matches_case_insensitively() {
        let now = Instant::now();
        let mut cfg = SwitchingConfig::default();
        cfg.blocked_programs = vec!["Game.EXE".into(), "obs64.exe".into()];

        let with_proc = |name: Option<&str>| {
            let mut ctx = open_ctx();
            ctx.active_process = name.map(str::to_string);
            ctx
        };
        assert!(!remote_gate(&cfg, with_proc(Some("game.exe")), &mut Memory::new(), Edge::Left, now));
        assert!(!remote_gate(&cfg, with_proc(Some("OBS64.EXE")), &mut Memory::new(), Edge::Left, now));
        assert!(!local_gate(&cfg, with_proc(Some("game.exe")), &mut Memory::new(), Edge::Left, now));
        assert!(remote_gate(&cfg, with_proc(Some("notepad.exe")), &mut Memory::new(), Edge::Left, now));
        assert!(remote_gate(&cfg, with_proc(None), &mut Memory::new(), Edge::Left, now), "unknown foreground process never blocks");
    }

    #[test]
    fn process_match_is_exact_not_substring() {
        let mut cfg = SwitchingConfig::default();
        cfg.blocked_programs = vec!["game.exe".into()];
        let mut ctx = open_ctx();
        ctx.active_process = Some("game.exe.bak".into());
        assert!(remote_gate(&cfg, ctx.clone(), &mut Memory::new(), Edge::Left, Instant::now()));
        ctx.active_process = Some("mygame.exe".into());
        assert!(remote_gate(&cfg, ctx, &mut Memory::new(), Edge::Left, Instant::now()));
    }

    fn blocked_cfg() -> SwitchingConfig {
        let mut cfg = cfg_override(SwitchingConfig::default());
        cfg.prevent_fullscreen = true;
        cfg
    }

    fn fullscreen_ctx() -> SwitchContext {
        let mut ctx = open_ctx();
        ctx.fullscreen = true;
        ctx
    }

    #[test]
    fn remote_gate_double_tap_sequence() {
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();

        // 1st tap: blocked, remembered.
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0));
        assert_eq!(m.last.map(|(e, _)| e), Some(EdgeKey::from_edge(Edge::Right)));

        // 2nd tap without leaving the edge: still blocked.
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(100)));

        // Cursor leaves the edge (the wrap loop sets this), then taps again in time: allowed.
        m.left = true;
        assert!(remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(250)));
        assert!(m.last.is_none() && !m.left, "override consumes the memory");

        // The override is one-shot: the next attempt is blocked again.
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(300)));
    }

    #[test]
    fn remote_gate_too_slow_second_tap_stays_blocked() {
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0));
        m.left = true;
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(401)));
    }

    #[test]
    fn override_is_off_by_default() {
        let mut cfg = SwitchingConfig::default();
        cfg.prevent_fullscreen = true;
        assert!(!cfg.double_tap_border_override);
        let t0 = Instant::now();
        let mut m = Memory::new();
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0));
        m.left = true;
        assert!(!remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(50)));
        assert!(m.last.is_none(), "no memory is kept without the option");
    }

    #[test]
    fn local_gate_double_tap_sequence() {
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();

        assert!(!local_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Top, t0));
        m.left = true;
        assert!(local_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Top, t0 + ms(300)));
        assert!(m.last.is_none() && !m.left);
    }

    #[test]
    fn local_gate_forgets_pending_tap_when_crossing_is_allowed() {
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();
        local_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Top, t0);
        assert!(m.last.is_some());

        // Fullscreen app closed: crossing is open again and the stale tap is dropped.
        assert!(local_gate(&cfg, open_ctx(), &mut m, Edge::Top, t0 + ms(50)));
        assert!(m.last.is_none() && !m.left);
    }

    #[test]
    fn remote_gate_keeps_pending_tap_when_crossing_is_allowed() {
        // Characterization: unlike the local gate, the remote gate does not clear the memory.
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();
        remote_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Top, t0);
        assert!(remote_gate(&cfg, open_ctx(), &mut m, Edge::Top, t0 + ms(50)));
        assert!(m.last.is_some());
    }

    #[test]
    fn tap_on_other_edge_does_not_unlock() {
        let cfg = blocked_cfg();
        let t0 = Instant::now();
        let mut m = Memory::new();
        assert!(!local_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Left, t0));
        m.left = true;
        assert!(!local_gate(&cfg, fullscreen_ctx(), &mut m, Edge::Right, t0 + ms(100)));
    }

    // ---- WrapControl flags ----

    #[test]
    fn wrap_control_initial_flags_and_setters() {
        let c = WrapControl::new(true, false, MonitorLayout::default(), SwitchingConfig::default());
        assert!(c.enabled.load(Ordering::Relaxed));
        assert!(!c.sharing.load(Ordering::Relaxed));
        assert!(!c.remote_capturing.load(Ordering::Relaxed));
        assert!(c.net_outbox.lock().unwrap().is_none());

        let clone = c.clone();
        c.set_wrap(false);
        c.set_sharing(true);
        c.set_remote_capturing(true);
        assert!(!clone.enabled.load(Ordering::Relaxed));
        assert!(clone.sharing.load(Ordering::Relaxed));
        assert!(clone.remote_capturing.load(Ordering::Relaxed));
    }

    #[test]
    fn wrap_control_stores_layout_switching_and_outbox() {
        let c = WrapControl::new(true, true, MonitorLayout::default(), SwitchingConfig::default());
        c.set_layout(row_local_remote());
        assert_eq!(c.layout.lock().unwrap().monitors.len(), 2);

        let mut cfg = SwitchingConfig::default();
        cfg.prevent_corners = true;
        c.set_switching(cfg);
        assert!(c.switching.lock().unwrap().prevent_corners);

        let outbox = CaptureOutbox::default();
        c.set_net_outbox(outbox.clone());
        let guard = c.net_outbox.lock().unwrap();
        guard
            .as_ref()
            .expect("outbox set")
            .request_enter("PEER".into(), Edge::Left, 0.5);
        drop(guard);
        let (host, edge, _) = outbox.take_enter().expect("enter queued through shared outbox");
        assert_eq!((host.as_str(), edge), ("PEER", Edge::Left));
    }
}
