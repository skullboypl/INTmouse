//! Wire types for INTmouse: UDP mouse and keyboard input, TCP handshake, clipboard and files.
//!
//! Design:
//! - UDP: mouse and keyboard input (`InputMsg` / `InputPacket`) + discovery (`DiscoveryMsg`)
//! - TCP: handshake, clipboard, files, control (`BulkMsg`) — length-prefixed frames

use serde::{Deserialize, Serialize};

/// v3: added `BulkMsg::FileCancel` / `BulkMsg::FileResult` (file send path).
/// A v2 peer cannot decode those variants, so the Hello version check must
/// reject the pairing up front instead of failing mid-transfer.
///
/// v4: added `BulkMsg::Monitors` (each side tells the other which displays it has, so the
/// layout can be filled in automatically).
///
/// v5: `BulkMsg::HelloAck` carries `password_required`.
///
/// v6: `BulkMsg::EnterAck` (the steered computer confirms or refuses control) and
/// `BulkMsg::Input` (input over TCP when UDP does not get through).
///
/// v9: `BulkMsg::DragDrop` (files dragged across the edge are dropped where the cursor is).
///
/// v10: `BulkMsg::ClipboardFiles` (files copied with Ctrl+C go as ordinary transfers, with
/// progress, and land on the other computer's clipboard).
pub const PROTOCOL_VERSION: u16 = 10;

/// One display of a computer, in that computer's own virtual-desktop pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Default ports (ShareMouse-inspired; changeable in settings).
pub const DEFAULT_TCP_PORT: u16 = 6555;
pub const DEFAULT_UDP_PORT: u16 = 1046;

/// Max TCP bulk frame (64 MiB) — guards against unbounded alloc from a peer.
pub const MAX_BULK_FRAME: u32 = 64 * 1024 * 1024;

/// Payload size of one `BulkMsg::FileChunk`.
pub const FILE_CHUNK_SIZE: usize = 256 * 1024;

// A chunk frame (payload + small bincode header) must always fit in one bulk frame.
const _: () = assert!(FILE_CHUNK_SIZE + 64 < MAX_BULK_FRAME as usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Portable key id — never send raw OS scancodes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    Char(char),
    Escape,
    Tab,
    CapsLock,
    NumLock,
    ScrollLock,
    Shift,
    Ctrl,
    Alt,
    Super,
    Enter,
    Backspace,
    Space,
    Left,
    Right,
    Up,
    Down,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    Unknown(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    /// Relative motion — preferred path for games / low lag.
    MouseMove { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    Key { key: Key, pressed: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputMsg {
    /// Coalesced events for one capture tick.
    Events(Vec<InputEvent>),
    /// Server → client: you now have control; enter along `edge` at normalized `entry` 0..1.
    Enter { edge: Edge, entry: f32 },
    /// Either side: return control.
    Leave,
    Ping { nonce: u64, sent_nanos: u64 },
    Pong { nonce: u64, sent_nanos: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputPacket {
    pub version: u16,
    pub seq: u64,
    pub msg: InputMsg,
}

/// UDP discovery datagrams (raw bincode, same port as input by default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryMsg {
    Announce {
        hostname: String,
        tcp_port: u16,
        version: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BulkMsg {
    Hello {
        version: u16,
        hostname: String,
        /// SHA-256(password) hex, or empty when password not required.
        auth_token: String,
    },
    /// `password_required`: whether the answering computer demands a password. The dialling side
    /// uses it to refuse a peer that does not (it would accept anybody, our password proves nothing).
    HelloAck {
        ok: bool,
        reason: Option<String>,
        password_required: bool,
    },
    ClipboardText { text: String },
    ClipboardRequest,
    FileOffer {
        name: String,
        size: u64,
        transfer_id: u64,
    },
    FileChunk {
        transfer_id: u64,
        offset: u64,
        data: Vec<u8>,
    },
    FileDone { transfer_id: u64 },
    LockState {
        caps: bool,
        num: bool,
        scroll: bool,
    },
    KeyboardLayout { name: String },
    Panic,
    /// Sender → receiver: abort an incoming transfer (receiver drops the partial file).
    FileCancel { transfer_id: u64, reason: String },
    /// Receiver → sender: final outcome of a transfer (`ok` = saved to disk).
    FileResult {
        transfer_id: u64,
        ok: bool,
        reason: Option<String>,
    },
    /// Steered computer → steering one: the `Enter` arrived and was taken (`ok`), or why not.
    EnterAck { ok: bool, reason: Option<String> },
    /// An input packet carried over TCP, used when UDP to that computer does not get through.
    Input(InputPacket),
    /// Sender's displays (v4). Sent right after the handshake and again whenever they change.
    Monitors {
        hostname: String,
        monitors: Vec<MonitorInfo>,
    },
    /// Steered computer → steering one (v7): a program there holds the cursor (a game's
    /// crosshair), so pushing back through the edge must not leave.
    CursorLock { locked: bool },
    /// The whole clipboard (v8, "full clipboard exchange"): every format this computer could
    /// read, so a copied picture, formatted text or a few files paste on the other one too.
    Clipboard(ClipboardPayload),
    /// Steering computer → steered one (v9): files were dragged across the edge and let go
    /// there. They follow as ordinary `FileOffer`s with these ids; once all of them are saved
    /// the receiver drops them under its cursor.
    DragDrop { transfer_ids: Vec<u64> },
    /// Steering-free clipboard sync (v10): files copied on the sender (Explorer Ctrl+C). They
    /// follow as ordinary `FileOffer`s with these ids; once all are saved the receiver puts them
    /// on its clipboard, so Ctrl+V pastes them.
    ClipboardFiles { transfer_ids: Vec<u64> },
}

/// Clipboard contents in the formats both computers understand. Empty fields were not on the
/// clipboard (or were too big to send).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardPayload {
    pub text: Option<String>,
    /// Windows "HTML Format" bytes (header + fragment, UTF-8).
    pub html: Option<Vec<u8>>,
    /// "Rich Text Format" bytes.
    pub rtf: Option<Vec<u8>>,
    /// A picture as a device-independent bitmap (Windows `CF_DIB`).
    pub image_dib: Option<Vec<u8>>,
    /// Copied files (Explorer Ctrl+C), carried whole.
    pub files: Vec<ClipboardFile>,
    /// Files that were on the clipboard but left out (too big, a folder, unreadable).
    pub skipped_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardFile {
    pub name: String,
    pub data: Vec<u8>,
}

/// Most a clipboard message may carry, so it still fits one bulk frame.
pub const MAX_CLIPBOARD_BYTES: usize = 48 * 1024 * 1024;
const _: () = assert!(MAX_CLIPBOARD_BYTES + 1024 * 1024 < MAX_BULK_FRAME as usize);

impl ClipboardPayload {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.html.is_none()
            && self.rtf.is_none()
            && self.image_dib.is_none()
            && self.files.is_empty()
    }

    /// Bytes this payload carries (close to its encoded size).
    pub fn byte_len(&self) -> usize {
        self.text.as_ref().map_or(0, String::len)
            + self.html.as_ref().map_or(0, Vec::len)
            + self.rtf.as_ref().map_or(0, Vec::len)
            + self.image_dib.as_ref().map_or(0, Vec::len)
            + self.files.iter().map(|f| f.name.len() + f.data.len()).sum::<usize>()
    }
}

// ─── encode / decode ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("bincode: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("bulk frame too large: {0} bytes")]
    FrameTooLarge(u32),
    #[error("incomplete bulk frame")]
    Incomplete,
}

/// Encode any serializable message to raw bincode bytes (UDP datagrams).
pub fn encode_msg<T: Serialize>(msg: &T) -> Result<Vec<u8>, CodecError> {
    Ok(bincode::serialize(msg)?)
}

/// Decode a raw bincode datagram.
pub fn decode_msg<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CodecError> {
    Ok(bincode::deserialize(bytes)?)
}

/// Length-prefixed TCP frame: `u32` LE length + bincode payload.
pub fn encode_bulk_frame(msg: &BulkMsg) -> Result<Vec<u8>, CodecError> {
    let body = encode_msg(msg)?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Parse one length-prefixed bulk frame from `buf`.
/// Returns `(msg, bytes_consumed)` or `Incomplete` if more data is needed.
pub fn try_decode_bulk_frame(buf: &[u8]) -> Result<(BulkMsg, usize), CodecError> {
    if buf.len() < 4 {
        return Err(CodecError::Incomplete);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if len > MAX_BULK_FRAME {
        return Err(CodecError::FrameTooLarge(len));
    }
    let total = 4 + len as usize;
    if buf.len() < total {
        return Err(CodecError::Incomplete);
    }
    let msg = decode_msg::<BulkMsg>(&buf[4..total])?;
    Ok((msg, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_sharemouse_ports() {
        assert_eq!(DEFAULT_TCP_PORT, 6555);
        assert_eq!(DEFAULT_UDP_PORT, 1046);
    }

    #[test]
    fn relative_move_is_core_event() {
        let e = InputEvent::MouseMove { dx: 3, dy: -1 };
        match e {
            InputEvent::MouseMove { dx, dy } => {
                assert_eq!(dx, 3);
                assert_eq!(dy, -1);
            }
            _ => panic!("expected relative move"),
        }
    }

    #[test]
    fn bulk_frame_roundtrip() {
        let msg = BulkMsg::Hello {
            version: PROTOCOL_VERSION,
            hostname: "PC".into(),
            auth_token: "abc".into(),
        };
        let frame = encode_bulk_frame(&msg).unwrap();
        let (decoded, n) = try_decode_bulk_frame(&frame).unwrap();
        assert_eq!(n, frame.len());
        match decoded {
            BulkMsg::Hello { hostname, .. } => assert_eq!(hostname, "PC"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn monitors_roundtrip_and_version_is_9() {
        assert_eq!(PROTOCOL_VERSION, 10, "an older peer cannot decode ClipboardFiles");
        let files = BulkMsg::ClipboardFiles { transfer_ids: vec![7] };
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&files).unwrap()).unwrap();
        assert!(matches!(back, BulkMsg::ClipboardFiles { transfer_ids } if transfer_ids == [7]));
        let drop = BulkMsg::DragDrop { transfer_ids: vec![3, u64::MAX] };
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&drop).unwrap()).unwrap();
        assert!(matches!(back, BulkMsg::DragDrop { transfer_ids } if transfer_ids == [3, u64::MAX]));
        let clip = BulkMsg::Clipboard(ClipboardPayload {
            text: Some("zażółć".into()),
            image_dib: Some(vec![1, 2, 3]),
            files: vec![ClipboardFile { name: "a.png".into(), data: vec![9; 10] }],
            ..Default::default()
        });
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&clip).unwrap()).unwrap();
        assert_eq!(back_payload(back).files[0].data.len(), 10);
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&BulkMsg::CursorLock { locked: true }).unwrap()).unwrap();
        assert!(matches!(back, BulkMsg::CursorLock { locked: true }));
        let msg = BulkMsg::Monitors {
            hostname: "PC2".into(),
            monitors: vec![
                MonitorInfo { label: "A".into(), x: -1920, y: 0, width: 1920, height: 1080, primary: false },
                MonitorInfo { label: "B".into(), x: 0, y: 0, width: 2560, height: 1440, primary: true },
            ],
        };
        let frame = encode_bulk_frame(&msg).unwrap();
        let (decoded, n) = try_decode_bulk_frame(&frame).unwrap();
        assert_eq!(n, frame.len());
        match decoded {
            BulkMsg::Monitors { hostname, monitors } => {
                assert_eq!(hostname, "PC2");
                assert_eq!(monitors.len(), 2);
                assert_eq!(monitors[0].x, -1920);
                assert!(monitors[1].primary);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn enter_ack_and_tcp_input_roundtrip() {
        let ack = BulkMsg::EnterAck { ok: false, reason: Some("ma wyłączone udostępnianie".into()) };
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&ack).unwrap()).unwrap();
        assert!(matches!(back, BulkMsg::EnterAck { ok: false, reason: Some(r) } if r.contains("wyłączone")));

        let input = BulkMsg::Input(InputPacket {
            version: PROTOCOL_VERSION,
            seq: 7,
            msg: InputMsg::Events(vec![InputEvent::MouseMove { dx: 3, dy: -2 }]),
        });
        let (back, _) = try_decode_bulk_frame(&encode_bulk_frame(&input).unwrap()).unwrap();
        match back {
            BulkMsg::Input(pkt) => {
                assert_eq!(pkt.seq, 7);
                assert!(matches!(&pkt.msg, InputMsg::Events(ev) if matches!(ev[0], InputEvent::MouseMove { dx: 3, dy: -2 })));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    fn back_payload(msg: BulkMsg) -> ClipboardPayload {
        match msg {
            BulkMsg::Clipboard(p) => p,
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn clipboard_payload_reports_its_size_and_emptiness() {
        assert!(ClipboardPayload::default().is_empty());
        let p = ClipboardPayload {
            rtf: Some(vec![0; 5]),
            files: vec![ClipboardFile { name: "ab".into(), data: vec![0; 3] }],
            ..Default::default()
        };
        assert!(!p.is_empty());
        assert_eq!(p.byte_len(), 10);
    }

    #[test]
    fn full_file_chunk_fits_in_one_frame() {
        let msg = BulkMsg::FileChunk {
            transfer_id: u64::MAX,
            offset: u64::MAX,
            data: vec![0xAB; FILE_CHUNK_SIZE],
        };
        let frame = encode_bulk_frame(&msg).unwrap();
        assert!(frame.len() - 4 <= MAX_BULK_FRAME as usize);
        let (decoded, n) = try_decode_bulk_frame(&frame).unwrap();
        assert_eq!(n, frame.len());
        match decoded {
            BulkMsg::FileChunk { data, .. } => assert_eq!(data.len(), FILE_CHUNK_SIZE),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn partial_frame_is_incomplete() {
        let frame = encode_bulk_frame(&BulkMsg::Panic).unwrap();
        for cut in 0..frame.len() {
            assert!(matches!(
                try_decode_bulk_frame(&frame[..cut]),
                Err(CodecError::Incomplete)
            ));
        }
    }

    #[test]
    fn oversized_frame_header_is_rejected() {
        let mut buf = (MAX_BULK_FRAME + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(&[0; 16]);
        assert!(matches!(
            try_decode_bulk_frame(&buf),
            Err(CodecError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn two_frames_back_to_back() {
        let mut buf = encode_bulk_frame(&BulkMsg::ClipboardRequest).unwrap();
        buf.extend(
            encode_bulk_frame(&BulkMsg::FileResult {
                transfer_id: 7,
                ok: false,
                reason: Some("nope".into()),
            })
            .unwrap(),
        );
        let (first, n) = try_decode_bulk_frame(&buf).unwrap();
        assert!(matches!(first, BulkMsg::ClipboardRequest));
        let (second, m) = try_decode_bulk_frame(&buf[n..]).unwrap();
        assert_eq!(n + m, buf.len());
        assert!(matches!(
            second,
            BulkMsg::FileResult { transfer_id: 7, ok: false, .. }
        ));
    }

    #[test]
    fn discovery_udp_roundtrip() {
        let msg = DiscoveryMsg::Announce {
            hostname: "A".into(),
            tcp_port: 6555,
            version: PROTOCOL_VERSION,
        };
        let bytes = encode_msg(&msg).unwrap();
        let back: DiscoveryMsg = decode_msg(&bytes).unwrap();
        match back {
            DiscoveryMsg::Announce { hostname, .. } => assert_eq!(hostname, "A"),
        }
    }
}
