use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke, TextStyle,
    Visuals,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Control-room palette: graphite + ice cyan + amber signal.
pub mod c {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(13, 15, 19);
    pub const PANEL: Color32 = Color32::from_rgb(19, 22, 27);
    pub const NAV: Color32 = Color32::from_rgb(17, 20, 25);
    pub const NAV_HOVER: Color32 = Color32::from_rgb(30, 35, 44);
    pub const NAV_SEL: Color32 = Color32::from_rgb(24, 52, 72);
    pub const CARD: Color32 = Color32::from_rgb(27, 31, 39);
    pub const INPUT: Color32 = Color32::from_rgb(15, 18, 23);
    pub const BORDER: Color32 = Color32::from_rgb(58, 67, 82);
    pub const DIVIDER: Color32 = Color32::from_rgb(42, 48, 59);
    pub const TEXT: Color32 = Color32::from_rgb(240, 243, 247);
    pub const MUTED: Color32 = Color32::from_rgb(168, 180, 196);
    pub const FAINT: Color32 = Color32::from_rgb(122, 134, 150);
    pub const CYAN: Color32 = Color32::from_rgb(125, 211, 252);
    pub const CYAN_DIM: Color32 = Color32::from_rgb(22, 70, 98);
    pub const AMBER: Color32 = Color32::from_rgb(245, 158, 11);
    pub const OK: Color32 = Color32::from_rgb(74, 222, 128);
    pub const OFF: Color32 = Color32::from_rgb(248, 113, 113);
}

const SEMIBOLD: &str = "semibold";
static HAS_SEMIBOLD: AtomicBool = AtomicBool::new(false);

/// Semibold font when Segoe UI Semibold is available, regular otherwise.
pub fn strong(size: f32) -> FontId {
    if HAS_SEMIBOLD.load(Ordering::Relaxed) {
        FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
    } else {
        FontId::proportional(size)
    }
}

fn install_fonts(ctx: &egui::Context) {
    let read = |name: &str| std::fs::read(format!("C:\\Windows\\Fonts\\{name}")).ok();
    let Some(regular) = read("segoeui.ttf") else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("segoe".into(), FontData::from_owned(regular));
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "segoe".into());
    let fallback = proportional.clone();

    if let Some(sb) = read("seguisb.ttf") {
        fonts.font_data.insert("segoe_sb".into(), FontData::from_owned(sb));
        let mut family = vec!["segoe_sb".to_string()];
        family.extend(fallback);
        fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), family);
        HAS_SEMIBOLD.store(true, Ordering::Relaxed);
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = Visuals::dark();
    v.window_fill = c::BG;
    v.panel_fill = c::PANEL;
    v.extreme_bg_color = c::INPUT;
    v.faint_bg_color = c::CARD;
    v.code_bg_color = c::INPUT;
    v.hyperlink_color = c::CYAN;
    v.override_text_color = Some(c::TEXT);
    v.window_stroke = Stroke::new(1.0_f32, c::BORDER);
    v.window_rounding = Rounding::same(10.0);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = c::CARD;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, c::DIVIDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, c::TEXT);

    w.inactive.bg_fill = Color32::from_rgb(40, 46, 58);
    w.inactive.weak_bg_fill = Color32::from_rgb(36, 42, 53);
    w.inactive.bg_stroke = Stroke::new(1.0_f32, c::BORDER);
    w.inactive.fg_stroke = Stroke::new(1.5_f32, c::TEXT);

    w.hovered.bg_fill = Color32::from_rgb(48, 56, 70);
    w.hovered.weak_bg_fill = Color32::from_rgb(44, 52, 65);
    w.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(92, 150, 186));
    w.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);

    w.active.bg_fill = Color32::from_rgb(36, 80, 108);
    w.active.weak_bg_fill = Color32::from_rgb(36, 80, 108);
    w.active.bg_stroke = Stroke::new(1.0_f32, c::CYAN);
    w.active.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);

    w.open.bg_fill = Color32::from_rgb(40, 46, 58);
    w.open.weak_bg_fill = Color32::from_rgb(40, 46, 58);
    w.open.bg_stroke = Stroke::new(1.0_f32, c::CYAN);

    for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.rounding = Rounding::same(7.0);
    }

    v.selection.bg_fill = Color32::from_rgb(30, 85, 118);
    v.selection.stroke = Stroke::new(1.5_f32, c::CYAN);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(10.0, 8.0);
    s.button_padding = egui::vec2(14.0, 7.0);
    s.interact_size = egui::vec2(40.0, 30.0);
    s.icon_width = 18.0;
    s.icon_width_inner = 10.0;
    s.icon_spacing = 8.0;
    s.slider_width = 280.0;
    s.text_edit_width = 320.0;
    s.combo_width = 220.0;
    s.tooltip_width = 340.0;
    style.interaction.tooltip_delay = 0.15;

    style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
    style.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
    style.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
    style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
    style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.5));
    ctx.set_style(style);
}
