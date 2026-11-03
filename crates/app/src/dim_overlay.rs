//! Semi-transparent black overlays on monitors that do not contain the cursor.

use inputshare_core::{MonitorLayout, MonitorTile};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
pub struct DimControl {
    enabled: Arc<AtomicBool>,
    /// dim_level 0..1 stored as u32 bits of f32.
    level_bits: Arc<AtomicU32>,
    layout: Arc<Mutex<MonitorLayout>>,
}

impl DimControl {
    pub fn new(enabled: bool, level: f32, layout: MonitorLayout) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            level_bits: Arc::new(AtomicU32::new(level.clamp(0.0, 1.0).to_bits())),
            layout: Arc::new(Mutex::new(layout)),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn set_level(&self, level: f32) {
        self.level_bits
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_layout(&self, layout: MonitorLayout) {
        if let Ok(mut g) = self.layout.lock() {
            *g = layout;
        }
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
}

pub fn spawn(ctrl: DimControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-dim".into())
        .spawn(move || run_loop(ctrl))
        .expect("spawn dim thread")
}

fn run_loop(ctrl: DimControl) {
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

/// Dim level 0..1 -> layered-window alpha 0..255.
#[cfg(any(windows, test))]
fn alpha_from_level(level: f32) -> u8 {
    (level * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Monitors that get an overlay: this PC's OS-synced tiles, or every tile when none is synced.
#[cfg(any(windows, test))]
fn monitors_for_dim(layout: &MonitorLayout) -> Vec<MonitorTile> {
    let mut local: Vec<_> = layout
        .monitors
        .iter()
        .filter(|m| m.system_synced)
        .cloned()
        .collect();
    if local.is_empty() {
        local = layout.monitors.clone();
    }
    local
}

#[cfg(any(windows, test))]
fn point_in(m: &MonitorTile, x: i32, y: i32) -> bool {
    x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + m.height as i32
}

/// True when the monitor with `id` exists and contains the cursor (its overlay stays hidden).
#[cfg(any(windows, test))]
fn is_under_cursor(monitors: &[MonitorTile], id: &str, x: i32, y: i32) -> bool {
    monitors
        .iter()
        .find(|m| m.id == id)
        .map(|m| point_in(m, x, y))
        .unwrap_or(false)
}

/// Overlay window rect `(x, y, w, h)` for a monitor.
#[cfg(any(windows, test))]
fn monitor_rect(m: &MonitorTile) -> (i32, i32, i32, i32) {
    (m.x, m.y, m.width as i32, m.height as i32)
}

/// Overlay ids that no longer match a monitor and must be destroyed.
#[cfg(any(windows, test))]
fn stale_overlay_ids<'a>(
    existing: impl Iterator<Item = &'a String>,
    monitors: &[MonitorTile],
) -> Vec<String> {
    let ids: std::collections::HashSet<_> = monitors.iter().map(|m| m.id.as_str()).collect();
    existing
        .filter(|k| !ids.contains(k.as_str()))
        .cloned()
        .collect()
}

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use ::windows::Win32::Graphics::Gdi::UpdateWindow;
    use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
        PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, ShowWindow, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, LWA_ALPHA, MSG, PM_REMOVE, SW_HIDE, SW_SHOWNOACTIVATE, WM_QUIT,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
    };
    use std::collections::HashMap;

    const CLASS: &str = "InputShareDimOverlay\0";

    struct Overlay {
        hwnd: HWND,
        rect: (i32, i32, i32, i32), // x,y,w,h
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    pub fn run(ctrl: DimControl) {
        let hmodule: HMODULE = unsafe { GetModuleHandleW(None) }.unwrap_or_default();

        let class_atom = unsafe {
            let name: Vec<u16> = CLASS.encode_utf16().collect();
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hmodule.into(),
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc)
        };
        if class_atom == 0 {
            tracing::warn!("dim overlay: RegisterClassW failed");
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }

        let mut overlays: HashMap<String, Overlay> = HashMap::new();
        let mut last_enabled = false;

        loop {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let enabled = ctrl.enabled.load(Ordering::Relaxed);
            if !enabled {
                if last_enabled {
                    destroy_all(&mut overlays);
                }
                last_enabled = false;
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            last_enabled = true;

            let layout = ctrl
                .layout
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();
            let monitors = monitors_for_dim(&layout);
            sync_overlays(&mut overlays, &monitors, hmodule);

            let mut pt = POINT::default();
            let _ = unsafe { GetCursorPos(&mut pt) };
            let alpha = alpha_from_level(ctrl.level());

            for (id, ov) in overlays.iter() {
                let under_cursor = is_under_cursor(&monitors, id, pt.x, pt.y);
                unsafe {
                    let _ =
                        SetLayeredWindowAttributes(ov.hwnd, COLORREF(0), alpha, LWA_ALPHA);
                    if under_cursor {
                        let _ = ShowWindow(ov.hwnd, SW_HIDE);
                    } else {
                        let _ = ShowWindow(ov.hwnd, SW_SHOWNOACTIVATE);
                        let _ = UpdateWindow(ov.hwnd);
                    }
                }
            }

            thread::sleep(Duration::from_millis(66));
        }
    }

    fn sync_overlays(
        map: &mut HashMap<String, Overlay>,
        monitors: &[MonitorTile],
        hmodule: HMODULE,
    ) {
        for id in stale_overlay_ids(map.keys(), monitors) {
            if let Some(ov) = map.remove(&id) {
                unsafe {
                    let _ = DestroyWindow(ov.hwnd);
                }
            }
        }
        for m in monitors {
            let rect = monitor_rect(m);
            if let Some(ov) = map.get_mut(&m.id) {
                if ov.rect != rect {
                    unsafe {
                        let _ = DestroyWindow(ov.hwnd);
                    }
                    if let Some(hwnd) = create_overlay(rect, hmodule) {
                        ov.hwnd = hwnd;
                        ov.rect = rect;
                    } else {
                        map.remove(&m.id);
                    }
                }
            } else if let Some(hwnd) = create_overlay(rect, hmodule) {
                map.insert(m.id.clone(), Overlay { hwnd, rect });
            }
        }
    }

    fn create_overlay(rect: (i32, i32, i32, i32), hmodule: HMODULE) -> Option<HWND> {
        let (x, y, w, h) = rect;
        if w <= 0 || h <= 0 {
            return None;
        }
        let class: Vec<u16> = CLASS.encode_utf16().collect();
        let title: Vec<u16> = "InputShare Dim\0".encode_utf16().collect();
        let ex = WS_EX_LAYERED
            | WS_EX_TRANSPARENT
            | WS_EX_TOPMOST
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE;
        let style = WS_POPUP | WS_VISIBLE;
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                style,
                x,
                y,
                w,
                h,
                HWND::default(),
                None,
                hmodule,
                None,
            )
        }
        .ok()?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        }
        Some(hwnd)
    }

    fn destroy_all(map: &mut HashMap<String, Overlay>) {
        for (_, ov) in map.drain() {
            unsafe {
                let _ = DestroyWindow(ov.hwnd);
            }
        }
    }
}

// Pure computations only: no overlay window is created and the cursor is never read.
#[cfg(test)]
mod tests {
    use super::*;

    fn tile(id: &str, x: i32, y: i32, w: u32, h: u32, synced: bool) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: "PC".into(),
            label: id.to_uppercase(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: synced,
        }
    }

    // ---- alpha / level ----

    #[test]
    fn alpha_endpoints() {
        assert_eq!(alpha_from_level(0.0), 0);
        assert_eq!(alpha_from_level(1.0), 255);
    }

    #[test]
    fn alpha_midpoint_rounds_to_nearest() {
        assert_eq!(alpha_from_level(0.5), 128); // 127.5 rounds away from zero
        assert_eq!(alpha_from_level(0.25), 64); // 63.75
        assert_eq!(alpha_from_level(0.75), 191); // 191.25
    }

    #[test]
    fn alpha_is_clamped() {
        assert_eq!(alpha_from_level(-1.0), 0);
        assert_eq!(alpha_from_level(2.0), 255);
        assert_eq!(alpha_from_level(f32::INFINITY), 255);
        assert_eq!(alpha_from_level(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn alpha_is_monotonic_over_the_slider() {
        let mut prev = 0u8;
        for i in 0..=100 {
            let a = alpha_from_level(i as f32 / 100.0);
            assert!(a >= prev, "alpha dropped at {i}%: {a} < {prev}");
            prev = a;
        }
        assert_eq!(prev, 255);
    }

    #[test]
    fn control_clamps_level_before_storing() {
        let c = DimControl::new(true, 5.0, MonitorLayout::default());
        assert_eq!(c.level(), 1.0);
        c.set_level(-3.0);
        assert_eq!(c.level(), 0.0);
        c.set_level(0.4);
        assert!((c.level() - 0.4).abs() < 1e-6);
        assert_eq!(alpha_from_level(c.level()), 102);

        let c = DimControl::new(false, 0.3, MonitorLayout::default());
        assert!((c.level() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn control_enabled_and_layout_are_shared_between_clones() {
        let c = DimControl::new(false, 0.5, MonitorLayout::default());
        let shared = c.clone();
        assert!(!shared.enabled.load(Ordering::Relaxed));
        c.set_enabled(true);
        assert!(shared.enabled.load(Ordering::Relaxed));

        c.set_layout(MonitorLayout {
            monitors: vec![tile("a", 0, 0, 100, 100, true)],
        });
        assert_eq!(shared.layout.lock().unwrap().monitors.len(), 1);
        c.set_level(0.9);
        assert!((shared.level() - 0.9).abs() < 1e-6);
    }

    // ---- which monitors are dimmed ----

    #[test]
    fn dim_uses_only_system_synced_tiles() {
        let layout = MonitorLayout {
            monitors: vec![
                tile("local1", 0, 0, 1920, 1080, true),
                tile("remote1", 1920, 0, 1920, 1080, false),
                tile("local2", -1920, 0, 1920, 1080, true),
            ],
        };
        let ids: Vec<_> = monitors_for_dim(&layout).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["local1", "local2"]);
    }

    #[test]
    fn dim_falls_back_to_all_tiles_when_none_synced() {
        let layout = MonitorLayout {
            monitors: vec![
                tile("a", 0, 0, 100, 100, false),
                tile("b", 100, 0, 100, 100, false),
            ],
        };
        assert_eq!(monitors_for_dim(&layout).len(), 2);
        assert!(monitors_for_dim(&MonitorLayout::default()).is_empty());
    }

    // ---- cursor / geometry ----

    #[test]
    fn point_in_is_half_open() {
        let m = tile("a", 100, 200, 50, 40, true);
        assert!(point_in(&m, 100, 200));
        assert!(point_in(&m, 149, 239));
        assert!(!point_in(&m, 150, 200));
        assert!(!point_in(&m, 100, 240));
        assert!(!point_in(&m, 99, 200));
        assert!(!point_in(&m, 100, 199));
    }

    #[test]
    fn point_in_negative_origin() {
        let m = tile("d", -2560, 400, 2560, 1440, true);
        assert!(point_in(&m, -2560, 400));
        assert!(point_in(&m, -1, 1839));
        assert!(!point_in(&m, 0, 400));
    }

    #[test]
    fn only_the_monitor_under_the_cursor_is_undimmed() {
        let monitors = vec![
            tile("a", 0, 0, 1920, 1080, true),
            tile("b", 1920, 0, 1920, 1080, true),
        ];
        assert!(is_under_cursor(&monitors, "a", 500, 500));
        assert!(!is_under_cursor(&monitors, "b", 500, 500));
        assert!(is_under_cursor(&monitors, "b", 2000, 10));
        assert!(!is_under_cursor(&monitors, "a", 2000, 10));
    }

    #[test]
    fn cursor_outside_every_monitor_dims_all() {
        let monitors = vec![tile("a", 0, 0, 100, 100, true)];
        assert!(!is_under_cursor(&monitors, "a", 5000, 5000));
    }

    #[test]
    fn unknown_overlay_id_is_never_under_cursor() {
        let monitors = vec![tile("a", 0, 0, 100, 100, true)];
        assert!(!is_under_cursor(&monitors, "ghost", 10, 10));
    }

    #[test]
    fn monitor_rect_is_x_y_w_h() {
        let m = tile("a", -10, 20, 640, 480, true);
        assert_eq!(monitor_rect(&m), (-10, 20, 640, 480));
    }

    // ---- overlay bookkeeping ----

    #[test]
    fn stale_ids_are_overlays_without_a_monitor() {
        let monitors = vec![tile("a", 0, 0, 10, 10, true), tile("b", 10, 0, 10, 10, true)];
        let existing: Vec<String> = ["a", "gone", "b", "old"].iter().map(|s| s.to_string()).collect();
        let mut stale = stale_overlay_ids(existing.iter(), &monitors);
        stale.sort();
        assert_eq!(stale, ["gone", "old"]);
    }

    #[test]
    fn no_stale_ids_when_sets_match_or_empty() {
        let monitors = vec![tile("a", 0, 0, 10, 10, true)];
        let existing = vec!["a".to_string()];
        assert!(stale_overlay_ids(existing.iter(), &monitors).is_empty());
        assert!(stale_overlay_ids(std::iter::empty(), &monitors).is_empty());
    }

    #[test]
    fn all_overlays_stale_when_monitor_list_is_empty() {
        let existing = vec!["a".to_string(), "b".to_string()];
        assert_eq!(stale_overlay_ids(existing.iter(), &[]).len(), 2);
    }
}
