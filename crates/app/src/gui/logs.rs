//! Zakładka „Logi”: pełne logi programu z tej sesji (ostatnie linie, bufor w pamięci), filtr,
//! szukanie, kopiowanie i przełącznik trybu szczegółowego, który dopisuje też plik logu.

use super::theme::c;
use super::widgets::{page_header, toggle_row};
use super::GuiApp;
use crate::diag;
use eframe::egui::{self, Color32, RichText};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFilter {
    #[default]
    All,
    /// Warnings and errors only.
    Problems,
    /// Lines from the network layer (`inputshare::net…`).
    Network,
}

impl LogFilter {
    fn label(self) -> &'static str {
        match self {
            LogFilter::All => "Wszystko",
            LogFilter::Problems => "Ostrzeżenia i błędy",
            LogFilter::Network => "Sieć i połączenia",
        }
    }

    fn matches(self, line: &str) -> bool {
        match self {
            LogFilter::All => true,
            LogFilter::Problems => level_of(line) >= Level::Warn,
            LogFilter::Network => line.contains("::net"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// `tracing` prints the level as a padded word between the timestamp and the target.
fn level_of(line: &str) -> Level {
    for (word, level) in [
        (" ERROR ", Level::Error),
        (" WARN ", Level::Warn),
        (" INFO ", Level::Info),
        (" DEBUG ", Level::Debug),
        (" TRACE ", Level::Trace),
    ] {
        if line.contains(word) {
            return level;
        }
    }
    Level::Info
}

fn color_of(level: Level) -> Color32 {
    match level {
        Level::Error => c::OFF,
        Level::Warn => c::AMBER,
        Level::Info => c::TEXT,
        Level::Debug | Level::Trace => c::MUTED,
    }
}

/// Lines passing the filter and the (case-insensitive) search text.
/// The Logi tab's lines, kept between frames: copying and filtering 3000 lines every frame
/// made the window lag while detailed logging writes many lines a second.
struct LogView {
    key: (u64, LogFilter, String),
    all: usize,
    shown: Vec<String>,
}

static LOG_VIEW: std::sync::Mutex<Option<LogView>> = std::sync::Mutex::new(None);

/// `(shown lines, all lines)` for this filter and search, rebuilt only when something changed.
fn cached_view(filter: LogFilter, search: &str) -> (Vec<String>, usize) {
    let key = (diag::log_revision(), filter, search.to_string());
    let mut g = LOG_VIEW.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|v| v.key != key) {
        let all = diag::recent_log_lines();
        let shown = visible_lines(&all, filter, search).into_iter().cloned().collect();
        *g = Some(LogView { key, all: all.len(), shown });
    }
    let v = g.as_ref().expect("just built");
    (v.shown.clone(), v.all)
}

fn visible_lines<'a>(lines: &'a [String], filter: LogFilter, search: &str) -> Vec<&'a String> {
    let needle = search.trim().to_lowercase();
    lines
        .iter()
        .filter(|l| filter.matches(l))
        .filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle))
        .collect()
}

impl GuiApp {
    pub(super) fn ui_logs(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_misc,
            "Logi",
            "Pełne logi programu z tej sesji (ostatnie 3000 linii).",
        );

        if toggle_row(
            ui,
            "Tryb szczegółowy",
            "Dodaje linie debug (uzgadnianie połączeń, powody odrzuceń) i dopisuje logi do pliku inputshare.log obok configu.",
            "Włącz na czas szukania błędu: szczegółowe logi są dłuższe. Zapisz ustawienie, jeśli ma zostać po restarcie.",
            &mut self.cfg.misc.diagnostics,
            false,
        ) {
            self.dirty = true;
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("log_filter")
                .selected_text(self.log_filter.label())
                .show_ui(ui, |ui| {
                    for f in [LogFilter::All, LogFilter::Problems, LogFilter::Network] {
                        ui.selectable_value(&mut self.log_filter, f, f.label());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.log_search)
                    .hint_text("Szukaj w logach")
                    .desired_width(200.0),
            );
            let (shown, all_len) = cached_view(self.log_filter, &self.log_search);
            if ui.button("Kopiuj widoczne").clicked() {
                ui.ctx().copy_text(shown.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"));
                self.toasts.info("Skopiowano logi do schowka.");
            }
            if ui.button("Wyczyść").clicked() {
                diag::clear_recent_log_lines();
            }
            if ui
                .button("Pokaż plik logu")
                .on_hover_text("Otwiera folder z inputshare.log (plik powstaje w trybie szczegółowym).")
                .clicked()
            {
                self.reveal_log_file();
            }
            ui.label(
                RichText::new(format!("{} z {} linii", shown.len(), all_len))
                    .size(12.5)
                    .color(c::MUTED),
            );
        });
        ui.add_space(6.0);

        let (lines, _) = cached_view(self.log_filter, &self.log_search);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 3.0;
        egui::ScrollArea::vertical()
            .id_salt("full_log")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, lines.len(), |ui, range| {
                for line in &lines[range] {
                    ui.add(
                        egui::Label::new(
                            RichText::new(line.as_str())
                                .monospace()
                                .size(12.0)
                                .color(color_of(level_of(line))),
                        )
                        .truncate(),
                    );
                }
            });
    }

    fn reveal_log_file(&mut self) {
        let path = self.diag.log_path().to_path_buf();
        if !path.exists() {
            self.toasts.error("Plik logu jeszcze nie istnieje. Włącz tryb szczegółowy.");
            return;
        }
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec![
            "2026-10-31 17:44:12  INFO inputshare::gui: Starting UI".into(),
            "2026-10-31 17:44:13  WARN inputshare::net::hub: 192.168.1.5: złe hasło".into(),
            "2026-10-31 17:44:14 ERROR inputshare::net::hub: bind failed".into(),
            "2026-10-31 17:44:15 DEBUG inputshare::input: tick".into(),
            "no level here".into(),
        ]
    }

    #[test]
    fn levels_are_read_from_the_padded_word() {
        let l = lines();
        assert!(level_of(&l[0]) == Level::Info);
        assert!(level_of(&l[1]) == Level::Warn);
        assert!(level_of(&l[2]) == Level::Error);
        assert!(level_of(&l[3]) == Level::Debug);
        assert!(level_of(&l[4]) == Level::Info, "unknown lines are shown as info");
    }

    #[test]
    fn filters_pick_problems_and_network_lines() {
        let l = lines();
        assert_eq!(visible_lines(&l, LogFilter::All, "").len(), 5);
        assert_eq!(visible_lines(&l, LogFilter::Problems, "").len(), 2);
        assert_eq!(visible_lines(&l, LogFilter::Network, "").len(), 2);
    }

    #[test]
    fn search_is_case_insensitive_and_combines_with_the_filter() {
        let l = lines();
        assert_eq!(visible_lines(&l, LogFilter::All, "ZŁE HASŁO").len(), 1);
        assert_eq!(visible_lines(&l, LogFilter::Problems, "bind").len(), 1);
        assert_eq!(visible_lines(&l, LogFilter::Problems, "starting").len(), 0);
        assert_eq!(visible_lines(&l, LogFilter::All, "   ").len(), 5, "blank search shows everything");
    }
}
