//! INTmouse logo, drawn procedurally so there is no binary asset to keep in sync.
//!
//! A mouse on a dark tile, left button in the site's accent orange (`www` uses the same
//! `#ff4d24` for the "INT" of the wordmark). The tray variant adds a status bubble:
//! green = sharing on, red = sharing off.

use eframe::egui::IconData;
use tray_icon::Icon;

const TILE: [u8; 3] = [14, 16, 20];
const BORDER: [u8; 3] = [46, 50, 58];
const BODY: [u8; 3] = [246, 243, 236];
const ACCENT: [u8; 3] = [255, 77, 36];
const GREEN: [u8; 3] = [34, 197, 94];
const RED: [u8; 3] = [239, 68, 68];

/// Mouse centre, half-size and the y of the line under the buttons (unit square).
const MOUSE_X: f32 = 0.42;
const MOUSE_Y: f32 = 0.45;
const BUTTONS_BOTTOM: f32 = 0.39;
const BADGE: (f32, f32) = (0.74, 0.74);

/// Sub-samples per pixel side; 4×4 gives clean edges at 16–32 px tray sizes.
const SUPERSAMPLE: u32 = 4;

/// Signed distance to a rounded box (negative inside).
fn rounded_box(x: f32, y: f32, cx: f32, cy: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = (x - cx).abs() - (half_w - radius);
    let qy = (y - cy).abs() - (half_h - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Logo colour at a point of the unit square; `None` is transparent.
/// `sharing` adds the status bubble (`None` = plain logo, used for the window icon).
fn sample(x: f32, y: f32, sharing: Option<bool>) -> Option<[u8; 3]> {
    if rounded_box(x, y, 0.5, 0.5, 0.49, 0.49, 0.22) >= 0.0 {
        return None;
    }
    let mut color = if rounded_box(x, y, 0.5, 0.5, 0.46, 0.46, 0.19) < 0.0 {
        TILE
    } else {
        BORDER
    };

    if rounded_box(x, y, MOUSE_X, MOUSE_Y, 0.18, 0.30, 0.16) < 0.0 {
        color = BODY;
        if y < BUTTONS_BOTTOM {
            if (x - MOUSE_X).abs() < 0.014 {
                color = TILE;
            } else if x < MOUSE_X {
                color = ACCENT;
            }
        }
        if (y - BUTTONS_BOTTOM).abs() < 0.008 {
            color = TILE;
        }
        if rounded_box(x, y, MOUSE_X, BUTTONS_BOTTOM - 0.09, 0.026, 0.05, 0.026) < 0.0 {
            color = TILE;
        }
    }

    if let Some(on) = sharing {
        let d = (x - BADGE.0).hypot(y - BADGE.1);
        // The ring separates the bubble from the mouse but must not eat the tile border.
        if d < 0.24 && rounded_box(x, y, 0.5, 0.5, 0.46, 0.46, 0.19) < 0.0 {
            color = TILE;
        }
        if d < 0.185 {
            color = if on { GREEN } else { RED };
        }
    }
    Some(color)
}

/// Straight-alpha RGBA of the logo at `size`×`size`.
pub fn render(size: u32, sharing: Option<bool>) -> Vec<u8> {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let samples = SUPERSAMPLE * SUPERSAMPLE;
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut covered) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let x = (px as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    if let Some([cr, cg, cb]) = sample(x, y, sharing) {
                        r += cr as u32;
                        g += cg as u32;
                        b += cb as u32;
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let i = ((py * size + px) * 4) as usize;
            rgba[i] = (r / covered) as u8;
            rgba[i + 1] = (g / covered) as u8;
            rgba[i + 2] = (b / covered) as u8;
            rgba[i + 3] = (covered * 255 / samples) as u8;
        }
    }
    rgba
}

/// Tray icon with the green (on) / red (off) bubble.
pub fn tray_icon(sharing: bool) -> Icon {
    const SIZE: u32 = 64;
    Icon::from_rgba(render(SIZE, Some(sharing)), SIZE, SIZE).expect("tray icon")
}

/// Window / taskbar icon: the logo without a status bubble.
pub fn window_icon() -> IconData {
    const SIZE: u32 = 128;
    IconData {
        rgba: render(SIZE, None),
        width: SIZE,
        height: SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: u32 = 64;

    fn pixel(rgba: &[u8], fx: f32, fy: f32) -> [u8; 4] {
        let x = (fx * SIZE as f32) as u32;
        let y = (fy * SIZE as f32) as u32;
        let i = ((y * SIZE + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn corners_are_transparent_and_tile_is_opaque() {
        let rgba = render(SIZE, Some(true));
        assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(pixel(&rgba, 0.0, 0.0)[3], 0);
        assert_eq!(pixel(&rgba, 0.12, 0.88), [TILE[0], TILE[1], TILE[2], 255]);
    }

    #[test]
    fn left_button_is_accent_and_right_button_is_light() {
        let rgba = render(SIZE, None);
        assert_eq!(pixel(&rgba, 0.34, 0.30), [ACCENT[0], ACCENT[1], ACCENT[2], 255]);
        assert_eq!(pixel(&rgba, 0.50, 0.30), [BODY[0], BODY[1], BODY[2], 255]);
    }

    #[test]
    fn badge_is_green_when_on_and_red_when_off() {
        let on = render(SIZE, Some(true));
        let off = render(SIZE, Some(false));
        assert_eq!(pixel(&on, BADGE.0, BADGE.1), [GREEN[0], GREEN[1], GREEN[2], 255]);
        assert_eq!(pixel(&off, BADGE.0, BADGE.1), [RED[0], RED[1], RED[2], 255]);
    }

    #[test]
    fn window_icon_has_no_badge() {
        let plain = render(SIZE, None);
        assert_ne!(pixel(&plain, BADGE.0, BADGE.1)[..3], GREEN);
        assert_ne!(pixel(&plain, BADGE.0, BADGE.1)[..3], RED);
    }

    /// `ICON_EXPORT=1 cargo test write_exe_icon` regenerates `assets/intmouse.ico`, the icon
    /// that `build.rs` embeds in the exe (Explorer, taskbar pins, shortcuts).
    #[test]
    fn write_exe_icon() {
        if std::env::var("ICON_EXPORT").is_err() {
            return;
        }
        let sizes = [16u32, 24, 32, 48, 64, 128, 256];
        let pngs: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&s| {
                let img = image::RgbaImage::from_raw(s, s, render(s, None)).expect("icon buffer");
                let mut png = std::io::Cursor::new(Vec::new());
                img.write_to(&mut png, image::ImageFormat::Png).expect("encode png");
                png.into_inner()
            })
            .collect();

        // ICONDIR + one ICONDIRENTRY per size, each pointing at a PNG blob.
        let mut ico = vec![0, 0, 1, 0];
        ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (&size, png) in sizes.iter().zip(&pngs) {
            let dim = if size >= 256 { 0 } else { size as u8 };
            ico.extend_from_slice(&[dim, dim, 0, 0]);
            ico.extend_from_slice(&1u16.to_le_bytes());
            ico.extend_from_slice(&32u16.to_le_bytes());
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += png.len() as u32;
        }
        for png in &pngs {
            ico.extend_from_slice(png);
        }
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/intmouse.ico"), ico)
            .expect("write ico");
    }

    /// `ICON_PREVIEW_DIR=<dir> cargo test dump_icon_previews` writes PNGs for a visual check.
    #[test]
    fn dump_icon_previews() {
        let Ok(dir) = std::env::var("ICON_PREVIEW_DIR") else {
            return;
        };
        for (name, sharing) in [("on", Some(true)), ("off", Some(false)), ("plain", None)] {
            let rgba = render(256, sharing);
            let img = image::RgbaImage::from_raw(256, 256, rgba).expect("preview buffer");
            img.save(std::path::Path::new(&dir).join(format!("intmouse-{name}.png")))
                .expect("save preview");
        }
    }
}
