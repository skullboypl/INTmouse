//! The whole clipboard ("full clipboard exchange"): plain and formatted text, a picture and
//! copied files, read from and written to the Windows clipboard in one go.

use inputshare_protocol::{ClipboardFile, ClipboardPayload, MAX_CLIPBOARD_BYTES};
use std::path::{Path, PathBuf};

/// Clipboard change counter (Windows `GetClipboardSequenceNumber`); 0 where unavailable.
pub fn clipboard_sequence() -> u32 {
    #[cfg(windows)]
    {
        unsafe { ::windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Most copied files sent on their own after a Ctrl+C (they go with progress, like a file send).
pub const MAX_CLIPBOARD_FILES_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What a copy put on the clipboard: the formats that travel in one message, and the copied
/// files that travel as transfers.
pub struct ClipboardCopy {
    pub payload: ClipboardPayload,
    pub files: Vec<PathBuf>,
}

/// Everything on the clipboard: the formats that fit [`MAX_CLIPBOARD_BYTES`], plus copied files
/// up to [`MAX_CLIPBOARD_FILES_BYTES`] (folders and bigger ones are named in `skipped_files`).
pub fn read_clipboard_copy() -> Option<ClipboardCopy> {
    #[cfg(windows)]
    {
        let raw = win::read()?;
        let mut payload = ClipboardPayload {
            text: raw.text,
            html: raw.html,
            rtf: raw.rtf,
            image_dib: raw.image_dib,
            ..Default::default()
        };
        if payload.byte_len() > MAX_CLIPBOARD_BYTES {
            payload.image_dib = None;
        }
        let sizes: Vec<(PathBuf, Option<u64>)> = raw
            .files
            .into_iter()
            .map(|path| {
                let size = std::fs::metadata(&path).ok().filter(|m| m.is_file()).map(|m| m.len());
                (path, size)
            })
            .collect();
        let (files, skipped) = pick_files(&sizes, MAX_CLIPBOARD_FILES_BYTES as usize);
        payload.skipped_files = skipped;
        Some(ClipboardCopy { payload, files })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Everything on the clipboard that fits [`MAX_CLIPBOARD_BYTES`], copied files carried whole.
/// Reads copied files from disk, so call it off the async runtime.
#[allow(dead_code)]
pub fn read_clipboard_all() -> Option<ClipboardPayload> {
    #[cfg(windows)]
    {
        let raw = win::read()?;
        let mut p = ClipboardPayload {
            text: raw.text,
            html: raw.html,
            rtf: raw.rtf,
            image_dib: raw.image_dib,
            ..Default::default()
        };
        if p.byte_len() > MAX_CLIPBOARD_BYTES {
            // A huge picture: keep the text forms rather than send nothing.
            p.image_dib = None;
        }
        let budget = MAX_CLIPBOARD_BYTES.saturating_sub(p.byte_len());
        let sizes: Vec<(PathBuf, Option<u64>)> = raw
            .files
            .into_iter()
            .map(|path| {
                let size = std::fs::metadata(&path).ok().filter(|m| m.is_file()).map(|m| m.len());
                (path, size)
            })
            .collect();
        let (take, skipped) = pick_files(&sizes, budget);
        p.skipped_files = skipped;
        for path in take {
            let name = file_label(&path);
            match std::fs::read(&path) {
                Ok(data) => p.files.push(ClipboardFile { name, data }),
                Err(_) => p.skipped_files.push(name),
            }
        }
        Some(p)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Put `p` on the clipboard; `files` are the local copies of `p.files` (see [`save_files`]).
pub fn write_clipboard_all(p: &ClipboardPayload, files: &[PathBuf]) -> bool {
    // Tests never touch the real clipboard of the machine running them.
    if cfg!(test) {
        return true;
    }
    #[cfg(windows)]
    {
        win::write(p, files)
    }
    #[cfg(not(windows))]
    {
        let _ = (p, files);
        false
    }
}

/// Save copied files from the other computer into a fresh folder under `root`, so Ctrl+V in
/// Explorer pastes them from there. Names are cleaned (no folders, no device names).
pub fn save_files(root: &Path, files: &[ClipboardFile]) -> std::io::Result<Vec<PathBuf>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = root.join(stamp.to_string());
    std::fs::create_dir_all(&dir)?;
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let name = crate::net::files::sanitize_file_name(&f.name).unwrap_or_else(|| "plik".into());
        let path = crate::net::files::unique_destination(&dir, &name);
        std::fs::write(&path, &f.data)?;
        out.push(path);
    }
    Ok(out)
}

/// Where pasted files from the other computer are kept: `%TEMP%\INTmouse\schowek`.
pub fn clipboard_files_root() -> PathBuf {
    std::env::temp_dir().join("INTmouse").join("schowek")
}

/// Files that fit `budget` bytes, in clipboard order; the rest (folders, unreadable, too big)
/// by name. `None` size = not a plain readable file.
#[cfg(any(windows, test))]
fn pick_files(files: &[(PathBuf, Option<u64>)], budget: usize) -> (Vec<PathBuf>, Vec<String>) {
    let mut left = budget as u64;
    let mut take = Vec::new();
    let mut skipped = Vec::new();
    for (path, size) in files {
        match size {
            Some(n) if *n <= left => {
                left -= n;
                take.push(path.clone());
            }
            _ => skipped.push(file_label(path)),
        }
    }
    (take, skipped)
}

#[cfg(any(windows, test))]
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// `CF_HDROP` contents: a `DROPFILES` header (20 bytes, wide names) and the paths, each ended
/// by a NUL, the list ended by one more.
#[cfg(any(windows, test))]
fn dropfiles_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&20u32.to_le_bytes()); // pFiles: names start after the header
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0i32.to_le_bytes()); // fNC
    out.extend_from_slice(&1i32.to_le_bytes()); // fWide
    for p in paths {
        for u in p.to_string_lossy().encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(windows)]
mod win {
    use super::dropfiles_bytes;
    use inputshare_protocol::ClipboardPayload;
    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use ::windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use ::windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
    use ::windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use std::path::PathBuf;

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

    pub struct Raw {
        pub text: Option<String>,
        pub html: Option<Vec<u8>>,
        pub rtf: Option<Vec<u8>>,
        pub image_dib: Option<Vec<u8>>,
        pub files: Vec<PathBuf>,
    }

    fn format_id(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
    }

    /// Clipboard open for this scope (closed on drop, also on early return).
    struct Open;
    impl Open {
        fn new() -> Option<Self> {
            // Another program may hold the clipboard for a moment.
            for _ in 0..5 {
                if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
                    return Some(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            None
        }
    }
    impl Drop for Open {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    unsafe fn bytes_of(format: u32) -> Option<Vec<u8>> {
        if format == 0 || IsClipboardFormatAvailable(format).is_err() {
            return None;
        }
        let handle = GetClipboardData(format).ok().filter(|h| !h.is_invalid())?;
        let hglobal = HGLOBAL(handle.0);
        let size = GlobalSize(hglobal);
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() || size == 0 {
            return None;
        }
        let data = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
        let _ = GlobalUnlock(hglobal);
        Some(data)
    }

    fn trim_nul(mut v: Vec<u8>) -> Vec<u8> {
        while v.last() == Some(&0) {
            v.pop();
        }
        v
    }

    pub fn read() -> Option<Raw> {
        let _open = Open::new()?;
        unsafe {
            let text = bytes_of(CF_UNICODETEXT).map(|b| {
                let wide: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let end = wide.iter().position(|&u| u == 0).unwrap_or(wide.len());
                String::from_utf16_lossy(&wide[..end])
            });
            let html = bytes_of(format_id("HTML Format")).map(trim_nul);
            let rtf = bytes_of(format_id("Rich Text Format")).map(trim_nul);
            let image_dib = bytes_of(CF_DIB);
            let mut files = Vec::new();
            if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
                if let Ok(h) = GetClipboardData(CF_HDROP) {
                    let drop = HDROP(h.0);
                    let count = DragQueryFileW(drop, u32::MAX, None);
                    for i in 0..count {
                        let len = DragQueryFileW(drop, i, None) as usize;
                        let mut buf = vec![0u16; len + 1];
                        let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                        files.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
                    }
                }
            }
            Some(Raw { text, html, rtf, image_dib, files })
        }
    }

    unsafe fn put(format: u32, data: &[u8]) -> bool {
        if format == 0 || data.is_empty() {
            return false;
        }
        let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, data.len()) else {
            return false;
        };
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
        let _ = GlobalUnlock(hmem);
        SetClipboardData(format, HANDLE(hmem.0)).is_ok()
    }

    pub fn write(p: &ClipboardPayload, files: &[PathBuf]) -> bool {
        let Some(_open) = Open::new() else {
            return false;
        };
        unsafe {
            if EmptyClipboard().is_err() {
                return false;
            }
            let mut any = false;
            if let Some(t) = &p.text {
                let wide: Vec<u8> = t.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
                any |= put(CF_UNICODETEXT, &wide);
            }
            if let Some(h) = &p.html {
                let mut h = h.clone();
                h.push(0);
                any |= put(format_id("HTML Format"), &h);
            }
            if let Some(r) = &p.rtf {
                let mut r = r.clone();
                r.push(0);
                any |= put(format_id("Rich Text Format"), &r);
            }
            if let Some(img) = &p.image_dib {
                any |= put(CF_DIB, img);
            }
            if !files.is_empty() {
                any |= put(CF_HDROP, &dropfiles_bytes(files));
                // Paste copies (Explorer would otherwise ask what to do with the files).
                any |= put(format_id("Preferred DropEffect"), &1u32.to_le_bytes());
            }
            any
        }
    }
}

// Pure helpers only: the clipboard is never opened here.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_that_fit_go_and_folders_or_big_ones_are_named_as_skipped() {
        let files = vec![
            (PathBuf::from(r"C:\a\photo.png"), Some(600)),
            (PathBuf::from(r"C:\a\folder"), None),
            (PathBuf::from(r"C:\a\movie.mp4"), Some(5_000)),
            (PathBuf::from(r"C:\a\note.txt"), Some(300)),
        ];
        let (take, skipped) = pick_files(&files, 1000);
        assert_eq!(take, vec![PathBuf::from(r"C:\a\photo.png"), PathBuf::from(r"C:\a\note.txt")]);
        assert_eq!(skipped, vec!["folder".to_string(), "movie.mp4".to_string()]);
    }

    #[test]
    fn dropfiles_has_a_wide_header_and_a_double_nul_list() {
        let b = dropfiles_bytes(&[PathBuf::from(r"C:\x\a.txt"), PathBuf::from(r"C:\b")]);
        assert_eq!(&b[0..4], &20u32.to_le_bytes(), "names start after the 20-byte header");
        assert_eq!(&b[16..20], &1i32.to_le_bytes(), "wide characters");
        let names: Vec<u16> = b[20..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&names).unwrap();
        assert_eq!(text, "C:\\x\\a.txt\0C:\\b\0\0");
    }

    #[test]
    fn saved_files_get_clean_names_in_a_fresh_folder() {
        let root = std::env::temp_dir().join(format!("intmouse-clip-test-{}", std::process::id()));
        let files = vec![
            ClipboardFile { name: r"..\..\evil.txt".into(), data: b"x".to_vec() },
            ClipboardFile { name: "evil.txt".into(), data: b"y".to_vec() },
        ];
        let paths = save_files(&root, &files).unwrap();
        assert_eq!(paths.len(), 2);
        for p in &paths {
            assert!(p.starts_with(&root), "stays inside the clipboard folder: {p:?}");
        }
        assert_ne!(paths[0], paths[1], "same name twice does not overwrite");
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"y");
        let _ = std::fs::remove_dir_all(&root);
    }
}
