//! Optional append log to `config_dir/inputshare.log`.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;

/// How many of the latest log lines the Logi tab can show.
const RING_CAP: usize = 3000;

static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
/// Goes up whenever the ring changes, so the Logi tab rebuilds its view only then.
static RING_REV: AtomicU64 = AtomicU64::new(0);

/// Changes whenever a line is added or the log is cleared.
pub fn log_revision() -> u64 {
    RING_REV.load(Ordering::Relaxed)
}

/// How often the buffered log file is written out (detailed mode writes many lines a second;
/// a disk write per line made the whole program lag).
const FILE_FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

fn ring() -> std::sync::MutexGuard<'static, VecDeque<String>> {
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAP)))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Removes `ESC [ ... m` colour sequences.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn ring_push(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    let mut ring = ring();
    for line in text.lines() {
        let line = strip_ansi(line);
        if line.trim().is_empty() {
            continue;
        }
        if ring.len() == RING_CAP {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    RING_REV.fetch_add(1, Ordering::Relaxed);
}

/// The latest log lines, oldest first (what the Logi tab shows).
pub fn recent_log_lines() -> Vec<String> {
    ring().iter().cloned().collect()
}

pub fn clear_recent_log_lines() {
    ring().clear();
    RING_REV.fetch_add(1, Ordering::Relaxed);
}

type DetailSwitch = Box<dyn Fn(bool) + Send + Sync>;
static DETAIL_SWITCH: OnceLock<DetailSwitch> = OnceLock::new();

/// Registers what turns detailed (debug) logging on or off while the program runs.
pub fn install_detail_switch(f: impl Fn(bool) + Send + Sync + 'static) {
    let _ = DETAIL_SWITCH.set(Box::new(f));
}

/// Shared toggle + file handle for a tee writer (stderr + optional file).
#[derive(Clone)]
pub struct DiagLog {
    path: PathBuf,
    file: Arc<Mutex<Option<BufWriter<File>>>>,
    /// The thread that writes the buffered file out runs once per log.
    flusher: Arc<std::sync::Once>,
    /// Last state passed to the detail switch, so it is only called when it changes.
    detailed: Arc<AtomicBool>,
}

impl DiagLog {
    pub fn path_for_config(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("inputshare.log")
    }

    pub fn new(config_path: &Path, enabled: bool) -> Self {
        let path = Self::path_for_config(config_path);
        let file = Arc::new(Mutex::new(None));
        let s = Self {
            path,
            file,
            flusher: Arc::new(std::sync::Once::new()),
            detailed: Arc::new(AtomicBool::new(enabled)),
        };
        if enabled {
            s.open();
        }
        s
    }

    pub fn log_path(&self) -> &Path {
        &self.path
    }

    /// Write the buffered lines out now (the log thread does it every [`FILE_FLUSH_EVERY`]).
    #[cfg(test)]
    pub fn flush_file(&self) {
        if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                let _ = f.flush();
            }
        }
    }

    pub fn set_enabled(&self, on: bool) {
        if self.detailed.swap(on, Ordering::Relaxed) != on {
            if let Some(switch) = DETAIL_SWITCH.get() {
                switch(on);
            }
        }
        if on {
            self.open();
        } else if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                let _ = f.flush();
            }
            *g = None;
        }
    }

    fn open(&self) {
        if let Ok(mut g) = self.file.lock() {
            if g.is_some() {
                return;
            }
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(f) => {
                    *g = Some(BufWriter::with_capacity(64 * 1024, f));
                    let file = self.file.clone();
                    self.flusher.call_once(move || {
                        let _ = std::thread::Builder::new().name("inputshare-log".into()).spawn(move || loop {
                            std::thread::sleep(FILE_FLUSH_EVERY);
                            if let Ok(mut g) = file.lock() {
                                if let Some(f) = g.as_mut() {
                                    let _ = f.flush();
                                }
                            }
                        });
                    });
                    tracing::info!("Diagnostyka: dopisywanie do {}", self.path.display());
                }
                Err(e) => {
                    tracing::warn!("Nie otwarto {}: {e}", self.path.display());
                }
            }
        }
    }
}

/// Writes every log line to stderr and, when open, to the diag file.
pub struct TeeMakeWriter {
    inner: DiagLog,
}

impl TeeMakeWriter {
    pub fn new(diag: DiagLog) -> Self {
        Self { inner: diag }
    }
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            file: self.inner.file.clone(),
        }
    }
}

pub struct TeeWriter {
    file: Arc<Mutex<Option<BufWriter<File>>>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        ring_push(buf);
        if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                // Buffered: the log thread writes it out every FILE_FLUSH_EVERY.
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

// File tests run in a unique directory under the OS temp dir and remove it afterwards;
// nothing outside that directory (and no config / registry) is touched.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "inputshare-diag-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.toml")
        }

        fn log_text(&self) -> String {
            std::fs::read_to_string(self.0.join("inputshare.log")).unwrap_or_default()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn file_open(log: &DiagLog) -> bool {
        log.file.lock().unwrap().is_some()
    }

    #[test]
    fn log_path_sits_next_to_config() {
        assert_eq!(
            DiagLog::path_for_config(Path::new("cfg").join("app").join("config.toml").as_path()),
            Path::new("cfg").join("app").join("inputshare.log")
        );
    }

    #[test]
    fn log_path_for_bare_file_name_is_relative() {
        assert_eq!(
            DiagLog::path_for_config(Path::new("config.toml")),
            PathBuf::from("inputshare.log")
        );
    }

    #[test]
    fn log_path_without_parent_falls_back_to_dot() {
        assert_eq!(
            DiagLog::path_for_config(Path::new("")),
            Path::new(".").join("inputshare.log")
        );
    }

    #[test]
    fn disabled_log_does_not_create_the_file() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), false);
        assert!(!file_open(&log));
        assert!(!log.log_path().exists());
        assert_eq!(log.log_path(), dir.0.join("inputshare.log"));
    }

    #[test]
    fn enabled_log_creates_the_file_and_tee_writes_into_it() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), true);
        assert!(file_open(&log));
        assert!(log.log_path().exists());

        let maker = TeeMakeWriter::new(log.clone());
        let mut w = maker.make_writer();
        assert_eq!(w.write(b"hello diag\n").unwrap(), 11);
        w.flush().unwrap();
        log.flush_file();
        assert_eq!(dir.log_text(), "hello diag\n");
    }

    #[test]
    fn writes_are_dropped_from_file_while_disabled() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), false);
        let maker = TeeMakeWriter::new(log.clone());

        let mut w = maker.make_writer();
        // Still reports the full length written (stderr always gets it).
        assert_eq!(w.write(b"ignored\n").unwrap(), 8);
        assert!(!log.log_path().exists());
    }

    #[test]
    fn toggle_closes_and_reopens_in_append_mode() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), true);
        let maker = TeeMakeWriter::new(log.clone());

        maker.make_writer().write_all(b"one\n").unwrap();
        log.set_enabled(false);
        assert!(!file_open(&log));
        maker.make_writer().write_all(b"skipped\n").unwrap();

        log.set_enabled(true);
        assert!(file_open(&log));
        maker.make_writer().write_all(b"two\n").unwrap();

        log.flush_file();
        assert_eq!(dir.log_text(), "one\ntwo\n");
    }

    #[test]
    fn enabling_twice_keeps_the_same_handle() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), true);
        let maker = TeeMakeWriter::new(log.clone());
        maker.make_writer().write_all(b"a\n").unwrap();
        log.set_enabled(true);
        maker.make_writer().write_all(b"b\n").unwrap();
        log.flush_file();
        assert_eq!(dir.log_text(), "a\nb\n");
    }

    #[test]
    fn existing_log_content_is_preserved_across_runs() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join("inputshare.log"), "old run\n").unwrap();
        let log = DiagLog::new(&dir.config_path(), true);
        let maker = TeeMakeWriter::new(log.clone());
        maker.make_writer().write_all(b"new run\n").unwrap();
        log.flush_file();
        assert_eq!(dir.log_text(), "old run\nnew run\n");
    }

    #[test]
    fn unopenable_path_leaves_log_closed_without_panicking() {
        let dir = TempDir::new();
        let missing = dir.0.join("no-such-dir").join("config.toml");
        let log = DiagLog::new(&missing, true);
        assert!(!file_open(&log));
        // Writing through the tee must still succeed (stderr only).
        assert_eq!(TeeMakeWriter::new(log).make_writer().write(b"x").unwrap(), 1);
    }

    #[test]
    fn writers_from_clones_share_the_same_toggle() {
        let dir = TempDir::new();
        let log = DiagLog::new(&dir.config_path(), false);
        let maker = TeeMakeWriter::new(log.clone());
        let mut early = maker.make_writer(); // created while disabled
        log.set_enabled(true);
        early.write_all(b"late bind\n").unwrap();
        log.flush_file();
        assert_eq!(dir.log_text(), "late bind\n");
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    // The ring is process-wide and the other diag tests write to it too, so these tests only
    // look at lines carrying their own marker.
    #[test]
    fn colour_escapes_are_stripped_and_lines_are_kept_in_order() {
        ring_push(b"\x1b[2mringtest-a\x1b[0m  \x1b[32m INFO\x1b[0m hello\n");
        ring_push(b"ringtest-b second\nringtest-c third\n\n");
        let own: Vec<String> = recent_log_lines().into_iter().filter(|l| l.starts_with("ringtest-")).collect();
        assert_eq!(own, ["ringtest-a   INFO hello", "ringtest-b second", "ringtest-c third"]);
    }

    #[test]
    fn the_ring_keeps_only_the_newest_lines() {
        for i in 0..(RING_CAP + 25) {
            ring_push(format!("ringcap-line {i}\n").as_bytes());
        }
        let lines = recent_log_lines();
        assert_eq!(lines.len(), RING_CAP);
        assert!(!lines.iter().any(|l| l == "ringcap-line 0"), "the oldest lines are dropped");
        assert_eq!(lines.last().unwrap(), &format!("ringcap-line {}", RING_CAP + 24));
    }
}
