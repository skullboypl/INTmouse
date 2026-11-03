//! Start with Windows and run as administrator.
//!
//! * Start at login, normal user: a value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! * Start at login, administrator: a Task Scheduler task with the highest run level that fires at
//!   logon (a Run entry cannot start an elevated program without a UAC prompt each time). Creating
//!   or deleting that task needs an elevated process, so the program restarts elevated first.
//! * Run as administrator lets the program steer windows of elevated programs (Task Manager, some
//!   games): Windows blocks input into a higher-privileged window from a normal-privileged process.
//!
//! The decisions are in [`plan`] (pure, unit-tested); [`apply`] runs the steps.

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "INTmouse";
const TASK_NAME: &str = "INTmouse";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `ShellExecuteW` reports a refused UAC prompt as "access denied".
const SE_ERR_ACCESSDENIED: isize = 5;

pub fn is_elevated() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}

/// What the Run entry / the task starts: the program, hidden in the tray.
pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

// ───────────────────────── planning ─────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    SetRunEntry,
    RemoveRunEntry,
    CreateTask,
    DeleteTask,
    /// Cannot be done now; the text says why and what to do.
    Cannot(String),
}

/// What to do so the system matches the two settings.
pub fn plan(launch_at_login: bool, run_as_admin: bool, elevated: bool, task_exists: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    match (launch_at_login, run_as_admin) {
        (true, true) => {
            if elevated {
                steps.push(Step::CreateTask);
                steps.push(Step::RemoveRunEntry);
            } else {
                steps.push(Step::Cannot(
                    "Start jako administrator da się ustawić tylko z programu uruchomionego jako administrator. Program uruchomi się ponownie z uprawnieniami.".into(),
                ));
            }
        }
        (true, false) => {
            steps.push(Step::SetRunEntry);
            leave_no_stale_task(&mut steps, elevated, task_exists);
        }
        (false, _) => {
            steps.push(Step::RemoveRunEntry);
            leave_no_stale_task(&mut steps, elevated, task_exists);
        }
    }
    steps
}

/// A task left from administrator mode would still start the program elevated at logon.
fn leave_no_stale_task(steps: &mut Vec<Step>, elevated: bool, task_exists: bool) {
    if !task_exists {
        return;
    }
    if elevated {
        steps.push(Step::DeleteTask);
    } else {
        steps.push(Step::Cannot(
            "Istnieje zadanie startu jako administrator. Usuń je, wyłączając „Uruchamiaj przy starcie systemu” w programie uruchomionym jako administrator.".into(),
        ));
    }
}

/// Make the system match the settings. `Err` carries the first thing that could not be done.
pub fn apply(launch_at_login: bool, run_as_admin: bool, exe: &Path) -> Result<(), String> {
    let elevated = is_elevated();
    let steps = plan(launch_at_login, run_as_admin, elevated, task_exists());
    let mut first_problem = None;
    for step in steps {
        let result = match step {
            Step::SetRunEntry => run_entry_set(VALUE_NAME, &autostart_command(exe)),
            Step::RemoveRunEntry => run_entry_remove(VALUE_NAME),
            Step::CreateTask => create_task(exe),
            Step::DeleteTask => delete_task(),
            Step::Cannot(why) => Err(why),
        };
        if let Err(e) = result {
            tracing::warn!("autostart: {e}");
            first_problem.get_or_insert(e);
        }
    }
    first_problem.map_or(Ok(()), Err)
}

/// Where the autostart lives right now, for the settings page.
pub fn describe() -> &'static str {
    if task_exists() {
        "Harmonogram zadań (jako administrator)"
    } else if run_entry_get(VALUE_NAME).is_some() {
        "Rejestr Windows (zwykły użytkownik)"
    } else {
        "wyłączony"
    }
}

// ───────────────────────── registry Run entry ─────────────────────────

fn win32(code: windows::Win32::Foundation::WIN32_ERROR) -> Result<(), String> {
    if code.0 == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(code.0 as i32).to_string())
    }
}

fn run_entry_set(name: &str, command: &str) -> Result<(), String> {
    let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let code = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(name),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    win32(code).map_err(|e| format!("nie udało się zapisać wpisu autostartu: {e}"))
}

fn run_entry_get(name: &str) -> Option<String> {
    let mut size = 0u32;
    let key = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(name);
    let probe = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_SZ, None, None, Some(&mut size))
    };
    if probe.0 != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if code.0 != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn run_entry_remove(name: &str) -> Result<(), String> {
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    let code = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(name)) };
    if code.0 == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    win32(code).map_err(|e| format!("nie udało się usunąć wpisu autostartu: {e}"))
}

// ───────────────────────── scheduled task (administrator) ─────────────────────────

fn quiet(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

fn task_exists() -> bool {
    quiet("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn delete_task() -> Result<(), String> {
    let out = quiet("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .output()
        .map_err(|e| format!("schtasks: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("nie udało się usunąć zadania startu: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// PowerShell that registers the logon task. Tasks made by plain `schtasks /Create` stop on
/// battery and when idle, which would break a laptop; this one runs always, with the program's
/// folder as the working directory.
pub fn task_script(exe: &Path) -> String {
    let quote = |s: &str| s.replace('\'', "''");
    let exe_str = quote(&exe.display().to_string());
    let dir = quote(&exe.parent().map(|p| p.display().to_string()).unwrap_or_default());
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name\n\
         $action = New-ScheduledTaskAction -Execute '{exe_str}' -Argument '--tray' -WorkingDirectory '{dir}'\n\
         $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user\n\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew\n\
         $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest\n\
         Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null\n"
    )
}

/// `-EncodedCommand` takes base64 of UTF-16LE, which sidesteps every quoting problem.
pub fn encode_powershell(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

fn create_task(exe: &Path) -> Result<(), String> {
    let out = quiet("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(encode_powershell(&task_script(exe)))
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "nie udało się utworzyć zadania startu jako administrator: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ───────────────────────── restart in the other mode ─────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum RelaunchError {
    /// The user declined the Windows (UAC) prompt.
    Cancelled,
    Failed(String),
}

/// Start another copy of the program with (`want_admin`) or without administrator rights.
/// On `Ok` the caller closes this copy. The settings are on disk already, the new copy reads them.
pub fn relaunch(exe: &Path, want_admin: bool) -> Result<(), RelaunchError> {
    match (want_admin, is_elevated()) {
        (true, false) => {
            let result = unsafe {
                ShellExecuteW(HWND::default(), w!("runas"), &HSTRING::from(exe.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
            };
            match result.0 as isize {
                n if n > 32 => Ok(()),
                SE_ERR_ACCESSDENIED => Err(RelaunchError::Cancelled),
                n => Err(RelaunchError::Failed(format!("ShellExecute zwrócił {n}"))),
            }
        }
        // Explorer starts what it is given with the normal user's token, which drops the elevation.
        (false, true) => Command::new("explorer.exe")
            .arg(exe)
            .spawn()
            .map(|_| ())
            .map_err(|e| RelaunchError::Failed(e.to_string())),
        _ => Command::new(exe)
            .args(std::env::args_os().skip(1).filter(|a| a != OsString::from("--tray").as_os_str()))
            .spawn()
            .map(|_| ())
            .map_err(|e| RelaunchError::Failed(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_autostart_command_quotes_the_path_and_starts_in_the_tray() {
        let exe = PathBuf::from(r"E:\[ ... ]\INTmouse.exe");
        assert_eq!(autostart_command(&exe), r#""E:\[ ... ]\INTmouse.exe" --tray"#);
    }

    #[test]
    fn a_normal_user_gets_a_run_entry_and_no_stale_task() {
        assert_eq!(plan(true, false, false, false), [Step::SetRunEntry]);
        assert_eq!(plan(true, false, true, true), [Step::SetRunEntry, Step::DeleteTask]);
        let steps = plan(true, false, false, true);
        assert_eq!(steps[0], Step::SetRunEntry);
        assert!(matches!(&steps[1], Step::Cannot(why) if why.contains("administrator")), "{steps:?}");
    }

    #[test]
    fn administrator_autostart_is_a_task_and_needs_an_elevated_process() {
        assert_eq!(plan(true, true, true, false), [Step::CreateTask, Step::RemoveRunEntry]);
        let steps = plan(true, true, false, false);
        assert_eq!(steps.len(), 1);
        assert!(matches!(&steps[0], Step::Cannot(why) if why.contains("uruchomi się ponownie")), "{steps:?}");
    }

    #[test]
    fn turning_autostart_off_removes_the_run_entry_and_the_task_when_it_can() {
        assert_eq!(plan(false, false, false, false), [Step::RemoveRunEntry]);
        assert_eq!(plan(false, true, true, true), [Step::RemoveRunEntry, Step::DeleteTask]);
    }

    #[test]
    fn the_task_script_runs_hidden_in_the_tray_from_the_program_folder_with_highest_rights() {
        let script = task_script(Path::new(r"C:\Users\O'Neil\App\INTmouse.exe"));
        assert!(script.contains(r"-Execute 'C:\Users\O''Neil\App\INTmouse.exe'"), "{script}");
        assert!(script.contains("-Argument '--tray'"));
        assert!(script.contains(r"-WorkingDirectory 'C:\Users\O''Neil\App'"));
        assert!(script.contains("-RunLevel Highest"));
        assert!(script.contains("-AllowStartIfOnBatteries -DontStopIfGoingOnBatteries"));
        assert!(script.contains("-TaskName 'INTmouse'"));
    }

    #[test]
    fn base64_of_utf16_matches_known_vectors() {
        // powershell -EncodedCommand for "hi": 68 00 69 00 -> aABpAA==
        assert_eq!(encode_powershell("hi"), "aABpAA==");
        assert_eq!(encode_powershell("a"), "YQA=");
        assert_eq!(encode_powershell(""), "");
        assert_eq!(encode_powershell("ąb"), "BQFiAA==");
    }

    /// Writes and removes a value under a unique name in the real HKCU Run key, never `INTmouse`.
    #[test]
    fn run_entry_roundtrip_under_a_throwaway_name() {
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = run_entry_remove(&self.0);
            }
        }
        let name = format!("INTmouseTest-{}", std::process::id());
        let _cleanup = Cleanup(name.clone());
        assert_eq!(run_entry_get(&name), None);
        run_entry_set(&name, r#""C:\Program Files\x\INTmouse.exe" --tray"#).unwrap();
        assert_eq!(run_entry_get(&name).as_deref(), Some(r#""C:\Program Files\x\INTmouse.exe" --tray"#));
        run_entry_set(&name, "cmd /c exit").unwrap();
        assert_eq!(run_entry_get(&name).as_deref(), Some("cmd /c exit"), "setting again replaces the value");
        run_entry_remove(&name).unwrap();
        assert_eq!(run_entry_get(&name), None);
        run_entry_remove(&name).expect("removing a missing value is not an error");
    }
}
