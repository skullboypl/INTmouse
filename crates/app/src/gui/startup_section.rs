//! Sekcja „Uruchamianie” w zakładce Bezpieczeństwo: start z systemem i uruchamianie jako
//! administrator. Zmiana trybu administratora zapisuje ustawienia i uruchamia program ponownie
//! w nowym trybie (Windows pyta wtedy o zgodę, UAC).

use super::theme::c;
use super::widgets::{custom_row, section, toggle_row};
use super::GuiApp;
use crate::startup::{self, RelaunchError};
use eframe::egui::{self, RichText};

impl GuiApp {
    pub(super) fn ui_startup(&mut self, ui: &mut egui::Ui) {
        section(
            ui,
            "Uruchamianie",
            "Start razem z Windows i uprawnienia programu.",
            |ui| {
                let mut launch = self.cfg.startup.launch_at_login;
                if toggle_row(
                    ui,
                    "Uruchamiaj przy starcie systemu",
                    "Po zalogowaniu program startuje schowany w ikonie obok zegara. Zmiana działa od razu.",
                    "Zwykły użytkownik: wpis w rejestrze Windows. Administrator: zadanie w Harmonogramie zadań, bo wpis w rejestrze nie uruchomi programu z uprawnieniami bez pytania przy każdym logowaniu.",
                    &mut launch,
                    false,
                ) {
                    self.set_launch_at_login(launch);
                }

                let mut admin = self.cfg.startup.run_as_admin;
                if toggle_row(
                    ui,
                    "Uruchamiaj jako administrator",
                    "Pozwala sterować oknami programów uruchomionych jako administrator (Menedżer zadań, niektóre gry i instalatory). Po zmianie program uruchomi się ponownie.",
                    "Windows blokuje wysyłanie klawiszy i myszy do okna o wyższych uprawnieniach z programu bez nich. Zobaczysz pytanie Windows (UAC). Przeciąganie plików z Eksploratora na okno programu nie działa w trybie administratora.",
                    &mut admin,
                    false,
                ) {
                    self.set_run_as_admin(admin);
                }

                custom_row(ui, "Stan", "", "", false, |ui| {
                    let (text, col) = if self.elevated {
                        ("Ta kopia działa jako administrator.", c::OK)
                    } else {
                        ("Ta kopia działa jako zwykły użytkownik.", c::MUTED)
                    };
                    ui.label(RichText::new(text).size(13.5).color(col));
                    ui.label(
                        RichText::new(format!("Autostart: {}", self.autostart_label))
                            .size(13.0)
                            .color(c::MUTED),
                    );
                    if let Some(note) = &self.startup_note {
                        ui.add(egui::Label::new(RichText::new(note).size(13.0).color(c::AMBER)).wrap());
                    }
                });
            },
        );
    }

    fn set_launch_at_login(&mut self, on: bool) {
        let Ok(exe) = std::env::current_exe() else {
            self.startup_note = Some("Nie znam ścieżki programu, autostart niedostępny.".into());
            return;
        };
        match startup::apply(on, self.cfg.startup.run_as_admin, &exe) {
            Ok(()) => {
                self.cfg.startup.launch_at_login = on;
                self.startup_note = None;
                self.save();
            }
            Err(e) => {
                // Keep the setting that matches the system.
                self.startup_note = Some(e);
            }
        }
        self.autostart_label = startup::describe().to_string();
    }

    /// Save the new mode, tidy the autostart while this copy still has the rights for it, then
    /// start the program again in the other mode and close this copy.
    fn set_run_as_admin(&mut self, on: bool) {
        let Ok(exe) = std::env::current_exe() else {
            self.startup_note = Some("Nie znam ścieżki programu, nie mogę uruchomić ponownie.".into());
            return;
        };
        self.cfg.startup.run_as_admin = on;
        self.save();
        // Going to administrator mode cannot touch the task yet (the new copy does it); going back
        // can, and must, before the rights are gone.
        if let Err(e) = startup::apply(self.cfg.startup.launch_at_login, on, &exe) {
            if !on {
                self.revert_run_as_admin(on, e);
                return;
            }
        }
        match startup::relaunch(&exe, on) {
            Ok(()) => {
                self.startup_note = None;
                self.quit = true;
            }
            Err(RelaunchError::Cancelled) => {
                self.revert_run_as_admin(on, "Anulowano w oknie Windows (UAC). Program działa bez zmian.".into());
            }
            Err(RelaunchError::Failed(e)) => {
                self.revert_run_as_admin(on, format!("Nie udało się uruchomić programu ponownie: {e}"));
            }
        }
    }

    fn revert_run_as_admin(&mut self, attempted: bool, note: String) {
        self.cfg.startup.run_as_admin = !attempted;
        self.startup_note = Some(note);
        self.save();
        self.autostart_label = startup::describe().to_string();
    }
}
