//! Aktualizacje: baner „Dostępna wersja…” (Aktualizuj / Później) jak w LiveDelay oraz panel
//! „Wersja i aktualizacja” (sprawdzanie, pobieranie z weryfikacją SHA-256, rollback).
//! Cała praca sieciowa i na plikach idzie w wątkach w tle — UI tylko czyta stan z
//! `Arc<Mutex<_>>`.

use super::theme::c;
use super::widgets::{custom_row, section};
use crate::net::NetControl;
use crate::updater::{self, UpdateInfo, UpdateStatus};
use eframe::egui::{self, RichText};
use inputshare_core::AppConfig;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Default)]
struct Inner {
    /// Opis trwającej operacji (`None` = bezczynny).
    busy: Option<&'static str>,
    status: Option<UpdateStatus>,
    /// Wynik ostatniej operacji innej niż sprawdzanie (rollback / pobranie).
    message: Option<String>,
    /// Gotowa aktualizacja albo rollback: restart ją uruchomi.
    restart_ready: bool,
    /// Istnieje `INTmouse.exe.old` do przywrócenia.
    has_old: bool,
    /// Toast do pokazania przez GUI: `(błąd?, tekst)`.
    toast: Option<(bool, String)>,
    /// Wersja odłożona przyciskiem „Później” (zapisana w pliku obok configu).
    skipped: String,
    /// Wersja, o której już powiadomiono systemowo, żeby nie powtarzać tego co 12 godzin.
    announced: String,
    /// Powiadomienie systemowe do wysłania przez GUI: numer nowej wersji.
    notice: Option<String>,
    /// „Aktualizuj” z banera: po pobraniu i weryfikacji uruchom ponownie od razu.
    restart_when_ready: bool,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn has_old_build() -> bool {
    std::env::current_exe()
        .map(|exe| updater::old_path(&exe).exists())
        .unwrap_or(false)
}

#[derive(Clone)]
pub struct UpdatesUi {
    inner: Arc<Mutex<Inner>>,
    /// Plik z odłożoną wersją (`update-skip.txt` obok configu).
    skip_path: Arc<PathBuf>,
}

impl UpdatesUi {
    pub fn new(skip_path: PathBuf) -> Self {
        let skipped = std::fs::read_to_string(&skip_path)
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        let inner = Inner { has_old: has_old_build(), skipped, ..Inner::default() };
        Self { inner: Arc::new(Mutex::new(inner)), skip_path: Arc::new(skip_path) }
    }

    /// Okresowe sprawdzanie ma sens tylko, gdy nic nie trwa i nie ma już
    /// przygotowanej aktualizacji.
    pub fn can_auto_check(&self) -> bool {
        let g = lock(&self.inner);
        g.busy.is_none() && !g.restart_ready
    }

    pub fn take_toast(&self) -> Option<(bool, String)> {
        lock(&self.inner).toast.take()
    }

    /// Numer nowej wersji do zgłoszenia powiadomieniem systemowym (raz na wersję).
    pub fn take_notice(&self) -> Option<String> {
        lock(&self.inner).notice.take()
    }

    fn remember_skip(&self, version: &str) {
        lock(&self.inner).skipped = version.to_string();
        let _ = std::fs::write(&*self.skip_path, version);
    }

    fn forget_skip(&self) {
        lock(&self.inner).skipped.clear();
        let _ = std::fs::remove_file(&*self.skip_path);
    }

    /// Uruchamia w tle sprawdzanie. `stage = true` (auto-install) dodatkowo
    /// pobiera i przygotowuje aktualizację — `updater` sam respektuje
    /// `internet.auto_install`. `forced` (przycisk „Sprawdź teraz”) pokazuje też
    /// wersję odłożoną wcześniej przyciskiem „Później”.
    pub fn check(&self, ctx: &egui::Context, cfg: &AppConfig, net: &Arc<NetControl>, stage: bool, forced: bool) {
        if forced {
            self.forget_skip();
        }
        let cfg = cfg.clone();
        let net = Arc::clone(net);
        self.run(ctx, "Sprawdzanie aktualizacji…", move |inner| {
            let status = catch_unwind(AssertUnwindSafe(|| {
                if stage {
                    updater::check_and_stage(&cfg)
                } else {
                    updater::check(&cfg)
                }
            }))
            .unwrap_or_else(|_| UpdateStatus::Error("wewnętrzny błąd sprawdzania".into()));
            let note = status.note();
            net.set_update_note(note.clone());
            let mut g = lock(inner);
            match &status {
                UpdateStatus::Staged(_) => {
                    g.restart_ready = true;
                    g.toast = Some((false, note));
                }
                UpdateStatus::Available(info) if g.skipped != info.latest && g.announced != info.latest => {
                    g.announced = info.latest.clone();
                    g.notice = Some(info.latest.clone());
                }
                _ => {}
            }
            g.status = Some(status);
        });
    }

    fn download_and_stage(&self, ctx: &egui::Context, info: UpdateInfo) {
        self.run(ctx, "Pobieranie i weryfikacja…", move |inner| {
            let res = catch_unwind(AssertUnwindSafe(|| {
                let path = updater::download_and_verify(&info).map_err(|e| format!("{e:#}"))?;
                let staged = updater::install_on_restart(&path).map_err(|e| format!("{e:#}"));
                let _ = std::fs::remove_file(&path);
                staged
            }))
            .unwrap_or_else(|_| Err("wewnętrzny błąd pobierania".into()));
            let mut g = lock(inner);
            match res {
                Ok(()) => {
                    let text = format!(
                        "Wersja {} pobrana i zweryfikowana — zainstaluje się po restarcie.",
                        info.latest
                    );
                    g.status = Some(UpdateStatus::Staged(info));
                    g.restart_ready = true;
                    g.message = Some(text.clone());
                    g.toast = Some((false, text));
                }
                Err(e) => {
                    let text = format!("Pobieranie nie powiodło się: {e}");
                    g.restart_when_ready = false;
                    g.message = Some(text.clone());
                    g.toast = Some((true, text));
                }
            }
        });
    }

    fn rollback(&self, ctx: &egui::Context) {
        self.run(ctx, "Przywracanie poprzedniej wersji…", move |inner| {
            let res = catch_unwind(AssertUnwindSafe(updater::rollback))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("wewnętrzny błąd")));
            let mut g = lock(inner);
            match res {
                Ok(true) => {
                    let text = "Przywrócono poprzednią wersję — uruchom ponownie aplikację.".to_string();
                    g.restart_ready = true;
                    g.message = Some(text.clone());
                    g.toast = Some((false, text));
                }
                Ok(false) => {
                    g.message = Some("Brak poprzedniej wersji do przywrócenia.".into());
                }
                Err(e) => {
                    let text = format!("Rollback nie powiódł się: {e:#}");
                    g.message = Some(text.clone());
                    g.toast = Some((true, text));
                }
            }
            g.has_old = has_old_build();
        });
    }

    /// Uruchamia nową kopię `INTmouse.exe`. `Ok` = wywołujący ma zamknąć
    /// to okno (nowy proces sam zastosuje przygotowaną aktualizację).
    pub fn restart(&self) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        updater::restart_into(&exe).map_err(|e| format!("{e:#}"))
    }

    fn restart_failed(&self, e: &str) {
        let text = format!("Restart nie powiódł się: {e}");
        let mut g = lock(&self.inner);
        g.message = Some(text.clone());
        g.toast = Some((true, text));
    }

    fn run(
        &self,
        ctx: &egui::Context,
        what: &'static str,
        job: impl FnOnce(&Arc<Mutex<Inner>>) + Send + 'static,
    ) {
        {
            let mut g = lock(&self.inner);
            if g.busy.is_some() {
                return;
            }
            g.busy = Some(what);
        }
        let inner = Arc::clone(&self.inner);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("inputshare-update".into())
            .spawn({
                let inner = Arc::clone(&inner);
                move || {
                    job(&inner);
                    lock(&inner).busy = None;
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            let mut g = lock(&inner);
            g.busy = None;
            g.message = Some(format!("Nie udało się uruchomić zadania: {e}"));
        }
    }

    /// Pasek nad treścią, gdy jest nowsza wersja (jak w LiveDelay): „Aktualizuj” pobiera,
    /// weryfikuje SHA-256 i od razu uruchamia program ponownie, „Później” chowa tę wersję
    /// do następnej. Zwraca `true`, gdy nowa kopia już startuje i to okno ma się zamknąć.
    pub fn banner(&self, ctx: &egui::Context) -> bool {
        let (busy, status, skipped, ready, restart_when_ready) = {
            let g = lock(&self.inner);
            (g.busy, g.status.clone(), g.skipped.clone(), g.restart_ready, g.restart_when_ready)
        };

        // „Aktualizuj” kliknięte wcześniej: paczka jest już zweryfikowana, więc restart.
        if restart_when_ready && ready && busy.is_none() {
            lock(&self.inner).restart_when_ready = false;
            match self.restart() {
                Ok(()) => return true,
                Err(e) => self.restart_failed(&e),
            }
        }

        let (info, staged) = match &status {
            Some(UpdateStatus::Available(i)) if i.latest != skipped => (i.clone(), false),
            Some(UpdateStatus::Staged(i)) => (i.clone(), true),
            _ => return false,
        };

        let mut quit = false;
        egui::TopBottomPanel::top("update_banner")
            .frame(egui::Frame::none().fill(c::CYAN_DIM).inner_margin(egui::Margin::symmetric(18.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if let (Some(what), true) = (busy, restart_when_ready) {
                        ui.spinner();
                        ui.label(RichText::new(what).size(13.5).color(c::TEXT));
                        return;
                    }
                    if staged {
                        ui.label(
                            RichText::new(format!("Wersja {} jest gotowa do instalacji.", info.latest))
                                .size(13.5)
                                .color(c::TEXT),
                        );
                        if ui.button("Uruchom ponownie").clicked() {
                            match self.restart() {
                                Ok(()) => quit = true,
                                Err(e) => self.restart_failed(&e),
                            }
                        }
                        return;
                    }
                    ui.label(
                        RichText::new(format!("Dostępna wersja {}.", info.latest))
                            .size(13.5)
                            .color(c::TEXT),
                    );
                    if info.can_download() {
                        if ui
                            .add_enabled(busy.is_none(), egui::Button::new("Aktualizuj"))
                            .on_hover_text(
                                "Pobiera plik, sprawdza SHA-256 i uruchamia program ponownie. \
                                 Trwające udostępnianie zostanie na chwilę przerwane.",
                            )
                            .clicked()
                        {
                            lock(&self.inner).restart_when_ready = true;
                            self.download_and_stage(ctx, info.clone());
                        }
                    } else if !info.page.is_empty()
                        && ui
                            .button("Otwórz stronę wydania")
                            .on_hover_text("To wydanie nie ma pliku do automatycznej instalacji.")
                            .clicked()
                    {
                        let _ = webbrowser::open(&info.page);
                    }
                    if ui
                        .add_enabled(busy.is_none(), egui::Button::new("Później"))
                        .on_hover_text("Schowaj do następnej wersji")
                        .clicked()
                    {
                        self.remember_skip(&info.latest);
                    }
                    if !info.notes.trim().is_empty() {
                        ui.label(RichText::new(info.notes.trim()).size(12.5).color(c::MUTED));
                    }
                });
            });
        quit
    }

    /// Karta „Wersja i aktualizacja”. Zwraca `true`, gdy trzeba zamknąć okno
    /// (po udanym `restart`); błąd restartu ląduje w komunikacie panelu.
    pub fn ui(&self, ui: &mut egui::Ui, cfg: &AppConfig, net: &Arc<NetControl>) -> bool {
        let (busy, status, message, restart_ready, has_old) = {
            let g = lock(&self.inner);
            (g.busy, g.status.clone(), g.message.clone(), g.restart_ready, g.has_old)
        };
        let mut quit = false;
        section(
            ui,
            "Wersja i aktualizacja",
            "Pobranie jest weryfikowane sumą SHA-256; stara wersja zostaje do rollbacku.",
            |ui| {
                custom_row(
                    ui,
                    "Zainstalowana wersja",
                    &format!("INTmouse {}", updater::current_version()),
                    "",
                    false,
                    |ui| {
                        let (text, col) = match (&busy, &status) {
                            (Some(what), _) => (what.to_string(), c::CYAN),
                            (None, None) => ("Jeszcze nie sprawdzano.".to_string(), c::MUTED),
                            (None, Some(s @ UpdateStatus::Error(_))) => (s.note(), c::OFF),
                            (None, Some(s @ (UpdateStatus::Available(_) | UpdateStatus::Staged(_)))) => {
                                (s.note(), c::OK)
                            }
                            (None, Some(s)) => (s.note(), c::MUTED),
                        };
                        ui.horizontal(|ui| {
                            if busy.is_some() {
                                ui.spinner();
                            }
                            ui.add(egui::Label::new(RichText::new(text).size(13.5).color(col)).wrap());
                        });
                        if let Some(UpdateStatus::Available(info) | UpdateStatus::Staged(info)) = &status {
                            if !info.notes.trim().is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(info.notes.trim()).size(12.5).color(c::MUTED),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    },
                );
                custom_row(ui, "Akcje", "", "", false, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(busy.is_none(), egui::Button::new("Sprawdź teraz"))
                            .on_hover_text("Zapytaj stronę o nowszą wersję (bez pobierania)")
                            .clicked()
                        {
                            self.check(ui.ctx(), cfg, net, false, true);
                        }
                        let available = match &status {
                            Some(UpdateStatus::Available(i)) => Some(i.clone()),
                            _ => None,
                        };
                        let installable = available.as_ref().is_some_and(UpdateInfo::can_download);
                        if ui
                            .add_enabled(
                                busy.is_none() && installable,
                                egui::Button::new("Pobierz i zainstaluj przy restarcie"),
                            )
                            .on_hover_text("Pobiera plik, sprawdza SHA-256 i przygotowuje podmianę exe")
                            .clicked()
                        {
                            if let Some(info) = available.clone() {
                                self.download_and_stage(ui.ctx(), info);
                            }
                        }
                        if let Some(page) = available.as_ref().map(|i| i.page.as_str()).filter(|p| !p.is_empty()) {
                            if ui.button("Strona wydania").clicked() {
                                let _ = webbrowser::open(page);
                            }
                        }
                        if ui
                            .add_enabled(
                                busy.is_none() && has_old,
                                egui::Button::new("Przywróć poprzednią wersję"),
                            )
                            .on_hover_text(if has_old {
                                "Cofa do kopii INTmouse.exe.old (potem uruchom ponownie)"
                            } else {
                                "Brak poprzedniej wersji (INTmouse.exe.old)"
                            })
                            .clicked()
                        {
                            self.rollback(ui.ctx());
                        }
                        if ui
                            .add_enabled(
                                busy.is_none() && restart_ready,
                                egui::Button::new("Uruchom ponownie"),
                            )
                            .on_hover_text("Startuje nową kopię aplikacji i zamyka tę")
                            .clicked()
                        {
                            match self.restart() {
                                Ok(()) => quit = true,
                                Err(e) => self.restart_failed(&e),
                            }
                        }
                    });
                    if let Some(m) = &message {
                        ui.add(egui::Label::new(RichText::new(m).size(12.5).color(c::MUTED)).wrap());
                    }
                });
            },
        );
        quit
    }
}
