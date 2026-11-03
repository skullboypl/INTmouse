//! Zakładka Sieć: lista kart sieciowych (dropdown „Preferowany adapter”) i
//! karta „Stan sieci” (aktualny nasłuch + ostrzeżenia z huba).

use super::theme::c;
use super::widgets::{custom_row, section};
use crate::net::{self, NetAdapter, NetStatus};
use eframe::egui::{self, RichText};
use std::net::IpAddr;
use std::str::FromStr;

const ALL: &str = "All";

#[derive(Default)]
pub struct AdapterList {
    adapters: Vec<NetAdapter>,
    error: Option<String>,
    loaded: bool,
}

fn eq_ci(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim())
}

fn is_all(preferred: &str) -> bool {
    let p = preferred.trim();
    p.is_empty() || p.eq_ignore_ascii_case(ALL)
}

/// Tak jak w hubie: nazwa, opis albo GUID karty (bez rozróżniania wielkości liter).
fn adapter_matches(a: &NetAdapter, preferred: &str) -> bool {
    eq_ci(&a.name, preferred) || eq_ci(&a.description, preferred) || eq_ci(&a.id, preferred)
}

fn parse_ip(s: &str) -> Option<IpAddr> {
    let s = s.trim();
    let s = s.split('%').next().unwrap_or(s);
    IpAddr::from_str(s).ok()
}

fn adapter_label(a: &NetAdapter) -> String {
    if a.is_up {
        a.label()
    } else {
        format!("{} — nieaktywna", a.label())
    }
}

/// Tekst pokazywany na zamkniętej liście dla zapisanej wartości.
fn selected_text(preferred: &str, adapters: &[NetAdapter]) -> String {
    if is_all(preferred) {
        return ALL.into();
    }
    if let Some(a) = adapters.iter().find(|a| adapter_matches(a, preferred)) {
        return adapter_label(a);
    }
    if let Some(ip) = parse_ip(preferred) {
        return format!("{ip} (adres IP)");
    }
    format!("(niedostępna) {}", preferred.trim())
}

impl AdapterList {
    pub fn refresh(&mut self) {
        match net::list_adapters() {
            Ok(list) => {
                self.adapters = list.into_iter().filter(|a| !a.is_loopback).collect();
                self.error = None;
            }
            Err(e) => {
                self.adapters.clear();
                self.error = Some(e.to_string());
            }
        }
        self.loaded = true;
    }

    /// Dropdown + „Odśwież”. Zapisuje `NetAdapter.name` (albo „All”). `true` = zmiana.
    pub fn ui(&mut self, ui: &mut egui::Ui, preferred: &mut String) -> bool {
        if !self.loaded {
            self.refresh();
        }
        let mut changed = false;
        let mut refresh = false;
        let shown = selected_text(preferred, &self.adapters);
        let current_known = is_all(preferred) || self.adapters.iter().any(|a| adapter_matches(a, preferred));
        custom_row(
            ui,
            "Preferowany adapter",
            "„All” albo konkretna karta. Zapis po nazwie karty; wpisany wcześniej adres IP nadal działa.",
            "Gdy masz VPN lub wiele kart — wybierz kartę z tego samego LAN-u co drugi PC.",
            false,
            |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("net_adapter")
                        .selected_text(shown)
                        .width((ui.available_width().min(360.0) - 90.0).max(140.0))
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(is_all(preferred), ALL).clicked() && !is_all(preferred) {
                                *preferred = ALL.into();
                                changed = true;
                            }
                            if !current_known {
                                // Zapisana wartość spoza listy zostaje widoczna i zaznaczona.
                                let _ = ui.selectable_label(true, selected_text(preferred, &self.adapters));
                            }
                            for a in &self.adapters {
                                let selected = adapter_matches(a, preferred);
                                if ui.selectable_label(selected, adapter_label(a)).clicked() && !selected {
                                    *preferred = a.name.clone();
                                    changed = true;
                                }
                            }
                        });
                    if ui
                        .button("Odśwież")
                        .on_hover_text("Wczytaj ponownie listę kart sieciowych")
                        .clicked()
                    {
                        refresh = true;
                    }
                });
                if let Some(e) = &self.error {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Nie udało się wczytać listy kart: {e}"))
                                .size(12.5)
                                .color(c::OFF),
                        )
                        .wrap(),
                    );
                } else if !current_known {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Zapisanej karty nie ma w systemie — sieć użyje „All”, dopóki nie wybierzesz innej.",
                            )
                            .size(12.5)
                            .color(c::AMBER),
                        )
                        .wrap(),
                    );
                }
            },
        );
        if refresh {
            self.refresh();
        }
        changed
    }
}

fn addr_list(addrs: &[std::net::SocketAddr]) -> String {
    if addrs.is_empty() {
        "— (brak nasłuchu)".into()
    } else {
        addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
    }
}

/// Karta „Stan sieci”: realnie zbindowane gniazda, błąd i ostrzeżenia huba.
pub fn status_section(ui: &mut egui::Ui, st: &NetStatus) {
    section(
        ui,
        "Stan sieci",
        "Na czym INTmouse faktycznie nasłuchuje (po zastosowaniu adaptera i IPv6).",
        |ui| {
            custom_row(ui, "Nasłuch TCP", "Handshake, schowek, pliki.", "", false, |ui| {
                ui.label(RichText::new(addr_list(&st.tcp_listen)).monospace().size(13.0));
            });
            custom_row(ui, "Nasłuch UDP", "Pozycja myszy i ogłoszenia w sieci.", "", false, |ui| {
                ui.label(RichText::new(addr_list(&st.udp_listen)).monospace().size(13.0));
            });
            if let Some(e) = &st.last_error {
                custom_row(ui, "Ostatni błąd", "", "", false, |ui| {
                    ui.add(egui::Label::new(RichText::new(e).size(13.0).color(c::OFF)).wrap());
                });
            }
            if !st.warnings.is_empty() {
                custom_row(ui, "Ostrzeżenia", "Nie blokują pracy, ale warto je znać.", "", false, |ui| {
                    for w in &st.warnings {
                        ui.add(
                            egui::Label::new(RichText::new(format!("• {w}")).size(13.0).color(c::AMBER))
                                .wrap(),
                        );
                    }
                });
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str, desc: &str, ipv4: &str) -> NetAdapter {
        NetAdapter {
            name: name.into(),
            description: desc.into(),
            id: "{GUID-1}".into(),
            ipv4: vec![ipv4.parse().unwrap()],
            ipv6: vec![],
            ipv6_if_index: 0,
            is_up: true,
            is_loopback: false,
        }
    }

    #[test]
    fn empty_and_all_are_all() {
        assert_eq!(selected_text("", &[]), "All");
        assert_eq!(selected_text(" all ", &[]), "All");
    }

    #[test]
    fn known_adapter_shows_label_unknown_shows_marker() {
        let list = [adapter("Ethernet", "Intel I219", "192.168.1.10")];
        assert_eq!(selected_text("ethernet", &list), "Ethernet (192.168.1.10)");
        assert_eq!(selected_text("Intel I219", &list), "Ethernet (192.168.1.10)");
        assert_eq!(selected_text("Wi-Fi 2", &list), "(niedostępna) Wi-Fi 2");
    }

    #[test]
    fn ip_literal_is_kept_as_ip() {
        assert_eq!(selected_text("10.0.0.5", &[]), "10.0.0.5 (adres IP)");
        assert_eq!(selected_text("fe80::1%12", &[]), "fe80::1 (adres IP)");
    }
}
