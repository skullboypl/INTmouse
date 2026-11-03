//! Windows Firewall: one pair of port rules (TCP and UDP of this program), added once with the
//! user's consent. They let the computers of the local network reach those ports for any build of
//! INTmouse, so a new version, or the exe in another folder, does not need a new permission.

/// Rule names carry the port, so a changed port shows up as a missing rule.
pub fn rule_name(protocol: &str, port: u16) -> String {
    format!("INTmouse {protocol} {port}")
}

/// `netsh` arguments for one inbound allow rule: this port, local subnet only, every network
/// profile (a home network marked "public" is common).
pub fn add_rule_args(protocol: &str, port: u16) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        format!("name={}", rule_name(protocol, port)),
        "dir=in".into(),
        "action=allow".into(),
        format!("protocol={protocol}"),
        format!("localport={port}"),
        "remoteip=localsubnet".into(),
        "profile=any".into(),
    ]
}

pub fn delete_rule_args(protocol: &str, port: u16) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "delete".into(),
        "rule".into(),
        format!("name={}", rule_name(protocol, port)),
    ]
}

/// One `cmd /c` line that replaces both rules (delete first, so a repeat leaves no duplicates).
pub fn elevated_command_line(tcp: u16, udp: u16) -> String {
    let quote = |args: Vec<String>| {
        args.into_iter()
            .map(|a| match a.split_once('=') {
                Some((k, v)) if v.contains(' ') => format!("{k}=\"{v}\""),
                _ => a,
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    [
        format!("netsh {} >nul 2>&1", quote(delete_rule_args("TCP", tcp))),
        format!("netsh {} >nul 2>&1", quote(delete_rule_args("UDP", udp))),
        format!("netsh {}", quote(add_rule_args("TCP", tcp))),
        format!("netsh {}", quote(add_rule_args("UDP", udp))),
    ]
    .join(" & ")
}

/// Both rules for these ports are in place.
pub fn rules_present(tcp: u16, udp: u16) -> bool {
    #[cfg(windows)]
    {
        win::rule_exists(&rule_name("TCP", tcp)) && win::rule_exists(&rule_name("UDP", udp))
    }
    #[cfg(not(windows))]
    {
        let _ = (tcp, udp);
        true
    }
}

/// Add both rules; asks for administrator rights once (UAC) unless already elevated.
pub fn add_rules(tcp: u16, udp: u16) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::run_elevated_cmd(&elevated_command_line(tcp, udp))?;
        if rules_present(tcp, udp) {
            Ok(())
        } else {
            Err("reguły zapory nie zostały dodane".into())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (tcp, udp);
        Ok(())
    }
}

#[cfg(windows)]
mod win {
    use ::windows::core::{w, HSTRING, PCWSTR};
    use ::windows::Win32::Foundation::CloseHandle;
    use ::windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use ::windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use ::windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn rule_exists(name: &str) -> bool {
        Command::new("netsh")
            .args(["advfirewall", "firewall", "show", "rule", &format!("name={name}")])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// `cmd /c <line>` as administrator (the UAC prompt, or directly when already elevated).
    pub fn run_elevated_cmd(line: &str) -> Result<(), String> {
        let params = HSTRING::from(format!("/c {line}"));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: w!("cmd.exe"),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe {
            ShellExecuteExW(&mut info).map_err(|e| format!("bez zgody administratora ({e})"))?;
            if info.hProcess.is_invalid() {
                return Ok(());
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 0u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            if code == 0 {
                Ok(())
            } else {
                Err(format!("netsh zakończył się kodem {code}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_named_by_port_and_open_only_the_local_subnet() {
        assert_eq!(rule_name("TCP", 6555), "INTmouse TCP 6555");
        let args = add_rule_args("UDP", 1046);
        assert!(args.contains(&"protocol=UDP".to_string()));
        assert!(args.contains(&"localport=1046".to_string()));
        assert!(args.contains(&"remoteip=localsubnet".to_string()));
        assert!(args.contains(&"dir=in".to_string()));
    }

    #[test]
    fn the_elevated_line_replaces_both_rules_with_quoted_names() {
        let line = elevated_command_line(6555, 1046);
        assert!(line.contains(r#"delete rule name="INTmouse TCP 6555""#), "{line}");
        assert!(line.contains(r#"add rule name="INTmouse UDP 1046" dir=in"#), "{line}");
        let del = line.find("delete rule name=\"INTmouse TCP").unwrap();
        let add = line.find("add rule name=\"INTmouse TCP").unwrap();
        assert!(del < add, "delete before add, so a repeat leaves no duplicate");
    }
}
