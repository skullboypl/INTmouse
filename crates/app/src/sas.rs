//! Best-effort Secure Attention Sequence (Ctrl+Alt+Del) via hotkey.
//! Restricted on Windows — tries `sas.dll` SendSAS, then SendInput CAD.

use crate::hotkeys::KeyCombo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
pub struct SasControl {
    /// When false / empty hotkey — idle.
    hotkey: Arc<Mutex<Option<String>>>,
    /// Last status for the GUI status bar.
    pub last_status: Arc<Mutex<Option<String>>>,
    pub fired: Arc<AtomicBool>,
}

impl SasControl {
    pub fn new(hotkey: Option<String>) -> Self {
        Self {
            hotkey: Arc::new(Mutex::new(hotkey)),
            last_status: Arc::new(Mutex::new(None)),
            fired: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_hotkey(&self, hk: Option<String>) {
        if let Ok(mut g) = self.hotkey.lock() {
            *g = hk;
        }
    }

    pub fn take_fired(&self) -> Option<String> {
        if self
            .fired
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            self.last_status.lock().ok().and_then(|mut g| g.take())
        } else {
            None
        }
    }
}

pub fn spawn(ctrl: SasControl) -> JoinHandle<()> {
    thread::Builder::new()
        .name("inputshare-sas".into())
        .spawn(move || run_loop(ctrl))
        .expect("spawn sas thread")
}

fn run_loop(ctrl: SasControl) {
    #[cfg(windows)]
    {
        win::run(ctrl);
    }
    #[cfg(not(windows))]
    {
        let _ = ctrl;
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use ::windows::core::s;
    use ::windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_MENU,
    };

    type SendSasFn = unsafe extern "system" fn(as_user: i32);

    pub fn run(ctrl: SasControl) {
        let mut was_down = false;
        loop {
            thread::sleep(Duration::from_millis(15));
            let hk = ctrl
                .hotkey
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_default();
            let Some(combo) = KeyCombo::parse(&hk) else {
                was_down = false;
                continue;
            };
            let down = combo.is_down();
            if down && !was_down {
                let msg = attempt_sas();
                tracing::info!("{msg}");
                if let Ok(mut g) = ctrl.last_status.lock() {
                    *g = Some(msg);
                }
                ctrl.fired.store(true, Ordering::SeqCst);
                while combo.is_down() {
                    thread::sleep(Duration::from_millis(20));
                }
                was_down = false;
                continue;
            }
            was_down = down;
        }
    }

    fn attempt_sas() -> String {
        if try_send_sas_dll() {
            return "SAS: wywołano SendSAS (sas.dll).".into();
        }
        if try_send_input_cad() {
            return "SAS: wysłano Ctrl+Alt+Del przez SendInput (może być zablokowane przez Windows)."
                .into();
        }
        "SAS: nie udało się — Windows ogranicza Ctrl+Alt+Del (brak sas.dll / SendInput odrzucony)."
            .into()
    }

    fn try_send_sas_dll() -> bool {
        unsafe {
            let Ok(lib) = LoadLibraryA(s!("sas.dll")) else {
                return false;
            };
            let Some(proc) = GetProcAddress(lib, s!("SendSAS")) else {
                return false;
            };
            let send_sas: SendSasFn = std::mem::transmute(proc);
            // FALSE = as system / interactive session (docs vary; best-effort).
            send_sas(0);
            true
        }
    }

    fn try_send_input_cad() -> bool {
        unsafe {
            let mut inputs = [
                key_down(VK_CONTROL),
                key_down(VK_MENU),
                key_down(VK_DELETE),
                key_up(VK_DELETE),
                key_up(VK_MENU),
                key_up(VK_CONTROL),
            ];
            let n = SendInput(&mut inputs, std::mem::size_of::<INPUT>() as i32);
            n == inputs.len() as u32
        }
    }

    fn key_down(vk: VIRTUAL_KEY) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key_up(vk: VIRTUAL_KEY) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

// Only the shared status flags and hotkey parsing are tested. `attempt_sas`
// (sas.dll SendSAS / SendInput Ctrl+Alt+Del) is deliberately never called.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_control_is_idle() {
        let c = SasControl::new(None);
        assert!(c.hotkey.lock().unwrap().is_none());
        assert!(!c.fired.load(Ordering::SeqCst));
        assert!(c.last_status.lock().unwrap().is_none());
        assert_eq!(c.take_fired(), None);
    }

    #[test]
    fn set_hotkey_replaces_and_clears() {
        let c = SasControl::new(Some("Ctrl+Alt+Ins".into()));
        assert_eq!(c.hotkey.lock().unwrap().as_deref(), Some("Ctrl+Alt+Ins"));
        c.set_hotkey(Some("Ctrl+Alt+F12".into()));
        assert_eq!(c.hotkey.lock().unwrap().as_deref(), Some("Ctrl+Alt+F12"));
        c.set_hotkey(None);
        assert!(c.hotkey.lock().unwrap().is_none());
    }

    #[test]
    fn take_fired_returns_status_once() {
        let c = SasControl::new(None);
        *c.last_status.lock().unwrap() = Some("SAS: test".into());
        c.fired.store(true, Ordering::SeqCst);

        assert_eq!(c.take_fired().as_deref(), Some("SAS: test"));
        assert!(!c.fired.load(Ordering::SeqCst), "fired flag cleared");
        assert_eq!(c.take_fired(), None);
        assert!(c.last_status.lock().unwrap().is_none(), "status consumed");
    }

    #[test]
    fn take_fired_without_status_still_clears_flag() {
        let c = SasControl::new(None);
        c.fired.store(true, Ordering::SeqCst);
        assert_eq!(c.take_fired(), None);
        assert!(!c.fired.load(Ordering::SeqCst));
    }

    #[test]
    fn status_is_kept_until_fired_flag_is_set() {
        let c = SasControl::new(None);
        *c.last_status.lock().unwrap() = Some("pending".into());
        assert_eq!(c.take_fired(), None, "no fire -> nothing delivered");
        assert_eq!(c.last_status.lock().unwrap().as_deref(), Some("pending"));
    }

    #[test]
    fn clones_share_state() {
        let a = SasControl::new(None);
        let b = a.clone();
        *a.last_status.lock().unwrap() = Some("x".into());
        a.fired.store(true, Ordering::SeqCst);
        assert_eq!(b.take_fired().as_deref(), Some("x"));
        assert!(!a.fired.load(Ordering::SeqCst));
    }

    #[test]
    fn default_gui_hotkey_parses_to_ctrl_alt_insert() {
        // "Ctrl+Alt+Ins" is the default offered by the settings page.
        let combo = KeyCombo::parse("Ctrl+Alt+Ins").expect("default SAS hotkey must parse");
        assert!(combo.ctrl && combo.alt && !combo.shift && !combo.win);
        assert_eq!(combo.vk, 0x2D);
    }

    #[test]
    fn empty_or_invalid_hotkey_keeps_the_loop_idle() {
        // The poll loop treats an unparsable hotkey as "disabled".
        assert!(KeyCombo::parse("").is_none());
        assert!(KeyCombo::parse("Ctrl+Alt").is_none());
        assert!(KeyCombo::parse("Ctrl+Alt+Nonsense").is_none());
    }
}
