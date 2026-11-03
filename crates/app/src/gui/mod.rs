//! Nowoczesny Preferences + tray — własny copy (PL), tooltips, ikony.

mod assets;
mod connected;
mod icon;
mod logs;
mod network_panel;
mod theme;
mod startup_section;
mod toast;
mod transfers;
mod tray_wake;
mod updates;
mod firewall_banner;
mod widgets;

use crate::diag::DiagLog;
use crate::dim_overlay::{self, DimControl};
use crate::displays;
use crate::hotkeys::{self, HotkeyControl};
use crate::local_wrap::{self, WrapControl};
use crate::net::{self, NetConfig, NetControl};
use crate::startup;
use crate::notify;
use crate::panic_key::{self, PanicControl};
use crate::sas::{self, SasControl};
use assets::Icons;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, Image, RichText, Sense, Vec2};
use inputshare_core::{AppConfig, ClientEntry, MonitorTile, PanicMode};
use inputshare_protocol::MonitorInfo;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use theme::c;
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;
use widgets::{
    custom_row, nav_button, page_header, port_row, section, slider_row, text_row, toggle_row,
};

const CONTENT_MAX_W: f32 = 680.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Switching,
    Exchange,
    Output,
    Network,
    Clients,
    Connected,
    Internet,
    Misc,
    Logs,
    MonitorManager,
}

/// How often the update check repeats, and how soon after start the first one runs.
const UPDATE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
const UPDATE_FIRST_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(4);

pub fn run(config_path: PathBuf, diag: DiagLog, start_in_tray: bool) -> anyhow::Result<()> {
    let mut cfg = AppConfig::load_or_default(&config_path)?;
    // Always refresh local monitors from the OS so layout matches reality (e.g. 4 screens).
    match displays::system_layout() {
        Ok(sys) => {
            cfg.layout.merge_system_displays(sys);
            tracing::info!(
                "Wczytano {} monitor(ów) z systemu",
                cfg.layout.monitors.iter().filter(|m| m.system_synced).count()
            );
        }
        Err(e) => {
            tracing::warn!("Nie udało się odczytać monitorów: {e}");
            if cfg.layout.monitors.is_empty() {
                cfg.layout = inputshare_core::MonitorLayout::demo_local();
            }
        }
    }

    let wrap = WrapControl::new(
        cfg.switching.wrap_pointer,
        cfg.sharing_enabled,
        cfg.layout.clone(),
        cfg.switching.clone(),
    );
    let panic = PanicControl::new(
        cfg.misc.panic_enabled,
        cfg.misc.panic_mode,
        &cfg.misc.panic_key,
    );
    let hotkeys = HotkeyControl::new(
        &cfg.switching.enable_disable_hotkey,
        &cfg.switching.quick_jump_hotkey,
        cfg.layout.clone(),
        cfg.sharing_enabled,
    );
    let dim = DimControl::new(
        cfg.misc.dim_inactive,
        cfg.misc.dim_level,
        cfg.layout.clone(),
    );
    let sas = SasControl::new(cfg.misc.sas_hotkey.clone());

    let net = net::spawn(NetConfig::from_app(&cfg));
    wrap.bind_capture(net.capture());
    wrap.set_net_outbox(net.outbox());

    let _wrap_thread = local_wrap::spawn(wrap.clone());
    let _panic_thread = panic_key::spawn(panic.clone());
    let _hotkeys_thread = hotkeys::spawn(hotkeys.clone());
    let _dim_thread = dim_overlay::spawn(dim.clone());
    let _sas_thread = sas::spawn(sas.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(tray_wake::WINDOW_TITLE)
            .with_icon(icon::window_icon())
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([560.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "INTmouse",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(
                cc,
                config_path,
                cfg,
                wrap,
                panic,
                hotkeys,
                dim,
                sas,
                diag,
                net,
                start_in_tray,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

struct GuiApp {
    config_path: PathBuf,
    cfg: AppConfig,
    tab: Tab,
    dirty: bool,
    status: String,
    quit: bool,
    draft_ip: String,
    draft_port: String,
    drag_id: Option<String>,
    drag_offset: Vec2,
    /// The icon itself lives in `tray_wake`, which owns it for the menu handler.
    tray: bool,
    tray_ids: Option<TrayMenuIds>,
    icons: Icons,
    wrap: WrapControl,
    panic: PanicControl,
    hotkeys: HotkeyControl,
    dim: DimControl,
    sas: SasControl,
    diag: DiagLog,
    net: Arc<NetControl>,
    /// Last known sharing for edge-triggered tray toasts.
    last_sharing: bool,
    /// Last `NetStatus::monitors_rev` merged into the layout.
    seen_monitors_rev: u64,
    /// Started with `--tray`: go to the tray as soon as the icon exists.
    start_in_tray: bool,
    /// Whether this copy runs with administrator rights.
    elevated: bool,
    /// Last problem with the autostart / elevation settings, shown in the Bezpieczeństwo tab.
    startup_note: Option<String>,
    /// Where the autostart lives (looked up when the settings change, not on every frame).
    autostart_label: String,
    /// Names of the computers linked right now (top bar indicator).
    net_connected: Vec<String>,
    /// First unresolved link problem, shown as a tooltip when nothing is connected.
    net_issue: Option<String>,
    log_filter: logs::LogFilter,
    log_search: String,
    last_update_check: std::time::Instant,
    toasts: toast::Toasts,
    file_picker: transfers::FilePicker,
    adapters: network_panel::AdapterList,
    updates: updates::UpdatesUi,
    firewall: firewall_banner::FirewallPrompt,
}

struct TrayMenuIds {
    show: tray_icon::menu::MenuId,
    monitor: tray_icon::menu::MenuId,
    toggle: tray_icon::menu::MenuId,
    toggle_item: MenuItem,
}

impl GuiApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config_path: PathBuf,
        cfg: AppConfig,
        wrap: WrapControl,
        panic: PanicControl,
        hotkeys: HotkeyControl,
        dim: DimControl,
        sas: SasControl,
        diag: DiagLog,
        net: Arc<NetControl>,
        start_in_tray: bool,
    ) -> Self {
        let firewall_prompt =
            firewall_banner::FirewallPrompt::new(cfg.network.tcp_port, cfg.network.udp_port, cfg.network.firewall_asked);
        let local_n = cfg.layout.monitors.iter().filter(|m| m.system_synced).count();
        let last_sharing = cfg.sharing_enabled;
        let skip_path = config_path.with_file_name("update-skip.txt");
        let elevated = startup::is_elevated();
        // Keep the autostart entry pointing at this exe and in line with the settings.
        let startup_note = std::env::current_exe().ok().and_then(|exe| {
            startup::apply(cfg.startup.launch_at_login, cfg.startup.run_as_admin, &exe).err()
        });
        Self {
            config_path,
            cfg,
            tab: Tab::Switching,
            dirty: false,
            status: if local_n > 0 {
                format!("Wykryto {local_n} monitor(ów) z systemu. Sieć LAN gotowa.")
            } else {
                "Gotowe — zmień opcje i kliknij Zapisz.".into()
            },
            quit: false,
            draft_ip: String::new(),
            draft_port: "6555".into(),
            drag_id: None,
            drag_offset: Vec2::ZERO,
            tray: false,
            tray_ids: None,
            icons: Icons::load(&cc.egui_ctx),
            wrap,
            panic,
            hotkeys,
            dim,
            sas,
            diag,
            net,
            last_sharing,
            seen_monitors_rev: 0,
            start_in_tray,
            elevated,
            startup_note,
            autostart_label: startup::describe().to_string(),
            net_connected: Vec::new(),
            net_issue: None,
            log_filter: logs::LogFilter::default(),
            log_search: String::new(),
            // First look a few seconds after start, then every UPDATE_CHECK_EVERY.
            last_update_check: std::time::Instant::now()
                .checked_sub(UPDATE_CHECK_EVERY - UPDATE_FIRST_CHECK_DELAY)
                .unwrap_or_else(std::time::Instant::now),
            toasts: toast::Toasts::default(),
            file_picker: transfers::FilePicker::default(),
            adapters: network_panel::AdapterList::default(),
            updates: updates::UpdatesUi::new(skip_path),
            firewall: firewall_prompt,
        }
    }

    fn push_toast(&mut self, is_error: bool, text: String) {
        if is_error {
            self.toasts.error(text);
        } else {
            self.toasts.info(text);
        }
    }

    /// Przenosi toasty z wątków w tle (updater) do widoku; nową wersję zgłasza też powiadomieniem
    /// systemowym, bo okno może siedzieć zminimalizowane w tray.
    fn drain_background_toasts(&mut self) {
        if let Some((err, text)) = self.updates.take_toast() {
            self.push_toast(err, text);
        }
        if let Some(version) = self.updates.take_notice() {
            notify::show(
                self.cfg.misc.tray_notifications,
                "INTmouse",
                &format!("Dostępna wersja {version}. Otwórz okno i kliknij Aktualizuj."),
            );
        }
    }

    /// Wysyła pliki do połączonego peera. Błędy → toast, nigdy panika.
    fn send_files(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let (dirs, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_dir());
        if !dirs.is_empty() {
            let names = dirs
                .iter()
                .map(|d| {
                    d.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| d.display().to_string())
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.toasts.error(format!(
                "Katalogi nie są obsługiwane (pominięto: {names}). Wyślij pojedyncze pliki."
            ));
        }
        if files.is_empty() {
            return;
        }
        match self.net.send_files(files) {
            Ok(ids) => self.toasts.info(format!("Wysyłanie plików: {}…", ids.len())),
            Err(e) => self.toasts.error(format!("Nie udało się wysłać: {e:#}")),
        }
    }

    /// Pliki upuszczone na okno + wynik dialogu „Wyślij plik…”.
    fn handle_file_inputs(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.send_files(dropped);
        }
        if let Some(picked) = self.file_picker.poll() {
            self.send_files(picked);
        }
    }

    /// Półprzezroczysta nakładka podczas przeciągania plików nad oknem.
    fn paint_drop_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop_overlay"),
        ));
        let rect = ctx.screen_rect();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(190));
        painter.text(
            rect.center() - Vec2::new(0.0, 12.0),
            egui::Align2::CENTER_CENTER,
            "Upuść pliki, aby wysłać na drugi komputer",
            theme::strong(22.0),
            c::TEXT,
        );
        if !self.cfg.exchange.allow_drag_drop {
            painter.text(
                rect.center() + Vec2::new(0.0, 20.0),
                egui::Align2::CENTER_CENTER,
                "Najpierw włącz „Przeciąganie plików między PC” w zakładce Wymiana",
                egui::FontId::proportional(15.0),
                c::AMBER,
            );
        }
    }

    /// Pull a nearby computer onto the facing edge. Skipped while the user is dragging,
    /// so the group follows the pointer and sticks when they let go.
    fn glue_nearby_computers(&mut self) {
        if self.drag_id.is_some() {
            return;
        }
        let local = inputshare_core::local_hostname();
        let mut hosts: Vec<String> = self
            .cfg
            .layout
            .monitors
            .iter()
            .filter(|m| !m.system_synced && !m.host.eq_ignore_ascii_case(&local))
            .map(|m| m.host.clone())
            .collect();
        hosts.sort();
        hosts.dedup();
        let mut glued = false;
        for h in hosts {
            if self.cfg.layout.snap_host(&h) {
                self.status = format!("Doklejono {h} do krawędzi. Zapisz układ.");
                glued = true;
            }
        }
        if glued {
            self.dirty = true;
        }
    }

    fn sync_runtime_flags(&self) {
        self.wrap.set_wrap(self.cfg.switching.wrap_pointer);
        self.wrap.set_sharing(self.cfg.sharing_enabled);
        self.wrap.set_switching(self.cfg.switching.clone());
        self.wrap.set_layout(self.cfg.layout.clone());
        self.wrap.set_remote_capturing(
            self.net
                .capture()
                .capturing
                .load(Ordering::Relaxed),
        );
        self.panic.set_enabled(self.cfg.misc.panic_enabled);
        self.panic.set_mode(self.cfg.misc.panic_mode);
        self.panic.set_key(&self.cfg.misc.panic_key);
        self.hotkeys
            .set_enable_hotkey(&self.cfg.switching.enable_disable_hotkey);
        self.hotkeys
            .set_jump_hotkey(&self.cfg.switching.quick_jump_hotkey);
        self.hotkeys.set_layout(self.cfg.layout.clone());
        self.hotkeys.set_sharing(self.cfg.sharing_enabled);
        self.dim.set_enabled(self.cfg.misc.dim_inactive);
        self.dim.set_level(self.cfg.misc.dim_level);
        self.dim.set_layout(self.cfg.layout.clone());
        self.sas.set_hotkey(self.cfg.misc.sas_hotkey.clone());
        self.diag.set_enabled(self.cfg.misc.diagnostics);
        self.net.update_config(NetConfig::from_app(&self.cfg));
        // The other computers learn our displays from this (only sent when the list changes).
        let local = inputshare_core::local_hostname();
        self.net.set_local_monitors(
            self.cfg
                .layout
                .monitors
                .iter()
                .filter(|m| m.host.eq_ignore_ascii_case(&local))
                .map(|m| MonitorInfo {
                    label: m.label.clone(),
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    primary: m.primary,
                })
                .collect(),
        );
    }

    fn apply_net_status(&mut self, ctx: &egui::Context) {
        if self.net.take_remote_panic() {
            self.cfg.sharing_enabled = false;
            self.wrap.set_sharing(false);
            self.dirty = true;
            self.status = "Zdalny PANIC — udostępnianie wyłączone.".into();
            notify::show(
                self.cfg.misc.tray_notifications,
                "INTmouse — PANIC (zdalny)",
                "Peer wyłączył udostępnianie.",
            );
            return;
        }
        let st = self.net.status_snapshot();
        self.net_connected = st.peers.iter().filter(|p| p.tcp_ok).map(|p| p.hostname.clone()).collect();
        self.net_issue = st.link_issues.first().map(|i| format!("{}: {}", i.peer, i.text));
        // Hand-off only to displays of computers that are linked right now.
        let local = inputshare_core::local_hostname();
        let mut hosts: Vec<String> = self
            .cfg
            .layout
            .monitors
            .iter()
            .filter(|m| !m.host.eq_ignore_ascii_case(&local))
            .map(|m| m.host.clone())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts.retain(|h| st.host_is_up(h));
        self.wrap.set_remote_hosts_up(hosts);
        if st.monitors_rev != self.seen_monitors_rev {
            self.seen_monitors_rev = st.monitors_rev;
            self.apply_remote_monitors(&st.remote_monitors);
        }
        if let Some(err) = &st.last_error {
            if self.status.starts_with("Nasłuch")
                || self.status.starts_with("Połączono")
                || self.status.starts_with("Sieć")
                || self.status.is_empty()
            {
                self.status = format!("Sieć: {err}");
            }
        } else if !st.status_line.is_empty()
            && (self.status.starts_with("Nasłuch")
                || self.status.starts_with("Połączono")
                || self.status.starts_with("Sieć")
                || self.status.starts_with("Wykryto")
                || self.status.starts_with("Gotowe")
                || self.status.starts_with("Sprawdzanie")
                || self.status.contains("LAN gotowa"))
        {
            self.status = st.status_line;
            if let Some(note) = st.update_note {
                self.status = format!("{} · {}", self.status, note);
            }
        }

        // Update check in a background thread (a few seconds after start, then every 12 h).
        // With `internet.check_updates` off `updater` makes no request at all.
        // `check_and_stage` also downloads + stages the update, but only when
        // `internet.auto_install` is on.
        if self.cfg.internet.check_updates
            && self.last_update_check.elapsed() > UPDATE_CHECK_EVERY
            && self.updates.can_auto_check()
        {
            self.last_update_check = std::time::Instant::now();
            self.updates.check(ctx, &self.cfg, &self.net, true, false);
        }
    }

    /// Put the displays of connected computers into the layout (see
    /// `MonitorLayout::merge_remote_monitors`). The result is unsaved until the user saves.
    fn apply_remote_monitors(&mut self, remotes: &[net::RemoteMonitors]) {
        let local = inputshare_core::local_hostname();
        let mut changed = Vec::new();
        for r in remotes {
            let ip = (!r.ip.is_empty()).then_some(r.ip.as_str());
            if self.cfg.layout.merge_remote_monitors(&local, &r.hostname, ip, &r.monitors) {
                changed.push(r.hostname.clone());
            }
        }
        if changed.is_empty() {
            return;
        }
        let text = format!(
            "Układ ekranów: monitory komputera {} dodane automatycznie. Ułóż je w „Układzie ekranów” i zapisz.",
            changed.join(", ")
        );
        self.dirty = true;
        self.status = text.clone();
        self.push_toast(false, text);
    }

    fn apply_panic_if_fired(&mut self) {
        if self.panic.take_triggered() {
            self.net.broadcast_panic();
            self.cfg.sharing_enabled = false;
            self.wrap.set_sharing(false);
            self.dirty = true;
            self.status =
                "PANIC — udostępnianie i zapętlenie wyłączone. Włącz znowu przyciskiem u góry."
                    .into();
            notify::show(
                self.cfg.misc.tray_notifications,
                "INTmouse — PANIC",
                "Udostępnianie wyłączone.",
            );
        }
    }

    fn apply_hotkeys_if_fired(&mut self) {
        if self.hotkeys.take_toggle_sharing() {
            self.cfg.sharing_enabled = !self.cfg.sharing_enabled;
            self.dirty = true;
            self.status = if self.cfg.sharing_enabled {
                "Hotkey: udostępnianie włączone.".into()
            } else {
                "Hotkey: udostępnianie wyłączone.".into()
            };
            self.sync_tray_toggle_label();
        }
        if self.hotkeys.take_quick_jump() {
            self.status = "Quick jump: kursor na kolejny lokalny monitor.".into();
        }
        if let Some(msg) = self.sas.take_fired() {
            self.status = msg;
        }
    }

    fn notify_sharing_edge(&mut self) {
        if self.cfg.sharing_enabled == self.last_sharing {
            return;
        }
        self.last_sharing = self.cfg.sharing_enabled;
        let (title, body) = if self.cfg.sharing_enabled {
            ("INTmouse", "Udostępnianie włączone")
        } else {
            ("INTmouse", "Udostępnianie wyłączone")
        };
        notify::show(self.cfg.misc.tray_notifications, title, body);
        self.sync_tray_toggle_label();
    }

    fn sync_tray_toggle_label(&self) {
        tray_wake::set_sharing(self.cfg.sharing_enabled);
        if let Some(ids) = &self.tray_ids {
            ids.toggle_item.set_text(if self.cfg.sharing_enabled {
                "Wyłącz udostępnianie"
            } else {
                "Włącz udostępnianie"
            });
        }
    }

    fn refresh_system_monitors(&mut self) {
        match displays::system_layout() {
            Ok(sys) => {
                let n = sys.monitors.len();
                self.cfg.layout.merge_system_displays(sys);
                self.wrap.set_layout(self.cfg.layout.clone());
                self.hotkeys.set_layout(self.cfg.layout.clone());
                self.dim.set_layout(self.cfg.layout.clone());
                self.dirty = true;
                self.status = format!(
                    "Odświeżono układ: {n} monitor(ów). Zapętlenie: ta sama oś X/Y (A↑→B, D↔C)."
                );
            }
            Err(e) => {
                self.status = format!("Nie udało się odczytać monitorów: {e}");
            }
        }
    }

    fn ensure_tray(&mut self, ctx: &egui::Context) {
        if self.tray {
            return;
        }
        let show = MenuItem::new("Otwórz ustawienia", true, None);
        let monitor = MenuItem::new("Układ ekranów", true, None);
        let toggle = MenuItem::new(
            if self.cfg.sharing_enabled {
                "Wyłącz udostępnianie"
            } else {
                "Włącz udostępnianie"
            },
            true,
            None,
        );
        let quit = MenuItem::new("Zamknij INTmouse", true, None);
        let ids = TrayMenuIds {
            show: show.id().clone(),
            monitor: monitor.id().clone(),
            toggle: toggle.id().clone(),
            toggle_item: toggle.clone(),
        };
        let quit_id = quit.id().clone();
        let menu = Menu::new();
        let _ = menu.append(&show);
        let _ = menu.append(&monitor);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&toggle);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit);
        match TrayIconBuilder::new()
            .with_tooltip(tray_wake::tooltip(self.cfg.sharing_enabled))
            .with_menu(Box::new(menu))
            .with_icon(icon::tray_icon(self.cfg.sharing_enabled))
            .build()
        {
            Ok(t) => {
                tray_wake::install(ctx.clone(), t, quit_id, self.cfg.sharing_enabled);
                self.tray = true;
                self.tray_ids = Some(ids);
            }
            Err(e) => self.status = format!("Tray niedostępny: {e}"),
        }
    }

    /// The window lives in the tray as a minimized window without a taskbar button; a
    /// hidden window would get no frames and the hotkeys would stop working.
    fn minimize_to_tray(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        tray_wake::set_taskbar_button(false);
    }

    fn restore_from_tray(ctx: &egui::Context) {
        tray_wake::set_taskbar_button(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let Some(ids) = &self.tray_ids else {
            return;
        };
        let (show_id, monitor_id, toggle_id) =
            (ids.show.clone(), ids.monitor.clone(), ids.toggle.clone());
        for id in tray_wake::drain() {
            if id == show_id {
                self.tab = Tab::Switching;
                Self::restore_from_tray(ctx);
            } else if id == monitor_id {
                self.tab = Tab::MonitorManager;
                Self::restore_from_tray(ctx);
            } else if id == toggle_id {
                self.cfg.sharing_enabled = !self.cfg.sharing_enabled;
                self.dirty = true;
                self.sync_tray_toggle_label();
                self.status = if self.cfg.sharing_enabled {
                    "Udostępnianie włączone".into()
                } else {
                    "Udostępnianie wyłączone".into()
                };
            }
        }
    }

    /// The firewall question was answered: remember it, so it is asked only once.
    fn on_firewall(&mut self, outcome: firewall_banner::FirewallOutcome) {
        match outcome {
            firewall_banner::FirewallOutcome::Added => {
                self.toasts.info("Zapora: INTmouse może łączyć się w sieci lokalnej.")
            }
            firewall_banner::FirewallOutcome::Failed(e) => self.toasts.error(format!("Zapora: {e}")),
            firewall_banner::FirewallOutcome::Dismissed => {}
        }
        if !self.cfg.network.firewall_asked {
            self.cfg.network.firewall_asked = true;
            if self.dirty {
                // Other unsaved edits: the flag goes with the next save.
                return;
            }
            self.save();
        }
    }

    fn save(&mut self) {
        match self.cfg.save(&self.config_path) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("Zapisano → {}", self.config_path.display());
                notify::show(
                    self.cfg.misc.tray_notifications,
                    "INTmouse",
                    "Ustawienia zapisane.",
                );
            }
            Err(e) => self.status = format!("Błąd zapisu: {e}"),
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.glue_nearby_computers();
        self.sync_runtime_flags();
        self.apply_panic_if_fired();
        self.apply_hotkeys_if_fired();
        self.apply_net_status(ctx);
        self.notify_sharing_edge();
        self.ensure_tray(ctx);
        if self.start_in_tray && self.tray {
            self.start_in_tray = false;
            Self::minimize_to_tray(ctx);
        }
        self.poll_tray(ctx);
        if self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.handle_file_inputs(ctx);
        self.drain_background_toasts();
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                Self::minimize_to_tray(ctx);
                self.status = "Schowano do tacki systemowej. Zamknięcie: tray → Zamknij.".into();
            }
        }

        // Narrow window: the menu shrinks to icons and the top bar drops its tagline.
        let compact = ctx.screen_rect().width() < 860.0;

        // Top bar: brand | status pill | save
        egui::TopBottomPanel::top("top")
            .exact_height(60.0)
            .frame(egui::Frame::none().fill(c::NAV).inner_margin(egui::Margin::symmetric(18.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.add(Image::new(&self.icons.action_share).fit_to_exact_size(Vec2::splat(30.0)));
                    let mut brand = LayoutJob::default();
                    brand.append(
                        if compact { "INTmouse" } else { "INTmouse\n" },
                        0.0,
                        TextFormat { font_id: theme::strong(17.0), color: c::TEXT, ..Default::default() },
                    );
                    if !compact {
                        brand.append(
                            "mysz i klawiatura przez LAN",
                            0.0,
                            TextFormat {
                                font_id: egui::FontId::proportional(12.5),
                                color: c::MUTED,
                                ..Default::default()
                            },
                        );
                    }
                    ui.label(brand);
                    ui.add_space(if compact { 4.0 } else { 14.0 });

                    let (label, col, fill) = if self.cfg.sharing_enabled {
                        ("●  Aktywne", c::OK, Color32::from_rgb(18, 50, 34))
                    } else {
                        ("○  Wstrzymane", c::OFF, Color32::from_rgb(58, 24, 27))
                    };
                    let pill = egui::Button::new(RichText::new(label).font(theme::strong(13.5)).color(col))
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0_f32, col.gamma_multiply(0.55)))
                        .rounding(16.0)
                        .min_size(Vec2::new(0.0, 32.0));
                    if ui
                        .add(pill)
                        .on_hover_text("Szybkie włączenie / wyłączenie udostępniania między PC")
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.cfg.sharing_enabled = !self.cfg.sharing_enabled;
                        self.dirty = true;
                    }

                    let (link_text, link_color, link_tip) = if self.net_connected.is_empty() {
                        (
                            "○ brak połączenia".to_string(),
                            c::MUTED,
                            self.net_issue
                                .clone()
                                .unwrap_or_else(|| "Żaden komputer nie jest połączony. Szczegóły w zakładce Połączone.".into()),
                        )
                    } else {
                        (
                            format!("● połączono: {}", self.net_connected.join(", ")),
                            c::OK,
                            "Szczegóły w zakładce Połączone.".to_string(),
                        )
                    };
                    let link = ui
                        .add(
                            egui::Label::new(RichText::new(link_text).size(13.0).color(link_color))
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text(link_tip)
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if link.clicked() {
                        self.tab = Tab::Connected;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut save = egui::Button::image_and_text(
                            Image::new(&self.icons.action_save).fit_to_exact_size(Vec2::splat(18.0)),
                            RichText::new("Zapisz").font(theme::strong(14.0)),
                        )
                        .min_size(Vec2::new(108.0, 34.0));
                        if self.dirty {
                            save = save.fill(c::CYAN_DIM).stroke(egui::Stroke::new(1.0_f32, c::CYAN));
                        }
                        if ui
                            .add_enabled(self.dirty, save)
                            .on_hover_text("Zapisz ustawienia do pliku konfiguracyjnego")
                            .clicked()
                        {
                            self.save();
                        }
                        if self.dirty {
                            ui.label(RichText::new("● niezapisane zmiany").size(13.0).color(c::AMBER));
                        }
                    });
                });
            });

        // Banner: „Dostępna wersja… Aktualizuj / Później”. `true` = nowa kopia już startuje.
        let (tcp, udp) = (self.cfg.network.tcp_port, self.cfg.network.udp_port);
        if let Some(outcome) = self.firewall.banner(ctx, tcp, udp) {
            self.on_firewall(outcome);
        }
        if self.updates.banner(ctx) {
            self.quit = true;
        }

        egui::TopBottomPanel::bottom("bottom")
            .exact_height(30.0)
            .frame(egui::Frame::none().fill(c::NAV).inner_margin(egui::Margin::symmetric(18.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&self.status).size(12.5).color(c::MUTED))
                            .truncate(),
                    );
                });
            });

        // Sidebar
        egui::SidePanel::left("nav")
            .exact_width(if compact { 64.0 } else { 216.0 })
            .resizable(false)
            .frame(egui::Frame::none().fill(c::NAV).inner_margin(egui::Margin {
                left: if compact { 8.0 } else { 12.0 },
                right: if compact { 8.0 } else { 12.0 },
                top: 18.0,
                bottom: 12.0,
            }))
            .show(ctx, |ui| {
              // A short window scrolls the menu instead of cutting off its last items.
              egui::ScrollArea::vertical().id_salt("nav_scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                if !compact {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new("USTAWIENIA").font(theme::strong(11.5)).color(c::FAINT));
                    });
                    ui.add_space(4.0);
                }
                for (tab, label, tip) in [
                    (Tab::Switching, "Przejścia", "Gdy kursor ma skakać na inny komputer"),
                    (Tab::Exchange, "Wymiana", "Schowek, pliki, stan klawiatury"),
                    (Tab::Output, "Wskaźnik", "Prędkość myszy, scroll, tryb względny"),
                    (Tab::Network, "Sieć", "Hasło, porty TCP/UDP, adapter"),
                    (Tab::Clients, "Komputery", "Lista dozwolonych maszyn w LAN"),
                    (Tab::Connected, "Połączone", "Kto jest połączony, łączenie automatyczne i dziennik połączeń"),
                    (Tab::Internet, "Aktualizacje", "Wersja programu i aktualizacje"),
                    (Tab::Misc, "Bezpieczeństwo", "Panic key, przyciemnianie, diagnostyka"),
                    (Tab::Logs, "Logi", "Pełne logi programu, filtr i tryb szczegółowy"),
                ] {
                    let sel = self.tab == tab;
                    let r = nav_button(ui, self.icons.for_tab(tab), label, sel, compact);
                    let tip = if compact { format!("{label}: {tip}") } else { tip.to_string() };
                    if r.on_hover_text(tip).clicked() {
                        self.tab = tab;
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                let sel = self.tab == Tab::MonitorManager;
                let r = nav_button(ui, &self.icons.nav_layout, "Układ ekranów", sel, compact);
                let tip = "Przeciągnij monitory, żeby ustawić sąsiedztwo krawędzi";
                let tip = if compact { format!("Układ ekranów: {tip}") } else { tip.to_string() };
                if r.on_hover_text(tip).clicked() {
                    self.tab = Tab::MonitorManager;
                }
              });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(c::BG).inner_margin(egui::Margin {
                left: if compact { 14.0 } else { 28.0 },
                right: if compact { 8.0 } else { 16.0 },
                top: 24.0,
                bottom: 8.0,
            }))
            .show(ctx, |ui| {
                if self.tab == Tab::MonitorManager {
                    self.ui_monitors(ui);
                    return;
                }
                if self.tab == Tab::Logs {
                    self.ui_logs(ui);
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt(self.tab)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Leave room for the floating scrollbar and cap line length on wide windows.
                        let avail = ui.available_width() - 14.0;
                        let width = avail.min(CONTENT_MAX_W);
                        ui.horizontal_top(|ui| {
                            ui.add_space(((avail - width) / 2.0).max(0.0));
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                match self.tab {
                                    Tab::Switching => self.ui_switching(ui),
                                    Tab::Exchange => self.ui_exchange(ui),
                                    Tab::Output => self.ui_output(ui),
                                    Tab::Network => self.ui_network(ui),
                                    Tab::Clients => self.ui_clients(ui),
                                    Tab::Internet => self.ui_internet(ui),
                                    Tab::Misc => self.ui_misc(ui),
                                    Tab::Connected => self.ui_connected(ui),
                                    Tab::MonitorManager | Tab::Logs => {}
                                }
                                ui.add_space(12.0);
                            });
                        });
                    });
            });

        self.paint_drop_overlay(ctx);
        for (ok, text) in transfers::transfer_toasts(ctx, &self.net.transfers_snapshot()) {
            if ok {
                self.toasts.info(text);
            } else {
                self.toasts.error(text);
            }
        }
        self.toasts.show(ctx);

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tray_wake::remove_icon();
    }
}

impl GuiApp {
    fn ui_switching(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_switch,
            "Przejścia między komputerami",
            "Kontroluj, kiedy kursor może „przeskoczyć” na sąsiedni PC.",
        );
        section(
            ui,
            "Zachowanie krawędzi",
            "Podstawowe reguły ruchu kursora po monitorach.",
            |ui| {
                let c = &mut self.cfg.switching;
                let mut ch = false;
                ch |= toggle_row(
                    ui,                    "Wyrównaj różne rozdzielczości",
                    "Przy przejściu z dużego na mały ekran kursor nie „gubi się” poza krawędzią.",
                    "Mapuje pozycję na krawędzi proporcjonalnie do wysokości / szerokości sąsiada.",
                    &mut c.equalize_display_dimensions,
                    false);
                ch |= toggle_row(
                    ui,
                    "Zapętlij monitory",
                    "↑ z A → dół B (to samo X). ← z D → prawe C, → z C → lewe D (to samo Y).",
                    "Szuka monitora na tej samej osi, nie odbija od całego pulpitu. Wymaga: Aktywne.",
                    &mut c.wrap_pointer,
                    false);
                ch |= text_row(
                    ui,                    "Szybki skok (hotkey)",
                    "Cykl: środek kolejnego lokalnego monitora (kolejność układu).",
                    "Działa przy włączonym udostępnianiu. Np. Ctrl+Shift+Q.",
                    &mut c.quick_jump_hotkey,
                    false);
                ch |= text_row(
                    ui,                    "Włącz / wyłącz INTmouse",
                    "Globalny przełącznik — działa też gdy sharing jest wyłączony.",
                    "Ten sam efekt co przycisk „Aktywne / Wstrzymane” u góry. Np. Ctrl+Shift+A.",
                    &mut c.enable_disable_hotkey,
                    false);
                if ch {
                    self.dirty = true;
                }
            },
        );
        section(
            ui,
            "Blokady przypadkowego przejścia",
            "Żeby w grze albo przy precyzyjnej pracy kursor nie uciekł na drugi PC.",
            |ui| {
                let sw = &mut self.cfg.switching;
                let mut ch = false;
                ch |= toggle_row(
                    ui,                    "Blokuj w pełnym ekranie",
                    "Gry / filmy w true fullscreen — kursor zostaje na tym PC.",
                    "Najważniejsza opcja przeciw spadkom FPS i „dziwnej” myszy w grach.",
                    &mut sw.prevent_fullscreen,
                    false);
                ch |= toggle_row(
                    ui,                    "Blokuj przy zmaksymalizowanym oknie",
                    "Łagodniejsza blokada — także dla okien „na cały pulpit”.",
                    "Może przeszkadzać w codziennej pracy — włącz tylko gdy potrzebujesz.",
                    &mut sw.prevent_maximized,
                    false);
                ch |= toggle_row(
                    ui,                    "Martwe narożniki",
                    "Narożniki ekranu (Start, zamknij okno) nie przełączają PC.",
                    "Strefa kilku pikseli w rogu bez przejęcia kursora.",
                    &mut sw.prevent_corners,
                    false);
                {
                    let mut mod_key = sw.require_modifier.clone().unwrap_or_default();
                    if text_row(
                        ui,                        "Wymagaj klawisza przy przejściu",
                        "Np. Ctrl — przejście tylko gdy trzymasz ten klawisz.",
                        "Puste pole = przechodź zawsze (o ile inne blokady nie zatrzymają).",
                        &mut mod_key,
                    false) {
                        sw.require_modifier = if mod_key.trim().is_empty() {
                            None
                        } else {
                            Some(mod_key)
                        };
                        ch = true;
                    }
                }
                {
                    let mut block = sw.blocked_programs.join("\n");
                    let edited = custom_row(
                        ui,
                        "Aplikacje z blokadą (jedna na linię)",
                        "Nazwa procesu, np. game.exe — wtedy nie przełączaj.",
                        "Gdy któryś z tych procesów ma fokus, kursor zostaje na tym PC.", false,
                        |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut block)
                                    .desired_rows(3)
                                    .desired_width(ui.available_width())
                                    .hint_text("game.exe")
                                    .margin(Vec2::new(8.0, 6.0)),
                            )
                            .changed()
                        },
                    );
                    if edited {
                        sw.blocked_programs = block
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect();
                        ch = true;
                    }
                }
                ch |= toggle_row(
                    ui,                    "Podwójne stuknięcie w krawędź omija blokadę",
                    "Gdy coś blokuje przejście — dwa razy w krawędź = jednak przejdź.",
                    "Szybki wyjątek bez wyłączania całej ochrony.",
                    &mut sw.double_tap_border_override,
                    false);
                ch |= toggle_row(
                    ui,
                    "Blokuj wolne przejście myszki na krawędzi",
                    "Powolne dojechanie do krawędzi zatrzymuje kursor na tym ekranie (działa też przy powrocie).",
                    "Tylko szybki zdecydowany ruch myszki pozwala na przejście na drugi komputer",
                    &mut sw.require_fast_hit,
                    false);
                if sw.require_fast_hit {
                    ch |= custom_row(
                        ui,
                        "Wymagana prędkość uderzenia",
                        "Im wyżej, tym mocniej trzeba „rzucić” kursorem w krawędź.",
                        "Piksele na sekundę, mierzone w ostatnich 60 ms przed krawędzią. Domyślnie 500.",
                        false,
                        |ui| {
                            ui.add(
                                egui::Slider::new(&mut sw.fast_hit_speed, 1..=2000)
                                    .step_by(10.0)
                                    .suffix(" px/s"),
                            )
                            .changed()
                        },
                    );
                }
                ch |= toggle_row(
                    ui,
                    "Blokuj, gdy gra trzyma kursor",
                    "Celownik w grze (np. CS2): kursor ukryty albo zamknięty w oknie — nie ucieka na drugi PC.",
                    "Działa też w drugą stronę: gdy sterujesz drugim PC, a tam gra trzyma kursor, powrót przez krawędź czeka. Wyjście awaryjne: przytrzymaj Esc.",
                    &mut sw.block_when_cursor_locked,
                    false);
                if ch {
                    self.dirty = true;
                }
            },
        );
    }

    fn ui_exchange(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_exchange,
            "Wymiana danych",
            "Schowek, pliki i wspólny stan klawiatury między maszynami.",
        );
        section(ui, "Pliki i schowek", "Co wolno przesyłać przez sieć.", |ui| {
            let ex = &mut self.cfg.exchange;
            let mut ch = false;
            ch |= toggle_row(
                ui,                "Przeciąganie plików między PC",
                "Pozwala wysyłać i odbierać pliki; odebrane trafiają do folderu Pobrane.",
                "Pliki idą osobnym kanałem TCP (nie miesza się z ruchem myszy). Wyłączone = odrzucamy też pliki od peerów.",
                &mut ex.allow_drag_drop,
                    false);
            ch |= toggle_row(
                ui,
                "Pełna wymiana schowka między komputerami",
                "Kopiujesz na jednym PC, wklejasz na drugim: tekst z formatowaniem, obrazy i pliki.",
                "Wyłączone = tylko zwykły tekst. Pliki do 48 MB razem trafiają przy wklejaniu do folderu tymczasowego INTmouse; foldery i większe pliki prześlij przez przeciąganie plików.",
                &mut ex.full_clipboard,
                false);
            {
                let mut en = ex.insert_remote_clipboard_hotkey.is_some();
                let mut hk = ex
                    .insert_remote_clipboard_hotkey
                    .clone()
                    .unwrap_or_else(|| "Ctrl+V".into());
                ch |= toggle_row(
                    ui,                    "Wklej zdalny schowek skrótem",
                    "Hotkey ustawia ostatni zdalny tekst w schowku (Ctrl+V wkleja).",
                    "Osobne od lokalnego Ctrl+V — możesz zmienić skrót poniżej.",
                    &mut en,
                    false);
                if en {
                    if text_row(
                        ui,                        "Skrót wklejania zdalnego",
                        "Domyślnie Ctrl+V — zmień, jeśli koliduje z aplikacjami.",
                        "Unikaj konfliktów z grami i narzędziami deweloperskimi.",
                        &mut hk,
                    false) {
                        ch = true;
                    }
                    ex.insert_remote_clipboard_hotkey = Some(hk);
                } else {
                    ex.insert_remote_clipboard_hotkey = None;
                }
            }
            if ch {
                self.dirty = true;
            }
        });
        let st = self.net.status_snapshot();
        section(
            ui,
            "Wysyłanie plików",
            "Upuść plik na okno INTmouse albo wybierz go z dysku.",
            |ui| {
                custom_row(
                    ui,
                    "Wyślij plik…",
                    "Plik trafia do komputera, którym sterujesz (albo który steruje Tobą); \
                     gdy połączony jest jeden peer — do niego. Katalogi nie są obsługiwane.",
                    "Limit rozmiaru ustawisz w zakładce Sieć.",
                    false,
                    |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.file_picker.is_open(),
                                    egui::Button::new("Wyślij plik…"),
                                )
                                .on_hover_text("Otwiera okno wyboru plików")
                                .clicked()
                            {
                                self.file_picker.open(ui.ctx());
                            }
                            if self.file_picker.is_open() {
                                ui.spinner();
                            }
                        });
                        let linked: Vec<String> = st
                            .peers
                            .iter()
                            .filter(|p| p.tcp_ok)
                            .map(|p| p.hostname.clone())
                            .collect();
                        let (txt, col) = if !self.cfg.exchange.allow_drag_drop {
                            (
                                "Wysyłanie jest wyłączone — włącz „Przeciąganie plików między PC” powyżej."
                                    .to_string(),
                                c::AMBER,
                            )
                        } else if linked.is_empty() {
                            ("Brak połączonego komputera (TCP).".to_string(), c::MUTED)
                        } else {
                            (format!("Połączone komputery: {}", linked.join(", ")), c::MUTED)
                        };
                        ui.add(egui::Label::new(RichText::new(txt).size(12.5).color(col)).wrap());
                    },
                );
            },
        );
        for action in transfers::transfers_section(ui, &st.transfers) {
            match action {
                transfers::TransferAction::Cancel(id) => {
                    if self.net.cancel_transfer(id) {
                        self.toasts.info("Anulowanie transferu…");
                    } else {
                        self.toasts.info("Ten transfer już się zakończył.");
                    }
                }
                transfers::TransferAction::Reveal(path) => {
                    if let Err(e) = transfers::reveal_in_folder(&path) {
                        self.toasts.error(format!("Nie udało się otworzyć folderu: {e}"));
                    }
                }
            }
        }
        section(
            ui,
            "Stan klawiatury",
            "Żeby layout i Caps/Num nie „rozjeżdżały się” po skoku.",
            |ui| {
                let ex = &mut self.cfg.exchange;
                let mut ch = false;
                ch |= toggle_row(
                    ui,                    "Synchronizuj układ klawiatury",
                    "Best-effort ActivateKeyboardLayout przy Enter.",
                    "Pomaga przy PL/EN i nietypowych układach.",
                    &mut ex.sync_keyboard_layout,
                    false);
                ch |= toggle_row(
                    ui,                    "Synchronizuj Caps / Num / Scroll Lock",
                    "Diodowe stany klawiszy są wspólne (TCP LockState).",
                    "Zapobiega sytuacji, w której na jednym PC Caps jest ON, na drugim OFF.",
                    &mut ex.sync_lock_keys,
                    false);
                if ch {
                    self.dirty = true;
                }
            },
        );
    }

    fn ui_output(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_pointer,
            "Wskaźnik i scroll",
            "Jak mysz i kółko zachowują się na zdalnym komputerze.",
        );
        section(ui, "Czułość", "Skalowanie ruchu i kółka.", |ui| {
            let o = &mut self.cfg.output;
            let mut ch = false;
            ch |= slider_row(
                ui,                "Prędkość scrolla w pionie",
                "Wolniej ← → szybciej",
                "Mnożnik delty pionowego kółka na zdalnym PC.",
                &mut o.vertical_scroll_speed,
                    false);
            ch |= toggle_row(
                ui,                "Odwróć scroll pionowy",
                "Natural scrolling vs klasyczny.",
                "Jak „natural scroll” w macOS / touchpadach.",
                &mut o.invert_vertical_scroll,
                    false);
            ch |= slider_row(
                ui,                "Prędkość scrolla w poziomie",
                "Dla kółek z osią X / tilt.",
                "Mnożnik poziomego scrolla.",
                &mut o.horizontal_scroll_speed,
                    false);
            ch |= toggle_row(
                ui,                "Odwróć scroll poziomy",
                "Zamień kierunek lewo/prawo.",
                "Przydatne przy tabletach i nietypowych myszach.",
                &mut o.invert_horizontal_scroll,
                    false);
            ch |= slider_row(
                ui,                "Prędkość kursora (zdalnie)",
                "Jak szybko kursor rusza się na drugim PC.",
                "Nie zmienia ustawień Windows — tylko skaluje pakiety INTmouse.",
                &mut o.mouse_speed,
                    false);
            let reported = crate::input::detected_mouse_sample_hz();
            let opts = inputshare_core::mouse_hz_options(reported);
            let shown = inputshare_core::clamp_mouse_hz(o.mouse_hz, reported);
            let hz_sub = match reported {
                Some(max) => format!(
                    "Windows podaje maks. {max} Hz tej myszy. Wyższe kroki są ukryte. Nie zmienia Hz myszy na tym komputerze."
                ),
                None => "Windows nie podał limitu tej myszy, więc widać całą listę. Nie zmienia Hz myszy na tym komputerze.".into(),
            };
            ch |= custom_row(
                ui,
                "Częstotliwość kursora na drugim PC",
                &hz_sub,
                "125, 500, 1000, 2000, 4000 albo 8000 — od 125 Hz do limitu, który Windows podaje dla tej myszy. Tyle razy na sekundę drugi komputer dostaje ruch. To nie jest przełącznik Hz w Windows ani w oprogramowaniu myszy.",
                false,
                |ui| {
                    let mut picked = false;
                    egui::ComboBox::from_id_salt("remote_mouse_hz")
                        .selected_text(format!("{shown} Hz"))
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for hz in opts {
                                if ui.selectable_label(hz == shown, format!("{hz} Hz")).clicked() {
                                    o.mouse_hz = hz;
                                    picked = true;
                                }
                            }
                        });
                    picked
                },
            );
            if ch {
                self.dirty = true;
            }
        });
        section(
            ui,
            "Widoczność i tryb ruchu",
            "Krytyczne dla gier — trzymaj włączony tryb względny.",
            |ui| {
                let o = &mut self.cfg.output;
                let mut ch = false;
                ch |= toggle_row(
                    ui,                    "Rysuj kursor, gdy system go ukrywa",
                    "Warstwa HWND (czarne kółko) gdy jesteś sterowany zdalnie.",
                    "INTmouse narysuje własny wskaźnik nad pulpitem.",
                    &mut o.draw_cursor_if_invisible,
                    false);
                ui.add_enabled_ui(o.draw_cursor_if_invisible, |ui| {
                    if toggle_row(
                        ui,                        "Większy kursor",
                        "Łatwiej zauważyć na 4K / dalekim monitorze.",
                        "Działa tylko gdy rysowanie kursora jest włączone.",
                        &mut o.large_cursor,
                    false) {
                        ch = true;
                    }
                });
                ch |= toggle_row(
                    ui,                    "Ruch względny (zalecane do gier)",
                    "Wysyłamy delty dx/dy zamiast absolutnych współrzędnych.",
                    "Bez tego FPS-y i raw input w grach często się psują — domyślnie WŁĄCZONE.",
                    &mut o.use_relative_mouse,
                    false);
                if ch {
                    self.dirty = true;
                }
            },
        );
    }

    fn ui_network(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_network,
            "Sieć lokalna",
            "Jak maszyny się znajdują i łączą w LAN.",
        );
        section(ui, "Ochrona", "Nie wpuszczaj obcych z tej samej sieci Wi‑Fi.", |ui| {
            let n = &mut self.cfg.network;
            let mut ch = false;
            ch |= toggle_row(
                ui,                "Wymagaj wspólnego hasła",
                "Połączy się tylko PC z tym samym hasłem (SHA-256).",
                "Prosty shared-secret — włącz zawsze w biurze / na otwartym Wi‑Fi.",
                &mut n.password_required,
                    false);
            ui.add_enabled_ui(n.password_required, |ui| {
                if text_row(
                    ui,                    "Hasło połączenia",
                    "To samo na wszystkich komputerach w grupie.",
                    "Nie jest to konto w chmurze — tylko lokalny sekret LAN.",
                    &mut n.password,
                    false) {
                    ch = true;
                }
            });
            if ch {
                self.dirty = true;
            }
        });
        section(ui, "Porty i adapter", "TCP = sesja/schowek, UDP = mysz + discovery.", |ui| {
            let n = &mut self.cfg.network;
            let mut ch = false;
            ch |= self.adapters.ui(ui, &mut n.preferred_adapter);
            ch |= port_row(
                ui,                "Port TCP",
                "Niezawodne dane: handshake, schowek, pliki.",
                "Domyślnie 6555. Otwórz w firewallu Windows.",
                &mut n.tcp_port,
                    false);
            ch |= port_row(
                ui,                "Port UDP",
                "Szybka ścieżka: pozycja myszy i ogłoszenie w sieci.",
                "Domyślnie 1046. Osobny od TCP = mniej laga.",
                &mut n.udp_port,
                    false);
            ch |= toggle_row(
                ui,                "Używaj IPv6",
                "Dodatkowy nasłuch TCP + UDP na IPv6. Peera IPv6 dodaj po adresie IP w zakładce Komputery; \
                 wykrywanie (broadcast) działa tylko w IPv4.",
                "Działa end-to-end (wejście, schowek, pliki). Zostaw wyłączone, jeśli Twój router / VPN miesza z IPv6.",
                &mut n.use_ipv6,
                    false);
            let (tcp, udp) = (n.tcp_port, n.udp_port);
            if ch {
                self.dirty = true;
            }
            let busy = self.firewall.busy();
            let clicked = custom_row(
                ui,
                "Zapora Windows",
                &format!("Reguły dla TCP {tcp} i UDP {udp}, tylko sieć lokalna."),
                "Obejmują porty, nie plik programu, więc nowe wersje nie proszą o zgodę ponownie. Windows zapyta raz o uprawnienia administratora.",
                false,
                |ui| ui.add_enabled(!busy, egui::Button::new(if busy { "Dodaję…" } else { "Dodaj reguły zapory" })).clicked(),
            );
            if clicked {
                self.firewall.add_now(tcp, udp);
            }
        });
        section(
            ui,
            "Pliki",
            "Limit dla przesyłania plików między komputerami.",
            |ui| {
                let n = &mut self.cfg.network;
                let changed = custom_row(
                    ui,
                    "Maksymalny rozmiar pliku",
                    "W MB; 0 = bez limitu. Dotyczy wysyłania i odbioru.",
                    "Pliki większe niż limit są odrzucane przed wysłaniem.",
                    false,
                    |ui| {
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                egui::DragValue::new(&mut n.max_file_size_mb)
                                    .range(0..=1_048_576u64)
                                    .speed(16.0)
                                    .suffix(" MB"),
                            );
                            if n.max_file_size_mb == 0 {
                                ui.label(RichText::new("bez limitu").size(12.5).color(c::MUTED));
                            }
                            r.changed()
                        })
                        .inner
                    },
                );
                if changed {
                    self.dirty = true;
                }
            },
        );
        network_panel::status_section(ui, &self.net.status_snapshot());
    }

    fn ui_clients(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_machines,
            "Komputery w grupie",
            "Które adresy IP mogą się łączyć z tą instalacją.",
        );
        let issues = self.net.status_snapshot().link_issues;
        if !issues.is_empty() {
            section(
                ui,
                "Problemy z połączeniem",
                "Dlaczego ten komputer nie łączy się z drugim. Znika, gdy połączenie się uda.",
                |ui| {
                    for issue in &issues {
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
        section(ui, "Lista dozwolonych","Dodaj ręcznie albo polegaj na discovery.", |ui| {
            custom_row(
                ui,
                "Dodaj komputer",
                "Adres IP drugiej maszyny i jej port TCP.",
                "", false,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("IP").color(c::MUTED));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft_ip)
                                .desired_width(170.0)
                                .hint_text("192.168.1.20")
                                .margin(Vec2::new(8.0, 5.0)),
                        );
                        ui.add_space(4.0);
                        ui.label(RichText::new("TCP").color(c::MUTED));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft_port)
                                .desired_width(70.0)
                                .margin(Vec2::new(8.0, 5.0)),
                        );
                        if ui.button("Dodaj").on_hover_text("Dodaj maszynę do białej listy").clicked() {
                            let port: u16 = self.draft_port.parse().unwrap_or(6555);
                            let host = self.draft_ip.trim().to_string();
                            if !host.is_empty() {
                                self.cfg.clients.accepted.push(ClientEntry { host, tcp_port: port });
                                self.draft_ip.clear();
                                self.dirty = true;
                            }
                        }
                    });
                },
            );
            let mut remove = None;
            let accepted = &self.cfg.clients.accepted;
            custom_row(
                ui,
                "Zaakceptowane",
                if accepted.is_empty() {
                    "Pusta lista = każdy z hasłem OK. Inaczej tylko te IP."
                } else {
                    "Te adresy mogą się łączyć / są wybierane do outbound TCP."
                },
                "", false,
                |ui| {
                    for (i, c) in accepted.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}:{}", c.host, c.tcp_port))
                                    .monospace()
                                    .size(14.0),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("Usuń").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                },
            );
            if let Some(i) = remove {
                self.cfg.clients.accepted.remove(i);
                self.dirty = true;
            }
            if toggle_row(
                ui,                "Rozgłaszaj swoją obecność (UDP)",
                "Announce co ~2s na 255.255.255.255:udp_port.",
                "Wyłącz, jeśli nie chcesz być widoczny dla innych w sieci.",
                &mut self.cfg.clients.broadcast_udp,
                    false) {
                self.dirty = true;
            }
            let discovered = self.net.status_snapshot().discovered;
            if !discovered.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Wykryte (nie auto-dodawane): {}",
                        discovered
                            .iter()
                            .map(|d| format!("{} @ {}", d.hostname, d.addr.ip()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .size(12.5)
                    .color(c::MUTED),
                );
            }
        });
    }

    fn ui_internet(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_updates,
            "Aktualizacje",
            "Sprawdzanie nowszych wersji programu (nie wpływa na LAN).",
        );
        section(ui, "Wersje", "Możesz to całkowicie wyłączyć — apka działa offline.", |ui| {
            let i = &mut self.cfg.internet;
            let mut ch = false;
            ch |= toggle_row(
                ui,
                "Sprawdzaj dostępność aktualizacji",
                "Kilka sekund po starcie i co 12 godzin, w tle. Pobiera tylko plik version.json ze strony intmouse.vxh.pl.",
                "Nie wysyła żadnych danych o Twoim komputerze ani ruchu myszy.",
                &mut i.check_updates,
                false,
            );
            ch |= toggle_row(
                ui,
                "Instaluj poprawki automatycznie",
                "Sama pobierze, sprawdzi SHA-256 i przygotuje podmianę; zainstaluje się przy następnym starcie.",
                "Wyłączone = baner z przyciskiem Aktualizuj, instalacja dopiero po kliknięciu.",
                &mut i.auto_install,
                false,
            );
            if ch {
                self.dirty = true;
            }
        });
        if self.updates.ui(ui, &self.cfg, &self.net) {
            self.quit = true;
        }
        ui.label(
            RichText::new("Proxy HTTP — wyłączone na razie (nie będzie w UI, aż będzie potrzeba).")
                .size(12.5)
                .color(c::MUTED)
                .italics(),
        );
    }

    fn ui_misc(&mut self, ui: &mut egui::Ui) {
        page_header(
            ui,
            &self.icons.nav_misc,
            "Bezpieczeństwo i wygoda",
            "Panic, dim, hotkeye SAS i toasty działają lokalnie na Windows.",
        );
        self.ui_startup(ui);
        section(ui, "Awaryjne wyjście", "Wyłącza udostępnianie + zapętlenie na tym PC. Skrót Ctrl+Alt+Shift+Q zamyka program od razu.", |ui| {
            let m = &mut self.cfg.misc;
            let mut ch = false;
            ch |= toggle_row(
                ui,
                "Włącz klawisz paniki",
                "Gdy wyłączone — panika jest całkowicie nieaktywna.",
                "Domyślnie włączone.",
                &mut m.panic_enabled,
                    false);
            ui.add_enabled_ui(m.panic_enabled, |ui| {
                if text_row(
                    ui,
                    "Klawisz",
                    "Escape / Esc / F12 / Pause…",
                    "Hold ~0.45s albo podwójne wciśnięcie — zależnie od trybu poniżej.",
                    &mut m.panic_key,
                    false) {
                    ch = true;
                }
                let mut mode = m.panic_mode;
                custom_row(
                    ui,
                    "Tryb paniki",
                    "Domyślnie: przytrzymaj (hold).",
                    "Hold = mniej przypadkowych wyłączeń w grach.", false,
                    |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(mode == PanicMode::Hold, "Przytrzymaj (hold)")
                                .clicked()
                            {
                                mode = PanicMode::Hold;
                            }
                            if ui
                                .selectable_label(
                                    mode == PanicMode::DoublePress,
                                    "Podwójne wciśnięcie",
                                )
                                .clicked()
                            {
                                mode = PanicMode::DoublePress;
                            }
                        });
                    },
                );
                if mode != m.panic_mode {
                    m.panic_mode = mode;
                    ch = true;
                }
            });
            {
                let mut en = m.sas_hotkey.is_some();
                let mut hk = m
                    .sas_hotkey
                    .clone()
                    .unwrap_or_else(|| "Ctrl+Alt+Ins".into());
                ch |= toggle_row(
                    ui,
                    "Skrót = Ctrl+Alt+Del na tym PC",
                    "Best-effort: sas.dll SendSAS, potem SendInput (Windows często blokuje).",
                    "Wymaga uprawnień / komponentu systemowego — status w pasku dolnym.",
                    &mut en,
                    false);
                if en {
                    if text_row(ui, "Skrót SAS", "Domyślnie Ctrl+Alt+Ins", "", &mut hk,
                    false) {
                        ch = true;
                    }
                    m.sas_hotkey = Some(hk);
                } else {
                    m.sas_hotkey = None;
                }
            }
            if ch {
                self.dirty = true;
            }
        });
        section(
            ui,
            "Ekrany i diagnostyka",
            "Dim overlay, plik logów i toasty Windows.",
            |ui| {
                let diag_path = self.diag.log_path().display().to_string();
                let m = &mut self.cfg.misc;
                let mut ch = false;
                ch |= toggle_row(
                    ui,
                    "Przyciemnij nieaktywne monitory",
                    "Czarny overlay na ekranach bez kursora.",
                    "Aktualizacja ~66 ms; siła poniżej.",
                    &mut m.dim_inactive,
                    false);
                ui.add_enabled_ui(m.dim_inactive, |ui| {
                    if slider_row(
                        ui,
                        "Siła przyciemnienia",
                        "Jaśniej ← → ciemniej",
                        "",
                        &mut m.dim_level,
                    false) {
                        ch = true;
                    }
                });
                ch |= toggle_row(
                    ui,
                    "Tryb diagnostyczny",
                    &format!("Więcej logów + dopisywanie do {diag_path}"),
                    "Działa od startu (--diag / config); włączanie w trakcie otwiera plik.",
                    &mut m.diagnostics,
                    false);
                ch |= toggle_row(
                    ui,
                    "Powiadomienia w tray",
                    "Toast przy panic / włącz-wyłącz / zapisie.",
                    "Windows toast (winrt-notification).",
                    &mut m.tray_notifications,
                    false);
                if ch {
                    self.dirty = true;
                }
            },
        );
        ui.add(
            egui::Label::new(
                RichText::new(format!("Plik konfiguracji: {}", self.config_path.display()))
                    .size(12.5)
                    .color(c::FAINT),
            )
            .wrap(),
        );
    }

    fn ui_monitors(&mut self, ui: &mut egui::Ui) {
        let local_n = self.cfg.layout.monitors.iter().filter(|m| m.system_synced).count();
        page_header(
            ui,
            &self.icons.nav_layout,
            "Układ ekranów",
            &format!(
                "Każdy komputer trzyma układ z Windows ({local_n} lokalnych, nie do rozsunięcia). Drugi komputer przeciągasz w całości — krawędź dokleja się, i tamtędy przechodzi kursor."
            ),
        );
        ui.horizontal(|ui| {
            if ui
                .button("Odśwież z systemu")
                .on_hover_text("Wczytaj aktualne monitory i ich układ z Windows")
                .clicked()
            {
                self.refresh_system_monitors();
            }
            if ui
                .button("Dodaj zdalny (ręcznie)")
                .on_hover_text("Płytka innego PC — przeciągasz cały komputer, krawędź się dokleja")
                .clicked()
            {
                let n = self.cfg.layout.monitors.len();
                let label = char::from_u32(b'A' as u32 + (n as u32) % 26).unwrap_or('X');
                self.cfg.layout.monitors.push(MonitorTile {
                    id: format!("remote-{n}"),
                    host: "REMOTE-PC".into(),
                    label: label.to_string(),
                    x: self
                        .cfg
                        .layout
                        .monitors
                        .iter()
                        .map(|m| m.x + m.width as i32)
                        .max()
                        .unwrap_or(0)
                        + 64,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    primary: false,
                    system_synced: false,
                });
                self.dirty = true;
            }
            ui.label(
                RichText::new(if self.cfg.switching.wrap_pointer {
                    "Zapętlenie: ON (lokalne)"
                } else {
                    "Zapętlenie: OFF"
                })
                .size(12.5)
                .color(if self.cfg.switching.wrap_pointer {
                    c::OK
                } else {
                    c::MUTED
                }),
            );
        });
        ui.add_space(8.0);

        let (resp, painter) = ui.allocate_painter(
            Vec2::new(ui.available_width(), ui.available_height().max(380.0)),
            Sense::click_and_drag(),
        );
        let rect = resp.rect;
        painter.rect_filled(rect, 8.0, Color32::from_rgb(12, 14, 18));
        painter.rect_stroke(rect, 8.0, egui::Stroke::new(1.0_f32, c::BORDER));

        let mons = &self.cfg.layout.monitors;
        if mons.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Brak monitorów — kliknij „Odśwież z systemu”",
                egui::FontId::proportional(15.0),
                c::MUTED,
            );
            return;
        }

        let min_x = mons.iter().map(|m| m.x).min().unwrap_or(0) as f32;
        let min_y = mons.iter().map(|m| m.y).min().unwrap_or(0) as f32;
        let max_x = mons.iter().map(|m| m.x + m.width as i32).max().unwrap_or(1920) as f32;
        let max_y = mons.iter().map(|m| m.y + m.height as i32).max().unwrap_or(1080) as f32;
        let world_w = (max_x - min_x).max(1.0);
        let world_h = (max_y - min_y).max(1.0);
        let pad = 48.0;
        let scale = ((rect.width() - pad * 2.0) / world_w)
            .min((rect.height() - pad * 2.0) / world_h)
            .clamp(0.05, 0.45);

        let to_screen = |x: i32, y: i32| {
            egui::pos2(
                rect.left() + pad + (x as f32 - min_x) * scale,
                rect.top() + pad + (y as f32 - min_y) * scale,
            )
        };

        let pointer = resp.interact_pointer_pos();
        if resp.drag_started() {
            if let Some(p) = pointer {
                for m in self.cfg.layout.monitors.iter().rev() {
                    if m.system_synced {
                        continue; // local OS geometry is locked
                    }
                    let p0 = to_screen(m.x, m.y);
                    let p1 = to_screen(m.x + m.width as i32, m.y + m.height as i32);
                    if egui::Rect::from_min_max(p0, p1).contains(p) {
                        self.drag_id = Some(m.id.clone());
                        self.drag_offset = Vec2::new(p.x - p0.x, p.y - p0.y);
                        break;
                    }
                }
            }
        }
        if resp.dragged() {
            if let (Some(id), Some(p)) = (self.drag_id.clone(), pointer) {
                let grabbed = self.cfg.layout.monitors.iter().find(|m| m.id == id && !m.system_synced).map(|m| (m.host.clone(), m.x, m.y));
                if let Some((host, ox, oy)) = grabbed {
                    let world_x = min_x + (p.x - self.drag_offset.x - rect.left() - pad) / scale;
                    let world_y = min_y + (p.y - self.drag_offset.y - rect.top() - pad) / scale;
                    let dx = world_x.round() as i32 - ox;
                    let dy = world_y.round() as i32 - oy;
                    if self.cfg.layout.translate_host(&host, dx, dy) {
                        self.dirty = true;
                    }
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(id) = self.drag_id.clone() {
                if let Some(host) = self
                    .cfg
                    .layout
                    .monitors
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.host.clone())
                {
                    if self.cfg.layout.snap_host(&host) {
                        self.status = format!("Doklejono {host} do krawędzi. Zapisz układ.");
                        self.dirty = true;
                    }
                }
            }
            self.drag_id = None;
        }

        for m in &self.cfg.layout.monitors {
            let p0 = to_screen(m.x, m.y);
            let p1 = to_screen(m.x + m.width as i32, m.y + m.height as i32);
            let r = egui::Rect::from_min_max(p0, p1);
            let fill = if m.primary {
                Color32::from_rgb(36, 90, 130)
            } else if m.system_synced {
                Color32::from_rgb(30, 70, 110)
            } else {
                Color32::from_rgb(70, 55, 30)
            };
            let stroke = if m.system_synced { c::CYAN } else { c::AMBER };
            painter.rect_filled(r, 4.0, fill);
            painter.rect_stroke(r, 4.0, egui::Stroke::new(2.0_f32, stroke));
            painter.circle_filled(
                r.center() + Vec2::new(0.0, -18.0),
                13.0,
                Color32::from_rgb(10, 12, 16),
            );
            painter.text(
                r.center() + Vec2::new(0.0, -18.0),
                egui::Align2::CENTER_CENTER,
                &m.label,
                egui::FontId::proportional(13.0),
                Color32::WHITE,
            );
            let res = format!("{}×{}", m.width, m.height);
            let line2 = if m.primary {
                format!("{} · primary", res)
            } else {
                res
            };
            painter.text(
                r.center() + Vec2::new(0.0, 4.0),
                egui::Align2::CENTER_CENTER,
                line2,
                egui::FontId::proportional(11.0),
                Color32::from_rgb(200, 220, 240),
            );
            painter.text(
                r.center() + Vec2::new(0.0, 20.0),
                egui::Align2::CENTER_CENTER,
                &m.host,
                egui::FontId::proportional(10.5),
                c::MUTED,
            );
        }
    }
}
