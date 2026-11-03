//! Pasek „Zapora Windows”: raz, przy pierwszym uruchomieniu, prosi o zgodę na reguły zapory
//! (port TCP i UDP programu, tylko sieć lokalna). Reguły dotyczą portów, nie pliku exe, więc
//! nowa wersja nie pyta ponownie.

use super::theme::c;
use crate::firewall;
use eframe::egui::{self, RichText};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Co z paska wynikło (GUI zapisuje wtedy, że już pytano).
pub enum FirewallOutcome {
    Added,
    Failed(String),
    Dismissed,
}

enum State {
    /// Sprawdzanie w tle, czy reguły już są.
    Checking(Receiver<bool>),
    /// Reguł brak: pokaż pasek.
    Needed,
    /// Okno UAC / netsh w toku.
    Adding(Receiver<Result<(), String>>),
    Hidden,
}

pub struct FirewallPrompt {
    state: State,
}

impl FirewallPrompt {
    /// `asked`: zgody już raz pytano (dodano albo odmówiono) — wtedy pasek się nie pokazuje.
    pub fn new(tcp: u16, udp: u16, asked: bool) -> Self {
        if asked {
            return Self { state: State::Hidden };
        }
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("inputshare-firewall".into())
            .spawn(move || {
                let _ = tx.send(firewall::rules_present(tcp, udp));
            });
        Self { state: State::Checking(rx) }
    }

    /// Przycisk w zakładce Sieć: dodaj reguły teraz (też gdy pasek był odrzucony).
    pub fn add_now(&mut self, tcp: u16, udp: u16) {
        if matches!(self.state, State::Adding(_)) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("inputshare-firewall".into())
            .spawn(move || {
                let _ = tx.send(firewall::add_rules(tcp, udp));
            });
        self.state = State::Adding(rx);
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Adding(_))
    }

    /// Rysuje pasek, gdy trzeba. Wywołuj raz na klatkę.
    pub fn banner(&mut self, ctx: &egui::Context, tcp: u16, udp: u16) -> Option<FirewallOutcome> {
        match &self.state {
            State::Checking(rx) => match rx.try_recv() {
                Ok(true) | Err(TryRecvError::Disconnected) => {
                    self.state = State::Hidden;
                    return None;
                }
                Ok(false) => self.state = State::Needed,
                Err(TryRecvError::Empty) => return None,
            },
            State::Adding(rx) => match rx.try_recv() {
                Ok(Ok(())) => {
                    self.state = State::Hidden;
                    return Some(FirewallOutcome::Added);
                }
                Ok(Err(e)) => {
                    self.state = State::Hidden;
                    return Some(FirewallOutcome::Failed(e));
                }
                Err(TryRecvError::Disconnected) => {
                    self.state = State::Hidden;
                    return Some(FirewallOutcome::Failed("przerwano".into()));
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            },
            State::Needed | State::Hidden => {}
        }
        if !matches!(self.state, State::Needed | State::Adding(_)) {
            return None;
        }
        let mut outcome = None;
        let adding = self.busy();
        egui::TopBottomPanel::top("firewall_banner")
            .frame(egui::Frame::none().fill(c::CYAN_DIM).inner_margin(egui::Margin::symmetric(18.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if adding {
                        ui.spinner();
                        ui.label(RichText::new("Dodaję reguły zapory… (potwierdź w oknie Windows)").size(13.5).color(c::TEXT));
                        return;
                    }
                    ui.label(
                        RichText::new(format!(
                            "Zapora Windows: pozwolić INTmouse na połączenia w sieci lokalnej (TCP {tcp}, UDP {udp})? \
                             Pytam tylko raz — nowe wersje nie będą pytać."
                        ))
                        .size(13.5)
                        .color(c::TEXT),
                    );
                    if ui
                        .button("Zezwól")
                        .on_hover_text("Windows poprosi raz o zgodę administratora. Reguły obejmują tylko komputery z tej samej podsieci.")
                        .clicked()
                    {
                        self.add_now(tcp, udp);
                    }
                    if ui.button("Nie teraz").clicked() {
                        self.state = State::Hidden;
                        outcome = Some(FirewallOutcome::Dismissed);
                    }
                });
            });
        outcome
    }
}
