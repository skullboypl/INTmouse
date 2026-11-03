use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::config::SwitchingConfig;
use inputshare_protocol::Edge;

/// Runtime gate: should we allow leaving the current display toward a peer?
#[derive(Debug, Clone, Default)]
pub struct SwitchContext {
    pub fullscreen: bool,
    pub maximized: bool,
    pub in_corner: bool,
    pub modifier_held: bool,
    pub active_process: Option<String>,
    pub double_tap_border: bool,
    pub sharing_enabled: bool,
    /// The cursor hit the edge fast enough (see [`HitMeter`]); only read with `require_fast_hit`.
    pub fast_hit: bool,
    /// A program holds the cursor (hidden or clipped, e.g. a game's crosshair).
    pub cursor_locked: bool,
}

pub fn allow_switch(cfg: &SwitchingConfig, ctx: &SwitchContext) -> bool {
    if !ctx.sharing_enabled {
        return false;
    }
    // Not even a double tap gets out of a game that holds the mouse.
    if cfg.block_when_cursor_locked && ctx.cursor_locked {
        return false;
    }

    let mut blocked = false;
    if cfg.prevent_fullscreen && ctx.fullscreen {
        blocked = true;
    }
    if cfg.prevent_maximized && ctx.maximized {
        blocked = true;
    }
    if cfg.prevent_corners && ctx.in_corner {
        blocked = true;
    }
    if cfg.require_fast_hit && !ctx.fast_hit {
        blocked = true;
    }
    if let Some(mod_key) = &cfg.require_modifier {
        if !mod_key.is_empty() && !ctx.modifier_held {
            blocked = true;
        }
    }
    if let Some(proc) = &ctx.active_process {
        if cfg
            .blocked_programs
            .iter()
            .any(|p| p.eq_ignore_ascii_case(proc))
        {
            blocked = true;
        }
    }

    if blocked {
        if cfg.double_tap_border_override && ctx.double_tap_border {
            return true;
        }
        return false;
    }
    true
}

/// How far back [`HitMeter`] looks to measure the speed of a hit.
pub const HIT_WINDOW: Duration = Duration::from_millis(60);

/// Recent cursor positions, to tell a fast hit against a screen edge from a slow slide into it.
#[derive(Debug, Clone, Default)]
pub struct HitMeter {
    samples: VecDeque<(Instant, i32, i32)>,
}

impl HitMeter {
    pub fn record(&mut self, at: Instant, x: i32, y: i32) {
        self.samples.push_back((at, x, y));
        while self
            .samples
            .front()
            .is_some_and(|s| at.saturating_duration_since(s.0) > HIT_WINDOW * 2)
        {
            self.samples.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Speed (px/s) toward `edge` over the last [`HIT_WINDOW`] before the newest sample;
    /// 0 when moving away or with too few samples.
    pub fn speed_toward(&self, edge: Edge) -> f32 {
        let Some(&(t1, x1, y1)) = self.samples.back() else {
            return 0.0;
        };
        let Some(&(t0, x0, y0)) = self
            .samples
            .iter()
            .find(|s| t1.saturating_duration_since(s.0) <= HIT_WINDOW)
        else {
            return 0.0;
        };
        let dt = t1.saturating_duration_since(t0).as_secs_f32();
        if dt <= 0.0 {
            return 0.0;
        }
        let toward = match edge {
            Edge::Left => x0 - x1,
            Edge::Right => x1 - x0,
            Edge::Top => y0 - y1,
            Edge::Bottom => y1 - y0,
        };
        toward.max(0) as f32 / dt
    }

    /// `min_speed` 0 = no requirement.
    pub fn fast_enough(&self, edge: Edge, min_speed: u32) -> bool {
        min_speed == 0 || self.speed_toward(edge) >= min_speed as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_blocks_when_enabled() {
        let mut cfg = SwitchingConfig::default();
        cfg.prevent_fullscreen = true;
        let ctx = SwitchContext {
            fullscreen: true,
            sharing_enabled: true,
            ..Default::default()
        };
        assert!(!allow_switch(&cfg, &ctx));
    }

    #[test]
    fn double_tap_overrides() {
        let mut cfg = SwitchingConfig::default();
        cfg.prevent_fullscreen = true;
        cfg.double_tap_border_override = true;
        let ctx = SwitchContext {
            fullscreen: true,
            double_tap_border: true,
            sharing_enabled: true,
            ..Default::default()
        };
        assert!(allow_switch(&cfg, &ctx));
    }

    #[test]
    fn a_locked_cursor_blocks_even_a_double_tap() {
        let mut cfg = SwitchingConfig::default();
        cfg.double_tap_border_override = true;
        let ctx = SwitchContext {
            cursor_locked: true,
            double_tap_border: true,
            sharing_enabled: true,
            ..Default::default()
        };
        assert!(cfg.block_when_cursor_locked, "on by default");
        assert!(!allow_switch(&cfg, &ctx));
        cfg.block_when_cursor_locked = false;
        assert!(allow_switch(&cfg, &ctx));
    }

    #[test]
    fn fast_hit_is_required_only_when_enabled() {
        let mut cfg = SwitchingConfig::default();
        let slow = SwitchContext { sharing_enabled: true, ..Default::default() };
        assert!(allow_switch(&cfg, &slow), "off by default");
        cfg.require_fast_hit = true;
        assert!(!allow_switch(&cfg, &slow));
        let fast = SwitchContext { fast_hit: true, ..slow };
        assert!(allow_switch(&cfg, &fast));
    }

    fn meter(points: &[(u64, i32, i32)]) -> (HitMeter, Instant) {
        let t0 = Instant::now();
        let mut m = HitMeter::default();
        for &(ms, x, y) in points {
            m.record(t0 + Duration::from_millis(ms), x, y);
        }
        (m, t0)
    }

    #[test]
    fn hit_meter_measures_speed_toward_the_edge() {
        // 120 px to the right in 40 ms = 3000 px/s
        let (m, _) = meter(&[(0, 1800, 500), (8, 1830, 500), (16, 1860, 500), (24, 1890, 500), (40, 1919, 501)]);
        let v = m.speed_toward(Edge::Right);
        assert!((2900.0..3100.0).contains(&v), "{v}");
        assert_eq!(m.speed_toward(Edge::Left), 0.0, "moving away");
        assert!(m.fast_enough(Edge::Right, 1500));
        assert!(!m.fast_enough(Edge::Right, 4000));
        assert!(m.fast_enough(Edge::Left, 0), "0 = no requirement");
    }

    #[test]
    fn hit_meter_ignores_motion_older_than_the_window() {
        // a fast move long ago, then resting at the edge: no hit
        let (m, _) = meter(&[(0, 1000, 0), (8, 1919, 0), (200, 1919, 0), (208, 1919, 0)]);
        assert_eq!(m.speed_toward(Edge::Right), 0.0);
        // a slow slide: 20 px in 56 ms
        let (m, _) = meter(&[(0, 1899, 0), (24, 1908, 0), (56, 1919, 0)]);
        assert!(m.speed_toward(Edge::Right) < 500.0);
        let (m, _) = meter(&[(0, 5, 5)]);
        assert_eq!(m.speed_toward(Edge::Top), 0.0, "one sample is no speed");
    }
}
