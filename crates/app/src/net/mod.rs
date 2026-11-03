//! Network hub: UDP discovery + input, TCP bulk (clipboard / files / handshake).
//!
//! Runs a Tokio runtime on a dedicated thread. GUI holds [`NetControl`] (Arc)
//! and pushes config via [`NetControl::update_config`].

mod adapters;
mod addr;
mod auth;
mod drag_drop;
pub(crate) mod files;
mod framing;
mod hub;
mod link_issue;

pub use adapters::{list_adapters, NetAdapter};
pub use files::{FileTransferStatus, TransferDirection, TransferState};
pub use hub::{NetControl, NetStatus, RemoteMonitors};

use inputshare_core::{
    ClientsConfig, ExchangeConfig, MonitorLayout, NetworkConfig, OutputConfig, SwitchingConfig,
};
use std::sync::Arc;

/// Snapshot of settings the hub needs (cloned from GUI on each sync).
#[derive(Debug, Clone)]
pub struct NetConfig {
    pub sharing_enabled: bool,
    pub network: NetworkConfig,
    pub clients: ClientsConfig,
    pub exchange: ExchangeConfig,
    pub output: OutputConfig,
    pub switching: SwitchingConfig,
    #[allow(dead_code)]
    pub layout: MonitorLayout,
    pub local_hostname: String,
}

impl NetConfig {
    pub fn from_app(cfg: &inputshare_core::AppConfig) -> Self {
        Self {
            sharing_enabled: cfg.sharing_enabled,
            network: cfg.network.clone(),
            clients: cfg.clients.clone(),
            exchange: cfg.exchange.clone(),
            output: cfg.output.clone(),
            switching: cfg.switching.clone(),
            layout: cfg.layout.clone(),
            local_hostname: inputshare_core::local_hostname(),
        }
    }
}

/// Spawn the hub thread; returns shared control handle.
pub fn spawn(initial: NetConfig) -> Arc<NetControl> {
    let ctrl = NetControl::new(initial);
    let c = Arc::clone(&ctrl);
    std::thread::Builder::new()
        .name("inputshare-net".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("inputshare-tokio")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("tokio runtime failed: {e}");
                    return;
                }
            };
            rt.block_on(hub::run(c));
        })
        .expect("spawn net thread");
    ctrl
}
