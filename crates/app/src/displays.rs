//! Enumerate physical displays for Monitor Manager.

use inputshare_core::{local_hostname, MonitorLayout, MonitorTile};

/// Read the current OS monitor layout (positions match the Windows virtual desktop).
pub fn system_layout() -> anyhow::Result<MonitorLayout> {
    #[cfg(windows)]
    {
        win::enumerate()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("enumeracja monitorów na tej platformie jeszcze niezaimplementowana")
    }
}

/// One monitor as reported by the OS (virtual-desktop pixels, right/bottom exclusive).
#[cfg(any(windows, test))]
#[derive(Clone)]
struct RawMonitor {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    primary: bool,
    name: String,
}

/// Turn raw OS monitors into Monitor Manager tiles.
///
/// Order: top-to-bottom, then left-to-right; labels `A`, `B`, ... (wrap after `Z`) follow that
/// order; ids are `sys-{index}-{device slug}-{w}x{h}`; sizes are at least 1px.
#[cfg(any(windows, test))]
fn tiles_from_raw(mut raw: Vec<RawMonitor>, host: &str) -> Vec<MonitorTile> {
    raw.sort_by(|a, b| a.top.cmp(&b.top).then(a.left.cmp(&b.left)));
    raw.into_iter()
        .enumerate()
        .map(|(i, m)| {
            let label = char::from_u32(b'A' as u32 + (i as u32) % 26)
                .unwrap_or('X')
                .to_string();
            let width = (m.right - m.left).max(1) as u32;
            let height = (m.bottom - m.top).max(1) as u32;
            let slug = m
                .name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>();
            MonitorTile {
                id: format!("sys-{i}-{slug}-{width}x{height}"),
                host: host.to_string(),
                label,
                x: m.left,
                y: m.top,
                width,
                height,
                primary: m.primary,
                system_synced: true,
            }
        })
        .collect()
}

/// Device name from a NUL-terminated UTF-16 buffer (`MONITORINFOEXW::szDevice`).
#[cfg(any(windows, test))]
fn device_name_from_wide(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::mem::size_of;
    use ::windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use ::windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };

    /// MONITORINFOF_PRIMARY from winuser.h
    const MONITORINFOF_PRIMARY: u32 = 1;

    pub fn enumerate() -> anyhow::Result<MonitorLayout> {
        let mut raw: Vec<RawMonitor> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                HDC::default(),
                None,
                Some(monitor_enum_proc),
                LPARAM(&mut raw as *mut _ as isize),
            );
        }
        if raw.is_empty() {
            anyhow::bail!("system nie zwrócił żadnego monitora");
        }

        let monitors = tiles_from_raw(raw, &local_hostname());

        Ok(MonitorLayout { monitors })
    }

    unsafe extern "system" fn monitor_enum_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<RawMonitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        let ok = GetMonitorInfoW(
            hmonitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
        .as_bool();
        if ok {
            let rect = info.monitorInfo.rcMonitor;
            let primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
            let name = device_name_from_wide(&info.szDevice);
            list.push(RawMonitor {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
                primary,
                name,
            });
        }
        TRUE
    }
}

// Synthetic monitor data only: the real display configuration is never enumerated, so these
// tests give the same result on any machine / resolution.
#[cfg(test)]
mod tests {
    use super::*;

    fn raw(left: i32, top: i32, right: i32, bottom: i32, primary: bool, name: &str) -> RawMonitor {
        RawMonitor {
            left,
            top,
            right,
            bottom,
            primary,
            name: name.into(),
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn size_is_right_minus_left_and_bottom_minus_top() {
        let t = tiles_from_raw(vec![raw(0, 0, 2560, 1440, true, r"\\.\DISPLAY1")], "PC");
        assert_eq!((t[0].width, t[0].height), (2560, 1440));
        assert_eq!((t[0].x, t[0].y), (0, 0));
        assert!(t[0].primary);
        assert!(t[0].system_synced, "OS tiles are marked system_synced");
        assert_eq!(t[0].host, "PC");
    }

    #[test]
    fn negative_virtual_coordinates_are_preserved() {
        let t = tiles_from_raw(vec![raw(-2560, 400, 0, 1840, false, "D")], "PC");
        assert_eq!((t[0].x, t[0].y), (-2560, 400));
        assert_eq!((t[0].width, t[0].height), (2560, 1440));
    }

    #[test]
    fn degenerate_rect_has_minimum_size_of_one_pixel() {
        let t = tiles_from_raw(vec![raw(10, 10, 10, 10, false, "X"), raw(50, 0, 40, -5, false, "Y")], "PC");
        for tile in &t {
            assert_eq!((tile.width, tile.height), (1, 1));
        }
    }

    #[test]
    fn sorted_top_to_bottom_then_left_to_right_and_labelled_in_that_order() {
        // User's desk: A above B, D-B-C row (D slightly lower). Input in arbitrary order.
        let t = tiles_from_raw(
            vec![
                raw(2560, 0, 5120, 1440, false, "C"),
                raw(-2560, 400, 0, 1840, false, "D"),
                raw(0, 0, 2560, 1440, true, "B"),
                raw(320, -1080, 2240, 0, false, "A"),
            ],
            "PC",
        );
        let order: Vec<(&str, i32, i32)> = t.iter().map(|m| (m.label.as_str(), m.x, m.y)).collect();
        assert_eq!(
            order,
            [("A", 320, -1080), ("B", 0, 0), ("C", 2560, 0), ("D", -2560, 400)]
        );
    }

    #[test]
    fn same_top_is_ordered_by_left() {
        let t = tiles_from_raw(
            vec![
                raw(1920, 0, 3840, 1080, false, "R"),
                raw(0, 0, 1920, 1080, true, "L"),
            ],
            "PC",
        );
        assert_eq!(t[0].x, 0);
        assert_eq!(t[1].x, 1920);
        assert_eq!(t[0].label, "A");
        assert_eq!(t[1].label, "B");
    }

    #[test]
    fn labels_wrap_after_z() {
        let monitors: Vec<_> = (0..28)
            .map(|i| raw(i * 100, 0, i * 100 + 100, 100, false, "M"))
            .collect();
        let t = tiles_from_raw(monitors, "PC");
        assert_eq!(t[0].label, "A");
        assert_eq!(t[25].label, "Z");
        assert_eq!(t[26].label, "A");
        assert_eq!(t[27].label, "B");
    }

    #[test]
    fn id_contains_index_slug_and_size() {
        let t = tiles_from_raw(
            vec![
                raw(0, 0, 1920, 1080, true, r"\\.\DISPLAY1"),
                raw(1920, 0, 3840, 1080, false, r"\\.\DISPLAY2"),
            ],
            "PC",
        );
        // `\\.\` becomes four dashes; the format adds its own separators around the slug.
        assert_eq!(t[0].id, "sys-0-----DISPLAY1-1920x1080");
        assert_eq!(t[1].id, "sys-1-----DISPLAY2-1920x1080");
    }

    #[test]
    fn ids_are_unique_even_with_identical_monitors() {
        let t = tiles_from_raw(
            vec![
                raw(0, 0, 1920, 1080, false, "SAME"),
                raw(1920, 0, 3840, 1080, false, "SAME"),
            ],
            "PC",
        );
        assert_ne!(t[0].id, t[1].id);
    }

    #[test]
    fn slug_replaces_non_alphanumerics_with_dash() {
        let t = tiles_from_raw(vec![raw(0, 0, 10, 10, false, "a b.c_d-ż9")], "PC");
        assert_eq!(t[0].id, "sys-0-a-b-c-d--9-10x10");
    }

    #[test]
    fn empty_input_gives_no_tiles() {
        assert!(tiles_from_raw(vec![], "PC").is_empty());
    }

    #[test]
    fn device_name_stops_at_first_nul() {
        let mut buf = wide(r"\\.\DISPLAY3");
        buf.extend([0, 0, 0, b'x' as u16]);
        assert_eq!(device_name_from_wide(&buf), r"\\.\DISPLAY3");
    }

    #[test]
    fn device_name_without_nul_uses_whole_buffer() {
        assert_eq!(device_name_from_wide(&wide("ABC")), "ABC");
    }

    #[test]
    fn device_name_empty_and_all_nul() {
        assert_eq!(device_name_from_wide(&[]), "");
        assert_eq!(device_name_from_wide(&[0u16; 32]), "");
    }

    #[test]
    fn device_name_lossy_on_invalid_utf16() {
        let s = device_name_from_wide(&[0x41, 0xD800, 0x42]); // lone surrogate
        assert_eq!(s, "A\u{FFFD}B");
    }

    #[test]
    fn layout_from_tiles_feeds_wrap_logic() {
        // The tiles built here are what the wrap loop consumes: a quick sanity check that
        // the generated ids are findable and geometry is consistent with `MonitorLayout`.
        let layout = MonitorLayout {
            monitors: tiles_from_raw(
                vec![
                    raw(0, 0, 1920, 1080, true, "M1"),
                    raw(1920, 0, 3840, 1080, false, "M2"),
                ],
                "PC",
            ),
        };
        let hit = layout
            .neighbor_across(&layout.monitors[0].id, inputshare_protocol::Edge::Right, 0.5)
            .expect("M2 is right of M1");
        assert_eq!(hit.monitor_id, layout.monitors[1].id);
    }
}
