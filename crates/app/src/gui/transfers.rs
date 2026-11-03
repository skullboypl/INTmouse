//! Panel „Pliki”: lista transferów (`NetStatus.transfers`), wybór plików do
//! wysłania (dialog systemowy w wątku w tle) i „Pokaż w folderze”.

use super::theme::{self, c};
use super::widgets::{custom_row, section};
use crate::net::{FileTransferStatus, TransferDirection, TransferState};
use eframe::egui::{self, Color32, RichText, Stroke};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Transfers seen in progress, so their end shows once as an ordinary toast.
static WATCHED: std::sync::Mutex<Vec<(TransferDirection, String, u64)>> = std::sync::Mutex::new(Vec::new());

/// How many transfers get their own card; the rest are counted below them.
const MAX_CARDS: usize = 3;

/// Small cards at the top right for files on their way (also after Ctrl+C / Ctrl+V of files):
/// what goes where, a progress bar, the percentage and "X z Y". Returns the transfers that just
/// ended, `(ok, text)`, for an ordinary toast.
pub fn transfer_toasts(ctx: &egui::Context, transfers: &[FileTransferStatus]) -> Vec<(bool, String)> {
    let key = |t: &FileTransferStatus| (t.direction, t.peer.clone(), t.transfer_id);
    let active: Vec<&FileTransferStatus> = transfers
        .iter()
        .filter(|t| matches!(t.state, TransferState::InProgress | TransferState::AwaitingConfirm))
        .collect();
    let mut ended = Vec::new();
    {
        let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
        watched.retain(|k| {
            let Some(t) = transfers.iter().find(|t| key(t) == *k) else {
                return false;
            };
            match &t.state {
                TransferState::Done => {
                    let what = match t.direction {
                        TransferDirection::Send => format!("Wysłano {} do {}", t.name, t.peer),
                        TransferDirection::Receive => format!("Odebrano {} z {}", t.name, t.peer),
                    };
                    ended.push((true, what));
                    false
                }
                TransferState::Failed(why) => {
                    ended.push((false, format!("{}: {why}", t.name)));
                    false
                }
                _ => true,
            }
        });
        for t in &active {
            if !watched.contains(&key(t)) {
                watched.push(key(t));
            }
        }
    }
    if active.is_empty() {
        return ended;
    }
    egui::Area::new(egui::Id::new("transfer_toasts"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 72.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_width(320.0);
            for t in active.iter().take(MAX_CARDS) {
                egui::Frame::none()
                    .fill(Color32::from_rgb(20, 40, 54))
                    .stroke(Stroke::new(1.0_f32, c::CYAN.gamma_multiply(0.7)))
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 9.0))
                    .show(ui, |ui| {
                        let head = match t.direction {
                            TransferDirection::Send => format!("↑ Wysyłam do {}", t.peer),
                            TransferDirection::Receive => format!("↓ Odbieram z {}", t.peer),
                        };
                        ui.label(RichText::new(head).size(12.0).color(c::MUTED));
                        ui.add(egui::Label::new(RichText::new(&t.name).size(13.5).color(c::TEXT)).truncate());
                        let frac = progress_fraction(t);
                        let text = format!(
                            "{:.0}% · {} z {}",
                            frac * 100.0,
                            format_bytes(t.transferred.min(t.size)),
                            format_bytes(t.size)
                        );
                        ui.add(egui::ProgressBar::new(frac).desired_width(296.0).text(text));
                    });
                ui.add_space(6.0);
            }
            if active.len() > MAX_CARDS {
                ui.label(
                    RichText::new(format!("+{} w kolejce", active.len() - MAX_CARDS))
                        .size(12.0)
                        .color(c::MUTED),
                );
            }
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    ended
}

/// Co użytkownik kliknął na liście transferów.
pub enum TransferAction {
    Cancel(u64),
    Reveal(PathBuf),
}

/// Dialog wyboru plików; blokujące `rfd` działa w osobnym wątku, żeby nie
/// zamrażać okna.
#[derive(Default)]
pub struct FilePicker {
    rx: Option<Receiver<Vec<PathBuf>>>,
}

impl FilePicker {
    pub fn is_open(&self) -> bool {
        self.rx.is_some()
    }

    pub fn open(&mut self, ctx: &egui::Context) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("inputshare-file-dialog".into())
            .spawn(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Wybierz pliki do wysłania")
                    .pick_files()
                    .unwrap_or_default();
                let _ = tx.send(picked);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.rx = Some(rx);
        }
    }

    /// `Some(pliki)` raz, gdy dialog został zamknięty (lista może być pusta).
    pub fn poll(&mut self) -> Option<Vec<PathBuf>> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(files) => {
                self.rx = None;
                Some(files)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Otwiera Eksplorator z zaznaczonym plikiem (gdy plik zniknął — jego folder).
pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("explorer.exe");
        if path.exists() {
            cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        } else if let Some(dir) = path.parent().filter(|d| d.exists()) {
            cmd.raw_arg(format!("\"{}\"", dir.display()));
        } else {
            return Err(format!("{} już nie istnieje", path.display()));
        }
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Pokaż w folderze: tylko Windows".into())
    }
}

fn state_label(state: &TransferState) -> (String, Color32) {
    match state {
        TransferState::InProgress => ("W toku".into(), c::CYAN),
        TransferState::AwaitingConfirm => ("Czeka na potwierdzenie".into(), c::AMBER),
        TransferState::Done => ("Gotowe".into(), c::OK),
        TransferState::Failed(msg) => (format!("Błąd: {msg}"), c::OFF),
    }
}

fn progress_fraction(t: &FileTransferStatus) -> f32 {
    if t.state == TransferState::Done {
        return 1.0;
    }
    if t.size == 0 {
        return 0.0;
    }
    (t.transferred as f64 / t.size as f64).clamp(0.0, 1.0) as f32
}

fn transfer_row(ui: &mut egui::Ui, t: &FileTransferStatus, out: &mut Vec<TransferAction>) {
    let (state_text, state_col) = state_label(&t.state);
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(RichText::new(&t.name).font(theme::strong(14.5)).color(c::TEXT))
                .truncate(),
        );
    });
    let dir = match t.direction {
        TransferDirection::Send => "↑ do",
        TransferDirection::Receive => "↓ od",
    };
    ui.add(
        egui::Label::new(
            RichText::new(format!(
                "{dir} {} · {} / {}",
                t.peer,
                format_bytes(t.transferred),
                format_bytes(t.size)
            ))
            .size(12.5)
            .color(c::MUTED),
        )
        .wrap(),
    );
    if !t.state.is_finished() {
        ui.add(
            egui::ProgressBar::new(progress_fraction(t))
                .desired_width(ui.available_width().min(360.0))
                .show_percentage(),
        );
    }
    ui.add(egui::Label::new(RichText::new(state_text).size(13.0).color(state_col)).wrap());
    ui.horizontal(|ui| {
        if t.direction == TransferDirection::Send
            && t.state == TransferState::InProgress
            && ui
                .small_button("Anuluj")
                .on_hover_text("Przerwij wysyłanie (odbiorca usunie część pliku)")
                .clicked()
        {
            out.push(TransferAction::Cancel(t.transfer_id));
        }
        if t.direction == TransferDirection::Receive && t.state == TransferState::Done {
            if let Some(p) = &t.saved_to {
                if ui
                    .small_button("Pokaż w folderze")
                    .on_hover_text(p.display().to_string())
                    .clicked()
                {
                    out.push(TransferAction::Reveal(p.clone()));
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(p.display().to_string())
                            .size(11.5)
                            .color(c::FAINT),
                    )
                    .truncate(),
                );
            }
        }
    });
}

/// Karta z listą transferów (najnowsze na górze). Zwraca kliknięte akcje.
pub fn transfers_section(ui: &mut egui::Ui, transfers: &[FileTransferStatus]) -> Vec<TransferAction> {
    let mut actions = Vec::new();
    section(
        ui,
        "Transfery",
        "Aktywne i ostatnie przesyłki plików, w obu kierunkach.",
        |ui| {
            if transfers.is_empty() {
                custom_row(ui, "Brak transferów", "Upuść plik na okno INTmouse albo użyj „Wyślij plik…”.", "", false, |_| {});
                return;
            }
            for t in transfers.iter().rev() {
                // Rząd bez nagłówka: ciągła lista z cienką kreską między pozycjami.
                ui.add_space(6.0);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .hline(rect.x_range(), rect.center().y, Stroke::new(1.0_f32, c::DIVIDER));
                ui.add_space(6.0);
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    transfer_row(ui, t, &mut actions);
                });
            }
        },
    );
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn progress_handles_zero_size_and_overshoot() {
        let mut t = FileTransferStatus {
            transfer_id: 1,
            direction: TransferDirection::Send,
            peer: "pc".into(),
            name: "a".into(),
            size: 0,
            transferred: 0,
            state: TransferState::InProgress,
            saved_to: None,
        };
        assert_eq!(progress_fraction(&t), 0.0);
        t.size = 100;
        t.transferred = 250;
        assert_eq!(progress_fraction(&t), 1.0);
        t.transferred = 50;
        assert_eq!(progress_fraction(&t), 0.5);
        t.state = TransferState::Done;
        assert_eq!(progress_fraction(&t), 1.0);
    }
}
