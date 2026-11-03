//! File transfer over the TCP bulk link.
//!
//! Wire flow (sender → receiver): `FileOffer` → `FileChunk`* → `FileDone`,
//! or `FileCancel` on a sender-side error. The receiver answers with one
//! `FileResult` (saved / rejected / failed).
//!
//! Sending runs on a blocking thread and pushes frames into a small bounded
//! per-peer queue (backpressure), never touching the UDP input path.
//! Receiving streams chunks to a `.part` file (no whole-file RAM buffer) and
//! renames it into the download directory on `FileDone`.

use inputshare_protocol::{BulkMsg, FILE_CHUNK_SIZE};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// Bounded depth of the per-peer file frame queue (≈ 1 MiB of chunks in flight).
pub const FILE_QUEUE_DEPTH: usize = 4;
/// Concurrent incoming transfers accepted from one peer connection.
pub const MAX_INCOMING_PER_PEER: usize = 8;
/// Finished transfers kept in the status list.
const KEEP_FINISHED: usize = 20;
const MAX_NAME_CHARS: usize = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    InProgress,
    /// Sender: every byte queued, waiting for the receiver's `FileResult`.
    AwaitingConfirm,
    Done,
    Failed(String),
}

impl TransferState {
    pub fn is_finished(&self) -> bool {
        matches!(self, TransferState::Done | TransferState::Failed(_))
    }
}

/// One entry of [`super::hub::NetStatus::transfers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferStatus {
    pub transfer_id: u64,
    pub direction: TransferDirection,
    /// Peer key (hostname / host string of the TCP link).
    pub peer: String,
    /// File name (sanitized on the receive side).
    pub name: String,
    pub size: u64,
    pub transferred: u64,
    pub state: TransferState,
    /// Receive side: where the file was saved.
    pub saved_to: Option<PathBuf>,
}

/// Add a transfer and drop the oldest finished ones beyond the history limit.
pub fn push_transfer(list: &mut Vec<FileTransferStatus>, t: FileTransferStatus) {
    list.push(t);
    let finished = list.iter().filter(|t| t.state.is_finished()).count();
    if finished > KEEP_FINISHED {
        let mut drop = finished - KEEP_FINISHED;
        list.retain(|t| {
            if drop > 0 && t.state.is_finished() {
                drop -= 1;
                false
            } else {
                true
            }
        });
    }
}

pub fn find_transfer<'a>(
    list: &'a mut [FileTransferStatus],
    dir: TransferDirection,
    peer: &str,
    id: u64,
) -> Option<&'a mut FileTransferStatus> {
    list.iter_mut()
        .find(|t| t.direction == dir && t.transfer_id == id && t.peer == peer)
}

/// `max_file_size_mb` → byte limit (`0` = unlimited).
pub fn max_file_bytes(max_mb: u64) -> Option<u64> {
    (max_mb != 0).then(|| max_mb.saturating_mul(1024 * 1024))
}

// ─── name sanitization ───────────────────────────────────────────────────────

const RESERVED_WIN_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5",
    "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
    "LPT8", "LPT9",
];

/// Turn a peer-supplied file name into a safe single path component.
///
/// Drops any directory part (both `/` and `\`), drive / stream separators,
/// characters Windows forbids, control chars, trailing dots/spaces, and
/// prefixes reserved device names. Returns `None` if nothing usable is left.
pub fn sanitize_file_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        return None;
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let name = if RESERVED_WIN_NAMES.contains(&stem.as_str()) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };
    Some(truncate_name(name))
}

fn truncate_name(name: String) -> String {
    if name.chars().count() <= MAX_NAME_CHARS {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name[i..].chars().count() <= 16 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let keep = MAX_NAME_CHARS - ext.chars().count();
    let stem: String = stem.chars().take(keep).collect();
    format!("{stem}{ext}")
}

/// `dir/name`, or `dir/stem (n).ext` if that already exists.
pub fn unique_destination(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1..10_000u32 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem} ({}){ext}", std::process::id()))
}

// ─── receive ─────────────────────────────────────────────────────────────────

/// Receiver state of one transfer.
#[derive(Debug)]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
    pub received: u64,
    dir: PathBuf,
    part_path: PathBuf,
    file: Option<std::fs::File>,
}

impl IncomingFile {
    /// Validate the offer and create the `.part` file inside `dir`.
    pub fn open(
        dir: &Path,
        raw_name: &str,
        size: u64,
        transfer_id: u64,
        limit: Option<u64>,
    ) -> Result<Self, String> {
        if let Some(max) = limit {
            if size > max {
                return Err(format!(
                    "plik za duży: {size} B > limit {max} B (max_file_size_mb)"
                ));
            }
        }
        let name = sanitize_file_name(raw_name).unwrap_or_else(|| format!("plik_{transfer_id}"));
        std::fs::create_dir_all(dir).map_err(|e| format!("katalog {}: {e}", dir.display()))?;
        let part_path = dir.join(format!("{name}.{transfer_id:x}.inputshare-part"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part_path)
            .map_err(|e| format!("nie można utworzyć {}: {e}", part_path.display()))?;
        Ok(Self {
            name,
            size,
            received: 0,
            dir: dir.to_path_buf(),
            part_path,
            file: Some(file),
        })
    }

    /// Append one chunk (must be contiguous and within the announced size).
    pub async fn write_chunk(&mut self, offset: u64, data: Vec<u8>) -> Result<(), String> {
        if offset != self.received {
            return Err(format!(
                "chunk poza kolejnością: offset {offset}, oczekiwano {}",
                self.received
            ));
        }
        let new_len = self.received + data.len() as u64;
        if new_len > self.size {
            return Err(format!(
                "nadawca przysłał więcej danych ({new_len} B) niż zapowiedział ({} B)",
                self.size
            ));
        }
        let Some(mut file) = self.file.take() else {
            return Err("plik już zamknięty".into());
        };
        let (file, res) = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let r = file.write_all(&data);
            (file, r)
        })
        .await
        .map_err(|e| format!("zapis: {e}"))?;
        self.file = Some(file);
        res.map_err(|e| format!("zapis {}: {e}", self.part_path.display()))?;
        self.received = new_len;
        Ok(())
    }

    /// Close and move the `.part` file to its final unique name.
    pub async fn finish(mut self) -> Result<PathBuf, String> {
        if self.received != self.size {
            let msg = format!(
                "niekompletny plik: {} z {} B",
                self.received, self.size
            );
            self.discard();
            return Err(msg);
        }
        let file = self.file.take();
        let part = self.part_path.clone();
        let dir = self.dir.clone();
        let name = self.name.clone();
        let res = tokio::task::spawn_blocking(move || {
            drop(file);
            let dest = unique_destination(&dir, &name);
            std::fs::rename(&part, &dest).map(|()| dest)
        })
        .await
        .map_err(|e| format!("zapis: {e}"))?;
        match res {
            Ok(dest) => Ok(dest),
            Err(e) => {
                let msg = format!("zmiana nazwy {}: {e}", self.part_path.display());
                self.discard();
                Err(msg)
            }
        }
    }

    /// Drop the partial file.
    pub fn discard(mut self) {
        self.file.take();
        if let Err(e) = std::fs::remove_file(&self.part_path) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!("cannot remove {}: {e}", self.part_path.display());
            }
        }
    }
}

// ─── send ────────────────────────────────────────────────────────────────────

/// One file queued for sending (validated up front).
#[derive(Debug, Clone)]
pub struct OutgoingFile {
    pub transfer_id: u64,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

impl OutgoingFile {
    /// Check that `path` is a regular file within `limit`.
    pub fn prepare(path: PathBuf, transfer_id: u64, limit: Option<u64>) -> Result<Self, String> {
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            return Err(format!(
                "{}: wysyłanie katalogów nie jest obsługiwane",
                path.display()
            ));
        }
        if !meta.is_file() {
            return Err(format!("{}: to nie jest zwykły plik", path.display()));
        }
        let size = meta.len();
        if let Some(max) = limit {
            if size > max {
                return Err(format!(
                    "{}: plik za duży ({size} B > limit {max} B)",
                    path.display()
                ));
            }
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{}: brak nazwy pliku", path.display()))?;
        Ok(Self {
            transfer_id,
            path,
            name,
            size,
        })
    }
}

/// Stream one file into the peer's bounded file queue. Blocking — call from
/// `spawn_blocking`. `cancelled` is polled per chunk (receiver reject / user
/// cancel); `progress` gets the running byte count.
pub fn send_file_blocking(
    tx: &mpsc::Sender<BulkMsg>,
    job: &OutgoingFile,
    cancelled: impl Fn() -> Option<String>,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let closed = || "połączenie z peerem zamknięte".to_string();
    let mut file =
        std::fs::File::open(&job.path).map_err(|e| format!("{}: {e}", job.path.display()))?;
    tx.blocking_send(BulkMsg::FileOffer {
        name: job.name.clone(),
        size: job.size,
        transfer_id: job.transfer_id,
    })
    .map_err(|_| closed())?;

    let result = stream_chunks(tx, job, &mut file, &cancelled, &mut progress);
    match result {
        Ok(()) => tx
            .blocking_send(BulkMsg::FileDone {
                transfer_id: job.transfer_id,
            })
            .map_err(|_| closed()),
        Err(reason) => {
            let _ = tx.blocking_send(BulkMsg::FileCancel {
                transfer_id: job.transfer_id,
                reason: reason.clone(),
            });
            Err(reason)
        }
    }
}

fn stream_chunks(
    tx: &mpsc::Sender<BulkMsg>,
    job: &OutgoingFile,
    file: &mut std::fs::File,
    cancelled: &impl Fn() -> Option<String>,
    progress: &mut impl FnMut(u64),
) -> Result<(), String> {
    let mut buf = vec![0u8; FILE_CHUNK_SIZE];
    let mut offset = 0u64;
    loop {
        if let Some(reason) = cancelled() {
            return Err(reason);
        }
        let n = read_full(file, &mut buf).map_err(|e| format!("odczyt {}: {e}", job.path.display()))?;
        if n == 0 {
            break;
        }
        if offset + n as u64 > job.size {
            return Err(format!("{}: plik urósł w trakcie wysyłania", job.path.display()));
        }
        tx.blocking_send(BulkMsg::FileChunk {
            transfer_id: job.transfer_id,
            offset,
            data: buf[..n].to_vec(),
        })
        .map_err(|_| "połączenie z peerem zamknięte".to_string())?;
        offset += n as u64;
        progress(offset);
    }
    if offset != job.size {
        return Err(format!(
            "{}: plik zmienił rozmiar w trakcie wysyłania ({offset} z {} B)",
            job.path.display(),
            job.size
        ));
    }
    Ok(())
}

/// Read until `buf` is full or EOF.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "inputshare-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn traversal_and_separators_are_stripped() {
        assert_eq!(sanitize_file_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            sanitize_file_name("..\\..\\Windows\\System32\\evil.dll").as_deref(),
            Some("evil.dll")
        );
        assert_eq!(sanitize_file_name("C:\\Users\\x\\a.txt").as_deref(), Some("a.txt"));
        assert_eq!(sanitize_file_name("C:a.txt").as_deref(), Some("Ca.txt"));
        assert_eq!(sanitize_file_name("\\\\server\\share\\b.bin").as_deref(), Some("b.bin"));
        assert_eq!(sanitize_file_name("file.txt:ads").as_deref(), Some("file.txtads"));
        assert_eq!(sanitize_file_name("..").as_deref(), None);
        assert_eq!(sanitize_file_name(".").as_deref(), None);
        assert_eq!(sanitize_file_name("dir/").as_deref(), None);
        assert_eq!(sanitize_file_name("  ").as_deref(), None);
        assert_eq!(sanitize_file_name("....").as_deref(), None);
    }

    #[test]
    fn forbidden_chars_reserved_names_and_trailing_dots() {
        assert_eq!(sanitize_file_name("a<b>c|d?e*f\"g").as_deref(), Some("abcdefg"));
        assert_eq!(sanitize_file_name("bad\u{0}\u{7}name").as_deref(), Some("badname"));
        assert_eq!(sanitize_file_name("report.pdf. . ").as_deref(), Some("report.pdf"));
        assert_eq!(sanitize_file_name("CON").as_deref(), Some("_CON"));
        assert_eq!(sanitize_file_name("nul.txt").as_deref(), Some("_nul.txt"));
        assert_eq!(sanitize_file_name("com1.tar.gz").as_deref(), Some("_com1.tar.gz"));
        assert_eq!(sanitize_file_name("console.txt").as_deref(), Some("console.txt"));
        assert_eq!(sanitize_file_name(".gitignore").as_deref(), Some(".gitignore"));
        assert_eq!(sanitize_file_name("zażółć.txt").as_deref(), Some("zażółć.txt"));
    }

    #[test]
    fn long_names_keep_extension() {
        let long = format!("{}.docx", "a".repeat(500));
        let s = sanitize_file_name(&long).unwrap();
        assert_eq!(s.chars().count(), MAX_NAME_CHARS);
        assert!(s.ends_with(".docx"));
    }

    #[test]
    fn unique_destination_does_not_overwrite() {
        let dir = temp_dir("unique");
        assert_eq!(unique_destination(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_destination(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_destination(&dir, "a.txt"), dir.join("a (2).txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_limit_conversion() {
        assert_eq!(max_file_bytes(0), None);
        assert_eq!(max_file_bytes(1), Some(1024 * 1024));
        assert_eq!(max_file_bytes(u64::MAX), Some(u64::MAX));
    }

    #[test]
    fn history_is_bounded() {
        let mut list = Vec::new();
        for i in 0..(KEEP_FINISHED as u64 + 5) {
            push_transfer(
                &mut list,
                FileTransferStatus {
                    transfer_id: i,
                    direction: TransferDirection::Send,
                    peer: "p".into(),
                    name: "n".into(),
                    size: 1,
                    transferred: 1,
                    state: TransferState::Done,
                    saved_to: None,
                },
            );
        }
        assert_eq!(list.len(), KEEP_FINISHED);
        assert_eq!(list[0].transfer_id, 5);
    }

    #[tokio::test]
    async fn incoming_rejects_out_of_order_overflow_and_oversize() {
        let dir = temp_dir("incoming");
        assert!(IncomingFile::open(&dir, "x.bin", 10, 1, Some(5)).is_err());

        let mut inc = IncomingFile::open(&dir, "../x.bin", 4, 2, Some(5)).unwrap();
        assert_eq!(inc.name, "x.bin");
        assert!(inc.write_chunk(1, vec![1]).await.is_err());
        inc.write_chunk(0, vec![1, 2]).await.unwrap();
        assert!(inc.write_chunk(2, vec![3, 4, 5]).await.is_err());
        inc.write_chunk(2, vec![3, 4]).await.unwrap();
        let saved = inc.finish().await.unwrap();
        assert_eq!(saved, dir.join("x.bin"));
        assert_eq!(std::fs::read(&saved).unwrap(), vec![1, 2, 3, 4]);

        let inc = IncomingFile::open(&dir, "short.bin", 4, 3, None).unwrap();
        assert!(inc.finish().await.is_err());
        // Only the completed file is left — partials are removed.
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["x.bin".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_rejects_dirs_and_oversize() {
        let dir = temp_dir("prepare");
        assert!(OutgoingFile::prepare(dir.clone(), 1, None).is_err());
        let f = dir.join("f.bin");
        std::fs::write(&f, vec![0u8; 2048]).unwrap();
        assert!(OutgoingFile::prepare(f.clone(), 1, Some(1024)).is_err());
        let ok = OutgoingFile::prepare(f, 1, Some(4096)).unwrap();
        assert_eq!(ok.size, 2048);
        assert_eq!(ok.name, "f.bin");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
