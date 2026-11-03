//! Loopback integration tests for the hub (TCP handshake / ACL / file
//! transfer, IPv6 UDP + TCP). Only loopback sockets on ephemeral ports are
//! used, and no input packet is ever *applied* (that would move the real
//! cursor) — UDP tests only send / decode.

use super::*;
use inputshare_core::{ClientEntry, ExchangeConfig, NetworkConfig};
use std::net::Ipv6Addr;
use std::path::Path;

const WAIT: Duration = Duration::from_secs(15);

fn net_cfg(host: &str) -> NetConfig {
    NetConfig {
        sharing_enabled: true,
        network: NetworkConfig::default(),
        clients: Default::default(),
        exchange: ExchangeConfig {
            allow_drag_drop: true,
            ..Default::default()
        },
        output: Default::default(),
        switching: Default::default(),
        layout: Default::default(),
        local_hostname: host.into(),
    }
}

fn shared_with(cfg: NetConfig, dir: Option<PathBuf>) -> Arc<Shared> {
    let s = Arc::new(Shared::new(
        cfg,
        CaptureControl::new(),
        CaptureOutbox::default(),
        dir,
    ));
    *lock(&s.rt) = Some(tokio::runtime::Handle::current());
    s
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "inputshare-hubtest-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < WAIT, "timeout waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn transfer(s: &Shared, dir: TransferDirection, id: Option<u64>) -> Option<FileTransferStatus> {
    lock(&s.status)
        .transfers
        .iter()
        .find(|t| t.direction == dir && id.is_none_or(|i| t.transfer_id == i))
        .cloned()
}

/// Accept one connection on `listener` and run the inbound handler.
fn serve_one(
    shared: Arc<Shared>,
    listener: TcpListener,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let (stream, peer) = listener.accept().await?;
        handle_inbound_tcp(shared, stream, peer).await
    })
}

async fn listen_v4() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

/// Full pair: `client` dials `server` through the real handshake.
async fn connect_pair(
    server: &Arc<Shared>,
    client: &Arc<Shared>,
    client_key: &str,
) -> (JoinHandle<anyhow::Result<()>>, JoinHandle<anyhow::Result<()>>) {
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(server), l);
    let stream = TcpStream::connect(addr).await.unwrap();
    let cli = tokio::spawn(handle_outbound_tcp(
        Arc::clone(client),
        stream,
        client_key.to_string(),
    ));
    (srv, cli)
}

async fn join(h: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    tokio::time::timeout(WAIT, h).await.expect("task timeout").expect("task panicked")
}

/// Raw peer: handshake by hand, then speak BulkMsg directly.
async fn raw_client(addr: SocketAddr, token: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).await.unwrap();
    write_frame(
        &mut s,
        &BulkMsg::Hello {
            version: PROTOCOL_VERSION,
            hostname: "raw-peer".into(),
            auth_token: token.into(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut s, HANDSHAKE_MAX_FRAME).await.unwrap() {
        BulkMsg::HelloAck { ok: true, .. } => s,
        other => panic!("handshake failed: {other:?}"),
    }
}

async fn next_result(rd: &mut FrameReader, s: &mut TcpStream) -> (u64, bool, Option<String>) {
    loop {
        let msg = tokio::time::timeout(WAIT, rd.next(s))
            .await
            .expect("no FileResult")
            .unwrap()
            .expect("EOF before FileResult");
        if let BulkMsg::FileResult {
            transfer_id,
            ok,
            reason,
        } = msg
        {
            return (transfer_id, ok, reason);
        }
    }
}

// ─── handshake / auth / ACL ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_with_matching_password() {
    let mut sc = net_cfg("server-pc");
    sc.network.password_required = true;
    sc.network.password = "secret".into();
    let mut cc = net_cfg("client-pc");
    cc.network.password_required = true;
    cc.network.password = "secret".into();
    let server = shared_with(sc, None);
    let client = shared_with(cc, None);

    let (srv, cli) = connect_pair(&server, &client, "127.0.0.1").await;
    wait_until("both links registered", || {
        lock(&server.bulk_tx).contains_key("client-pc")
            && lock(&client.bulk_tx).contains_key("127.0.0.1")
    })
    .await;
    assert!(lock(&server.status)
        .peers
        .iter()
        .any(|p| p.hostname == "client-pc" && p.tcp_ok));
    // Server learned the client's UDP endpoint (same IP, configured UDP port).
    let udp = lock(&server.peer_udp).get("client-pc").copied().unwrap();
    assert_eq!(udp.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(udp.port(), inputshare_protocol::DEFAULT_UDP_PORT);
    srv.abort();
    cli.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_rejects_wrong_password() {
    let mut sc = net_cfg("server-pc");
    sc.network.password_required = true;
    sc.network.password = "secret".into();
    let mut cc = net_cfg("client-pc");
    cc.network.password_required = true;
    cc.network.password = "wrong".into();
    let (srv, cli) = connect_pair(&shared_with(sc, None), &shared_with(cc, None), "s").await;
    let e = join(srv).await.unwrap_err().to_string();
    assert!(e.contains("auth failed"), "{e}");
    let e = join(cli).await.unwrap_err().to_string();
    assert!(e.contains("denied"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_rejects_missing_password() {
    let mut sc = net_cfg("server-pc");
    sc.network.password_required = true;
    sc.network.password = "secret".into();
    // Client does not require a password → sends an empty token.
    let cc = net_cfg("client-pc");
    let (srv, cli) = connect_pair(&shared_with(sc, None), &shared_with(cc, None), "s").await;
    assert!(join(srv).await.is_err());
    assert!(join(cli).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_without_password_requirement() {
    // Neither side asks for a password: the link comes up as before.
    let server = shared_with(net_cfg("server-pc"), None);
    let client = shared_with(net_cfg("client-pc"), None);
    let (srv, cli) = connect_pair(&server, &client, "s").await;
    wait_until("link up", || lock(&server.bulk_tx).contains_key("client-pc")).await;
    srv.abort();
    cli.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_with_a_password_refuses_a_server_without_one() {
    // The server would accept anybody, so our password proves nothing to it: do not link.
    let server = shared_with(net_cfg("server-pc"), None);
    let mut cc = net_cfg("client-pc");
    cc.network.password_required = true;
    cc.network.password = "anything".into();
    let client = shared_with(cc, None);
    let (srv, cli) = connect_pair(&server, &client, "s").await;
    let e = join(cli).await.unwrap_err().to_string();
    assert!(e.contains("peer password off"), "{e}");
    srv.abort();
    assert!(!lock(&client.bulk_tx).contains_key("s"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_mismatch_is_refused_with_reason() {
    let server = shared_with(net_cfg("server-pc"), None);
    let (l, addr) = listen_v4().await;
    let srv = serve_one(server, l);
    let mut s = TcpStream::connect(addr).await.unwrap();
    write_frame(
        &mut s,
        &BulkMsg::Hello {
            version: PROTOCOL_VERSION - 1,
            hostname: "old".into(),
            auth_token: String::new(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut s, HANDSHAKE_MAX_FRAME).await.unwrap() {
        BulkMsg::HelloAck { ok: false, reason, .. } => {
            assert!(reason.unwrap().contains("version mismatch"))
        }
        other => panic!("{other:?}"),
    }
    assert!(join(srv).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_pre_auth_frame_is_refused() {
    let server = shared_with(net_cfg("server-pc"), None);
    let (l, addr) = listen_v4().await;
    let srv = serve_one(server, l);
    let mut s = TcpStream::connect(addr).await.unwrap();
    use tokio::io::AsyncWriteExt;
    s.write_all(&(32u32 * 1024 * 1024).to_le_bytes()).await.unwrap();
    let e = join(srv).await.unwrap_err().to_string();
    assert!(e.contains("too large"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acl_non_empty_rejects_unlisted_ip() {
    let mut sc = net_cfg("server-pc");
    sc.clients.accepted = vec![ClientEntry {
        host: "10.9.9.9".into(),
        tcp_port: 6555,
    }];
    let (srv, cli) =
        connect_pair(&shared_with(sc, None), &shared_with(net_cfg("c"), None), "s").await;
    let e = join(srv).await.unwrap_err().to_string();
    assert!(e.contains("not in accepted list"), "{e}");
    assert!(join(cli).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acl_non_empty_accepts_listed_ip() {
    let mut sc = net_cfg("server-pc");
    sc.clients.accepted = vec![ClientEntry {
        host: "127.0.0.1".into(),
        tcp_port: 6555,
    }];
    let server = shared_with(sc, None);
    let (srv, cli) = connect_pair(&server, &shared_with(net_cfg("c"), None), "s").await;
    wait_until("link up", || lock(&server.bulk_tx).contains_key("c")).await;
    srv.abort();
    cli.abort();
}

// ─── file transfer ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_roundtrip_over_loopback() {
    let recv_dir = temp_dir("recv");
    let src_dir = temp_dir("src");
    let server = shared_with(net_cfg("server-pc"), Some(recv_dir.clone()));
    let client = shared_with(net_cfg("client-pc"), None);
    let (srv, cli) = connect_pair(&server, &client, "127.0.0.1").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("127.0.0.1")).await;

    // Several chunks + a partial tail.
    let content: Vec<u8> = (0..(3 * inputshare_protocol::FILE_CHUNK_SIZE + 12_345))
        .map(|i| (i * 31 % 251) as u8)
        .collect();
    let src = src_dir.join("raport końcowy.bin");
    std::fs::write(&src, &content).unwrap();
    let empty = src_dir.join("empty.txt");
    std::fs::write(&empty, b"").unwrap();

    // No explicit target: the only connected peer is picked.
    let ids = start_file_send(&client, None, vec![src.clone(), empty.clone()]).unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);

    for id in &ids {
        wait_until("sender Done", || {
            transfer(&client, TransferDirection::Send, Some(*id))
                .is_some_and(|t| t.state == TransferState::Done)
        })
        .await;
    }
    let sent = transfer(&client, TransferDirection::Send, Some(ids[0])).unwrap();
    assert_eq!(sent.transferred, content.len() as u64);
    assert_eq!(sent.peer, "127.0.0.1");

    let got = transfer(&server, TransferDirection::Receive, Some(ids[0])).unwrap();
    assert_eq!(got.state, TransferState::Done);
    assert_eq!(got.peer, "client-pc");
    assert_eq!(got.saved_to.as_deref(), Some(recv_dir.join("raport końcowy.bin").as_path()));
    assert_eq!(std::fs::read(recv_dir.join("raport końcowy.bin")).unwrap(), content);
    assert_eq!(std::fs::read(recv_dir.join("empty.txt")).unwrap(), b"");
    // No .part leftovers.
    assert_eq!(dir_names(&recv_dir), vec!["empty.txt", "raport końcowy.bin"]);

    // Same name again → not overwritten.
    let ids2 = start_file_send(&client, Some("127.0.0.1"), vec![empty]).unwrap();
    wait_until("second Done", || {
        transfer(&client, TransferDirection::Send, Some(ids2[0]))
            .is_some_and(|t| t.state == TransferState::Done)
    })
    .await;
    assert!(recv_dir.join("empty (1).txt").exists());

    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}

fn has_event(s: &Shared, needle: &str) -> bool {
    lock(&s.status).events.iter().any(|e| e.text.contains(needle))
}

/// A drag across the edge arrives as a drop; a receiver nobody steers (as here) keeps the files in
/// its download folder and never starts a drag of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dragged_files_arrive_and_stay_in_downloads_when_not_steered() {
    let recv_dir = temp_dir("recv-drop");
    let src_dir = temp_dir("src-drop");
    let server = shared_with(net_cfg("server-pc"), Some(recv_dir.clone()));
    let client = shared_with(net_cfg("client-pc"), None);
    let (srv, cli) = connect_pair(&server, &client, "srv").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("srv")).await;

    let a = src_dir.join("zdjęcie.png");
    std::fs::write(&a, vec![5u8; inputshare_protocol::FILE_CHUNK_SIZE + 7]).unwrap();
    let b = src_dir.join("notatka.txt");
    std::fs::write(&b, b"abc").unwrap();
    let ids = start_drag_drop_send(&client, "srv", vec![a, b]).unwrap();
    assert_eq!(ids.len(), 2);

    wait_until("drop settled", || has_event(&server, "są w Pobranych")).await;
    assert!(has_event(&server, "przeciągnięto pliki (2)"));
    assert!(has_event(&server, "przeciągnięte pliki (2) są w Pobranych"));
    assert_eq!(dir_names(&recv_dir), vec!["notatka.txt", "zdjęcie.png"]);

    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_refused_by_the_receiver_says_nothing_arrived() {
    let recv_dir = temp_dir("recv-drop-off");
    let src_dir = temp_dir("src-drop-off");
    let mut sc = net_cfg("server-pc");
    sc.exchange.allow_drag_drop = false;
    let server = shared_with(sc, Some(recv_dir.clone()));
    let client = shared_with(net_cfg("client-pc"), None);
    let (srv, cli) = connect_pair(&server, &client, "srv").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("srv")).await;

    let a = src_dir.join("a.png");
    std::fs::write(&a, b"x").unwrap();
    let ids = start_drag_drop_send(&client, "srv", vec![a]).unwrap();
    wait_until("drop settled", || has_event(&server, "przeciągnięte pliki nie dotarły")).await;
    wait_until("sender Failed", || {
        transfer(&client, TransferDirection::Send, Some(ids[0]))
            .is_some_and(|t| matches!(t.state, TransferState::Failed(_)))
    })
    .await;
    assert!(dir_names(&recv_dir).is_empty());

    // With drag & drop off here the drop is not even sent.
    let mut off = net_cfg("client-pc");
    off.exchange.allow_drag_drop = false;
    *lock(&client.cfg) = off;
    assert!(start_drag_drop_send(&client, "srv", vec![src_dir.join("a.png")]).is_err());

    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receiver_with_drag_drop_off_rejects() {
    let recv_dir = temp_dir("recv-off");
    let src_dir = temp_dir("src-off");
    let mut sc = net_cfg("server-pc");
    sc.exchange.allow_drag_drop = false;
    let server = shared_with(sc, Some(recv_dir.clone()));
    let client = shared_with(net_cfg("client-pc"), None);
    let (srv, cli) = connect_pair(&server, &client, "srv").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("srv")).await;

    let src = src_dir.join("big.bin");
    std::fs::write(&src, vec![7u8; 2 * inputshare_protocol::FILE_CHUNK_SIZE]).unwrap();
    let ids = start_file_send(&client, None, vec![src]).unwrap();
    wait_until("sender Failed", || {
        transfer(&client, TransferDirection::Send, Some(ids[0]))
            .is_some_and(|t| matches!(t.state, TransferState::Failed(_)))
    })
    .await;
    match transfer(&client, TransferDirection::Send, Some(ids[0])).unwrap().state {
        TransferState::Failed(r) => assert!(r.contains("allow_drag_drop"), "{r}"),
        s => panic!("{s:?}"),
    }
    assert!(dir_names(&recv_dir).is_empty());
    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_side_validation() {
    let src_dir = temp_dir("validate");
    let f = src_dir.join("a.txt");
    std::fs::write(&f, b"abc").unwrap();

    // No peer connected.
    let client = shared_with(net_cfg("client-pc"), None);
    let e = start_file_send(&client, None, vec![f.clone()]).unwrap_err().to_string();
    assert!(e.contains("brak połączonego peera"), "{e}");

    // allow_drag_drop off on the sender.
    let mut cc = net_cfg("client-pc");
    cc.exchange.allow_drag_drop = false;
    let e = start_file_send(&shared_with(cc, None), None, vec![f.clone()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("allow_drag_drop"), "{e}");

    // Two peers → ambiguous; directories and oversize refused.
    let s1 = shared_with(net_cfg("s1"), None);
    let s2 = shared_with(net_cfg("s2"), None);
    let mut cc = net_cfg("client-pc");
    cc.network.max_file_size_mb = 1;
    let client = shared_with(cc, None);
    let (a1, b1) = connect_pair(&s1, &client, "peer-a").await;
    let (a2, b2) = connect_pair(&s2, &client, "peer-b").await;
    wait_until("two links", || lock(&client.bulk_tx).len() == 2).await;
    let e = start_file_send(&client, None, vec![f.clone()]).unwrap_err().to_string();
    assert!(e.contains("kilkoma"), "{e}");
    let e = start_file_send(&client, Some("peer-a"), vec![src_dir.clone()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("katalog"), "{e}");
    let big = src_dir.join("big.bin");
    std::fs::write(&big, vec![0u8; 1024 * 1024 + 1]).unwrap();
    let e = start_file_send(&client, Some("PEER-B"), vec![big]).unwrap_err().to_string();
    assert!(e.contains("za duży"), "{e}");
    assert!(start_file_send(&client, Some("nobody"), vec![f]).is_err());

    for h in [a1, b1, a2, b2] {
        h.abort();
    }
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn traversal_names_are_saved_inside_download_dir() {
    let root = temp_dir("traversal");
    let recv_dir = root.join("downloads");
    let server = shared_with(net_cfg("server-pc"), Some(recv_dir.clone()));
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(&server), l);
    let mut s = raw_client(addr, "").await;
    let mut rd = FrameReader::new();

    let names = [
        "..\\evil1.txt",
        "../evil2.txt",
        "..\\..\\..\\Windows\\evil3.txt",
        "C:\\evil4.txt",
        "..",
    ];
    for (i, name) in names.iter().enumerate() {
        let id = 100 + i as u64;
        for msg in [
            BulkMsg::FileOffer {
                name: (*name).into(),
                size: 3,
                transfer_id: id,
            },
            BulkMsg::FileChunk {
                transfer_id: id,
                offset: 0,
                data: b"pwn".to_vec(),
            },
            BulkMsg::FileDone { transfer_id: id },
        ] {
            write_frame(&mut s, &msg).await.unwrap();
        }
        let (rid, ok, reason) = next_result(&mut rd, &mut s).await;
        assert_eq!(rid, id);
        assert!(ok, "{name}: {reason:?}");
    }
    assert_eq!(
        dir_names(&recv_dir),
        vec!["evil1.txt", "evil2.txt", "evil3.txt", "evil4.txt", "plik_104"]
    );
    // Nothing escaped into the parent.
    assert_eq!(dir_names(&root), vec!["downloads"]);
    drop(s);
    let _ = join(srv).await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_enforces_size_limit_and_declared_size() {
    let recv_dir = temp_dir("limit");
    let mut sc = net_cfg("server-pc");
    sc.network.max_file_size_mb = 1;
    let server = shared_with(sc, Some(recv_dir.clone()));
    let (l, addr) = listen_v4().await;
    let srv = serve_one(server, l);
    let mut s = raw_client(addr, "").await;
    let mut rd = FrameReader::new();

    write_frame(
        &mut s,
        &BulkMsg::FileOffer {
            name: "huge.iso".into(),
            size: 2 * 1024 * 1024,
            transfer_id: 1,
        },
    )
    .await
    .unwrap();
    let (id, ok, reason) = next_result(&mut rd, &mut s).await;
    assert_eq!((id, ok), (1, false));
    assert!(reason.unwrap().contains("za duży"));

    // Sender lies about the size: more bytes than announced → failure, no file.
    for msg in [
        BulkMsg::FileOffer {
            name: "liar.bin".into(),
            size: 2,
            transfer_id: 2,
        },
        BulkMsg::FileChunk {
            transfer_id: 2,
            offset: 0,
            data: vec![1, 2, 3],
        },
    ] {
        write_frame(&mut s, &msg).await.unwrap();
    }
    let (id, ok, _) = next_result(&mut rd, &mut s).await;
    assert_eq!((id, ok), (2, false));
    assert!(dir_names(&recv_dir).is_empty());
    drop(s);
    let _ = join(srv).await;
    let _ = std::fs::remove_dir_all(&recv_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_connection_removes_partial_file() {
    let recv_dir = temp_dir("drop");
    let server = shared_with(net_cfg("server-pc"), Some(recv_dir.clone()));
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(&server), l);
    let mut s = raw_client(addr, "").await;
    for msg in [
        BulkMsg::FileOffer {
            name: "half.bin".into(),
            size: 10,
            transfer_id: 5,
        },
        BulkMsg::FileChunk {
            transfer_id: 5,
            offset: 0,
            data: vec![0; 4],
        },
    ] {
        write_frame(&mut s, &msg).await.unwrap();
    }
    wait_until("chunk received", || {
        transfer(&server, TransferDirection::Receive, Some(5)).is_some_and(|t| t.transferred == 4)
    })
    .await;
    drop(s);
    join(srv).await.unwrap();
    match transfer(&server, TransferDirection::Receive, Some(5)).unwrap().state {
        TransferState::Failed(r) => assert!(r.contains("zerwane"), "{r}"),
        st => panic!("{st:?}"),
    }
    assert!(dir_names(&recv_dir).is_empty());
    let _ = std::fs::remove_dir_all(&recv_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_cancel_discards_partial_on_receiver() {
    let recv_dir = temp_dir("cancel");
    let server = shared_with(net_cfg("server-pc"), Some(recv_dir.clone()));
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(&server), l);
    let mut s = raw_client(addr, "").await;
    for msg in [
        BulkMsg::FileOffer {
            name: "c.bin".into(),
            size: 10,
            transfer_id: 9,
        },
        BulkMsg::FileChunk {
            transfer_id: 9,
            offset: 0,
            data: vec![0; 4],
        },
        BulkMsg::FileCancel {
            transfer_id: 9,
            reason: "user".into(),
        },
    ] {
        write_frame(&mut s, &msg).await.unwrap();
    }
    wait_until("cancelled", || {
        transfer(&server, TransferDirection::Receive, Some(9))
            .is_some_and(|t| matches!(t.state, TransferState::Failed(_)))
    })
    .await;
    assert!(dir_names(&recv_dir).is_empty());
    drop(s);
    let _ = join(srv).await;
    let _ = std::fs::remove_dir_all(&recv_dir);
}

// ─── IPv6 ────────────────────────────────────────────────────────────────────

fn v6_loopback(port: u16) -> SocketAddrV6 {
    SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_over_ipv6_with_acl_and_file() {
    let recv_dir = temp_dir("v6");
    let src_dir = temp_dir("v6src");
    let mut sc = net_cfg("server-pc");
    sc.clients.accepted = vec![ClientEntry {
        host: "::1".into(),
        tcp_port: 6555,
    }];
    let server = shared_with(sc, Some(recv_dir.clone()));
    let mut cc = net_cfg("client-pc");
    cc.network.use_ipv6 = true;
    let client = shared_with(cc, None);

    let listener = bind_tcp_v6(v6_loopback(0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = serve_one(Arc::clone(&server), listener);

    let targets = connect_targets("::1", port, true).await.unwrap();
    assert_eq!(targets, vec![SocketAddr::V6(v6_loopback(port))]);
    let stream = TcpStream::connect(&targets[..]).await.unwrap();
    let cli = tokio::spawn(handle_outbound_tcp(Arc::clone(&client), stream, "::1".into()));
    wait_until("v6 link up", || lock(&client.bulk_tx).contains_key("::1")).await;
    assert!(lock(&client.peer_udp).get("::1").unwrap().is_ipv6());

    let f = src_dir.join("v6.txt");
    std::fs::write(&f, b"hello over v6").unwrap();
    let ids = start_file_send(&client, Some("::1"), vec![f]).unwrap();
    wait_until("v6 file Done", || {
        transfer(&client, TransferDirection::Send, Some(ids[0]))
            .is_some_and(|t| t.state == TransferState::Done)
    })
    .await;
    assert_eq!(std::fs::read(recv_dir.join("v6.txt")).unwrap(), b"hello over v6");

    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test]
async fn ipv6_peer_is_refused_when_ipv6_disabled() {
    match connect_targets("::1", 6555, false).await {
        Err(ConnectTargetError::Config(m)) => assert!(m.contains("IPv6"), "{m}"),
        other => panic!("{other:?}"),
    }
    match connect_targets("[fe80::1%3]", 6555, true).await {
        Ok(v) => assert!(matches!(v[0], SocketAddr::V6(a) if a.scope_id() == 3)),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_input_goes_out_on_ipv6_socket() {
    let shared = shared_with(net_cfg("me"), None);
    let tx6 = Arc::new(bind_udp_v6(v6_loopback(0)).unwrap());
    let rx6 = bind_udp_v6(v6_loopback(0)).unwrap();
    let rx_addr = rx6.local_addr().unwrap();
    lock(&shared.peer_udp).insert("peer6".into(), rx_addr);

    let pkt = InputPacket {
        version: PROTOCOL_VERSION,
        seq: 42,
        msg: InputMsg::Leave,
    };
    let set = UdpSet {
        v4: None,
        v6: Some(Arc::clone(&tx6)),
    };
    send_input_to(&shared, &set, "peer6", &pkt).await.unwrap();
    let mut buf = [0u8; 2048];
    let (n, from) = tokio::time::timeout(WAIT, rx6.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(from, tx6.local_addr().unwrap());
    let back: InputPacket = decode_msg(&buf[..n]).unwrap();
    assert_eq!(back.seq, 42);
    assert!(matches!(back.msg, InputMsg::Leave));

    // IPv6 peer but no IPv6 socket → readable error, not a silent drop.
    let v4_only = UdpSet {
        v4: Some(Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap())),
        v6: None,
    };
    let e = send_input_to(&shared, &v4_only, "peer6", &pkt).await.unwrap_err();
    assert!(e.contains("IPv6"), "{e}");

    // Literal IPv6 host (not yet in the map) resolves with the UDP port.
    assert_eq!(
        resolve_peer_udp(&shared, "fe80::5%2"),
        Some(SocketAddr::V6(SocketAddrV6::new(
            "fe80::5".parse().unwrap(),
            inputshare_protocol::DEFAULT_UDP_PORT,
            0,
            2
        )))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_ipv6_discovery_is_received() {
    let shared = shared_with(net_cfg("me"), None);
    let rx6 = Arc::new(bind_udp_v6(v6_loopback(0)).unwrap());
    let rx_addr = rx6.local_addr().unwrap();
    let task = tokio::spawn(udp_recv_loop(Arc::clone(&shared), rx6));

    let tx6 = bind_udp_v6(v6_loopback(0)).unwrap();
    let bytes = encode_msg(&DiscoveryMsg::Announce {
        hostname: "v6-peer".into(),
        tcp_port: 7000,
        version: PROTOCOL_VERSION,
    })
    .unwrap();
    tx6.send_to(&bytes, rx_addr).await.unwrap();
    wait_until("v6 announce seen", || {
        lock(&shared.status)
            .discovered
            .iter()
            .any(|p| p.hostname == "v6-peer" && p.addr.is_ipv6() && p.tcp_port == 7000)
    })
    .await;
    assert_eq!(
        lock(&shared.peer_udp).get("v6-peer").copied(),
        Some(tx6.local_addr().unwrap())
    );
    task.abort();
}

#[tokio::test]
async fn ipv6_and_ipv4_sockets_share_a_port_on_loopback() {
    let v4 = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = v4.local_addr().unwrap().port();
    let v6 = bind_udp_v6(v6_loopback(port)).unwrap();
    assert_eq!(v6.local_addr().unwrap().port(), port);

    let t4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let tport = t4.local_addr().unwrap().port();
    let t6 = bind_tcp_v6(v6_loopback(tport)).unwrap();
    assert_eq!(t6.local_addr().unwrap().port(), tport);
}

#[test]
fn udp_set_reports_missing_family() {
    let set = UdpSet::default();
    assert!(set
        .socket_for(&"1.2.3.4:1".parse().unwrap())
        .unwrap_err()
        .contains("IPv4"));
    assert!(set
        .socket_for(&"[::1]:1".parse().unwrap())
        .unwrap_err()
        .contains("IPv6"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_issue_is_recorded_replaced_shown_in_status_line_and_cleared() {
    let s = shared_with(net_cfg("pc"), None);
    {
        let mut st = lock(&s.status);
        st.listening_tcp = Some(6555);
        st.listening_udp = Some(1046);
    }
    s.note_link_issue("192.168.100.52", "auth failed from PC2", false);
    let st = lock(&s.status).clone();
    assert_eq!(st.link_issues.len(), 1);
    assert!(st.link_issues[0].text.contains("Wymagaj hasła"), "{:?}", st.link_issues);
    assert!(st.status_line.contains("192.168.100.52"), "{}", st.status_line);
    assert!(st.status_line.contains("brak połączenia"), "{}", st.status_line);

    // the same peer failing differently replaces the old reason instead of piling up
    s.note_link_issue("192.168.100.52", "version mismatch", false);
    let st = lock(&s.status).clone();
    assert_eq!(st.link_issues.len(), 1);
    assert!(st.link_issues[0].text.contains("wersji"));

    s.clear_link_issue("192.168.100.52");
    let st = lock(&s.status).clone();
    assert!(st.link_issues.is_empty());
    assert_eq!(st.status_line, "Nasłuch TCP 6555 UDP 1046");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_handshake_clears_the_issue_on_both_sides() {
    let mut sc = net_cfg("server-pc");
    sc.network.password_required = true;
    sc.network.password = "secret".into();
    let mut cc = net_cfg("client-pc");
    cc.network.password_required = true;
    cc.network.password = "secret".into();
    let server = shared_with(sc, None);
    let client = shared_with(cc, None);
    // Earlier failed attempts: the client dialled "s", the server saw the loopback address.
    client.note_link_issue("s", "auth failed", true);
    server.note_link_issue("127.0.0.1", "auth failed from client-pc", false);
    let (srv, cli) = connect_pair(&server, &client, "s").await;
    wait_until("link up", || lock(&server.bulk_tx).contains_key("client-pc")).await;
    assert!(lock(&client.status).link_issues.is_empty());
    assert!(lock(&server.status).link_issues.is_empty());
    srv.abort();
    cli.abort();
}

fn info(label: &str, x: i32, y: i32, w: u32, h: u32, primary: bool) -> MonitorInfo {
    MonitorInfo { label: label.into(), x, y, width: w, height: h, primary }
}

#[test]
fn sanitize_drops_garbage_caps_the_count_and_dedupes_labels() {
    let raw = vec![
        info("A", 0, 0, 1920, 1080, true),
        info("A", 1920, 0, 1920, 1080, false), // duplicate label
        info("", 0, 0, 100, 100, false),       // no label
        info("B", 0, 0, 0, 1080, false),       // zero width
        info("C", 0, 0, 100_000, 1080, false), // absurd size
        info("D", 5_000_000, 0, 100, 100, false), // absurd position
        info("  E-long-label-here ", 0, 0, 800, 600, false),
    ];
    let clean = sanitize_monitors(raw);
    let labels: Vec<&str> = clean.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["A", "E-long-l"]);

    let many: Vec<MonitorInfo> = (0..40).map(|i| info(&format!("M{i}"), 0, 0, 100, 100, false)).collect();
    assert_eq!(sanitize_monitors(many).len(), 16);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peers_exchange_their_displays_on_connect_and_when_they_change() {
    let server = shared_with(net_cfg("server-pc"), None);
    let client = shared_with(net_cfg("client-pc"), None);
    *lock(&server.local_monitors) = vec![info("A", 0, 0, 2560, 1440, true)];
    *lock(&client.local_monitors) = vec![info("A", 0, 0, 1920, 1080, true)];
    let (srv, cli) = connect_pair(&server, &client, "s").await;

    wait_until("client learns the server's display", || {
        lock(&client.status).remote_monitors.iter().any(|r| r.hostname == "server-pc")
    })
    .await;
    wait_until("server learns the client's display", || {
        lock(&server.status).remote_monitors.iter().any(|r| r.hostname == "client-pc")
    })
    .await;
    let seen = lock(&client.status).remote_monitors.clone();
    assert_eq!(seen[0].monitors, vec![info("A", 0, 0, 2560, 1440, true)]);
    assert_eq!(seen[0].ip, "127.0.0.1", "the session's address, so a hand-made IP tile can be adopted");

    // a display is plugged in on the server: only the changed list goes out
    let rev = lock(&client.status).monitors_rev;
    server.set_local_monitors(vec![
        info("A", 0, 0, 2560, 1440, true),
        info("B", 2560, 0, 1920, 1080, false),
    ]);
    wait_until("client sees the new display", || {
        lock(&client.status).remote_monitors.iter().any(|r| r.monitors.len() == 2)
    })
    .await;
    assert!(lock(&client.status).monitors_rev > rev);

    // the same list again is not sent and does not bump the revision
    let rev = lock(&client.status).monitors_rev;
    server.set_local_monitors(vec![
        info("A", 0, 0, 2560, 1440, true),
        info("B", 2560, 0, 1920, 1080, false),
    ]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock(&client.status).monitors_rev, rev);

    // the peer's name now resolves for input
    assert!(lock(&client.peer_udp).contains_key("server-pc"));
    srv.abort();
    cli.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_with_our_own_name_is_ignored_not_merged() {
    let server = shared_with(net_cfg("same-pc"), None);
    let client = shared_with(net_cfg("same-pc"), None);
    *lock(&server.local_monitors) = vec![info("A", 0, 0, 1920, 1080, true)];
    let (srv, cli) = connect_pair(&server, &client, "s").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("s")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(lock(&client.status).remote_monitors.is_empty());
    srv.abort();
    cli.abort();
}

// ───────────────────────── auto-connect ─────────────────────────

fn ago(secs: u64) -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_secs(secs)).unwrap_or(now)
}

fn peer(host: &str, ip: [u8; 4], version: u16, seen_secs_ago: u64, first_secs_ago: u64) -> DiscoveredPeer {
    DiscoveredPeer {
        hostname: host.into(),
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 1046),
        tcp_port: 6555,
        version,
        first_seen: ago(first_secs_ago),
        last_seen: ago(seen_secs_ago),
    }
}

fn with_password(local: &str) -> NetConfig {
    let mut cfg = net_cfg(local);
    cfg.network.password_required = true;
    cfg.network.password = "pw".into();
    cfg
}

#[test]
fn auto_connect_needs_a_password_and_an_empty_list_and_the_switch() {
    let found = [peer("ZETA", [192, 168, 1, 7], PROTOCOL_VERSION, 1, 1)];
    let now = Instant::now();

    // password set, empty list, switch on: the peer is dialled (our name sorts first)
    assert_eq!(auto_targets(&with_password("ALFA"), &found, now), [("192.168.1.7".to_string(), 6555)]);

    // no password: a stranger on the LAN must not be dialled
    let mut cfg = with_password("ALFA");
    cfg.network.password_required = false;
    assert!(auto_targets(&cfg, &found, now).is_empty());
    let mut cfg = with_password("ALFA");
    cfg.network.password.clear();
    assert!(auto_targets(&cfg, &found, now).is_empty());

    // switch off
    let mut cfg = with_password("ALFA");
    cfg.clients.auto_connect = false;
    assert!(auto_targets(&cfg, &found, now).is_empty());

    // a filled list means "exactly these"
    let mut cfg = with_password("ALFA");
    cfg.clients.accepted.push(ClientEntry { host: "10.0.0.5".into(), tcp_port: 6555 });
    assert!(auto_targets(&cfg, &found, now).is_empty());
}

#[test]
fn the_smaller_name_dials_first_and_the_other_waits_for_the_fallback() {
    let now = Instant::now();
    let fresh = [peer("ALFA", [192, 168, 1, 7], PROTOCOL_VERSION, 1, 3)];
    // we are ZETA: ALFA sorts first and is expected to dial us, so we wait ...
    assert!(auto_targets(&with_password("ZETA"), &fresh, now).is_empty());
    // ... unless it has been in view for a while and nothing arrived: then we dial it ourselves
    let long_seen = [peer("ALFA", [192, 168, 1, 7], PROTOCOL_VERSION, 1, 20)];
    assert_eq!(auto_targets(&with_password("ZETA"), &long_seen, now).len(), 1);
    // case does not decide who goes first
    assert_eq!(auto_targets(&with_password("alfa"), &[peer("ZETA", [1, 2, 3, 4], PROTOCOL_VERSION, 1, 1)], now).len(), 1);
}

#[test]
fn stale_and_foreign_version_peers_are_not_dialled() {
    let now = Instant::now();
    let gone = [peer("ZETA", [192, 168, 1, 7], PROTOCOL_VERSION, 60, 120)];
    assert!(auto_targets(&with_password("ALFA"), &gone, now).is_empty(), "announce is a minute old");
    let old = [peer("ZETA", [192, 168, 1, 7], PROTOCOL_VERSION - 1, 1, 1)];
    assert!(auto_targets(&with_password("ALFA"), &old, now).is_empty(), "different protocol cannot link up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_in_either_direction_counts_as_linked() {
    let s = shared_with(net_cfg("pc"), None);
    assert!(!peer_linked(&s, "192.168.100.52"));
    lock(&s.status).peers.push(PeerConnInfo {
        hostname: "DESKTOP-PEER".into(),
        addr: "192.168.100.52:6555".parse().unwrap(),
        tcp_ok: true,
    });
    assert!(peer_linked(&s, "192.168.100.52"), "inbound link, known by the peer's name");
    assert!(!peer_linked(&s, "192.168.100.53"));
    lock(&s.status).peers[0].tcp_ok = false;
    assert!(!peer_linked(&s, "192.168.100.52"), "a closed link is not a link");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_journal_keeps_the_newest_lines_and_throttles_repeats() {
    let s = shared_with(net_cfg("pc"), None);
    for i in 0..(MAX_EVENTS + 5) {
        s.push_event(format!("zdarzenie {i}"));
    }
    let events = lock(&s.status).events.clone();
    assert_eq!(events.len(), MAX_EVENTS);
    assert_eq!(events[0].text, "zdarzenie 5");
    assert_eq!(events[0].at.len(), 8, "HH:MM:SS");

    s.push_event_throttled("k", "raz".into());
    s.push_event_throttled("k", "dwa".into());
    s.push_event_throttled("inny", "trzy".into());
    let texts: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(texts.iter().any(|t| t == "raz"));
    assert!(!texts.iter().any(|t| t == "dwa"), "the same key is written once a minute");
    assert!(texts.iter().any(|t| t == "trzy"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computers_with_the_same_password_link_up_on_their_own() {
    // The "server" never has the client on a list; the client only heard its announce.
    let server = shared_with(with_password("zeta-pc"), None);
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(&server), l);

    let client = shared_with(with_password("alfa-pc"), None);
    lock(&client.status).discovered.push(DiscoveredPeer {
        hostname: "zeta-pc".into(),
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1046),
        tcp_port: addr.port(),
        version: PROTOCOL_VERSION,
        first_seen: Instant::now(),
        last_seen: Instant::now(),
    });
    let dialer = tokio::spawn(outbound_connect_loop(Arc::clone(&client)));

    wait_until("auto link up", || lock(&server.bulk_tx).contains_key("alfa-pc")).await;
    assert!(lock(&client.bulk_tx).contains_key("127.0.0.1"));
    let journal: Vec<String> = lock(&client.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("Łączę automatycznie")), "{journal:?}");
    assert!(journal.iter().any(|t| t.starts_with("Połączono z")), "{journal:?}");
    dialer.abort();
    srv.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_to_dials_a_computer_that_is_not_on_the_list() {
    let server = shared_with(net_cfg("server-pc"), None);
    let (l, addr) = listen_v4().await;
    let srv = serve_one(Arc::clone(&server), l);
    let client = shared_with(net_cfg("client-pc"), None);
    lock(&client.dial_requests).push(("127.0.0.1".into(), addr.port()));
    let dialer = tokio::spawn(outbound_connect_loop(Arc::clone(&client)));
    wait_until("manual dial links", || lock(&server.bulk_tx).contains_key("client-pc")).await;
    assert!(lock(&client.dial_requests).is_empty(), "the request is consumed");
    dialer.abort();
    srv.abort();
}

// ───────────────────────── way back / no loops ─────────────────────────

fn link(hostname: &str, ip: &str, up: bool) -> PeerConnInfo {
    PeerConnInfo {
        hostname: hostname.into(),
        addr: format!("{ip}:6555").parse().unwrap(),
        tcp_ok: up,
    }
}

#[test]
fn a_display_host_is_up_when_a_link_to_that_computer_is_up() {
    let mut st = NetStatus::default();
    st.peers.push(link("DESKTOP-PEER", "192.168.100.52", true));
    // by the peer's name (inbound link), case-insensitive
    assert!(st.host_is_up("desktop-peer"));
    // a hand-made tile that carries the bare IP
    assert!(st.host_is_up("192.168.100.52"));
    assert!(!st.host_is_up("192.168.100.53"));
    assert!(!st.host_is_up("OTHER"));

    // an outbound link is keyed by the IP; the name comes from the displays the peer reported
    let mut st = NetStatus::default();
    st.peers.push(link("192.168.100.52", "192.168.100.52", true));
    assert!(!st.host_is_up("DESKTOP-PEER"), "name unknown until the peer reports its displays");
    st.remote_monitors.push(RemoteMonitors {
        hostname: "DESKTOP-PEER".into(),
        ip: "192.168.100.52".into(),
        monitors: vec![info("A", 0, 0, 1920, 1080, true)],
    });
    assert!(st.host_is_up("DESKTOP-PEER"));

    // a closed link is not a link
    st.peers[0].tcp_ok = false;
    assert!(!st.host_is_up("DESKTOP-PEER"));
}

fn steering(s: &Shared, host: &str, ip: &str) {
    s.capture.begin_capture(host);
    lock(&s.peer_udp).insert(host.to_string(), format!("{ip}:1046").parse().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_the_link_to_the_computer_being_steered_hands_the_cursor_back() {
    let s = shared_with(net_cfg("pc"), None);
    steering(&s, "PEER", "192.168.100.52");
    assert!(s.capture.capturing.load(Ordering::SeqCst));
    peer_gone(&s, "192.168.100.52".parse().unwrap());
    assert!(s.capture.leave_requested.load(Ordering::SeqCst), "the capture loop will end the capture");
    assert!(s.outbox.take_leave(), "and the peer is told");
    let journal: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("zerwane")), "{journal:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_link_to_the_same_computer_keeps_the_capture() {
    let s = shared_with(net_cfg("pc"), None);
    steering(&s, "PEER", "192.168.100.52");
    lock(&s.status).peers.push(link("PEER", "192.168.100.52", true));
    peer_gone(&s, "192.168.100.52".parse().unwrap());
    assert!(!s.capture.leave_requested.load(Ordering::SeqCst));
    assert!(!s.outbox.take_leave());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrelated_link_dropping_does_not_end_the_capture() {
    let s = shared_with(net_cfg("pc"), None);
    steering(&s, "PEER", "192.168.100.52");
    peer_gone(&s, "192.168.100.77".parse().unwrap());
    assert!(!s.capture.leave_requested.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_the_link_to_the_computer_steering_us_clears_the_controlled_state() {
    let s = shared_with(net_cfg("pc"), None);
    s.capture.touch_controlled();
    *lock(&s.controller) = Some("192.168.100.52:1046".parse().unwrap());
    lock(&s.status).being_controlled = true;
    assert!(s.capture.is_controlled());
    peer_gone(&s, "192.168.100.52".parse().unwrap());
    assert!(!s.capture.is_controlled(), "no Leave will ever come from a computer that is gone");
    assert!(lock(&s.controller).is_none());
    assert!(!lock(&s.status).being_controlled);
}

// ───────────────────────── Enter handshake ─────────────────────────

/// A fake TCP link to `ip` registered under `key`; the receiver end shows what was sent on it.
fn fake_link(s: &Shared, key: &str, ip: &str) -> mpsc::UnboundedReceiver<BulkMsg> {
    let (ctrl, rx) = mpsc::unbounded_channel();
    let (files, _files_rx) = mpsc::channel(1);
    lock(&s.bulk_tx).insert(key.to_string(), PeerLink { ctrl, files });
    lock(&s.status).peers.push(PeerConnInfo {
        hostname: key.to_string(),
        addr: format!("{ip}:6555").parse().unwrap(),
        tcp_ok: true,
    });
    rx
}

fn enter_pkt() -> InputPacket {
    InputPacket { version: PROTOCOL_VERSION, seq: 1, msg: InputMsg::Enter { edge: inputshare_protocol::Edge::Left, entry: 0.5 } }
}

/// The controller has just entered `host` at `ip`: capturing, Enter sent `ago` ago over UDP.
fn entered(s: &Shared, host: &str, ip: &str, ago: Duration) {
    s.capture.begin_capture(host);
    lock(&s.peer_udp).insert(host.to_string(), format!("{ip}:1046").parse().unwrap());
    *lock(&s.enter_wait) = Some(EnterWait {
        host: host.to_string(),
        pkt: enter_pkt(),
        sent_at: Instant::now().checked_sub(ago).unwrap_or_else(Instant::now),
        over_tcp: false,
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_udp_enter_is_resent_over_tcp_and_input_follows_it() {
    let s = shared_with(net_cfg("pc"), None);
    let mut link = fake_link(&s, "PEER", "192.168.100.52");
    entered(&s, "PEER", "192.168.100.52", Duration::from_millis(10));

    check_enter_wait(&s);
    assert!(link.try_recv().is_err(), "still within the UDP wait");

    entered(&s, "PEER", "192.168.100.52", ENTER_ACK_UDP + Duration::from_millis(50));
    check_enter_wait(&s);
    match link.try_recv() {
        Ok(BulkMsg::Input(pkt)) => assert!(matches!(pkt.msg, InputMsg::Enter { .. })),
        other => panic!("expected the Enter over TCP, got {other:?}"),
    }
    assert!(s.input_over_tcp.load(Ordering::SeqCst), "the rest of this capture goes over TCP");
    assert!(lock(&s.enter_wait).as_ref().is_some_and(|w| w.over_tcp));
    assert!(!s.capture.leave_requested.load(Ordering::SeqCst), "not given up yet");
    let journal: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("UDP 1046")), "{journal:?}");

    // an acknowledgement settles it
    on_enter_ack(&s, true, None);
    assert!(lock(&s.enter_wait).is_none());
    assert!(!s.capture.leave_requested.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_answer_over_tcp_either_gives_the_cursor_back_with_a_reason() {
    let s = shared_with(net_cfg("pc"), None);
    let _link = fake_link(&s, "PEER", "192.168.100.52");
    entered(&s, "PEER", "192.168.100.52", Duration::from_millis(10));
    lock(&s.enter_wait).as_mut().unwrap().over_tcp = true;
    lock(&s.enter_wait).as_mut().unwrap().sent_at =
        Instant::now().checked_sub(ENTER_ACK_TCP + Duration::from_millis(50)).unwrap();
    check_enter_wait(&s);
    assert!(s.capture.leave_requested.load(Ordering::SeqCst), "control comes back");
    assert!(s.outbox.take_leave());
    let issue = lock(&s.status).link_issues.iter().find(|i| i.peer == "PEER").cloned().expect("issue shown");
    assert!(issue.text.contains("nie odpowiada"), "{}", issue.text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_any_tcp_link_an_unanswered_enter_gives_up_right_away() {
    let s = shared_with(net_cfg("pc"), None);
    entered(&s, "PEER", "192.168.100.52", ENTER_ACK_UDP + Duration::from_millis(50));
    check_enter_wait(&s);
    assert!(s.capture.leave_requested.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_names_the_reason_and_hands_control_back() {
    let s = shared_with(net_cfg("pc"), None);
    entered(&s, "PEER", "192.168.100.52", Duration::from_millis(10));
    on_enter_ack(&s, false, Some("ma wyłączone udostępnianie.".into()));
    assert!(s.capture.leave_requested.load(Ordering::SeqCst));
    let journal: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("wyłączone udostępnianie")), "{journal:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_computer_with_sharing_off_refuses_the_enter_over_tcp() {
    // Refusal paths only: an accepted Enter would move the real cursor.
    let mut cfg = net_cfg("pc2");
    cfg.sharing_enabled = false;
    let s = shared_with(cfg, None);
    let mut link = fake_link(&s, "PC1", "192.168.100.218");
    handle_input_packet(&s, "192.168.100.218:1046".parse().unwrap(), enter_pkt()).await;
    match link.try_recv() {
        Ok(BulkMsg::EnterAck { ok: false, reason: Some(r) }) => assert!(r.contains("wyłączone"), "{r}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!s.capture.is_controlled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_computer_that_is_steering_refuses_to_be_steered() {
    let s = shared_with(net_cfg("pc2"), None);
    let mut link = fake_link(&s, "PC1", "192.168.100.218");
    s.capture.begin_capture("SOMEONE-ELSE");
    handle_input_packet(&s, "192.168.100.218:1046".parse().unwrap(), enter_pkt()).await;
    match link.try_recv() {
        Ok(BulkMsg::EnterAck { ok: false, reason: Some(r) }) => assert!(r.contains("steruje"), "{r}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_also_goes_over_tcp_and_resets_the_transport() {
    let s = shared_with(net_cfg("pc"), None);
    let mut link = fake_link(&s, "PEER", "192.168.100.52");
    entered(&s, "PEER", "192.168.100.52", Duration::from_millis(10));
    s.input_over_tcp.store(true, Ordering::SeqCst);
    assert!(send_bulk_to_ip(&s, "192.168.100.52".parse().unwrap(), BulkMsg::Input(enter_pkt())));
    assert!(matches!(link.try_recv(), Ok(BulkMsg::Input(_))));
    assert!(!send_bulk_to_ip(&s, "10.9.9.9".parse().unwrap(), BulkMsg::Input(enter_pkt())), "no link to that address");
}

// ───────────────────────── held cursor (games) ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steered_computer_reports_a_held_cursor_to_its_controller_only_on_change() {
    let s = shared_with(net_cfg("pc2"), None);
    let mut link = fake_link(&s, "PC1", "192.168.100.218");
    let mut last = None;

    report_cursor_lock(&s, &mut last, || true);
    assert!(link.try_recv().is_err(), "nobody steers this computer: nothing to tell");

    s.capture.touch_controlled();
    *lock(&s.controller) = Some("192.168.100.218:1046".parse().unwrap());
    report_cursor_lock(&s, &mut last, || false);
    assert!(link.try_recv().is_err(), "a free cursor at the start is the default");
    report_cursor_lock(&s, &mut last, || true);
    assert!(matches!(link.try_recv(), Ok(BulkMsg::CursorLock { locked: true })));
    report_cursor_lock(&s, &mut last, || true);
    assert!(link.try_recv().is_err(), "unchanged, not due again yet");
    report_cursor_lock(&s, &mut last, || false);
    assert!(matches!(link.try_recv(), Ok(BulkMsg::CursorLock { locked: false })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_cursor_there_shuts_the_way_back_unless_switched_off() {
    let s = shared_with(net_cfg("pc"), None);
    on_remote_cursor_lock(&s, "PEER", true);
    assert!(!s.capture.remote_cursor_locked.load(Ordering::SeqCst), "not steering: ignored");

    s.capture.begin_capture("PEER");
    on_remote_cursor_lock(&s, "PEER", true);
    assert!(s.capture.remote_cursor_locked.load(Ordering::SeqCst));
    let journal: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("trzyma tam kursor")), "{journal:?}");
    on_remote_cursor_lock(&s, "PEER", false);
    assert!(!s.capture.remote_cursor_locked.load(Ordering::SeqCst));

    let mut cfg = net_cfg("pc");
    cfg.switching.block_when_cursor_locked = false;
    let s = shared_with(cfg, None);
    s.capture.begin_capture("PEER");
    on_remote_cursor_lock(&s, "PEER", true);
    assert!(!s.capture.remote_cursor_locked.load(Ordering::SeqCst), "the option is off");
}

// ───────────────────────── this computer's own mouse takes it back ─────────────────────────

#[test]
fn own_mouse_movement_takes_back_control_only_after_the_grace_time() {
    let t0 = Instant::now();
    let mut tb = TakeBack::default();
    assert!(!tb.due(false, 100, t0), "not steered: nothing to take back");
    assert!(!tb.due(true, 110, t0), "control just arrived");
    assert!(!tb.due(true, 120, t0 + Duration::from_millis(300)), "still inside the grace time");
    assert!(!tb.due(true, 121, t0 + Duration::from_millis(600)), "a single twitch is not someone at the desk");
    assert!(tb.due(true, 130, t0 + Duration::from_millis(650)), "the mouse here is moving");
    assert!(!tb.due(false, 140, t0 + Duration::from_millis(700)));
    assert!(!tb.due(true, 150, t0 + Duration::from_millis(750)), "a new steering starts a new grace time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taking_back_tells_the_steering_computer_and_drops_its_late_input() {
    let s = shared_with(net_cfg("pc2"), None);
    let mut link = fake_link(&s, "PC1", "192.168.100.218");
    let from: SocketAddr = "192.168.100.218:1046".parse().unwrap();
    s.capture.touch_controlled();
    *lock(&s.controller) = Some(from);
    take_back_control(&s);
    assert!(!s.capture.is_controlled());
    match link.try_recv() {
        Ok(BulkMsg::EnterAck { ok: false, reason: Some(r) }) => assert!(r.contains("rusza myszą"), "{r}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // input still on the way from there does not take control again (events would move the
    // real cursor, so only the guard is checked: no event is applied and nothing is controlled)
    handle_input_packet(&s, from, InputPacket { version: PROTOCOL_VERSION, seq: 9, msg: InputMsg::Events(vec![]) }).await;
    assert!(!s.capture.is_controlled(), "late packets are dropped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_refusal_ends_the_steering_here() {
    let s = shared_with(net_cfg("pc"), None);
    s.capture.begin_capture("PEER");
    on_steering_refused_later(&s, "PEER", false, Some("ktoś rusza myszą przy tamtym komputerze.".into()));
    assert!(s.capture.leave_requested.load(Ordering::SeqCst));
    assert!(s.outbox.take_leave());
    let journal: Vec<String> = lock(&s.status).events.iter().map(|e| e.text.clone()).collect();
    assert!(journal.iter().any(|t| t.contains("rusza myszą")), "{journal:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_copied_for_the_clipboard_arrive_with_full_clipboard_alone() {
    let recv_dir = temp_dir("recv-clip");
    let src_dir = temp_dir("src-clip");
    let mut sc = net_cfg("server-pc");
    sc.exchange.allow_drag_drop = false;
    let server = shared_with(sc, Some(recv_dir.clone()));
    let mut cc = net_cfg("client-pc");
    cc.exchange.allow_drag_drop = false;
    let client = shared_with(cc, None);
    let (srv, cli) = connect_pair(&server, &client, "srv").await;
    wait_until("link up", || lock(&client.bulk_tx).contains_key("srv")).await;

    let src = src_dir.join("photo.png");
    std::fs::write(&src, vec![3u8; inputshare_protocol::FILE_CHUNK_SIZE + 10]).unwrap();
    assert!(start_file_send(&client, None, vec![src.clone()]).is_err(), "a plain send stays off");
    let ids = start_send(&client, Some("srv"), vec![src], SendKind::Clipboard).unwrap();
    wait_until("sender Done", || {
        transfer(&client, TransferDirection::Send, Some(ids[0])).is_some_and(|t| t.state == TransferState::Done)
    })
    .await;
    assert_eq!(dir_names(&recv_dir), vec!["photo.png".to_string()]);
    wait_until("clipboard batch settled", || {
        lock(&server.status).events.iter().any(|e| e.text.contains("są w schowku"))
    })
    .await;
    srv.abort();
    cli.abort();
    let _ = std::fs::remove_dir_all(&recv_dir);
    let _ = std::fs::remove_dir_all(&src_dir);
}
