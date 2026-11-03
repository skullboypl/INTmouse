use super::theme::{self, c};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Image, Label, Rect, RichText, Sense, Stroke,
    TextureHandle, Vec2,
};

const TITLE_SIZE: f32 = 15.0;
const SUBTITLE_SIZE: f32 = 13.0;
const CONTROL_MAX_W: f32 = 360.0;

pub fn page_header(ui: &mut egui::Ui, icon: &TextureHandle, title: &str, subtitle: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        ui.add(Image::new(icon).fit_to_exact_size(Vec2::splat(34.0)));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(theme::strong(22.0)).color(c::TEXT));
            ui.add(Label::new(RichText::new(subtitle).size(14.0).color(c::MUTED)).wrap());
        });
    });
    ui.add_space(18.0);
}

/// Card with a header; rows added inside draw their own divider above themselves.
pub fn section(ui: &mut egui::Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(c::CARD)
        .stroke(Stroke::new(1.0_f32, c::BORDER))
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(RichText::new(title).font(theme::strong(16.5)).color(c::CYAN));
                ui.add(Label::new(RichText::new(subtitle).size(SUBTITLE_SIZE).color(c::MUTED)).wrap());
            });
            ui.add_space(2.0);
            body(ui);
        });
    ui.add_space(16.0);
}

fn row_divider(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0_f32, c::DIVIDER));
    ui.add_space(4.0);
}

/// Small amber pill: „wkrótce”.
pub fn soon_chip(ui: &mut egui::Ui) {
    egui::Frame::none()
        .fill(Color32::from_rgb(60, 42, 18))
        .stroke(Stroke::new(1.0_f32, c::AMBER))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new("wkrótce")
                    .size(10.5)
                    .strong()
                    .color(c::AMBER),
            );
        });
}

fn title_line(ui: &mut egui::Ui, title: &str, tip: &str, soon: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let r = ui.add(
            Label::new(RichText::new(title).font(theme::strong(TITLE_SIZE)).color(c::TEXT))
                .sense(Sense::click()),
        );
        if soon {
            soon_chip(ui);
        }
        info_tip(ui, tip);
        r
    })
    .inner
}

fn subtitle(ui: &mut egui::Ui, text: &str) {
    if !text.is_empty() {
        ui.add(Label::new(RichText::new(text).size(SUBTITLE_SIZE).color(c::MUTED)).wrap());
    }
}

/// Divider + title/subtitle stack, then `body` underneath at full row width.
/// `soon = true` shows the amber „wkrótce” chip (option saved, runtime not ready).
pub fn custom_row<R>(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    tip: &str,
    soon: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    row_divider(ui);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        title_line(ui, title, tip, soon);
        subtitle(ui, sub);
    });
    ui.add_space(4.0);
    ui.scope(body).inner
}

/// Checkbox on the left, title + wrapped subtitle in the main column. Title text toggles too.
pub fn toggle_row(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    tip: &str,
    value: &mut bool,
    soon: bool,
) -> bool {
    row_divider(ui);
    let mut changed = false;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = 22.0;
            changed |= ui.checkbox(value, "").changed();
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let t = title_line(ui, title, tip, soon);
            if t.clicked() {
                *value = !*value;
                changed = true;
            }
            t.on_hover_cursor(CursorIcon::PointingHand);
            subtitle(ui, sub);
        });
    });
    changed
}

pub fn text_row(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    tip: &str,
    value: &mut String,
    soon: bool,
) -> bool {
    custom_row(ui, title, sub, tip, soon, |ui| {
        let w = ui.available_width().min(CONTROL_MAX_W);
        ui.add(egui::TextEdit::singleline(value).desired_width(w).margin(Vec2::new(8.0, 5.0)))
            .changed()
    })
}

pub fn slider_row(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    tip: &str,
    value: &mut f32,
    soon: bool,
) -> bool {
    custom_row(ui, title, sub, tip, soon, |ui| {
        ui.spacing_mut().slider_width =
            (ui.available_width() - 90.0).clamp(140.0, CONTROL_MAX_W - 70.0);
        ui.add(egui::Slider::new(value, 0.0..=1.0).show_value(true).fixed_decimals(2))
            .changed()
    })
}

pub fn port_row(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    tip: &str,
    value: &mut u16,
    soon: bool,
) -> bool {
    custom_row(ui, title, sub, tip, soon, |ui| {
        ui.spacing_mut().interact_size.x = 96.0;
        let mut v = *value as i32;
        let changed = ui
            .add(egui::DragValue::new(&mut v).range(1..=65535).speed(1.0))
            .on_hover_text("Przeciągnij lub kliknij dwukrotnie, aby wpisać")
            .changed();
        if changed {
            *value = v as u16;
        }
        changed
    })
}

/// 20px "?" badge with a help cursor; nothing is drawn for an empty tip.
pub fn info_tip(ui: &mut egui::Ui, tip: &str) {
    if tip.is_empty() {
        return;
    }
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
    let hovered = resp.hovered();
    let col = if hovered { c::CYAN } else { c::MUTED };
    let fill = if hovered { c::CYAN_DIM } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.circle(rect.center(), 9.0, fill, Stroke::new(1.2_f32, col));
    p.text(rect.center(), Align2::CENTER_CENTER, "?", theme::strong(12.5), col);
    resp.on_hover_cursor(CursorIcon::Help).on_hover_text(tip);
}

/// `compact`: icon only (narrow window), the label goes into the tooltip.
pub fn nav_button(
    ui: &mut egui::Ui,
    icon: &TextureHandle,
    label: &str,
    selected: bool,
    compact: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 8.0, c::NAV_SEL);
        let bar = Rect::from_min_size(rect.min + Vec2::new(0.0, 9.0), Vec2::new(3.0, rect.height() - 18.0));
        p.rect_filled(bar, 2.0, c::CYAN);
    } else if resp.hovered() {
        p.rect_filled(rect, 8.0, c::NAV_HOVER);
    }
    let icon_x = if compact { rect.center().x } else { rect.left() + 24.0 };
    let icon_rect = Rect::from_center_size(egui::pos2(icon_x, rect.center().y), Vec2::splat(20.0));
    Image::new(icon).paint_at(ui, icon_rect);
    if compact {
        return resp.on_hover_cursor(CursorIcon::PointingHand);
    }
    let (font, color) = if selected {
        (theme::strong(14.5), c::TEXT)
    } else if resp.hovered() {
        (FontId::proportional(14.5), c::TEXT)
    } else {
        (FontId::proportional(14.5), c::MUTED)
    };
    ui.painter().text(
        egui::pos2(icon_rect.right() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
