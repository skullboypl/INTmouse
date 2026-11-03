//! Lekkie powiadomienia (toasty) w prawym dolnym rogu okna.

use super::theme::c;
use eframe::egui::{self, Align2, Color32, RichText, Stroke};
use std::time::{Duration, Instant};

const MAX_VISIBLE: usize = 4;
const INFO_TTL: Duration = Duration::from_secs(6);
const ERROR_TTL: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: Kind,
    born: Instant,
}

impl Toast {
    fn ttl(&self) -> Duration {
        match self.kind {
            Kind::Info => INFO_TTL,
            Kind::Error => ERROR_TTL,
        }
    }
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(text.into(), Kind::Info);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(text.into(), Kind::Error);
    }

    fn push(&mut self, text: String, kind: Kind) {
        // Ta sama wiadomość pod rząd tylko odświeża czas życia.
        if let Some(t) = self.items.iter_mut().find(|t| t.text == text && t.kind == kind) {
            t.born = Instant::now();
            return;
        }
        self.items.push(Toast { text, kind, born: Instant::now() });
        if self.items.len() > MAX_VISIBLE * 2 {
            self.items.remove(0);
        }
    }

    /// Rysuje aktywne toasty; klik zamyka. Wywołuj raz na klatkę.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.items.retain(|t| t.born.elapsed() < t.ttl());
        if self.items.is_empty() {
            return;
        }
        let mut dismiss: Option<usize> = None;
        let start = self.items.len().saturating_sub(MAX_VISIBLE);
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -44.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for (i, t) in self.items.iter().enumerate().skip(start) {
                    let (accent, fill) = match t.kind {
                        Kind::Info => (c::CYAN, Color32::from_rgb(20, 40, 54)),
                        Kind::Error => (c::OFF, Color32::from_rgb(58, 24, 27)),
                    };
                    let resp = egui::Frame::none()
                        .fill(fill)
                        .stroke(Stroke::new(1.0_f32, accent.gamma_multiply(0.7)))
                        .rounding(egui::Rounding::same(10.0))
                        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&t.text).size(13.5).color(c::TEXT))
                                    .wrap(),
                            );
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        dismiss = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.items.remove(i);
        }
        // Wygaszanie wymaga odświeżania nawet gdy nic innego się nie dzieje.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}
