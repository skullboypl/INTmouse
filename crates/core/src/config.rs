use inputshare_protocol::{DEFAULT_TCP_PORT, DEFAULT_UDP_PORT};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Global on/off (tray + hotkey Ctrl+Shift+A).
    pub sharing_enabled: bool,
    pub switching: SwitchingConfig,
    pub exchange: ExchangeConfig,
    pub output: OutputConfig,
    pub network: NetworkConfig,
    pub clients: ClientsConfig,
    pub internet: InternetConfig,
    pub startup: StartupConfig,
    pub misc: MiscConfig,
    pub layout: crate::layout::MonitorLayout,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            sharing_enabled: true,
            switching: SwitchingConfig::default(),
            exchange: ExchangeConfig::default(),
            output: OutputConfig::default(),
            network: NetworkConfig::default(),
            clients: ClientsConfig::default(),
            internet: InternetConfig::default(),
            startup: StartupConfig::default(),
            misc: MiscConfig::default(),
            // Filled from the OS on first GUI/run (real monitors).
            layout: crate::layout::MonitorLayout::default(),
        }
    }
}

impl AppConfig {
    pub fn load_or_default(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            let mut cfg: Self = toml::from_str(&text)?;
            cfg.misc.normalize_legacy();
            Ok(cfg)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwitchingConfig {
    pub equalize_display_dimensions: bool,
    pub wrap_pointer: bool,
    pub quick_jump_hotkey: String,
    pub layout_toggle_hotkey: Option<String>,
    pub enable_disable_hotkey: String,
    pub prevent_fullscreen: bool,
    pub prevent_maximized: bool,
    pub prevent_corners: bool,
    pub require_modifier: Option<String>,
    pub blocked_programs: Vec<String>,
    pub double_tap_border_override: bool,
    /// Switch computers only when the cursor hits the edge fast (a flick), not when it slides in.
    pub require_fast_hit: bool,
    /// Minimum speed of that hit, px/s.
    pub fast_hit_speed: u32,
    /// Never switch while a program holds the cursor (hidden or clipped, as games do).
    pub block_when_cursor_locked: bool,
}

/// Default minimum speed of a "fast hit" (px/s).
pub const DEFAULT_FAST_HIT_SPEED: u32 = 500;

impl Default for SwitchingConfig {
    fn default() -> Self {
        Self {
            equalize_display_dimensions: true,
            wrap_pointer: true,
            quick_jump_hotkey: "Ctrl+Shift+Q".into(),
            layout_toggle_hotkey: None,
            enable_disable_hotkey: "Ctrl+Shift+A".into(),
            prevent_fullscreen: false,
            prevent_maximized: false,
            prevent_corners: false,
            require_modifier: None,
            blocked_programs: Vec::new(),
            double_tap_border_override: false,
            require_fast_hit: false,
            fast_hit_speed: DEFAULT_FAST_HIT_SPEED,
            block_when_cursor_locked: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExchangeConfig {
    pub allow_drag_drop: bool,
    pub insert_remote_clipboard_hotkey: Option<String>,
    pub sync_keyboard_layout: bool,
    pub sync_lock_keys: bool,
    /// Share the whole clipboard (pictures, formatted text, copied files), not only plain text.
    pub full_clipboard: bool,
}

impl Default for ExchangeConfig {
    fn default() -> Self {
        Self {
            allow_drag_drop: false,
            insert_remote_clipboard_hotkey: Some("Ctrl+V".into()),
            sync_keyboard_layout: true,
            sync_lock_keys: true,
            full_clipboard: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub vertical_scroll_speed: f32,
    pub invert_vertical_scroll: bool,
    pub horizontal_scroll_speed: f32,
    pub invert_horizontal_scroll: bool,
    pub mouse_speed: f32,
    /// How often the computer being steered receives cursor motion.
    /// Does not change the USB polling rate of the mouse on this computer.
    /// One of [`MOUSE_HZ_STEPS`], capped by what Windows reports for the mouse.
    pub mouse_hz: u32,
    pub draw_cursor_if_invisible: bool,
    pub large_cursor: bool,
    /// Always prefer relative deltas (ShareMouse option; we default ON for games).
    pub use_relative_mouse: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            vertical_scroll_speed: 0.5,
            invert_vertical_scroll: false,
            horizontal_scroll_speed: 0.5,
            invert_horizontal_scroll: false,
            mouse_speed: 0.5,
            mouse_hz: 1000,
            draw_cursor_if_invisible: false,
            large_cursor: false,
            use_relative_mouse: true,
        }
    }
}

/// Polling steps offered for the cursor on the computer being steered.
pub const MOUSE_HZ_STEPS: [u32; 6] = [125, 500, 1000, 2000, 4000, 8000];

/// Steps from 125 Hz up to the mouse's reported maximum.
/// `None` (or a report below 125) means Windows did not give a usable rate, so every step is offered.
pub fn mouse_hz_options(reported_max: Option<u32>) -> Vec<u32> {
    match reported_max.filter(|&n| n >= 125) {
        None => MOUSE_HZ_STEPS.to_vec(),
        Some(max) => {
            let opts: Vec<u32> = MOUSE_HZ_STEPS.into_iter().filter(|h| *h <= max).collect();
            if opts.is_empty() {
                vec![MOUSE_HZ_STEPS[0]]
            } else {
                opts
            }
        }
    }
}

/// Nearest offered step that is not above the mouse's reported maximum.
pub fn clamp_mouse_hz(configured: u32, reported_max: Option<u32>) -> u32 {
    let opts = mouse_hz_options(reported_max);
    if opts.contains(&configured) {
        configured
    } else {
        opts.iter().copied().rev().find(|h| *h <= configured).unwrap_or(opts[0])
    }
}

/// How long the capture thread waits between cursor updates at `hz` (clamped to 125..=8000).
pub fn mouse_tick_period(hz: u32) -> Duration {
    let hz = hz.clamp(125, 8000);
    Duration::from_nanos(1_000_000_000 / u64::from(hz))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub password_required: bool,
    pub password: String,
    pub preferred_adapter: String,
    pub tcp_port: u16,
    pub udp_port: u16,
    pub use_ipv6: bool,
    /// Max size of one sent/received file in MiB (drag & drop). 0 = no limit.
    pub max_file_size_mb: u64,
    /// The one-time question about Windows Firewall rules was answered (added or declined).
    pub firewall_asked: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            password_required: false,
            password: String::new(),
            preferred_adapter: "All".into(),
            tcp_port: DEFAULT_TCP_PORT,
            udp_port: DEFAULT_UDP_PORT,
            use_ipv6: false,
            max_file_size_mb: 4096,
            firewall_asked: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientsConfig {
    pub accepted: Vec<ClientEntry>,
    pub broadcast_udp: bool,
    /// Dial computers announced on the LAN when the list above is empty and a password is set
    /// (same password on both sides = they link up on their own).
    pub auto_connect: bool,
}

impl Default for ClientsConfig {
    fn default() -> Self {
        Self {
            accepted: Vec::new(),
            broadcast_udp: true,
            auto_connect: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEntry {
    pub host: String,
    pub tcp_port: u16,
}

/// Start with Windows / run elevated (see `startup.rs` in the app).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StartupConfig {
    /// Start hidden in the tray after logon.
    pub launch_at_login: bool,
    /// Run with administrator rights (steers elevated windows too). Changing it restarts the program.
    pub run_as_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InternetConfig {
    pub check_updates: bool,
    pub auto_install: bool,
}

impl Default for InternetConfig {
    fn default() -> Self {
        Self {
            check_updates: true,
            auto_install: false,
        }
    }
}

/// How the panic key fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PanicMode {
    /// Hold the key ~0.45s (default — harder to trigger by accident in games).
    #[default]
    Hold,
    /// Two presses within ~400ms.
    DoublePress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MiscConfig {
    /// Master switch for panic key.
    pub panic_enabled: bool,
    pub panic_key: String,
    pub panic_mode: PanicMode,
    /// Legacy field — mapped to `panic_mode` on load if present in old configs.
    #[serde(default, skip_serializing)]
    pub panic_double_press: Option<bool>,
    pub sas_hotkey: Option<String>,
    pub dim_inactive: bool,
    pub dim_level: f32,
    pub diagnostics: bool,
    pub tray_notifications: bool,
}

impl Default for MiscConfig {
    fn default() -> Self {
        Self {
            panic_enabled: true,
            panic_key: "Escape".into(),
            panic_mode: PanicMode::Hold,
            panic_double_press: None,
            sas_hotkey: None,
            dim_inactive: false,
            dim_level: 0.4,
            diagnostics: false,
            tray_notifications: true,
        }
    }
}

impl MiscConfig {
    /// Normalize legacy `panic_double_press` into `panic_mode`.
    pub fn normalize_legacy(&mut self) {
        if let Some(dbl) = self.panic_double_press.take() {
            self.panic_mode = if dbl {
                PanicMode::DoublePress
            } else {
                PanicMode::Hold
            };
        }
    }
}

#[cfg(test)]
mod update_config_tests {
    use super::*;

    #[test]
    fn old_config_with_removed_license_and_channel_keys_still_loads() {
        let old = "sharing_enabled = true
[internet]
check_updates = false
update_channel = \"Wszystkie\"
[license]
api_base = \"https://example.test\"
enforce = true
";
        let cfg: AppConfig = toml::from_str(old).expect("old config must still parse");
        assert!(!cfg.internet.check_updates);
        assert!(!cfg.internet.auto_install);
    }

    #[test]
    fn startup_options_are_off_by_default_and_old_configs_load() {
        let d = StartupConfig::default();
        assert!(!d.launch_at_login && !d.run_as_admin);
        let old: AppConfig = toml::from_str("sharing_enabled = true\n").expect("config without [startup]");
        assert!(!old.startup.launch_at_login);
        let set: AppConfig = toml::from_str("[startup]\nrun_as_admin = true\n").expect("partial [startup]");
        assert!(set.startup.run_as_admin && !set.startup.launch_at_login);
    }

    #[test]
    fn auto_connect_is_on_by_default_and_old_configs_get_it() {
        assert!(ClientsConfig::default().auto_connect);
        let old: AppConfig = toml::from_str("[clients]\nbroadcast_udp = false\n").expect("old clients section");
        assert!(old.clients.auto_connect, "a missing key falls back to the default");
        assert!(!old.clients.broadcast_udp);
    }

    #[test]
    fn updates_are_checked_by_default_but_never_installed_on_their_own() {
        let net = InternetConfig::default();
        assert!(net.check_updates);
        assert!(!net.auto_install);
    }

    #[test]
    fn mouse_hz_defaults_to_1000_and_old_configs_keep_loading() {
        assert_eq!(OutputConfig::default().mouse_hz, 1000);
        let old: AppConfig = toml::from_str("[output]\nmouse_speed = 0.5\n").expect("old output");
        assert_eq!(old.output.mouse_hz, 1000);
        assert!((old.output.mouse_speed - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mouse_hz_options_stop_at_the_reported_maximum() {
        assert_eq!(
            mouse_hz_options(None),
            vec![125, 500, 1000, 2000, 4000, 8000]
        );
        assert_eq!(
            mouse_hz_options(Some(0)),
            vec![125, 500, 1000, 2000, 4000, 8000],
            "0 means Windows did not report a rate"
        );
        assert_eq!(mouse_hz_options(Some(1000)), vec![125, 500, 1000]);
        assert_eq!(mouse_hz_options(Some(4000)), vec![125, 500, 1000, 2000, 4000]);
        assert_eq!(mouse_hz_options(Some(8000)), vec![125, 500, 1000, 2000, 4000, 8000]);
        assert_eq!(mouse_hz_options(Some(126)), vec![125]);
        assert_eq!(clamp_mouse_hz(8000, Some(1000)), 1000);
        assert_eq!(clamp_mouse_hz(125, Some(8000)), 125);
        assert_eq!(clamp_mouse_hz(50, Some(8000)), 125);
        assert_eq!(clamp_mouse_hz(8000, None), 8000);
    }

    #[test]
    fn mouse_tick_period_matches_the_step() {
        assert_eq!(mouse_tick_period(125), Duration::from_millis(8));
        assert_eq!(mouse_tick_period(500), Duration::from_millis(2));
        assert_eq!(mouse_tick_period(1000), Duration::from_millis(1));
        assert_eq!(mouse_tick_period(8000), Duration::from_micros(125));
        assert_eq!(mouse_tick_period(1), Duration::from_millis(8));
        assert_eq!(mouse_tick_period(99_999), Duration::from_micros(125));
    }
}
