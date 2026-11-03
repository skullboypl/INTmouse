// Release is a GUI-subsystem exe so double-clicking it opens no console window.
// Debug keeps the console for `cargo run` logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clock;
mod diag;
mod dim_overlay;
mod displays;
mod firewall;
mod gui;
mod hotkeys;
mod http;
mod input;
mod local_wrap;
mod net;
mod notify;
mod panic_key;
mod sas;
mod startup;
mod updater;
mod win_query;

use anyhow::Context;
use clap::{Parser, Subcommand};
use diag::{DiagLog, TeeMakeWriter};
use inputshare_core::AppConfig;
use inputshare_protocol::{DEFAULT_TCP_PORT, DEFAULT_UDP_PORT, PROTOCOL_VERSION};
use std::path::PathBuf;
use tracing::{info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser, Debug)]
#[command(
    name = "inputshare",
    about = "Low-latency mouse/keyboard sharing over LAN (Windows / Linux / macOS)"
)]
struct Cli {
    /// Path to config TOML (default: platform config dir)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Enable diagnostics logging (also writes config_dir/inputshare.log)
    #[arg(long)]
    diag: bool,

    /// Start hidden in the tray (what the autostart entry passes).
    #[arg(long, hide = true)]
    tray: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Open Preferences UI + system tray (default)
    Gui,
    /// Print resolved config
    Status,
    /// Write a default config file
    InitConfig,
    /// Headless stub (no UI)
    Run,
}

fn default_config_path() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "INTmouse")
        .context("could not resolve config directory")?;
    let path = dirs.config_dir().join("config.toml");
    // Settings of the earlier test builds (an InputShare folder) come along once.
    if !path.exists() {
        if let Some(old) = directories::ProjectDirs::from("dev", "InputShare", "InputShare") {
            let old = old.config_dir().join("config.toml");
            if old.exists() && std::fs::create_dir_all(dirs.config_dir()).is_ok() {
                let _ = std::fs::copy(&old, &path);
            }
        }
    }
    Ok(path)
}

/// Applies a staged update. Returns `true` only when the new build was started
/// and this (old) process must exit. Any failure is logged and never blocks startup.
fn apply_pending_update() -> bool {
    match updater::apply_pending_on_startup() {
        Ok(updater::ApplyOutcome::NothingPending) => false,
        Ok(updater::ApplyOutcome::Applied { exe }) => {
            info!("Update applied, relaunching {}", exe.display());
            match updater::restart_into(&exe) {
                Ok(()) => true,
                Err(e) => {
                    warn!("Update applied but relaunch failed, continuing with this process: {e:#}");
                    false
                }
            }
        }
        Err(e) => {
            warn!("Applying staged update failed, starting normally: {e:#}");
            false
        }
    }
}

/// A GUI-subsystem exe starts without std handles. When it is run from a terminal with
/// arguments (`status`, `--help`, ...), print into that terminal; redirected handles are kept.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    use windows::core::w;
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let has_handle = GetStdHandle(std_handle).is_ok_and(|h| !h.is_invalid());
            if has_handle {
                continue;
            }
            if let Ok(con) = CreateFileW(
                w!("CONOUT$"),
                GENERIC_WRITE.0,
                FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) {
                let _ = SetStdHandle(std_handle, con);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(all(windows, not(debug_assertions)))]
    if std::env::args_os().len() > 1 {
        attach_parent_console();
    }
    let cli = Cli::parse();

    let config_path = match cli.config {
        Some(p) => p,
        None => default_config_path()?,
    };

    // Honor --diag and config.misc.diagnostics (config may not exist yet).
    let cfg_for_log = AppConfig::load_or_default(&config_path).unwrap_or_default();
    let diag_on = cli.diag || cfg_for_log.misc.diagnostics;
    let diag = DiagLog::new(&config_path, diag_on);

    let filter = if diag_on {
        "inputshare=debug,info"
    } else {
        "info"
    };
    // The filter can be switched at run time (Logi tab / Tryb diagnostyczny) without a restart.
    let (filter_layer, filter_handle) =
        tracing_subscriber::reload::Layer::new(tracing_subscriber::EnvFilter::new(filter));
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_timer(clock::LocalTimer)
                .with_writer(TeeMakeWriter::new(diag.clone())),
        )
        .init();
    diag::install_detail_switch(move |detailed| {
        let level = if detailed { "inputshare=debug,info" } else { "info" };
        let _ = filter_handle.modify(|f| *f = tracing_subscriber::EnvFilter::new(level));
    });

    let command = cli.command.unwrap_or(Commands::Gui);
    // A staged update (`INTmouse.exe.new`) is swapped in before any UI, hook or
    // socket exists. Only for the commands that actually run the app.
    if matches!(command, Commands::Gui | Commands::Run) && apply_pending_update() {
        return Ok(());
    }

    // "Run as administrator" is a setting, not a property of the shortcut: a copy started the normal
    // way hands over to an elevated one (the user sees the Windows prompt). Declining keeps this copy.
    if matches!(command, Commands::Gui) && cfg_for_log.startup.run_as_admin && !startup::is_elevated() {
        let exe = std::env::current_exe()?;
        match startup::relaunch(&exe, true) {
            Ok(()) => return Ok(()),
            Err(e) => warn!("Nie uruchomiono jako administrator: {e:?}"),
        }
    }

    match command {
        Commands::Gui => {
            info!("Starting UI + tray…");
            gui::run(config_path, diag, cli.tray)
        }
        Commands::InitConfig => {
            let cfg = AppConfig::default();
            cfg.save(&config_path)?;
            println!("Wrote default config to {}", config_path.display());
            Ok(())
        }
        Commands::Status => {
            let cfg = AppConfig::load_or_default(&config_path)?;
            println!(
                "INTmouse v{} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION
            );
            println!("Config: {}", config_path.display());
            println!(
                "Sharing: {} | TCP {} / UDP {}",
                cfg.sharing_enabled, cfg.network.tcp_port, cfg.network.udp_port
            );
            println!(
                "Relative mouse: {} | Monitors: {}",
                cfg.output.use_relative_mouse,
                cfg.layout.monitors.len()
            );
            println!("Defaults: TCP {DEFAULT_TCP_PORT}, UDP {DEFAULT_UDP_PORT}");
            println!("Diag log: {}", diag.log_path().display());
            if !config_path.exists() {
                warn!("No config yet — run: inputshare init-config  (or Save in GUI)");
            }
            Ok(())
        }
        Commands::Run => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let cfg = AppConfig::load_or_default(&config_path)?;
                info!(
                    "Daemon stub TCP={} UDP={} (no input hooks yet)",
                    cfg.network.tcp_port, cfg.network.udp_port
                );
                info!("Use `inputshare` / `dev.cmd` for UI+tray");
                tokio::signal::ctrl_c().await?;
                info!("Shutting down");
                Ok(())
            })
        }
    }
}
