//! Windows CF_UNICODETEXT clipboard get/set + simulated Ctrl+V paste.

pub fn get_clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        win::get_text()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn set_clipboard_text(text: &str) -> bool {
    #[cfg(windows)]
    {
        win::set_text(text)
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

/// Set clipboard then synthesize Ctrl+V (for insert_remote_clipboard_hotkey).
pub fn simulate_paste(text: &str) -> bool {
    if !set_clipboard_text(text) {
        return false;
    }
    #[cfg(windows)]
    {
        win::ctrl_v();
        true
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod win {
    use ::windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use ::windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    use ::windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
        VK_CONTROL, VK_V,
    };

    /// CF_UNICODETEXT = 13 (Win32 clipboard format).
    const CF_UNICODETEXT: u32 = 13;

    pub fn get_text() -> Option<String> {
        unsafe {
            if OpenClipboard(HWND::default()).is_err() {
                return None;
            }
            let handle = match GetClipboardData(CF_UNICODETEXT) {
                Ok(h) if !h.is_invalid() => h,
                _ => {
                    let _ = CloseClipboard();
                    return None;
                }
            };
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                let _ = CloseClipboard();
                return None;
            }
            let mut len = 0usize;
            let p = ptr as *const u16;
            while *p.add(len) != 0 {
                len += 1;
                if len > 4 * 1024 * 1024 {
                    break;
                }
            }
            let slice = std::slice::from_raw_parts(p, len);
            let text = String::from_utf16_lossy(slice);
            let _ = GlobalUnlock(hglobal);
            let _ = CloseClipboard();
            Some(text)
        }
    }

    pub fn set_text(text: &str) -> bool {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * 2;
        unsafe {
            if OpenClipboard(HWND::default()).is_err() {
                return false;
            }
            let _ = EmptyClipboard();
            let hmem = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
                Ok(h) => h,
                Err(_) => {
                    let _ = CloseClipboard();
                    return false;
                }
            };
            let ptr = GlobalLock(hmem);
            if ptr.is_null() {
                let _ = CloseClipboard();
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
            let _ = GlobalUnlock(hmem);
            let ok = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_ok();
            let _ = CloseClipboard();
            ok
        }
    }

    pub fn ctrl_v() {
        unsafe {
            let inputs = [
                key(VK_CONTROL.0, false),
                key(VK_V.0, false),
                key(VK_V.0, true),
                key(VK_CONTROL.0, true),
            ];
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        Default::default()
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}
