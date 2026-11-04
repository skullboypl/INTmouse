//! Tokio hub: discovery announce/listen, TCP accept/connect, UDP input I/O,
//! file transfers over the TCP bulk link.
//!
//! Sockets per session: IPv4 TCP + UDP (unless the adapter is pinned to an
//! IPv6 literal) and, with `use_ipv6`, IPv6 TCP + UDP (`IPV6_V6ONLY`, so both
//! families can share a port). UDP input goes out on the socket matching the
//! peer address family.

use super::adapters::{list_adapters, resolve_bind_plan};
use super::addr::{acl_allows, canonical_ip, host_socket_addr, parse_host_ip, with_port};
use super::auth::{password_token, tokens_match};
use super::drag_drop::{self, PendingDrops};
use super::link_issue::{describe_link_failure, LinkIssue};
use super::files::{
    self, FileTransferStatus, IncomingFile, OutgoingFile, TransferDirection, TransferState,
    FILE_QUEUE_DEPTH, MAX_INCOMING_PER_PEER,
};
use super::framing::{read_frame, write_frame, FrameReader, HANDSHAKE_MAX_FRAME};
use super::NetConfig;
use crate::input::clipboard_full::{self, clipboard_sequence};
use crate::input::{
    self, apply_event, apply_lock_state, apply_scroll, get_clipboard_text, place_on_edge,
    set_clipboard_text, try_activate_layout, CaptureControl, CaptureOutbox,
};
use inputshare_core::RemoteSpace;
use inputshare_protocol::{
    decode_msg, encode_msg, BulkMsg, ClipboardPayload, DiscoveryMsg, InputEvent, InputMsg, InputPacket, MonitorInfo,
    PROTOCOL_VERSION,
};
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WARN_THROTTLE: Duration = Duration::from_secs(10);
/// The same journal line (e.g. "connecting to …") is written at most this often.
const EVENT_THROTTLE: Duration = Duration::from_secs(60);
/// Length of the connection journal kept for the GUI.
const MAX_EVENTS: usize = 300;
/// A discovered computer counts as present while its announce is this fresh (sent every ~2 s).
const AUTO_FRESH: Duration = Duration::from_secs(10);
/// Of two computers that find each other, the one with the smaller name dials at once; the other
/// waits this long for the first dial before dialling itself.
const AUTO_FALLBACK: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub hostname: String,
    pub addr: SocketAddr,
    pub tcp_port: u16,
    /// Protocol version it announced (a different one cannot link up).
    pub version: u16,
    pub first_seen: Instant,
    pub last_seen: Instant,
}

#[derive(Debug, Clone)]
pub struct PeerConnInfo {
    pub hostname: String,
    pub addr: SocketAddr,
    pub tcp_ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NetStatus {
    pub peers: Vec<PeerConnInfo>,
    pub discovered: Vec<DiscoveredPeer>,
    pub last_error: Option<String>,
    pub listening_tcp: Option<u16>,
    pub listening_udp: Option<u16>,
    pub controlling: Option<String>,
    pub being_controlled: bool,
    pub status_line: String,
    pub update_note: Option<String>,
    /// Actually bound TCP listeners (IPv4 and/or IPv6).
    pub tcp_listen: Vec<SocketAddr>,
    /// Actually bound UDP sockets (IPv4 and/or IPv6).
    pub udp_listen: Vec<SocketAddr>,
    /// Non-fatal network notes (adapter fallback to All, IPv6 not bound, …).
    pub warnings: Vec<String>,
    /// File transfers (active + recent history), both directions.
    pub transfers: Vec<FileTransferStatus>,
    /// Peers that could not be linked (wrong password, not on the other list, …), with the reason.
    pub link_issues: Vec<LinkIssue>,
    /// Displays reported by connected computers (kept after they disconnect).
    pub remote_monitors: Vec<RemoteMonitors>,
    /// Bumped whenever `remote_monitors` changes, so the GUI merges only when needed.
    pub monitors_rev: u64,
    /// Journal of connection events, oldest first (capped at [`MAX_EVENTS`]).
    pub events: Vec<ConnEvent>,
}

impl NetStatus {
    /// A link to the computer behind this display host is up. `host` is a tile host: the peer's
    /// name, or the bare IP of a hand-made tile.
    pub fn host_is_up(&self, host: &str) -> bool {
        self.peers.iter().filter(|p| p.tcp_ok).any(|p| {
            let ip = p.addr.ip().to_string();
            p.hostname.eq_ignore_ascii_case(host)
                || ip == host
                || self
                    .remote_monitors
                    .iter()
                    .any(|r| r.hostname.eq_ignore_ascii_case(host) && !r.ip.is_empty() && r.ip == ip)
        })
    }
}

/// One line of the connection journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnEvent {
    /// Local time, `HH:MM:SS`.
    pub at: String,
    pub text: String,
}

/// The displays another computer told us about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMonitors {
    pub hostname: String,
    /// Address the session runs over; lets an older hand-made tile under that IP be adopted.
    pub ip: String,
    pub monitors: Vec<MonitorInfo>,
}

/// What a peer sends is not trusted: cap the count, drop absurd sizes and positions, trim labels.
fn sanitize_monitors(raw: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    const MAX_MONITORS: usize = 16;
    const MAX_SIDE: u32 = 16_384;
    const MAX_COORD: i32 = 100_000;
    let mut out: Vec<MonitorInfo> = Vec::new();
    for m in raw {
        let label: String = m.label.trim().chars().take(8).collect();
        let sane = (1..=MAX_SIDE).contains(&m.width)
            && (1..=MAX_SIDE).contains(&m.height)
            && m.x.abs() <= MAX_COORD
            && m.y.abs() <= MAX_COORD
            && !label.is_empty()
            && !out.iter().any(|o| o.label == label);
        if sane && out.len() < MAX_MONITORS {
            out.push(MonitorInfo { label, ..m });
        }
    }
    out
}

/// Write side of one TCP bulk link: control frames have priority over the
/// bounded file queue (clipboard / panic never wait behind a big file).
#[derive(Clone)]
struct PeerLink {
    ctrl: mpsc::UnboundedSender<BulkMsg>,
    files: mpsc::Sender<BulkMsg>,
}

struct Shared {
    cfg: Mutex<NetConfig>,
    status: Mutex<NetStatus>,
    remote_panic: AtomicBool,
    last_remote_clipboard: Mutex<Option<String>>,
    /// The other computer's clipboard came in after the last copy made on this one.
    remote_clipboard_newer: AtomicBool,
    /// Clipboard sequence number right after this program wrote what came from the other
    /// computer: that change is not a new copy to send back.
    own_clipboard_seq: AtomicU32,
    paste_remote: AtomicBool,
    seq: AtomicU64,
    capture: CaptureControl,
    outbox: CaptureOutbox,
    /// hostname → UDP addr for input
    peer_udp: Mutex<HashMap<String, SocketAddr>>,
    /// link key (peer hostname / accepted host) → TCP write side
    bulk_tx: Mutex<HashMap<String, PeerLink>>,
    /// accepted hosts with an outbound connect in flight
    connecting: Mutex<HashSet<String>>,
    /// UDP source of the peer currently controlling us
    controller: Mutex<Option<SocketAddr>>,
    /// The computer whose steering was ended by moving this computer's own mouse: its input is
    /// dropped until it enters again.
    dismissed_controller: Mutex<Option<IpAddr>>,
    /// Runtime handle (set by [`run`]) for file jobs started from the GUI thread.
    rt: Mutex<Option<tokio::runtime::Handle>>,
    next_transfer_id: AtomicU64,
    /// outgoing transfer id → reason it must stop (receiver reject / user cancel)
    send_cancel: Mutex<HashMap<u64, String>>,
    /// Receive directory override (tests); `None` = user Downloads.
    download_dir: Option<PathBuf>,
    warn_gate: Mutex<HashMap<String, Instant>>,
    /// This computer's displays, sent to every peer on connect and whenever they change.
    local_monitors: Mutex<Vec<MonitorInfo>>,
    /// "Połącz teraz" requests from the GUI, taken by the connect loop.
    dial_requests: Mutex<Vec<(String, u16)>>,
    /// The `Enter` of the capture that just started, until the steered computer confirms it.
    enter_wait: Mutex<Option<EnterWait>>,
    /// This capture sends input over TCP: UDP to that computer did not get through.
    input_over_tcp: AtomicBool,
    /// Files dragged across the edge to this computer, waiting to be all saved.
    drops: Mutex<PendingDrops>,
    /// Files copied on another computer (Ctrl+C), on their way to this one's clipboard.
    clipboard_files: Mutex<PendingDrops>,
}

/// An `Enter` waiting for its `EnterAck`.
#[derive(Debug, Clone)]
struct EnterWait {
    host: String,
    pkt: InputPacket,
    sent_at: Instant,
    over_tcp: bool,
}

/// How long an `Enter` sent over UDP waits for its acknowledgement before it is resent over TCP.
const ENTER_ACK_UDP: Duration = Duration::from_millis(400);
/// How long the TCP resend waits before control is handed back to this computer.
const ENTER_ACK_TCP: Duration = Duration::from_millis(1200);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    fn new(
        cfg: NetConfig,
        capture: CaptureControl,
        outbox: CaptureOutbox,
        download_dir: Option<PathBuf>,
    ) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self {
            cfg: Mutex::new(cfg),
            status: Mutex::new(NetStatus::default()),
            remote_panic: AtomicBool::new(false),
            last_remote_clipboard: Mutex::new(None),
            remote_clipboard_newer: AtomicBool::new(false),
            own_clipboard_seq: AtomicU32::new(0),
            paste_remote: AtomicBool::new(false),
            seq: AtomicU64::new(1),
            capture,
            outbox,
            peer_udp: Mutex::new(HashMap::new()),
            bulk_tx: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashSet::new()),
            controller: Mutex::new(None),
            dismissed_controller: Mutex::new(None),
            rt: Mutex::new(None),
            next_transfer_id: AtomicU64::new(seed | 1),
            send_cancel: Mutex::new(HashMap::new()),
            download_dir,
            warn_gate: Mutex::new(HashMap::new()),
            local_monitors: Mutex::new(Vec::new()),
            dial_requests: Mutex::new(Vec::new()),
            enter_wait: Mutex::new(None),
            input_over_tcp: AtomicBool::new(false),
            drops: Mutex::new(PendingDrops::default()),
            clipboard_files: Mutex::new(PendingDrops::default()),
        }
    }

    fn cfg(&self) -> NetConfig {
        lock(&self.cfg).clone()
    }

    fn download_dir(&self) -> PathBuf {
        self.download_dir.clone().unwrap_or_else(downloads_dir)
    }

    /// Add a line to the connection journal (and the log).
    fn push_event(&self, text: String) {
        tracing::info!("{text}");
        let mut st = lock(&self.status);
        if st.events.len() >= MAX_EVENTS {
            st.events.remove(0);
        }
        st.events.push(ConnEvent { at: crate::clock::hms(), text });
    }

    /// Like [`Self::push_event`] but at most once per [`EVENT_THROTTLE`] for the same key.
    fn push_event_throttled(&self, key: &str, text: String) {
        {
            let mut gate = lock(&self.warn_gate);
            let now = Instant::now();
            let key = format!("ev:{key}");
            if gate.get(&key).is_some_and(|t| now.duration_since(*t) < EVENT_THROTTLE) {
                return;
            }
            gate.insert(key, now);
        }
        self.push_event(text);
    }

    fn set_local_monitors(&self, monitors: Vec<MonitorInfo>) {
        let monitors = sanitize_monitors(monitors);
        {
            let mut cur = lock(&self.local_monitors);
            if *cur == monitors {
                return;
            }
            *cur = monitors.clone();
        }
        if monitors.is_empty() {
            return;
        }
        let hostname = self.cfg().local_hostname;
        broadcast_bulk(self, BulkMsg::Monitors { hostname, monitors });
    }

    /// Remember why the link to `peer` is down (shown in the GUI) and log it, throttled.
    fn note_link_issue(&self, peer: &str, err: &str, outbound: bool) {
        let text = describe_link_failure(err, outbound);
        let changed = {
            let mut st = lock(&self.status);
            match st.link_issues.iter_mut().find(|i| i.peer == peer) {
                Some(issue) if issue.text == text => false,
                Some(issue) => {
                    issue.text = text.clone();
                    true
                }
                None => {
                    st.link_issues.push(LinkIssue { peer: peer.to_string(), text: text.clone() });
                    true
                }
            }
        };
        refresh_status_line(self);
        // A retry every few seconds fails the same way; the journal gets the news once.
        if changed {
            self.push_event(format!("{peer}: {text}"));
        }
    }

    fn clear_link_issue(&self, peer: &str) {
        let removed = {
            let mut st = lock(&self.status);
            let before = st.link_issues.len();
            st.link_issues.retain(|i| i.peer != peer);
            st.link_issues.len() != before
        };
        if removed {
            refresh_status_line(self);
        }
    }

    /// `debug!` at most once per [`WARN_THROTTLE`] for the same key: for lines that would
    /// otherwise come with every mouse packet.
    fn debug_throttled(&self, key: &str, msg: &str) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let key = format!("dbg:{key}");
        let mut gate = lock(&self.warn_gate);
        let now = Instant::now();
        if gate.get(&key).is_some_and(|t| now.duration_since(*t) < WARN_THROTTLE) {
            return;
        }
        gate.insert(key, now);
        tracing::debug!("{msg}");
    }

    /// `warn!` at most once per [`WARN_THROTTLE`] for the same key.
    fn warn_throttled(&self, key: &str, msg: &str) {
        let mut gate = lock(&self.warn_gate);
        let now = Instant::now();
        if gate
            .get(key)
            .is_some_and(|t| now.duration_since(*t) < WARN_THROTTLE)
        {
            return;
        }
        gate.insert(key.to_string(), now);
        tracing::warn!("{msg}");
    }
}

pub struct NetControl {
    shared: Arc<Shared>,
}

impl NetControl {
    pub fn new(initial: NetConfig) -> Arc<Self> {
        let capture = CaptureControl::new();
        let outbox = CaptureOutbox::default();
        let shared = Arc::new(Shared::new(initial, capture.clone(), outbox.clone(), None));
        let _ = input::spawn_capture(capture.clone(), outbox);
        let _ = input::spawn_cursor_shield(capture.clone());
        let _ = input::spawn_cursor_overlay(capture);
        Arc::new(Self { shared })
    }

    pub fn update_config(&self, cfg: NetConfig) {
        // Push output knobs into capture.
        if let Ok(mut g) = self.shared.capture.mouse_speed.lock() {
            *g = cfg.output.mouse_speed;
        }
        if let Ok(mut g) = self.shared.capture.output_scroll_v.lock() {
            *g = (
                cfg.output.vertical_scroll_speed,
                cfg.output.invert_vertical_scroll,
            );
        }
        if let Ok(mut g) = self.shared.capture.output_scroll_h.lock() {
            *g = (
                cfg.output.horizontal_scroll_speed,
                cfg.output.invert_horizontal_scroll,
            );
        }
        self.shared.capture.mouse_hz.store(cfg.output.mouse_hz, Ordering::Relaxed);
        let fast_hit = if cfg.switching.require_fast_hit {
            cfg.switching.fast_hit_speed.max(1)
        } else {
            0
        };
        self.shared.capture.fast_hit_speed.store(fast_hit, Ordering::Relaxed);
        self.shared
            .capture
            .draw_cursor
            .store(cfg.output.draw_cursor_if_invisible, Ordering::Relaxed);
        self.shared
            .capture
            .large_cursor
            .store(cfg.output.large_cursor, Ordering::Relaxed);
        input::drag_drop::set_enabled(cfg.sharing_enabled && cfg.exchange.allow_drag_drop);

        *lock(&self.shared.cfg) = cfg;
    }

    pub fn status_snapshot(&self) -> NetStatus {
        lock(&self.shared.status).clone()
    }

    /// Just the file transfers (for the progress toasts, every frame).
    pub fn transfers_snapshot(&self) -> Vec<FileTransferStatus> {
        lock(&self.shared.status).transfers.clone()
    }

    pub fn take_remote_panic(&self) -> bool {
        self.shared.remote_panic.swap(false, Ordering::SeqCst)
    }

    pub fn capture(&self) -> CaptureControl {
        self.shared.capture.clone()
    }

    pub fn outbox(&self) -> CaptureOutbox {
        self.shared.outbox.clone()
    }

    /// Edge watcher / wrap: enter remote control toward `host`.
    #[allow(dead_code)]
    pub fn request_enter_remote(&self, host: String, edge: inputshare_protocol::Edge, entry: f32) {
        self.shared.outbox.request_enter(host, edge, entry);
    }

    #[allow(dead_code)]
    pub fn request_paste_remote_clipboard(&self) {
        self.shared.paste_remote.store(true, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn last_remote_clipboard(&self) -> Option<String> {
        lock(&self.shared.last_remote_clipboard).clone()
    }

    /// Tell the connected computers which displays this one has (and every later peer too).
    /// Cheap to call every frame: nothing is sent unless the list changed.
    pub fn set_local_monitors(&self, monitors: Vec<MonitorInfo>) {
        self.shared.set_local_monitors(monitors);
    }

    /// Empty the connection journal.
    pub fn clear_events(&self) {
        lock(&self.shared.status).events.clear();
    }

    /// Dial `host:port` on the next connect tick, even if it is not on the list.
    pub fn connect_to(&self, host: String, port: u16) {
        lock(&self.shared.dial_requests).push((host, port));
    }

    pub fn set_update_note(&self, note: impl Into<String>) {
        lock(&self.shared.status).update_note = Some(note.into());
    }

    /// Local panic: tell peers + leave capture.
    pub fn broadcast_panic(&self) {
        broadcast_bulk(&self.shared, BulkMsg::Panic);
        self.shared.capture.request_leave();
        self.shared.outbox.signal_leave();
    }

    /// Send files to the current peer: the one we control, else the one
    /// controlling us, else the only connected peer. Validation errors
    /// (sharing off, `allow_drag_drop` off, no / ambiguous peer, directory,
    /// size limit) are returned immediately; progress and the final outcome
    /// appear in [`NetStatus::transfers`]. Returns the transfer ids.
    pub fn send_files(&self, paths: Vec<PathBuf>) -> anyhow::Result<Vec<u64>> {
        start_file_send(&self.shared, None, paths)
    }

    /// Like [`Self::send_files`] but to an explicit peer (hostname, accepted
    /// host string or IP of a connected peer).
    #[allow(dead_code)] // not used by the GUI yet (it lets `send_files` pick the peer)
    pub fn send_files_to(&self, host: &str, paths: Vec<PathBuf>) -> anyhow::Result<Vec<u64>> {
        start_file_send(&self.shared, Some(host), paths)
    }

    /// Stop an outgoing transfer (the receiver drops its partial file).
    /// Returns `false` if no such transfer is in progress.
    pub fn cancel_transfer(&self, transfer_id: u64) -> bool {
        cancel_outgoing(&self.shared, transfer_id, "anulowane przez użytkownika")
    }
}

pub async fn run(ctrl: Arc<NetControl>) {
    let shared = Arc::clone(&ctrl.shared);
    *lock(&shared.rt) = Some(tokio::runtime::Handle::current());
    loop {
        let cfg = shared.cfg();

        if !cfg.sharing_enabled {
            set_status_line(&shared, "Udostępnianie wyłączone".into());
            clear_listening(&shared);
            tokio::time::sleep(Duration::from_millis(400)).await;
            continue;
        }

        match run_session(Arc::clone(&shared), cfg).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("net session ended: {e:#}");
                set_error(&shared, format!("{e:#}"));
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// Aborts every task of one listener session when dropped (config change,
/// error), so old sockets are released before re-binding.
#[derive(Default)]
struct SessionTasks(Vec<JoinHandle<()>>);

impl SessionTasks {
    fn spawn(&mut self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        self.0.push(tokio::spawn(fut));
    }
}

impl Drop for SessionTasks {
    fn drop(&mut self) {
        for h in &self.0 {
            h.abort();
        }
    }
}

/// UDP sockets of a session, one per address family.
#[derive(Clone, Default)]
struct UdpSet {
    v4: Option<Arc<UdpSocket>>,
    v6: Option<Arc<UdpSocket>>,
}

impl UdpSet {
    fn socket_for(&self, addr: &SocketAddr) -> Result<&UdpSocket, String> {
        match addr {
            SocketAddr::V4(_) => self.v4.as_deref().ok_or_else(|| {
                format!("{addr}: IPv4 nie jest nasłuchiwane (adapter ustawiony na adres IPv6)")
            }),
            SocketAddr::V6(_) => self.v6.as_deref().ok_or_else(|| {
                format!(
                    "{addr}: adres IPv6, a gniazdo IPv6 nie działa — \
                     włącz IPv6 w ustawieniach sieci (lub sprawdź ostrzeżenia bindowania)"
                )
            }),
        }
    }
}

async fn run_session(shared: Arc<Shared>, cfg: NetConfig) -> anyhow::Result<()> {
    let tcp_port = cfg.network.tcp_port;
    let udp_port = cfg.network.udp_port;
    let plan = resolve_bind_plan(
        &cfg.network.preferred_adapter,
        cfg.network.use_ipv6,
        list_adapters,
    )
    .map_err(anyhow::Error::msg)?;
    let mut warnings = plan.notes.clone();

    let mut listeners: Vec<TcpListener> = Vec::new();
    let mut udp = UdpSet::default();

    if let Some(v4) = plan.v4 {
        let (tcp, udp4) = bind_v4(v4, tcp_port, udp_port, &mut warnings).await?;
        udp4.set_broadcast(true)?;
        listeners.push(tcp);
        udp.v4 = Some(Arc::new(udp4));
    }
    if let Some(v6) = plan.v6 {
        match bind_v6(v6, tcp_port, udp_port) {
            Ok((tcp, udp6)) => {
                tracing::info!("IPv6 listening on [{}] TCP {tcp_port} UDP {udp_port}", v6.ip());
                listeners.push(tcp);
                udp.v6 = Some(Arc::new(udp6));
            }
            Err(e) => {
                let msg = format!("IPv6: bind [{}] nie powiódł się: {e}", v6.ip());
                if plan.v4.is_none() {
                    anyhow::bail!(msg);
                }
                tracing::warn!("{msg}");
                warnings.push(msg);
            }
        }
    }

    {
        let mut st = lock(&shared.status);
        st.listening_tcp = Some(tcp_port);
        st.listening_udp = Some(udp_port);
        st.tcp_listen = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();
        st.udp_listen = [&udp.v4, &udp.v6]
            .into_iter()
            .flatten()
            .filter_map(|s| s.local_addr().ok())
            .collect();
        st.warnings = warnings;
        st.last_error = None;
    }
    refresh_status_line(&shared);

    let mut tasks = SessionTasks::default();
    for sock in [&udp.v4, &udp.v6].into_iter().flatten() {
        tasks.spawn(udp_recv_loop(Arc::clone(&shared), Arc::clone(sock)));
    }
    tasks.spawn(discovery_and_send_loop(Arc::clone(&shared), udp.clone()));
    tasks.spawn(outbound_connect_loop(Arc::clone(&shared)));
    tasks.spawn(clipboard_watch_loop(Arc::clone(&shared)));
    tasks.spawn(paste_and_misc_loop(Arc::clone(&shared)));

    let (acc_tx, mut acc_rx) = mpsc::channel::<(TcpStream, SocketAddr)>(16);
    for l in listeners {
        tasks.spawn(accept_loop(l, acc_tx.clone()));
    }
    drop(acc_tx);

    // Exit when config ports / adapter / IPv6 change or sharing goes off.
    let mut cfg_tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = cfg_tick.tick() => {
                let cur = shared.cfg();
                if !cur.sharing_enabled
                    || cur.network.tcp_port != tcp_port
                    || cur.network.udp_port != udp_port
                    || cur.network.preferred_adapter != cfg.network.preferred_adapter
                    || cur.network.use_ipv6 != cfg.network.use_ipv6
                {
                    tracing::info!("net config changed — restarting listeners");
                    return Ok(());
                }
                refresh_status_line(&shared);
            }
            Some((stream, peer)) = acc_rx.recv() => {
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    if let Err(e) = handle_inbound_tcp(Arc::clone(&shared), stream, peer).await {
                        tracing::debug!("inbound tcp {peer}: {e:#}");
                        shared.note_link_issue(&peer.ip().to_string(), &format!("{e:#}"), false);
                    }
                });
            }
        }
    }
}

async fn accept_loop(listener: TcpListener, tx: mpsc::Sender<(TcpStream, SocketAddr)>) {
    loop {
        match listener.accept().await {
            Ok(conn) => {
                if tx.send(conn).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!("tcp accept: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// Bind IPv4 TCP + UDP. A specific IP that fails to bind falls back to
/// `0.0.0.0` (with a warning) — same as before adapter-name support.
async fn bind_v4(
    ip: Ipv4Addr,
    tcp_port: u16,
    udp_port: u16,
    warnings: &mut Vec<String>,
) -> anyhow::Result<(TcpListener, UdpSocket)> {
    let any = Ipv4Addr::UNSPECIFIED;
    let tcp = match TcpListener::bind((ip, tcp_port)).await {
        Ok(l) => l,
        Err(e) if !ip.is_unspecified() => {
            let msg = format!("bind TCP {ip}:{tcp_port} nie powiódł się ({e}) — nasłuch na 0.0.0.0");
            tracing::warn!("{msg}");
            warnings.push(msg);
            TcpListener::bind((any, tcp_port)).await?
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("bind TCP 0.0.0.0:{tcp_port}"))),
    };
    let udp = match UdpSocket::bind((ip, udp_port)).await {
        Ok(s) => s,
        Err(e) if !ip.is_unspecified() => {
            let msg = format!("bind UDP {ip}:{udp_port} nie powiódł się ({e}) — nasłuch na 0.0.0.0");
            tracing::warn!("{msg}");
            warnings.push(msg);
            UdpSocket::bind((any, udp_port)).await?
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("bind UDP 0.0.0.0:{udp_port}"))),
    };
    Ok((tcp, udp))
}

fn bind_v6(addr: SocketAddrV6, tcp_port: u16, udp_port: u16) -> io::Result<(TcpListener, UdpSocket)> {
    let tcp = bind_tcp_v6(SocketAddrV6::new(*addr.ip(), tcp_port, 0, addr.scope_id()))?;
    let udp = bind_udp_v6(SocketAddrV6::new(*addr.ip(), udp_port, 0, addr.scope_id()))?;
    Ok((tcp, udp))
}

/// IPv6-only TCP listener (coexists with an IPv4 listener on the same port).
fn bind_tcp_v6(addr: SocketAddrV6) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    s.set_only_v6(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::V6(addr).into())?;
    s.listen(128)?;
    TcpListener::from_std(s.into())
}

/// IPv6-only UDP socket (coexists with an IPv4 socket on the same port).
fn bind_udp_v6(addr: SocketAddrV6) -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_only_v6(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::V6(addr).into())?;
    UdpSocket::from_std(s.into())
}

async fn udp_recv_loop(shared: Arc<Shared>, udp: Arc<UdpSocket>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let (n, from) = match udp.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                // Windows reports ICMP port-unreachable of an earlier send as a
                // recv error (WSAECONNRESET) — harmless for UDP, so keep the
                // pause short (this is the input hot path) and the log throttled.
                shared.warn_throttled("udp-recv", &format!("udp recv: {e}"));
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
        };
        let data = &buf[..n];

        // Try discovery first (small), then input packet.
        if let Ok(DiscoveryMsg::Announce {
            hostname,
            tcp_port,
            version,
        }) = decode_msg::<DiscoveryMsg>(data)
        {
            handle_announce(&shared, from, hostname, tcp_port, version);
            continue;
        }

        if let Ok(pkt) = decode_msg::<InputPacket>(data) {
            if pkt.version != PROTOCOL_VERSION {
                continue;
            }
            handle_input_packet(&shared, from, pkt).await;
        }
    }
}

fn handle_announce(shared: &Shared, from: SocketAddr, hostname: String, tcp_port: u16, version: u16) {
    let local = lock(&shared.cfg).local_hostname.clone();
    if hostname.eq_ignore_ascii_case(&local) {
        return;
    }
    tracing::debug!(%hostname, %from, tcp_port, version, "discovered peer");
    let mut is_new = false;
    {
        let mut st = lock(&shared.status);
        if let Some(p) = st.discovered.iter_mut().find(|p| p.hostname == hostname) {
            p.addr = from;
            p.tcp_port = tcp_port;
            p.version = version;
            p.last_seen = Instant::now();
        } else {
            is_new = true;
            st.discovered.push(DiscoveredPeer {
                hostname: hostname.clone(),
                addr: from,
                tcp_port,
                version,
                first_seen: Instant::now(),
                last_seen: Instant::now(),
            });
        }
        // Keep last 32.
        if st.discovered.len() > 32 {
            st.discovered.sort_by_key(|p| std::cmp::Reverse(p.last_seen));
            st.discovered.truncate(32);
        }
    }
    if is_new {
        let note = if version == PROTOCOL_VERSION {
            String::new()
        } else {
            format!(" — inna wersja protokołu ({version}, ten komputer ma {PROTOCOL_VERSION}), nie połączą się")
        };
        shared.push_event(format!("Wykryto komputer {hostname} ({}){note}", from.ip()));
    }
    lock(&shared.peer_udp).insert(hostname, from);
}

async fn handle_input_packet(shared: &Shared, from: SocketAddr, pkt: InputPacket) {
    let cfg = shared.cfg();
    if !cfg.sharing_enabled {
        if matches!(pkt.msg, InputMsg::Enter { .. }) {
            ack_enter(shared, from, false, Some("ma wyłączone udostępnianie (Ctrl+Shift+A albo przycisk u góry okna)."));
        }
        return;
    }

    match pkt.msg {
        InputMsg::Enter { .. } if shared.capture.capturing.load(Ordering::SeqCst) => {
            // This computer steers another one right now; it cannot be steered at the same time.
            tracing::info!(%from, "remote Enter ignored: this computer is steering");
            ack_enter(shared, from, false, Some("sam właśnie steruje innym komputerem."));
        }
        InputMsg::Events(_) if shared.capture.capturing.load(Ordering::SeqCst) => {}
        // Steering from there was ended here by moving this computer's mouse: the packets still
        // on the way must not take control again.
        InputMsg::Events(_) if *lock(&shared.dismissed_controller) == Some(from.ip()) => {}
        InputMsg::Enter { edge, entry } => {
            tracing::info!(%from, ?edge, entry, "remote Enter — being controlled");
            *lock(&shared.dismissed_controller) = None;
            shared.capture.touch_controlled();
            *lock(&shared.controller) = Some(from);
            lock(&shared.status).being_controlled = true;
            place_on_edge(edge, entry);
            ack_enter(shared, from, true, None);
            if cfg.exchange.sync_lock_keys {
                // Request locks via bulk if connected — send our state outbound on enter.
                broadcast_bulk(shared, current_lock_msg());
            }
        }
        InputMsg::Leave => {
            tracing::info!(%from, "remote Leave");
            shared
                .capture
                .being_controlled
                .store(false, Ordering::SeqCst);
            shared.capture.end_capture();
            *lock(&shared.controller) = None;
            let mut st = lock(&shared.status);
            st.being_controlled = false;
            st.controlling = None;
        }
        InputMsg::Events(events) => {
            // Events without a prior Enter (a lost packet) still count: robustness over strictness.
            shared.capture.touch_controlled();
            let (sv, iv) = shared
                .capture
                .output_scroll_v
                .lock()
                .map(|g| *g)
                .unwrap_or((0.5, false));
            let (sh, ih) = shared
                .capture
                .output_scroll_h
                .lock()
                .map(|g| *g)
                .unwrap_or((0.5, false));
            for ev in events {
                match &ev {
                    InputEvent::Scroll { dx, dy } => {
                        apply_scroll(*dx, *dy, sv, iv, sh, ih);
                    }
                    InputEvent::MouseMove { dx, dy } => {
                        apply_event(&ev);
                        if let Ok(mut pos) = shared.capture.inject_pos.lock() {
                            pos.0 = pos.0.saturating_add(*dx);
                            pos.1 = pos.1.saturating_add(*dy);
                        }
                    }
                    _ => apply_event(&ev),
                }
            }
        }
        InputMsg::Ping { nonce, sent_nanos } => {
            let reply = InputPacket {
                version: PROTOCOL_VERSION,
                seq: shared.seq.fetch_add(1, Ordering::Relaxed),
                msg: InputMsg::Pong { nonce, sent_nanos },
            };
            // Best-effort reply needs udp — skip here (handled in send loop via outbox later).
            let _ = reply;
        }
        InputMsg::Pong { .. } => {}
    }
}

async fn discovery_and_send_loop(shared: Arc<Shared>, udp: UdpSet) {
    let mut announce_every = Instant::now()
        .checked_sub(Duration::from_secs(3))
        .unwrap_or_else(Instant::now);

    loop {
        // Woken the moment the capture pushes a mouse batch (or an enter / leave), so each batch
        // goes out at once; otherwise every 50 ms for discovery and the Enter timeouts.
        shared.outbox.wait(Duration::from_millis(50)).await;
        let cfg = shared.cfg();
        if !cfg.sharing_enabled {
            continue;
        }

        // Announce ~2s (IPv4 broadcast; IPv6 has no broadcast — peers by IP).
        if cfg.clients.broadcast_udp && announce_every.elapsed() >= Duration::from_secs(2) {
            announce_every = Instant::now();
            if let Some(v4) = &udp.v4 {
                let msg = DiscoveryMsg::Announce {
                    hostname: cfg.local_hostname.clone(),
                    tcp_port: cfg.network.tcp_port,
                    version: PROTOCOL_VERSION,
                };
                if let Ok(bytes) = encode_msg(&msg) {
                    let bcast =
                        SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), cfg.network.udp_port);
                    let _ = v4.send_to(&bytes, bcast).await;
                }
            }
        }

        // Enter request from edge watcher.
        if let Some((host, edge, entry)) = shared.outbox.take_enter() {
            let tile = shared.outbox.take_enter_tile();
            let up = lock(&shared.status).host_is_up(&host);
            if !up {
                shared.push_event_throttled(
                    &format!("noup:{host}"),
                    format!("Nie przechodzę na {host}: brak połączenia z tym komputerem."),
                );
            }
            // Not while another computer steers this one: the two would hand control to each other
            // in a loop and the cursor would fly.
            if up && !shared.capture.is_controlled() && !shared.capture.capturing.load(Ordering::SeqCst) {
                shared.capture.begin_capture(&host);
                // The way back: the user pushes past the edge they came in by, or any other edge
                // that leads to this computer (a neighbour, or the monitor loop).
                *lock(&shared.capture.remote_space) =
                    RemoteSpace::enter(&cfg.layout, &host, tile.as_deref(), edge, entry).map(|space| {
                        space.with_returns(&cfg.layout, &host, &cfg.local_hostname, cfg.switching.wrap_pointer)
                    });
                lock(&shared.status).controlling = Some(host.clone());
                let pkt = InputPacket {
                    version: PROTOCOL_VERSION,
                    seq: shared.seq.fetch_add(1, Ordering::Relaxed),
                    msg: InputMsg::Enter { edge, entry },
                };
                shared.input_over_tcp.store(false, Ordering::SeqCst);
                send_input_logged(&shared, &udp, &host, &pkt).await;
                *lock(&shared.enter_wait) = Some(EnterWait {
                    host: host.clone(),
                    pkt: pkt.clone(),
                    sent_at: Instant::now(),
                    over_tcp: false,
                });
                if cfg.exchange.sync_lock_keys {
                    broadcast_bulk(&shared, current_lock_msg());
                }
                if cfg.exchange.sync_keyboard_layout {
                    broadcast_bulk(
                        &shared,
                        BulkMsg::KeyboardLayout {
                            name: current_layout_name(),
                        },
                    );
                }
                tracing::info!(%host, ?edge, "entered remote control");
            }
        }

        check_enter_wait(&shared);

        if shared.outbox.take_leave() {
            let host = lock(&shared.capture.target_host).clone();
            let pkt = InputPacket {
                version: PROTOCOL_VERSION,
                seq: shared.seq.fetch_add(1, Ordering::Relaxed),
                msg: InputMsg::Leave,
            };
            *lock(&shared.enter_wait) = None;
            shared.input_over_tcp.store(false, Ordering::SeqCst);
            if let Some(h) = &host {
                send_input_logged(&shared, &udp, h, &pkt).await;
                // Also over TCP: a Leave lost to a blocked UDP port would leave the other computer
                // believing it is still being steered.
                if let Some(ip) = target_ip(&shared, h) {
                    send_bulk_to_ip(&shared, ip, BulkMsg::Input(pkt.clone()));
                }
            } else {
                // Broadcast leave to all known UDP peers.
                let peers: Vec<_> = lock(&shared.peer_udp).keys().cloned().collect();
                for h in peers {
                    send_input_logged(&shared, &udp, &h, &pkt).await;
                }
            }
            shared.capture.end_capture();
            shared
                .capture
                .being_controlled
                .store(false, Ordering::SeqCst);
            let mut st = lock(&shared.status);
            st.controlling = None;
            st.being_controlled = false;
        }

        // Captured events → UDP.
        let events = shared.outbox.take_events();
        if !events.is_empty() {
            let target = lock(&shared.capture.target_host).clone();
            if let Some(host) = target {
                let pkt = InputPacket {
                    version: PROTOCOL_VERSION,
                    seq: shared.seq.fetch_add(1, Ordering::Relaxed),
                    msg: InputMsg::Events(events),
                };
                send_input(&shared, &udp, &host, &pkt).await;
            }
        }
    }
}

/// Input for the steered computer: over TCP when this capture found UDP blocked, else UDP.
async fn send_input(shared: &Shared, udp: &UdpSet, host: &str, pkt: &InputPacket) {
    if shared.input_over_tcp.load(Ordering::SeqCst) {
        if let Some(ip) = target_ip(shared, host) {
            if send_bulk_to_ip(shared, ip, BulkMsg::Input(pkt.clone())) {
                return;
            }
        }
    }
    send_input_logged(shared, udp, host, pkt).await;
}

/// The address of the computer behind display host `host`.
fn target_ip(shared: &Shared, host: &str) -> Option<IpAddr> {
    resolve_peer_udp(shared, host).map(|a| a.ip())
}

/// Queue `msg` on the TCP link to the computer at `ip`. `false` when no such link is up.
fn send_bulk_to_ip(shared: &Shared, ip: IpAddr, msg: BulkMsg) -> bool {
    let key = lock(&shared.status)
        .peers
        .iter()
        .find(|p| p.tcp_ok && p.addr.ip() == ip)
        .map(|p| p.hostname.clone());
    let Some(key) = key else {
        return false;
    };
    let link = lock(&shared.bulk_tx).get(&key).cloned();
    link.is_some_and(|l| l.ctrl.send(msg).is_ok())
}

/// Hand control back to this computer because the other one did not take it, and say why.
fn give_up_capture(shared: &Shared, host: &str, why: &str) {
    shared.push_event(format!("{host}: {why} Wracam do tego komputera."));
    {
        let mut st = lock(&shared.status);
        let text = format!("nie przejął sterowania: {why}");
        match st.link_issues.iter_mut().find(|i| i.peer == host) {
            Some(issue) => issue.text = text,
            None => st.link_issues.push(LinkIssue { peer: host.to_string(), text }),
        }
    }
    shared.capture.request_leave();
    shared.outbox.signal_leave();
}

/// The steered computer has a moment to confirm the `Enter`. No answer over UDP: send it again
/// over TCP and keep steering over TCP (a firewall blocking UDP 1046 is the usual reason). No
/// answer over TCP either: give control back instead of leaving the cursor trapped.
fn check_enter_wait(shared: &Shared) {
    let mut wait = lock(&shared.enter_wait);
    let Some(w) = wait.as_mut() else {
        return;
    };
    if !shared.capture.capturing.load(Ordering::SeqCst) {
        *wait = None;
        return;
    }
    let waited = w.sent_at.elapsed();
    if !w.over_tcp && waited >= ENTER_ACK_UDP {
        let resent = target_ip(shared, &w.host)
            .is_some_and(|ip| send_bulk_to_ip(shared, ip, BulkMsg::Input(w.pkt.clone())));
        if resent {
            w.over_tcp = true;
            w.sent_at = Instant::now();
            shared.input_over_tcp.store(true, Ordering::SeqCst);
            let host = w.host.clone();
            drop(wait);
            shared.push_event_throttled(
                &format!("udp-blocked:{host}"),
                format!("UDP do {host} nie dochodzi (zapora, port UDP 1046?). Sterowanie idzie przez TCP."),
            );
            return;
        }
    }
    let timed_out = if w.over_tcp { waited >= ENTER_ACK_TCP } else { waited >= ENTER_ACK_UDP };
    if timed_out {
        let host = w.host.clone();
        *wait = None;
        drop(wait);
        give_up_capture(shared, &host, "nie odpowiada na przejęcie sterowania (czy program tam działa i ma włączone udostępnianie?).");
    }
}

/// The steered computer answered our `Enter`.
fn on_enter_ack(shared: &Shared, ok: bool, reason: Option<String>) {
    let Some(w) = lock(&shared.enter_wait).take() else {
        return;
    };
    if ok {
        tracing::info!(host = %w.host, over_tcp = w.over_tcp, "steered computer took control");
        shared.clear_link_issue(&w.host);
        return;
    }
    let why = reason.unwrap_or_else(|| "odmówił przejęcia sterowania.".into());
    give_up_capture(shared, &w.host, &why);
}

/// The steered computer says a program there holds (or let go of) its cursor. Only the computer
/// steered from here sends it, so it applies to the current capture.
fn on_remote_cursor_lock(shared: &Shared, from: &str, locked: bool) {
    if !shared.capture.capturing.load(Ordering::SeqCst) {
        return;
    }
    let hold = locked && shared.cfg().switching.block_when_cursor_locked;
    let was = shared.capture.remote_cursor_locked.swap(hold, Ordering::SeqCst);
    if hold && !was {
        shared.push_event_throttled(
            &format!("cursor-lock:{from}"),
            format!("{from}: program (gra) trzyma tam kursor — powrót przez krawędź wstrzymany, Esc oddaje mysz"),
        );
    }
}

/// How often a held cursor is reported again (a new `Enter` resets it on the steering side).
const CURSOR_LOCK_RESEND: Duration = Duration::from_millis(500);

/// While another computer steers this one, tell it when a program here starts or stops holding
/// the cursor (a game's crosshair), so its way back through the edge stays shut meanwhile.
fn report_cursor_lock(shared: &Shared, last: &mut Option<(bool, Instant)>, locked: impl FnOnce() -> bool) {
    let controller = *lock(&shared.controller);
    let Some(to) = controller.filter(|_| shared.capture.is_controlled()) else {
        *last = None;
        return;
    };
    let locked = locked();
    let due = match *last {
        None => locked,
        Some((was, at)) => was != locked || (locked && at.elapsed() >= CURSOR_LOCK_RESEND),
    };
    if due {
        send_bulk_to_ip(shared, to.ip(), BulkMsg::CursorLock { locked });
        *last = Some((locked, Instant::now()));
    } else if last.is_none() {
        *last = Some((locked, Instant::now()));
    }
}

/// A refusal after the `Enter` was already taken: the steered computer's own mouse moved, so
/// the user is there. Control comes back here.
fn on_steering_refused_later(shared: &Shared, from: &str, ok: bool, reason: Option<String>) {
    if ok || !shared.capture.capturing.load(Ordering::SeqCst) {
        return;
    }
    let host = lock(&shared.capture.target_host).clone().unwrap_or_else(|| from.to_string());
    let why = reason.unwrap_or_else(|| "zakończył sterowanie.".into());
    shared.push_event(format!("{host}: {why} Wracam do tego komputera."));
    shared.capture.request_leave();
    shared.outbox.signal_leave();
}

/// How long after control arrives this computer's own mouse is not yet taken as the user coming
/// back (a bump of the desk while the cursor crosses).
const TAKE_BACK_GRACE: Duration = Duration::from_millis(500);
/// Physical mouse events within one check (50 ms) that mean someone uses this computer's mouse.
const TAKE_BACK_EVENTS: u32 = 3;

/// Watches this computer's own mouse while another computer steers it.
#[derive(Debug, Default)]
struct TakeBack {
    since: Option<Instant>,
    seen: u32,
}

impl TakeBack {
    /// `true` once this computer's own mouse moves while it is being steered.
    fn due(&mut self, controlled: bool, physical: u32, now: Instant) -> bool {
        let moved = physical.wrapping_sub(self.seen);
        self.seen = physical;
        if !controlled {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= TAKE_BACK_GRACE && moved >= TAKE_BACK_EVENTS
    }
}

/// The user moved this computer's own mouse while another computer steered it: this computer
/// is theirs again, and the other one is told to stop (its cursor goes back there).
fn take_back_control(shared: &Shared) {
    let controller = lock(&shared.controller).take();
    shared.capture.being_controlled.store(false, Ordering::SeqCst);
    lock(&shared.status).being_controlled = false;
    *lock(&shared.dismissed_controller) = controller.map(|a| a.ip());
    shared.push_event("Ruch myszy na tym komputerze: sterowanie z drugiego komputera zakończone.".into());
    if let Some(addr) = controller {
        ack_enter(shared, addr, false, Some("ktoś rusza myszą przy tamtym komputerze, więc sterowanie wraca tutaj."));
    }
}

/// Answer an `Enter` over the TCP link to the computer that sent it.
fn ack_enter(shared: &Shared, from: SocketAddr, ok: bool, reason: Option<&str>) {
    let msg = BulkMsg::EnterAck { ok, reason: reason.map(str::to_string) };
    if !send_bulk_to_ip(shared, from.ip(), msg) {
        tracing::debug!(%from, "no TCP link to acknowledge the Enter");
    }
}

async fn send_input_logged(shared: &Shared, udp: &UdpSet, host: &str, pkt: &InputPacket) {
    if let Err(e) = send_input_to(shared, udp, host, pkt).await {
        shared.warn_throttled(&format!("udp-send:{host}"), &format!("UDP input → {host}: {e}"));
    }
}

/// Send one input packet on the socket matching the peer's address family.
async fn send_input_to(
    shared: &Shared,
    udp: &UdpSet,
    host: &str,
    pkt: &InputPacket,
) -> Result<(), String> {
    let Some(addr) = resolve_peer_udp(shared, host) else {
        shared.debug_throttled(&format!("noudp:{host}"), &format!("no UDP addr for peer yet: {host}"));
        return Ok(());
    };
    let sock = udp.socket_for(&addr)?;
    let bytes = encode_msg(pkt).map_err(|e| e.to_string())?;
    sock.send_to(&bytes, addr)
        .await
        .map(|_| ())
        .map_err(|e| format!("{addr}: {e}"))
}

fn resolve_peer_udp(shared: &Shared, host: &str) -> Option<SocketAddr> {
    if let Some(a) = lock(&shared.peer_udp).get(host).copied() {
        return Some(a);
    }
    let cfg = shared.cfg();
    let udp_port = cfg.network.udp_port;
    let remember = |addr: SocketAddr| {
        lock(&shared.peer_udp).insert(host.to_string(), addr);
        Some(addr)
    };
    if let Some(addr) = host_socket_addr(host, udp_port) {
        return remember(addr);
    }
    for c in &cfg.clients.accepted {
        if c.host.eq_ignore_ascii_case(host) {
            if let Some(addr) = host_socket_addr(&c.host, udp_port) {
                return remember(addr);
            }
        }
    }
    // Last resort: first accepted IP (single-peer LAN setups).
    for c in &cfg.clients.accepted {
        if let Some(addr) = host_socket_addr(&c.host, udp_port) {
            return remember(addr);
        }
    }
    None
}

/// Why a computer is being dialled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dial {
    /// On the list in the Komputery tab.
    Listed,
    /// "Połącz teraz" in the GUI.
    Requested,
    /// Found on the LAN, same password (see [`auto_targets`]).
    Auto,
}

/// Computers announced on the LAN that this one dials without them being on the list.
///
/// Only with a password set, because the password is what tells a friend from a stranger, and
/// only while the list in Komputery is empty (a filled list means "exactly these"). Of two
/// computers that find each other, the one whose name sorts first dials at once and the other
/// waits [`AUTO_FALLBACK`], so the pair does not open two links at the same moment.
fn auto_targets(cfg: &NetConfig, discovered: &[DiscoveredPeer], now: Instant) -> Vec<(String, u16)> {
    if !cfg.clients.auto_connect
        || !cfg.network.password_required
        || cfg.network.password.is_empty()
        || !cfg.clients.accepted.is_empty()
    {
        return Vec::new();
    }
    let local = cfg.local_hostname.to_lowercase();
    discovered
        .iter()
        .filter(|p| now.duration_since(p.last_seen) <= AUTO_FRESH)
        .filter(|p| p.version == PROTOCOL_VERSION)
        .filter(|p| local < p.hostname.to_lowercase() || now.duration_since(p.first_seen) >= AUTO_FALLBACK)
        .map(|p| (p.addr.ip().to_string(), p.tcp_port))
        .collect()
}

/// Already linked to this address (an inbound link counts: its key is the peer's name).
fn peer_linked(shared: &Shared, host: &str) -> bool {
    if lock(&shared.bulk_tx).contains_key(host) {
        return true;
    }
    lock(&shared.status)
        .peers
        .iter()
        .any(|p| p.tcp_ok && p.addr.ip().to_string() == host)
}

async fn outbound_connect_loop(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        let cfg = shared.cfg();
        if !cfg.sharing_enabled {
            continue;
        }
        let mut targets: Vec<(String, u16, Dial)> = cfg
            .clients
            .accepted
            .iter()
            .map(|e| (e.host.clone(), e.tcp_port, Dial::Listed))
            .collect();
        let requested = std::mem::take(&mut *lock(&shared.dial_requests));
        for (host, port) in requested {
            if !targets.iter().any(|t| t.0 == host) {
                targets.push((host, port, Dial::Requested));
            }
        }
        let discovered = lock(&shared.status).discovered.clone();
        for (host, port) in auto_targets(&cfg, &discovered, Instant::now()) {
            if !targets.iter().any(|t| t.0 == host) {
                targets.push((host, port, Dial::Auto));
            }
        }
        let use_ipv6 = cfg.network.use_ipv6;
        for (host, port, why) in targets {
            if peer_linked(&shared, &host) {
                continue;
            }
            if !lock(&shared.connecting).insert(host.clone()) {
                continue;
            }
            match why {
                Dial::Auto => shared.push_event_throttled(
                    &format!("auto:{host}"),
                    format!("Łączę automatycznie z {host}:{port} (to samo hasło)"),
                ),
                Dial::Requested => shared.push_event(format!("Łączę z {host}:{port}")),
                Dial::Listed => {}
            }
            let shared2 = Arc::clone(&shared);
            tokio::spawn(async move {
                match connect_targets(&host, port, use_ipv6).await {
                    Ok(targets) => match TcpStream::connect(&targets[..]).await {
                        Ok(stream) => {
                            if let Err(e) =
                                handle_outbound_tcp(Arc::clone(&shared2), stream, host.clone()).await
                            {
                                tracing::debug!("outbound {host}: {e:#}");
                                shared2.note_link_issue(&host, &format!("{e:#}"), true);
                            }
                        }
                        Err(e) => {
                            tracing::trace!("connect {host}:{port}: {e}");
                            shared2.note_link_issue(&host, &e.to_string(), true);
                        }
                    },
                    Err(ConnectTargetError::Config(msg)) => {
                        shared2.warn_throttled(&format!("connect:{host}"), &msg);
                    }
                    Err(ConnectTargetError::Resolve(msg)) => tracing::debug!("{msg}"),
                }
                lock(&shared2.connecting).remove(&host);
            });
        }
    }
}

#[derive(Debug)]
enum ConnectTargetError {
    /// Settings make the host unreachable (IPv6 peer with IPv6 off).
    Config(String),
    /// DNS failure / no usable address right now.
    Resolve(String),
}

/// TCP addresses to try for an accepted host (IPv4 first).
async fn connect_targets(
    host: &str,
    port: u16,
    use_ipv6: bool,
) -> Result<Vec<SocketAddr>, ConnectTargetError> {
    if let Some(addr) = host_socket_addr(host, port) {
        if addr.is_ipv6() && !use_ipv6 {
            return Err(ConnectTargetError::Config(format!(
                "peer {host} ma adres IPv6, a IPv6 jest wyłączone — włącz IPv6 w ustawieniach sieci"
            )));
        }
        return Ok(vec![addr]);
    }
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ConnectTargetError::Resolve(format!("DNS {host}: {e}")))?;
    let mut addrs: Vec<SocketAddr> = resolved.filter(|a| use_ipv6 || a.is_ipv4()).collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    if addrs.is_empty() {
        return Err(ConnectTargetError::Resolve(format!(
            "{host}: brak adresu {}",
            if use_ipv6 { "IP" } else { "IPv4 (IPv6 wyłączone)" }
        )));
    }
    Ok(addrs)
}

async fn handle_inbound_tcp(
    shared: Arc<Shared>,
    mut stream: TcpStream,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let cfg = shared.cfg();

    // ACL: accepted list empty → allow any with valid password; else IP must match.
    if !acl_allows(&cfg.clients.accepted, peer.ip()) {
        anyhow::bail!("rejected {peer}: not in accepted list");
    }

    let hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream, HANDSHAKE_MAX_FRAME))
        .await
        .map_err(|_| anyhow::anyhow!("handshake timeout"))??;
    let (hostname, token) = match hello {
        BulkMsg::Hello {
            version,
            hostname,
            auth_token,
        } => {
            if version != PROTOCOL_VERSION {
                let _ = write_frame(
                    &mut stream,
                    &BulkMsg::HelloAck {
                        ok: false,
                        reason: Some(format!(
                            "version mismatch: peer={version} us={PROTOCOL_VERSION}"
                        )),
                        password_required: cfg.network.password_required,
                    },
                )
                .await;
                anyhow::bail!("version mismatch");
            }
            (hostname, auth_token)
        }
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };

    let expected = password_token(&cfg.network.password, cfg.network.password_required);
    if !tokens_match(&expected, &token, cfg.network.password_required) {
        let _ = write_frame(
            &mut stream,
            &BulkMsg::HelloAck {
                ok: false,
                reason: Some("auth failed".into()),
                password_required: cfg.network.password_required,
            },
        )
        .await;
        anyhow::bail!("auth failed from {hostname}");
    }

    write_frame(
        &mut stream,
        &BulkMsg::HelloAck {
            ok: true,
            reason: None,
            password_required: cfg.network.password_required,
        },
    )
    .await?;

    // Register peer UDP guess (same IP incl. IPv6 scope, our udp port).
    lock(&shared.peer_udp).insert(hostname.clone(), with_port(peer, cfg.network.udp_port));
    shared.clear_link_issue(&peer.ip().to_string());

    run_bulk_session(shared, stream, hostname, peer).await
}

async fn handle_outbound_tcp(
    shared: Arc<Shared>,
    mut stream: TcpStream,
    host: String,
) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let cfg = shared.cfg();
    let token = password_token(&cfg.network.password, cfg.network.password_required);
    write_frame(
        &mut stream,
        &BulkMsg::Hello {
            version: PROTOCOL_VERSION,
            hostname: cfg.local_hostname.clone(),
            auth_token: token,
        },
    )
    .await?;
    let ack = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream, HANDSHAKE_MAX_FRAME))
        .await
        .map_err(|_| anyhow::anyhow!("handshake timeout"))??;
    match ack {
        BulkMsg::HelloAck {
            ok: true,
            password_required,
            ..
        } => {
            // Our password proves nothing to a computer that does not ask for one.
            if cfg.network.password_required && !password_required {
                anyhow::bail!("peer password off");
            }
        }
        BulkMsg::HelloAck {
            ok: false, reason, ..
        } => anyhow::bail!("HelloAck denied: {reason:?}"),
        other => anyhow::bail!("expected HelloAck, got {other:?}"),
    }
    let peer = stream.peer_addr()?;
    lock(&shared.peer_udp).insert(host.clone(), with_port(peer, cfg.network.udp_port));
    shared.clear_link_issue(&host);
    run_bulk_session(shared, stream, host, peer).await
}

async fn run_bulk_session(
    shared: Arc<Shared>,
    stream: TcpStream,
    hostname: String,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<BulkMsg>();
    let (file_tx, mut file_rx) = mpsc::channel::<BulkMsg>(FILE_QUEUE_DEPTH);
    lock(&shared.bulk_tx).insert(
        hostname.clone(),
        PeerLink {
            ctrl: ctrl_tx.clone(),
            files: file_tx,
        },
    );
    register_peer_status(&shared, &hostname, peer, true);

    // Introduce our displays; the peer does the same, so both layouts fill in on their own.
    let ours = lock(&shared.local_monitors).clone();
    if !ours.is_empty() {
        let _ = ctrl_tx.send(BulkMsg::Monitors {
            hostname: shared.cfg().local_hostname,
            monitors: ours,
        });
    }

    let host_w = hostname.clone();
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                biased;
                m = ctrl_rx.recv() => match m {
                    Some(m) => m,
                    None => break,
                },
                Some(m) = file_rx.recv() => m,
            };
            if let Err(e) = write_frame(&mut wr, &msg).await {
                tracing::debug!("bulk write → {host_w}: {e:#}");
                break;
            }
        }
    });

    let mut reader = FrameReader::new();
    let mut incoming: HashMap<u64, IncomingFile> = HashMap::new();
    let result = loop {
        match reader.next(&mut rd).await {
            Ok(Some(msg)) => handle_bulk_msg(&shared, &hostname, &ctrl_tx, msg, &mut incoming).await,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };

    writer.abort();
    let was_current = {
        let mut map = lock(&shared.bulk_tx);
        let ours = map
            .get(&hostname)
            .is_some_and(|l| l.ctrl.same_channel(&ctrl_tx));
        if ours {
            map.remove(&hostname);
        }
        ours
    };
    for (id, inc) in incoming.drain() {
        inc.discard();
        fail_transfer(&shared, TransferDirection::Receive, &hostname, id, "połączenie zerwane");
    }
    if was_current {
        fail_outgoing_for_peer(&shared, &hostname);
    }
    register_peer_status(&shared, &hostname, peer, false);
    peer_gone(&shared, peer.ip());
    result
}

/// A link to a computer ended. If that was the computer this one steers (or the one steering this
/// one) nobody may be left holding the cursor.
fn peer_gone(shared: &Shared, ip: IpAddr) {
    if lock(&shared.status).peers.iter().any(|p| p.tcp_ok && p.addr.ip() == ip) {
        return; // another link to the same computer is still up
    }
    let target = lock(&shared.capture.target_host).clone();
    let steering_it = shared.capture.capturing.load(Ordering::SeqCst)
        && target
            .and_then(|h| resolve_peer_udp(shared, &h))
            .is_some_and(|a| a.ip() == ip);
    if steering_it {
        shared.push_event(format!("Połączenie z {ip} zerwane: wracam do tego komputera."));
        shared.capture.request_leave();
        shared.outbox.signal_leave();
    }
    let steered_by_it = lock(&shared.controller).is_some_and(|a| a.ip() == ip);
    if steered_by_it {
        *lock(&shared.controller) = None;
        shared.capture.being_controlled.store(false, Ordering::SeqCst);
        let mut st = lock(&shared.status);
        st.being_controlled = false;
    }
}

async fn handle_bulk_msg(
    shared: &Shared,
    from: &str,
    reply: &mpsc::UnboundedSender<BulkMsg>,
    msg: BulkMsg,
    incoming: &mut HashMap<u64, IncomingFile>,
) {
    let cfg = shared.cfg();
    match msg {
        BulkMsg::ClipboardText { text } => {
            *lock(&shared.last_remote_clipboard) = Some(text.clone());
            shared.remote_clipboard_newer.store(true, Ordering::SeqCst);
            let _ = set_clipboard_text(&text);
            shared.own_clipboard_seq.store(clipboard_sequence(), Ordering::SeqCst);
            tracing::debug!(from, len = text.len(), "clipboard received");
        }
        BulkMsg::Clipboard(payload) => receive_full_clipboard(shared, from, payload).await,
        BulkMsg::ClipboardRequest => {
            if let Some(t) = get_clipboard_text() {
                let _ = reply.send(BulkMsg::ClipboardText { text: t });
            }
        }
        BulkMsg::LockState { caps, num, scroll } => {
            if cfg.exchange.sync_lock_keys {
                apply_lock_state(caps, num, scroll);
            }
        }
        BulkMsg::KeyboardLayout { name } => {
            if cfg.exchange.sync_keyboard_layout {
                try_activate_layout(&name);
            }
        }
        BulkMsg::Panic => {
            tracing::warn!(from, "remote Panic");
            shared.remote_panic.store(true, Ordering::SeqCst);
            shared.capture.request_leave();
            shared.outbox.signal_leave();
            shared
                .capture
                .being_controlled
                .store(false, Ordering::SeqCst);
        }
        BulkMsg::FileOffer {
            name,
            size,
            transfer_id,
        } => on_file_offer(shared, &cfg, from, reply, incoming, name, size, transfer_id),
        BulkMsg::FileChunk {
            transfer_id,
            offset,
            data,
        } => {
            let Some(inc) = incoming.get_mut(&transfer_id) else {
                // Rejected / cancelled earlier — sender will stop on our FileResult.
                tracing::trace!(from, transfer_id, "chunk for unknown transfer");
                return;
            };
            match inc.write_chunk(offset, data).await {
                Ok(()) => {
                    let received = inc.received;
                    with_transfer(shared, TransferDirection::Receive, from, transfer_id, |t| {
                        t.transferred = received;
                    });
                }
                Err(e) => {
                    if let Some(inc) = incoming.remove(&transfer_id) {
                        inc.discard();
                    }
                    receive_failed(shared, from, reply, transfer_id, e);
                }
            }
        }
        BulkMsg::FileDone { transfer_id } => {
            let Some(inc) = incoming.remove(&transfer_id) else {
                return;
            };
            match inc.finish().await {
                Ok(path) => {
                    tracing::info!(from, "saved file → {}", path.display());
                    with_transfer(shared, TransferDirection::Receive, from, transfer_id, |t| {
                        t.transferred = t.size;
                        t.state = TransferState::Done;
                        t.saved_to = Some(path.clone());
                    });
                    let _ = reply.send(BulkMsg::FileResult {
                        transfer_id,
                        ok: true,
                        reason: None,
                    });
                    drop_part_ended(shared, from, transfer_id, Some(path));
                }
                Err(e) => receive_failed(shared, from, reply, transfer_id, e),
            }
        }
        BulkMsg::FileCancel {
            transfer_id,
            reason,
        } => {
            if let Some(inc) = incoming.remove(&transfer_id) {
                inc.discard();
                tracing::info!(from, transfer_id, %reason, "incoming file cancelled by sender");
                fail_transfer(
                    shared,
                    TransferDirection::Receive,
                    from,
                    transfer_id,
                    &format!("anulowane przez nadawcę: {reason}"),
                );
                drop_part_ended(shared, from, transfer_id, None);
            }
        }
        BulkMsg::FileResult {
            transfer_id,
            ok,
            reason,
        } => on_file_result(shared, from, transfer_id, ok, reason),
        BulkMsg::Monitors { hostname, monitors } => store_remote_monitors(shared, from, hostname, monitors),
        BulkMsg::EnterAck { ok, reason } => {
            if lock(&shared.enter_wait).is_none() {
                on_steering_refused_later(shared, from, ok, reason);
            } else {
                on_enter_ack(shared, ok, reason);
            }
        }
        BulkMsg::CursorLock { locked } => on_remote_cursor_lock(shared, from, locked),
        BulkMsg::ClipboardFiles { transfer_ids } => {
            if cfg.exchange.full_clipboard {
                shared.push_event(format!("{from}: skopiowano pliki ({}), odbieram do schowka…", transfer_ids.len()));
                lock(&shared.clipboard_files).expect(from, transfer_ids, Instant::now());
            }
        }
        BulkMsg::DragDrop { transfer_ids } => {
            shared.push_event(format!("{from}: przeciągnięto pliki ({}), odbieram…", transfer_ids.len()));
            lock(&shared.drops).expect(from, transfer_ids, Instant::now());
        }
        BulkMsg::Input(pkt) => {
            // Same handling as a UDP packet; replies go back by the source address's IP.
            let addr = lock(&shared.peer_udp).get(from).copied();
            match addr {
                Some(addr) if pkt.version == PROTOCOL_VERSION => handle_input_packet(shared, addr, pkt).await,
                _ => shared.debug_throttled(
                    &format!("tcpin:{from}"),
                    &format!("TCP input from {from} ignored (unknown peer address or version)"),
                ),
            }
        }
        BulkMsg::Hello { .. } | BulkMsg::HelloAck { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn on_file_offer(
    shared: &Shared,
    cfg: &NetConfig,
    from: &str,
    reply: &mpsc::UnboundedSender<BulkMsg>,
    incoming: &mut HashMap<u64, IncomingFile>,
    name: String,
    size: u64,
    transfer_id: u64,
) {
    let reject = |reason: String| {
        tracing::info!(from, transfer_id, %name, "FileOffer rejected: {reason}");
        let _ = reply.send(BulkMsg::FileResult {
            transfer_id,
            ok: false,
            reason: Some(reason),
        });
        drop_part_ended(shared, from, transfer_id, None);
    };
    // Files copied there for this clipboard come in with "full clipboard" alone.
    let for_clipboard = cfg.exchange.full_clipboard && lock(&shared.clipboard_files).expects(from, transfer_id);
    if !cfg.exchange.allow_drag_drop && !for_clipboard {
        reject("odbiorca ma wyłączone przesyłanie plików (allow_drag_drop)".into());
        return;
    }
    if incoming.contains_key(&transfer_id) {
        reject("transfer o tym id już trwa".into());
        return;
    }
    if incoming.len() >= MAX_INCOMING_PER_PEER {
        reject(format!(
            "za dużo równoległych transferów (max {MAX_INCOMING_PER_PEER})"
        ));
        return;
    }
    let limit = files::max_file_bytes(cfg.network.max_file_size_mb);
    match IncomingFile::open(&shared.download_dir(), &name, size, transfer_id, limit) {
        Ok(inc) => {
            tracing::info!(from, transfer_id, size, "FileOffer {name:?} → {:?}", inc.name);
            push_transfer_status(
                shared,
                FileTransferStatus {
                    transfer_id,
                    direction: TransferDirection::Receive,
                    peer: from.to_string(),
                    name: inc.name.clone(),
                    size,
                    transferred: 0,
                    state: TransferState::InProgress,
                    saved_to: None,
                },
            );
            incoming.insert(transfer_id, inc);
        }
        Err(e) => reject(e),
    }
}

fn receive_failed(
    shared: &Shared,
    from: &str,
    reply: &mpsc::UnboundedSender<BulkMsg>,
    transfer_id: u64,
    reason: String,
) {
    tracing::warn!(from, transfer_id, "incoming file failed: {reason}");
    fail_transfer(shared, TransferDirection::Receive, from, transfer_id, &reason);
    let _ = reply.send(BulkMsg::FileResult {
        transfer_id,
        ok: false,
        reason: Some(reason),
    });
    drop_part_ended(shared, from, transfer_id, None);
}

/// One received file ended (saved to `saved`, or failed). When it was the last one of a drag
/// across the edge, the saved files are dropped under the cursor, or left in Downloads.
fn drop_part_ended(shared: &Shared, from: &str, transfer_id: u64, saved: Option<PathBuf>) {
    if lock(&shared.clipboard_files).expects(from, transfer_id) {
        clipboard_file_ended(shared, from, transfer_id, saved);
        return;
    }
    let Some(done) = lock(&shared.drops).settle(from, transfer_id, saved, Instant::now()) else {
        return;
    };
    let failed = if done.failed > 0 { format!(", {} nie dotarło", done.failed) } else { String::new() };
    if done.saved.is_empty() {
        shared.push_event(format!("{from}: przeciągnięte pliki nie dotarły{failed}"));
    } else if drag_drop::drop_at_cursor(&done, shared.capture.is_controlled()) {
        shared.push_event(format!("{from}: upuszczam pliki ({}) pod kursorem{failed}", done.saved.len()));
        input::drag_drop::drop_at_cursor(done.saved);
    } else {
        shared.push_event(format!("{from}: przeciągnięte pliki ({}) są w Pobranych{failed}", done.saved.len()));
    }
}

/// One file copied on `from` arrived (or failed). When it was the last of that copy, the saved
/// files go on this computer's clipboard: Ctrl+V in Explorer pastes them.
fn clipboard_file_ended(shared: &Shared, from: &str, transfer_id: u64, saved: Option<PathBuf>) {
    let Some(done) = lock(&shared.clipboard_files).settle(from, transfer_id, saved, Instant::now()) else {
        return;
    };
    let failed = if done.failed > 0 { format!(", {} nie dotarło", done.failed) } else { String::new() };
    if done.saved.is_empty() {
        shared.push_event(format!("{from}: skopiowane pliki nie dotarły{failed}"));
        return;
    }
    if clipboard_full::write_clipboard_all(&ClipboardPayload::default(), &done.saved) {
        shared.own_clipboard_seq.store(clipboard_sequence(), Ordering::SeqCst);
        shared.remote_clipboard_newer.store(true, Ordering::SeqCst);
        shared.push_event(format!("{from}: pliki ({}) są w schowku, Ctrl+V wkleja{failed}", done.saved.len()));
    } else {
        shared.push_event(format!("{from}: pliki ({}) są w Pobranych (schowek zajęty){failed}", done.saved.len()));
    }
}

/// Send files copied here (Ctrl+C) to every linked computer, as transfers with progress.
fn send_clipboard_files(shared: &Arc<Shared>, files: Vec<PathBuf>) {
    let hosts: Vec<String> = lock(&shared.bulk_tx).keys().cloned().collect();
    for host in hosts {
        if let Err(e) = start_send(shared, Some(&host), files.clone(), SendKind::Clipboard) {
            shared.warn_throttled(&format!("clip-files:{host}"), &format!("Pliki ze schowka → {host}: {e}"));
        }
    }
}

fn on_file_result(shared: &Shared, from: &str, transfer_id: u64, ok: bool, reason: Option<String>) {
    let mut stop_sender = None;
    with_transfer(shared, TransferDirection::Send, from, transfer_id, |t| {
        if ok {
            t.transferred = t.size;
            t.state = TransferState::Done;
        } else {
            let r = format!(
                "odrzucone przez odbiorcę: {}",
                reason.as_deref().unwrap_or("bez powodu")
            );
            if t.state == TransferState::InProgress {
                stop_sender = Some(r.clone());
            }
            t.state = TransferState::Failed(r);
        }
    });
    if let Some(r) = stop_sender {
        lock(&shared.send_cancel).insert(transfer_id, r);
    }
    tracing::info!(from, transfer_id, ok, ?reason, "file transfer result");
}

// ─── file send ───────────────────────────────────────────────────────────────

fn start_file_send(
    shared: &Arc<Shared>,
    host: Option<&str>,
    paths: Vec<PathBuf>,
) -> anyhow::Result<Vec<u64>> {
    start_send(shared, host, paths, SendKind::Plain)
}

/// Files dragged across the edge to `host` and let go there.
fn start_drag_drop_send(shared: &Arc<Shared>, host: &str, paths: Vec<PathBuf>) -> anyhow::Result<Vec<u64>> {
    start_send(shared, Some(host), paths, SendKind::Drop)
}

/// What a batch of sent files is for on the other computer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SendKind {
    /// Saved to Downloads.
    Plain,
    /// Announced with `DragDrop`: dropped under the receiver's cursor.
    Drop,
    /// Announced with `ClipboardFiles`: put on the receiver's clipboard.
    Clipboard,
}

fn start_send(
    shared: &Arc<Shared>,
    host: Option<&str>,
    paths: Vec<PathBuf>,
    kind: SendKind,
) -> anyhow::Result<Vec<u64>> {
    let as_drop = kind == SendKind::Drop;
    let cfg = shared.cfg();
    if !cfg.sharing_enabled {
        anyhow::bail!("udostępnianie jest wyłączone");
    }
    let allowed = match kind {
        SendKind::Clipboard => cfg.exchange.full_clipboard,
        _ => cfg.exchange.allow_drag_drop,
    };
    if !allowed {
        anyhow::bail!("przesyłanie plików jest wyłączone (allow_drag_drop)");
    }
    if paths.is_empty() {
        anyhow::bail!("brak plików do wysłania");
    }
    let rt = lock(&shared.rt)
        .clone()
        .ok_or_else(|| anyhow::anyhow!("warstwa sieciowa nie jest uruchomiona"))?;
    let peer = match host {
        Some(h) => resolve_bulk_key(shared, h)
            .ok_or_else(|| anyhow::anyhow!("brak połączenia TCP z {h}"))?,
        None => pick_file_target(shared)?,
    };
    let link = lock(&shared.bulk_tx)
        .get(&peer)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("brak połączenia TCP z {peer}"))?;
    let tx = link.files;

    let limit = files::max_file_bytes(cfg.network.max_file_size_mb);
    let jobs = paths
        .into_iter()
        .map(|p| {
            let id = shared.next_transfer_id.fetch_add(1, Ordering::Relaxed);
            OutgoingFile::prepare(p, id, limit)
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(anyhow::Error::msg)?;

    let ids: Vec<u64> = jobs.iter().map(|j| j.transfer_id).collect();
    // Control frames go out before queued file frames, so the receiver knows of the drop before
    // the first offer.
    let announce = match kind {
        SendKind::Plain => None,
        SendKind::Drop => Some(BulkMsg::DragDrop { transfer_ids: ids.clone() }),
        SendKind::Clipboard => Some(BulkMsg::ClipboardFiles { transfer_ids: ids.clone() }),
    };
    if let Some(msg) = announce {
        if link.ctrl.send(msg).is_err() {
            anyhow::bail!("brak połączenia TCP z {peer}");
        }
    }
    for job in &jobs {
        push_transfer_status(
            shared,
            FileTransferStatus {
                transfer_id: job.transfer_id,
                direction: TransferDirection::Send,
                peer: peer.clone(),
                name: job.name.clone(),
                size: job.size,
                transferred: 0,
                state: TransferState::InProgress,
                saved_to: None,
            },
        );
    }
    tracing::info!(%peer, files = jobs.len(), as_drop, "sending files");
    let shared2 = Arc::clone(shared);
    rt.spawn_blocking(move || run_send_jobs(&shared2, &peer, &tx, jobs));
    Ok(ids)
}

/// Files let go over the cursor shield while steering: they go to that computer, to be dropped
/// under its cursor.
fn send_dropped(shared: &Arc<Shared>, d: input::drag_drop::DroppedFiles) {
    if !d.skipped.is_empty() {
        shared.push_event(format!("Foldery nie są przesyłane, pominięto: {}", d.skipped.join(", ")));
    }
    match start_drag_drop_send(shared, &d.host, d.files) {
        Ok(ids) => shared.push_event(format!("{}: przeciągam pliki ({})", d.host, ids.len())),
        Err(e) => shared.push_event(format!("{}: przeciągnięte pliki nie zostały wysłane: {e:#}", d.host)),
    }
}

/// Blocking thread: send queued files one by one.
fn run_send_jobs(shared: &Shared, peer: &str, tx: &mpsc::Sender<BulkMsg>, jobs: Vec<OutgoingFile>) {
    for job in jobs {
        let id = job.transfer_id;
        let res = files::send_file_blocking(
            tx,
            &job,
            || lock(&shared.send_cancel).get(&id).cloned(),
            |sent| {
                with_transfer(shared, TransferDirection::Send, peer, id, |t| {
                    t.transferred = sent;
                })
            },
        );
        lock(&shared.send_cancel).remove(&id);
        match res {
            Ok(()) => with_transfer(shared, TransferDirection::Send, peer, id, |t| {
                if t.state == TransferState::InProgress {
                    t.state = TransferState::AwaitingConfirm;
                }
            }),
            Err(e) => {
                tracing::warn!(peer, id, "file send failed: {e}");
                fail_transfer(shared, TransferDirection::Send, peer, id, &e);
            }
        }
    }
}

fn cancel_outgoing(shared: &Shared, transfer_id: u64, reason: &str) -> bool {
    let active = lock(&shared.status).transfers.iter().any(|t| {
        t.direction == TransferDirection::Send
            && t.transfer_id == transfer_id
            && t.state == TransferState::InProgress
    });
    if active {
        lock(&shared.send_cancel).insert(transfer_id, reason.to_string());
    }
    active
}

/// Peer for `send_files`: controlled peer, else controller, else the only link.
fn pick_file_target(shared: &Shared) -> anyhow::Result<String> {
    if shared.capture.capturing.load(Ordering::SeqCst) {
        if let Some(h) = lock(&shared.capture.target_host).clone() {
            return resolve_bulk_key(shared, &h)
                .ok_or_else(|| anyhow::anyhow!("sterujesz {h}, ale brak połączenia TCP z nim"));
        }
    }
    if shared.capture.being_controlled.load(Ordering::SeqCst) {
        if let Some(addr) = *lock(&shared.controller) {
            if let Some(k) = bulk_key_for_ip(shared, addr.ip()) {
                return Ok(k);
            }
        }
    }
    let keys: Vec<String> = lock(&shared.bulk_tx).keys().cloned().collect();
    match keys.len() {
        0 => anyhow::bail!("brak połączonego peera"),
        1 => Ok(keys.into_iter().next().unwrap_or_default()),
        _ => anyhow::bail!(
            "połączono z kilkoma peerami ({}) — wskaż odbiorcę",
            keys.join(", ")
        ),
    }
}

/// Map a layout / settings host to the key of its live TCP link.
fn resolve_bulk_key(shared: &Shared, host: &str) -> Option<String> {
    let keys: Vec<String> = lock(&shared.bulk_tx).keys().cloned().collect();
    if keys.iter().any(|k| k == host) {
        return Some(host.to_string());
    }
    if let Some(k) = keys.iter().find(|k| k.eq_ignore_ascii_case(host)) {
        return Some(k.clone());
    }
    let ip = parse_host_ip(host)
        .map(|(ip, _)| ip)
        .or_else(|| lock(&shared.peer_udp).get(host).map(|a| a.ip()))?;
    bulk_key_for_ip(shared, ip)
}

fn bulk_key_for_ip(shared: &Shared, ip: IpAddr) -> Option<String> {
    let ip = canonical_ip(ip);
    let candidates: Vec<String> = lock(&shared.status)
        .peers
        .iter()
        .filter(|p| p.tcp_ok && canonical_ip(p.addr.ip()) == ip)
        .map(|p| p.hostname.clone())
        .collect();
    let map = lock(&shared.bulk_tx);
    candidates.into_iter().find(|k| map.contains_key(k))
}

// ─── transfer status helpers ─────────────────────────────────────────────────

fn push_transfer_status(shared: &Shared, t: FileTransferStatus) {
    files::push_transfer(&mut lock(&shared.status).transfers, t);
}

fn with_transfer(
    shared: &Shared,
    dir: TransferDirection,
    peer: &str,
    id: u64,
    f: impl FnOnce(&mut FileTransferStatus),
) {
    let mut st = lock(&shared.status);
    if let Some(t) = files::find_transfer(&mut st.transfers, dir, peer, id) {
        f(t);
    }
}

fn fail_transfer(shared: &Shared, dir: TransferDirection, peer: &str, id: u64, reason: &str) {
    with_transfer(shared, dir, peer, id, |t| {
        if !t.state.is_finished() {
            t.state = TransferState::Failed(reason.to_string());
        }
    });
}

/// Link to `peer` closed: unfinished outgoing transfers can no longer complete.
fn fail_outgoing_for_peer(shared: &Shared, peer: &str) {
    let mut stopped = Vec::new();
    {
        let mut st = lock(&shared.status);
        for t in st
            .transfers
            .iter_mut()
            .filter(|t| t.direction == TransferDirection::Send && t.peer == peer)
        {
            if !t.state.is_finished() {
                if t.state == TransferState::InProgress {
                    stopped.push(t.transfer_id);
                }
                t.state = TransferState::Failed("połączenie zerwane".into());
            }
        }
    }
    let mut cancel = lock(&shared.send_cancel);
    for id in stopped {
        cancel.insert(id, "połączenie zerwane".into());
    }
}

// ─── misc loops ──────────────────────────────────────────────────────────────

/// The other computer sent its whole clipboard. With "full clipboard" off here only its text
/// is taken, as before.
async fn receive_full_clipboard(shared: &Shared, from: &str, payload: ClipboardPayload) {
    if let Some(text) = &payload.text {
        *lock(&shared.last_remote_clipboard) = Some(text.clone());
    }
    shared.remote_clipboard_newer.store(true, Ordering::SeqCst);
    if !shared.cfg().exchange.full_clipboard {
        if let Some(text) = &payload.text {
            let _ = set_clipboard_text(text);
            shared.own_clipboard_seq.store(clipboard_sequence(), Ordering::SeqCst);
        }
        return;
    }
    let skipped = payload.skipped_files.len();
    let files = payload.files.len();
    if skipped > 0 {
        shared.push_event_throttled(
            &format!("clip-skip:{from}"),
            format!("Schowek z {from}: pominięto {skipped} — foldery albo pliki ponad 2 GB; prześlij je przez Wymianę plików"),
        );
    }
    // Only skipped files: nothing to put on the clipboard (and it must not be emptied).
    if payload.is_empty() {
        return;
    }
    let written = tokio::task::spawn_blocking(move || {
        let paths = clipboard_full::save_files(&clipboard_full::clipboard_files_root(), &payload.files)
            .map_err(|e| format!("nie zapisano plików ze schowka: {e}"))?;
        if clipboard_full::write_clipboard_all(&payload, &paths) {
            Ok(clipboard_sequence())
        } else {
            Err("schowek zajęty przez inny program".to_string())
        }
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match written {
        Ok(seq) => {
            shared.own_clipboard_seq.store(seq, Ordering::SeqCst);
            tracing::debug!(from, files, "full clipboard received");
        }
        Err(e) => shared.warn_throttled(&format!("clip-in:{from}"), &format!("Schowek z {from}: {e}")),
    }
}

async fn clipboard_watch_loop(shared: Arc<Shared>) {
    let mut last = String::new();
    let mut last_seq = clipboard_sequence();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        if !shared.cfg().sharing_enabled {
            continue;
        }
        if shared.cfg().exchange.full_clipboard {
            let seq = clipboard_sequence();
            if seq == last_seq {
                continue;
            }
            last_seq = seq;
            if seq == shared.own_clipboard_seq.load(Ordering::SeqCst) {
                continue;
            }
            let Ok(Some(copy)) = tokio::task::spawn_blocking(clipboard_full::read_clipboard_copy).await else {
                continue;
            };
            let clipboard_full::ClipboardCopy { payload, files } = copy;
            if payload.is_empty() && files.is_empty() {
                continue;
            }
            if let Some(t) = &payload.text {
                last = t.clone();
            }
            shared.remote_clipboard_newer.store(false, Ordering::SeqCst);
            if !payload.is_empty() || !payload.skipped_files.is_empty() {
                broadcast_bulk(&shared, BulkMsg::Clipboard(payload));
            }
            if !files.is_empty() {
                send_clipboard_files(&shared, files);
            }
            continue;
        }
        // Always sync clipboard over bulk when connected (exchange path).
        let Some(text) = get_clipboard_text() else {
            continue;
        };
        if text == last || text.is_empty() {
            continue;
        }
        // Avoid echo loops: if we just set from remote, skip once.
        let remote = lock(&shared.last_remote_clipboard).clone();
        if remote.as_ref() == Some(&text) {
            last = text;
            continue;
        }
        last = text.clone();
        shared.remote_clipboard_newer.store(false, Ordering::SeqCst);
        broadcast_bulk(&shared, BulkMsg::ClipboardText { text });
    }
}

async fn paste_and_misc_loop(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut hk_prev = false;
    let mut cursor_lock_sent = None;
    let mut take_back = TakeBack::default();
    loop {
        tick.tick().await;
        if take_back.due(shared.capture.is_controlled(), input::key_hook::physical_mouse_events(), Instant::now()) {
            take_back_control(&shared);
        }
        report_cursor_lock(&shared, &mut cursor_lock_sent, crate::win_query::cursor_locked);
        for dropped in input::drag_drop::take_dropped() {
            send_dropped(&shared, dropped);
        }
        if shared.paste_remote.swap(false, Ordering::SeqCst) {
            if let Some(t) = lock(&shared.last_remote_clipboard).clone() {
                let _ = input::simulate_paste(&t);
            }
        }

        // Hotkey for insert_remote_clipboard.
        let cfg = shared.cfg();
        if let Some(hk) = &cfg.exchange.insert_remote_clipboard_hotkey {
            let down = crate::hotkeys::KeyCombo::parse(hk)
                .map(|c| c.is_down())
                .unwrap_or(false);
            // Only when the other computer copied last: a copy made here afterwards must not be
            // replaced by an older text from there (Ctrl+C then Ctrl+V on this computer).
            // With the full clipboard it already holds everything from there; setting the text
            // again would drop a picture or files.
            if down && !hk_prev && !cfg.exchange.full_clipboard && shared.remote_clipboard_newer.load(Ordering::SeqCst) {
                if let Some(t) = lock(&shared.last_remote_clipboard).clone() {
                    let _ = set_clipboard_text(&t);
                }
            }
            hk_prev = down;
        } else {
            hk_prev = false;
        }
    }
}

/// Remember the displays a peer reported (`from` is the session key, i.e. the key in `peer_udp`).
fn store_remote_monitors(shared: &Shared, from: &str, hostname: String, monitors: Vec<MonitorInfo>) {
    let hostname = hostname.trim().to_string();
    let monitors = sanitize_monitors(monitors);
    if hostname.is_empty() || monitors.is_empty() {
        return;
    }
    if hostname.eq_ignore_ascii_case(&shared.cfg().local_hostname) {
        shared.warn_throttled(
            &format!("samename:{from}"),
            &format!("{from} ma tę samą nazwę komputera ({hostname}) co ten komputer. Zmień nazwę jednego z nich, żeby układ monitorów się wypełnił."),
        );
        return;
    }
    // Input to this computer is addressed by name later on, so make the name resolvable now.
    let addr = lock(&shared.peer_udp).get(from).copied();
    if let Some(addr) = addr {
        lock(&shared.peer_udp).insert(hostname.clone(), addr);
    }
    let ip = addr.map(|a| a.ip().to_string()).unwrap_or_default();
    let entry = RemoteMonitors { hostname: hostname.clone(), ip, monitors };
    let mut st = lock(&shared.status);
    match st.remote_monitors.iter_mut().find(|r| r.hostname.eq_ignore_ascii_case(&hostname)) {
        Some(cur) if *cur == entry => {}
        Some(cur) => {
            *cur = entry;
            st.monitors_rev += 1;
        }
        None => {
            st.remote_monitors.push(entry);
            st.monitors_rev += 1;
        }
    }
}

fn broadcast_bulk(shared: &Shared, msg: BulkMsg) {
    for link in lock(&shared.bulk_tx).values() {
        let _ = link.ctrl.send(msg.clone());
    }
}

fn register_peer_status(shared: &Shared, hostname: &str, addr: SocketAddr, tcp_ok: bool) {
    {
        let mut st = lock(&shared.status);
        if let Some(p) = st.peers.iter_mut().find(|p| p.hostname == hostname) {
            p.tcp_ok = tcp_ok;
            p.addr = addr;
        } else if tcp_ok {
            st.peers.push(PeerConnInfo {
                hostname: hostname.to_string(),
                addr,
                tcp_ok,
            });
        }
        if !tcp_ok {
            st.peers.retain(|p| p.tcp_ok);
        }
    }
    refresh_status_line(shared);
    if tcp_ok {
        shared.push_event(format!("Połączono z {hostname} ({})", addr.ip()));
    } else {
        shared.push_event(format!("Rozłączono z {hostname}"));
    }
}

fn refresh_status_line(shared: &Shared) {
    let mut st = lock(&shared.status);
    let peers: Vec<_> = st
        .peers
        .iter()
        .filter(|p| p.tcp_ok)
        .map(|p| p.hostname.clone())
        .collect();
    let tcp = st.listening_tcp;
    let udp = st.listening_udp;
    let issue = st.link_issues.first().map(|i| format!("{}: {}", i.peer, i.text));
    st.status_line = if !peers.is_empty() {
        format!("Połączono: {}", peers.join(", "))
    } else if let (Some(t), Some(u)) = (tcp, udp) {
        match issue {
            Some(issue) => format!("Nasłuch TCP {t} UDP {u} · brak połączenia, {issue}"),
            None => format!("Nasłuch TCP {t} UDP {u}"),
        }
    } else {
        "Sieć: start…".into()
    };
    if let Some(c) = &st.controlling {
        st.status_line = format!("{} · sterujesz: {c}", st.status_line);
    } else if st.being_controlled {
        st.status_line = format!("{} · zdalne sterowanie", st.status_line);
    }
}

fn set_status_line(shared: &Shared, line: String) {
    lock(&shared.status).status_line = line;
}

fn set_error(shared: &Shared, err: String) {
    lock(&shared.status).last_error = Some(err);
}

fn clear_listening(shared: &Shared) {
    let mut st = lock(&shared.status);
    st.listening_tcp = None;
    st.listening_udp = None;
    st.tcp_listen.clear();
    st.udp_listen.clear();
    st.peers.clear();
}

fn current_lock_msg() -> BulkMsg {
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
        unsafe {
            BulkMsg::LockState {
                caps: (GetKeyState(0x14) as u16) & 1 != 0,
                num: (GetKeyState(0x90) as u16) & 1 != 0,
                scroll: (GetKeyState(0x91) as u16) & 1 != 0,
            }
        }
    }
    #[cfg(not(windows))]
    {
        BulkMsg::LockState {
            caps: false,
            num: false,
            scroll: false,
        }
    }
}

fn current_layout_name() -> String {
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
        let hkl = unsafe { GetKeyboardLayout(0) };
        format!("{:08X}", hkl.0 as u32)
    }
    #[cfg(not(windows))]
    {
        "unknown".into()
    }
}

fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
