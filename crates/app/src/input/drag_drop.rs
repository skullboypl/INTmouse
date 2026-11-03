//! Dragging files across the edge to the other computer.
//!
//! Steering side: the cursor shield is also an OLE drop target. When a drag from Explorer carries
//! the cursor over the edge, the parked cursor sits on the shield, so Explorer offers its files to
//! it. Letting go of the button there hands the file list to the hub, which sends the files with
//! `BulkMsg::DragDrop`.
//!
//! Steered side: once the files are saved, a drag of them starts under the cursor and lets go by
//! itself shortly after, so they land in the folder or window there.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Drag & drop across the edge is on (sharing and "allow drag & drop" both on).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Drops waiting for the hub.
static DROPPED: Mutex<Vec<DroppedFiles>> = Mutex::new(Vec::new());

/// Files let go over the shield while steering `host`.
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedFiles {
    pub host: String,
    pub files: Vec<PathBuf>,
    /// Dragged along but not sent (folders).
    pub skipped: Vec<String>,
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Drops made since the last call, oldest first.
pub fn take_dropped() -> Vec<DroppedFiles> {
    DROPPED.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
}

#[cfg(any(windows, test))]
fn push_dropped(d: DroppedFiles) {
    if let Ok(mut g) = DROPPED.lock() {
        g.push(d);
    }
}

/// Split dragged paths into files that can be sent and the names of the rest (folders, gone).
#[cfg(any(windows, test))]
fn split_sendable(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<String>) {
    let (files, rest): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_file());
    let skipped = rest
        .iter()
        .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()))
        .collect();
    (files, skipped)
}

/// How long the drag on the steered computer stays under the cursor before it lets go, so the
/// window there has entered it.
#[cfg(any(windows, test))]
const LET_GO_AFTER: Duration = Duration::from_millis(250);

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq)]
enum SourceStep {
    Continue,
    Drop,
    Cancel,
}

#[cfg(any(windows, test))]
fn source_step(escape: bool, elapsed: Duration) -> SourceStep {
    if escape {
        SourceStep::Cancel
    } else if elapsed >= LET_GO_AFTER {
        SourceStep::Drop
    } else {
        SourceStep::Continue
    }
}

/// Drop `files` (all in one folder) under the cursor of this computer, on a thread of its own.
pub fn drop_at_cursor(files: Vec<PathBuf>) {
    #[cfg(windows)]
    {
        let _ = std::thread::Builder::new()
            .name("inputshare-drop".into())
            .spawn(move || win::drag_out(files));
    }
    #[cfg(not(windows))]
    {
        let _ = files;
    }
}

#[cfg(windows)]
pub(super) use win::register_shield_target;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::input::CaptureControl;
    use ::windows::core::{implement, HRESULT, PCWSTR};
    use ::windows::Win32::Foundation::{
        BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINTL, S_OK,
    };
    use ::windows::Win32::System::Com::{IBindCtx, IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use ::windows::Win32::System::Ole::{
        DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
        RegisterDragDrop, ReleaseStgMedium, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
    };
    use ::windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use ::windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use ::windows::Win32::UI::Shell::{
        DragQueryFileW, ILClone, ILFindLastID, ILFree, ILRemoveLastID, SHCreateDataObject, SHParseDisplayName, HDROP,
    };
    use std::os::windows::ffi::OsStrExt;
    use std::time::Instant;

    const CF_HDROP: u16 = 15;

    /// Make the shield window a drop target. Call on the shield's thread, which pumps its messages.
    pub fn register_shield_target(hwnd: HWND, ctrl: CaptureControl) -> Option<IDropTarget> {
        unsafe {
            if let Err(e) = OleInitialize(None) {
                tracing::warn!("OleInitialize failed ({e}); files cannot be dragged to the other computer");
                return None;
            }
            let target: IDropTarget = ShieldTarget { ctrl, files: Mutex::new(None) }.into();
            match RegisterDragDrop(hwnd, &target) {
                Ok(()) => Some(target),
                Err(e) => {
                    tracing::warn!("RegisterDragDrop failed ({e}); files cannot be dragged to the other computer");
                    None
                }
            }
        }
    }

    #[implement(IDropTarget)]
    struct ShieldTarget {
        ctrl: CaptureControl,
        /// What the drag under way carries: files to send and skipped names (`None` = not taken).
        files: Mutex<Option<(Vec<PathBuf>, Vec<String>)>>,
    }

    impl ShieldTarget {
        fn take_drag(&self, data: Option<&IDataObject>, offered: DROPEFFECT) -> DROPEFFECT {
            let taken = data
                .filter(|_| ENABLED.load(Ordering::Relaxed) && self.ctrl.capturing.load(Ordering::SeqCst))
                .filter(|_| offered.0 & DROPEFFECT_COPY.0 != 0)
                .and_then(dropped_paths)
                .map(split_sendable)
                .filter(|(files, _)| !files.is_empty());
            let effect = if taken.is_some() { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            if let Ok(mut g) = self.files.lock() {
                *g = taken;
            }
            effect
        }

        fn effect(&self) -> DROPEFFECT {
            let taken = self.files.lock().is_ok_and(|g| g.is_some());
            if taken && self.ctrl.capturing.load(Ordering::SeqCst) {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            }
        }
    }

    impl IDropTarget_Impl for ShieldTarget_Impl {
        fn DragEnter(
            &self,
            data: Option<&IDataObject>,
            _keys: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> ::windows::core::Result<()> {
            unsafe {
                *effect = self.take_drag(data, *effect);
            }
            Ok(())
        }

        fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> ::windows::core::Result<()> {
            unsafe {
                *effect = self.effect();
            }
            Ok(())
        }

        fn DragLeave(&self) -> ::windows::core::Result<()> {
            if let Ok(mut g) = self.files.lock() {
                *g = None;
            }
            Ok(())
        }

        fn Drop(
            &self,
            _data: Option<&IDataObject>,
            _keys: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> ::windows::core::Result<()> {
            let out = self.effect();
            let taken = self.files.lock().ok().and_then(|mut g| g.take());
            let host = self.ctrl.target_host.lock().ok().and_then(|g| g.clone());
            match (out == DROPEFFECT_COPY, taken, host) {
                (true, Some((files, skipped)), Some(host)) => {
                    tracing::info!(%host, files = files.len(), skipped = skipped.len(), "files dropped across the edge");
                    push_dropped(DroppedFiles { host, files, skipped });
                    unsafe { *effect = DROPEFFECT_COPY };
                }
                _ => unsafe { *effect = DROPEFFECT_NONE },
            }
            Ok(())
        }
    }

    /// The paths of a file drag (`CF_HDROP`), if it is one.
    fn dropped_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
        let fmt = FORMATETC {
            cfFormat: CF_HDROP,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        unsafe {
            let mut medium = data.GetData(&fmt).ok()?;
            let drop = HDROP(medium.u.hGlobal.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
            }
            ReleaseStgMedium(&mut medium);
            Some(paths)
        }
    }

    /// Lets go of the drag by itself once the window under the cursor has entered it.
    #[implement(IDropSource)]
    struct LetGo {
        started: Instant,
    }

    impl IDropSource_Impl for LetGo_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, _keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            match source_step(escape.as_bool(), self.started.elapsed()) {
                SourceStep::Continue => S_OK,
                SourceStep::Drop => DRAGDROP_S_DROP,
                SourceStep::Cancel => DRAGDROP_S_CANCEL,
            }
        }

        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// An item id list owned by this program.
    struct Pidl(*mut ITEMIDLIST);

    impl Drop for Pidl {
        fn drop(&mut self) {
            unsafe { ILFree(Some(self.0)) };
        }
    }

    fn parse(path: &std::path::Path) -> ::windows::core::Result<Pidl> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut pidl = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None)? };
        Ok(Pidl(pidl))
    }

    /// A shell data object for files of one folder, as Explorer makes when dragging them.
    fn shell_data_object(files: &[PathBuf]) -> ::windows::core::Result<IDataObject> {
        let items = files.iter().map(|p| parse(p)).collect::<::windows::core::Result<Vec<_>>>()?;
        let first = items.first().ok_or_else(|| ::windows::core::Error::from(::windows::Win32::Foundation::E_INVALIDARG))?;
        unsafe {
            let folder = Pidl(ILClone(first.0));
            let _ = ILRemoveLastID(Some(folder.0));
            let children: Vec<*const ITEMIDLIST> = items.iter().map(|i| ILFindLastID(i.0) as *const _).collect();
            SHCreateDataObject(Some(folder.0), Some(&children), None::<&IDataObject>)
        }
    }

    pub fn drag_out(files: Vec<PathBuf>) {
        unsafe {
            if let Err(e) = OleInitialize(None) {
                tracing::warn!("OleInitialize failed ({e}); dropped files stay in Downloads");
                return;
            }
        }
        match drag_out_inner(&files) {
            Ok((true, effect)) => tracing::info!(files = files.len(), effect = effect.0, "files dropped under the cursor"),
            Ok((false, _)) => tracing::info!("nothing under the cursor took the files; they stay in Downloads"),
            Err(e) => tracing::warn!("dropping files under the cursor failed ({e}); they stay in Downloads"),
        }
        unsafe { OleUninitialize() };
    }

    /// `(dropped, effect)`. Move is allowed too: on the same disk Explorer then moves the file out
    /// of Downloads instead of leaving a second copy there. This program never deletes anything.
    fn drag_out_inner(files: &[PathBuf]) -> ::windows::core::Result<(bool, DROPEFFECT)> {
        let data = shell_data_object(files)?;
        let source: IDropSource = LetGo { started: Instant::now() }.into();
        let mut effect = DROPEFFECT_NONE;
        let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE, &mut effect) };
        hr.ok()?;
        Ok((hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE, effect))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_are_sent_and_folders_are_named_as_skipped() {
        let dir = std::env::temp_dir().join(format!("inputshare-dragtest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let (files, skipped) =
            split_sendable(vec![dir.join("a.png"), dir.join("folder"), dir.join("gone.txt")]);
        assert_eq!(files, vec![dir.join("a.png")]);
        assert_eq!(skipped, vec!["folder".to_string(), "gone.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_drag_on_the_other_computer_lets_go_by_itself_unless_escaped() {
        assert_eq!(source_step(false, Duration::ZERO), SourceStep::Continue);
        assert_eq!(source_step(false, LET_GO_AFTER), SourceStep::Drop);
        assert_eq!(source_step(true, LET_GO_AFTER), SourceStep::Cancel);
    }

    #[test]
    fn drops_are_handed_over_once_in_order() {
        let _ = take_dropped();
        let d = |h: &str| DroppedFiles { host: h.into(), files: vec!["a".into()], skipped: vec![] };
        push_dropped(d("PC2"));
        push_dropped(d("PC3"));
        let got = take_dropped();
        assert_eq!(got.iter().map(|d| d.host.as_str()).collect::<Vec<_>>(), ["PC2", "PC3"]);
        assert!(take_dropped().is_empty());
    }
}
