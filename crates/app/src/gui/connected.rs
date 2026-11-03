//! Zakładka „Połączone”: kto jest teraz połączony, łączenie automatyczne po haśle, wykryte
//! komputery i dziennik połączeń. Dane przychodzą z `NetStatus` (tylko odczyt), ustawienia
//! trafiają do configu przez zwykły przycisk Zapisz.

use super::theme::c;
use super::widgets::{custom_row, page_header, section, toggle_row};
use super::{GuiApp, Tab};
use eframe::egui::{self, RichText};
use inputshare_protocol::PROTOCOL_VERSION;
use std::time::{Duration, Instant};

/// A discovered computer is shown while its last announce is younger than this.
const DISCOVERED_VISIBLE: Duration = Duration::from_secs(30);

impl GuiApp {
    pub(super) fn ui_connected(&mut self, ui: &mut egui::Ui) {
        let st = self.net.status_snapshot();
        page_header(
            ui,
            &self.icons.nav_machines,
            "Połączone",
            "Kto jest teraz połączony, łączenie automatyczne i dziennik połączeń.",
        );

        section(ui, "Teraz", "Komputery z działającym połączeniem.", |ui| {
            let linked: Vec<_> = st.peers.iter().filter(|p| p.tcp_ok).collect();
            if linked.is_empty() {
                custom_row(ui, "Nikt nie jest połączony", "Poniżej zobaczysz, dlaczego i co możesz zmienić.", "", false, |_| {});
            }
            for p in linked {
                let ip = p.addr.ip().to_string();
                let displays = st
                    .remote_monitors
                    .iter()
                    .find(|r| r.hostname.eq_ignore_ascii_case(&p.hostname) || r.ip == ip)
                    .map(|r| format!("{} monitor(ów)", r.monitors.len()))
                    .unwrap_or_else(|| "monitory: jeszcze nie przysłał".into());
                custom_row(ui, &p.hostname, &format!("{ip} · {displays}"), "", false, |ui| {
                    ui.label(RichText::new("● połączono").size(13.0).color(c::OK));
                });
            }
        });

        let password_set = self.cfg.network.password_required && !self.cfg.network.password.is_empty();
        let list_in_use = !self.cfg.clients.accepted.is_empty();
        section(
            ui,
            "Łączenie automatyczne",
            "Komputery z tym samym hasłem łączą się same, bez wpisywania adresów IP.",
            |ui| {
                let mut changed = false;
                changed |= toggle_row(
                    ui,
                    "Łącz automatycznie z komputerami z tym samym hasłem",
                    "Program szuka w sieci innych kopii INTmouse i łączy się z tymi, które mają to samo hasło. Działa, gdy lista w zakładce Komputery jest pusta.",
                    "Tylko przy włączonym „Wymagaj hasła”: hasło odróżnia Twój komputer od obcego w sieci.",
                    &mut self.cfg.clients.auto_connect,
                    false,
                );
                changed |= toggle_row(
                    ui,
                    "Ogłaszaj się w sieci",
                    "Co ok. 2 sekundy rozgłasza w sieci lokalnej nazwę tego komputera. Bez tego inne komputery nie zobaczą go same.",
                    "Rozgłoszenie nie zawiera hasła ani żadnych danych poza nazwą, portem i wersją.",
                    &mut self.cfg.clients.broadcast_udp,
                    false,
                );
                if changed {
                    self.dirty = true;
                }
                custom_row(ui, "Stan", "", "", false, |ui| {
                    let (text, col) = if !self.cfg.clients.auto_connect {
                        ("Wyłączone w ustawieniu powyżej.", c::MUTED)
                    } else if !password_set {
                        (
                            "Nieaktywne: ustaw hasło i włącz „Wymagaj hasła” w zakładce Sieć (to samo na obu komputerach).",
                            c::AMBER,
                        )
                    } else if list_in_use {
                        (
                            "Lista w zakładce Komputery nie jest pusta, więc łączę się tylko z adresami z listy.",
                            c::AMBER,
                        )
                    } else {
                        ("Aktywne: łączę się z komputerami, które mają to samo hasło.", c::OK)
                    };
                    ui.add(egui::Label::new(RichText::new(text).size(13.5).color(col)).wrap());
                    if !password_set && self.cfg.clients.auto_connect && ui.button("Przejdź do zakładki Sieć").clicked() {
                        self.tab = Tab::Network;
                    }
                });
            },
        );

        section(
            ui,
            "Wykryte w sieci",
            "Inne kopie INTmouse, które ogłosiły się przed chwilą.",
            |ui| {
                let now = Instant::now();
                let seen: Vec<_> = st
                    .discovered
                    .iter()
                    .filter(|d| now.duration_since(d.last_seen) < DISCOVERED_VISIBLE)
                    .collect();
                if seen.is_empty() {
                    custom_row(
                        ui,
                        "Nikogo nie widać",
                        "Drugi komputer musi mieć uruchomiony program, być w tej samej sieci i ogłaszać się (ustawienie wyżej). Sprawdź też zaporę (UDP 1046).",
                        "",
                        false,
                        |_| {},
                    );
                }
                for d in seen {
                    let ip = d.addr.ip().to_string();
                    let linked = st.peers.iter().any(|p| p.tcp_ok && p.addr.ip().to_string() == ip);
                    let same_version = d.version == PROTOCOL_VERSION;
                    let sub = if same_version {
                        format!("{ip}:{} · protokół {}", d.tcp_port, d.version)
                    } else {
                        format!(
                            "{ip}:{} · protokół {} (ten komputer ma {PROTOCOL_VERSION}): zainstaluj tę samą wersję programu na obu",
                            d.tcp_port, d.version
                        )
                    };
                    custom_row(ui, &d.hostname, &sub, "", false, |ui| {
                        ui.horizontal(|ui| {
                            if linked {
                                ui.label(RichText::new("● połączono").size(13.0).color(c::OK));
                            } else if ui
                                .add_enabled(same_version, egui::Button::new("Połącz teraz"))
                                .on_hover_text("Spróbuj od razu, bez czekania na łączenie automatyczne. Hasło musi się zgadzać.")
                                .clicked()
                            {
                                self.net.connect_to(ip.clone(), d.tcp_port);
                            }
                        });
                    });
                }
            },
        );

        if !st.link_issues.is_empty() {
            section(
                ui,
                "Problemy z połączeniem",
                "Dlaczego nie udało się połączyć. Znika, gdy połączenie się uda.",
                |ui| {
                    for issue in &st.link_issues {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} — {}", issue.peer, issue.text))
                                    .size(13.5)
                                    .color(c::OFF),
                            )
                            .wrap(),
                        );
                    }
                },
            );
        }

        section(ui, "Dziennik połączeń", "Najnowsze na górze. Pełne logi są w zakładce Logi.", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Kopiuj").clicked() {
                    let text = st
                        .events
                        .iter()
                        .map(|e| format!("{}  {}", e.at, e.text))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ui.ctx().copy_text(text);
                }
                if ui.button("Wyczyść").clicked() {
                    self.net.clear_events();
                }
                ui.label(RichText::new(format!("{} wpisów", st.events.len())).size(12.5).color(c::MUTED));
            });
            if st.events.is_empty() {
                ui.label(RichText::new("Jeszcze nic się nie wydarzyło.").size(13.0).color(c::MUTED));
            }
            egui::ScrollArea::vertical()
                .id_salt("connection_journal")
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for e in st.events.iter().rev().take(200) {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}  {}", e.at, e.text))
                                    .monospace()
                                    .size(12.5)
                                    .color(journal_color(&e.text)),
                            )
                            .wrap(),
                        );
                    }
                });
        });
    }
}

/// Journal lines about problems stand out; connects are green.
fn journal_color(text: &str) -> egui::Color32 {
    if text.starts_with("Połączono") {
        c::OK
    } else if text.starts_with("Rozłączono") || text.contains("odrzu") || text.contains("nie ") {
        c::AMBER
    } else {
        c::TEXT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_colours_follow_the_event_kind() {
        assert_eq!(journal_color("Połączono z PC (192.168.1.5)"), c::OK);
        assert_eq!(journal_color("Rozłączono z PC"), c::AMBER);
        assert_eq!(journal_color("Wykryto komputer PC (192.168.1.5)"), c::TEXT);
    }
}
