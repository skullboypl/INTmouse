//! Network adapter enumeration (for the GUI dropdown) and resolving the
//! `preferred_adapter` setting into concrete bind addresses.
//!
//! `preferred_adapter` accepts: `All` / empty (= every interface), an IP literal
//! (`192.168.1.10`, `fe80::1%12`), or an adapter friendly name / description /
//! GUID as returned by [`list_adapters`] (case-insensitive).

use super::addr::parse_host_ip;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV6};

/// One local network adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetAdapter {
    /// Friendly name, e.g. `Ethernet`, `Wi-Fi` — what the GUI should store.
    pub name: String,
    /// Driver description, e.g. `Intel(R) Ethernet Connection I219-V`.
    pub description: String,
    /// OS adapter id (Windows: GUID string `{...}`).
    pub id: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    /// Interface index used as scope id for link-local IPv6.
    pub ipv6_if_index: u32,
    pub is_up: bool,
    pub is_loopback: bool,
}

impl NetAdapter {
    /// Dropdown label: `Ethernet (192.168.1.10)`.
    #[allow(dead_code)] // public API for the GUI adapter dropdown
    pub fn label(&self) -> String {
        match self.ipv4.first() {
            Some(ip) => format!("{} ({ip})", self.name),
            None => match self.ipv6.first() {
                Some(ip) => format!("{} ({ip})", self.name),
                None => self.name.clone(),
            },
        }
    }

    fn matches(&self, query: &str) -> bool {
        [&self.name, &self.description, &self.id]
            .iter()
            .any(|s| !s.is_empty() && s.trim().eq_ignore_ascii_case(query))
    }

    fn owns(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.ipv4.contains(&v4),
            IpAddr::V6(v6) => self.ipv6.contains(&v6),
        }
    }

    /// Best IPv4 to bind: first non-APIPA, else first.
    fn best_ipv4(&self) -> Option<Ipv4Addr> {
        self.ipv4
            .iter()
            .copied()
            .find(|ip| !ip.is_link_local())
            .or_else(|| self.ipv4.first().copied())
    }

    /// Best IPv6 to bind: first non-link-local, else link-local with scope.
    fn best_ipv6(&self) -> Option<SocketAddrV6> {
        if let Some(ip) = self.ipv6.iter().copied().find(|ip| !is_ipv6_link_local(ip)) {
            return Some(SocketAddrV6::new(ip, 0, 0, 0));
        }
        self.ipv6
            .first()
            .map(|ip| SocketAddrV6::new(*ip, 0, 0, self.ipv6_if_index))
    }
}

fn is_ipv6_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn is_all(preferred: &str) -> bool {
    let p = preferred.trim();
    p.is_empty() || p.eq_ignore_ascii_case("All")
}

/// List local adapters with their addresses (up and down, loopback included —
/// filter in the GUI if needed).
pub fn list_adapters() -> io::Result<Vec<NetAdapter>> {
    os::list_adapters()
}

/// Pick the adapter a non-`All`, non-literal setting refers to (prefers adapters that are up).
fn find_named<'a>(name: &str, adapters: &'a [NetAdapter]) -> Option<&'a NetAdapter> {
    let q = name.trim();
    adapters
        .iter()
        .filter(|a| a.matches(q))
        .max_by_key(|a| (a.is_up, !a.ipv4.is_empty()))
}

/// Resolve `preferred_adapter` to the IPv4-side bind IP:
/// `All` → `0.0.0.0`, IP literal → that IP, adapter name → its IPv4,
/// unknown name / adapter without IPv4 → `0.0.0.0` + warning in the log.
#[allow(dead_code)] // public API (the hub uses `resolve_bind_plan`)
pub fn resolve_bind_ip(preferred: &str) -> IpAddr {
    resolve_bind_ip_with(preferred, list_adapters)
}

/// [`resolve_bind_ip`] with an injectable adapter source (enumerated lazily,
/// only for names).
pub fn resolve_bind_ip_with(
    preferred: &str,
    adapters: impl FnOnce() -> io::Result<Vec<NetAdapter>>,
) -> IpAddr {
    let all = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    if is_all(preferred) {
        return all;
    }
    if let Some((ip, _)) = parse_host_ip(preferred) {
        return ip;
    }
    let list = match adapters() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(preferred, "cannot enumerate adapters ({e}) — binding All");
            return all;
        }
    };
    match find_named(preferred, &list) {
        Some(a) => match a.best_ipv4() {
            Some(ip) => IpAddr::V4(ip),
            None => {
                tracing::warn!(
                    preferred,
                    adapter = %a.name,
                    "adapter has no IPv4 address — binding All"
                );
                all
            }
        },
        None => {
            tracing::warn!(preferred, "unknown network adapter — binding All");
            all
        }
    }
}

/// What the hub should bind, per address family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindPlan {
    /// IPv4 bind address; `None` when the user pinned an IPv6 literal (IPv6-only).
    pub v4: Option<Ipv4Addr>,
    /// IPv6 bind address (port 0); `None` when IPv6 is off or unavailable.
    pub v6: Option<SocketAddrV6>,
    /// Human-readable notes about anything that could not be honoured.
    pub notes: Vec<String>,
}

/// Full bind plan for `preferred_adapter` + `use_ipv6`.
///
/// Errors only when the setting is self-contradictory (an IPv6 literal while
/// IPv6 is disabled) — that must be fixed by the user, not silently replaced.
pub fn resolve_bind_plan(
    preferred: &str,
    use_ipv6: bool,
    adapters: impl FnOnce() -> io::Result<Vec<NetAdapter>>,
) -> Result<BindPlan, String> {
    let mut notes = Vec::new();

    if is_all(preferred) {
        return Ok(BindPlan {
            v4: Some(Ipv4Addr::UNSPECIFIED),
            v6: use_ipv6.then(|| SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
            notes,
        });
    }

    if let Some((ip, scope)) = parse_host_ip(preferred) {
        return match ip {
            IpAddr::V6(v6) => {
                if !use_ipv6 {
                    return Err(format!(
                        "adapter ustawiony na adres IPv6 {preferred}, a IPv6 jest wyłączone — \
                         włącz IPv6 albo wybierz adres IPv4 / All"
                    ));
                }
                notes.push(format!(
                    "adapter = {preferred} (IPv6) — IPv4 nie jest nasłuchiwane"
                ));
                Ok(BindPlan {
                    v4: None,
                    v6: Some(SocketAddrV6::new(v6, 0, 0, scope)),
                    notes,
                })
            }
            IpAddr::V4(v4) => {
                let v6 = if use_ipv6 {
                    ipv6_for_owner(ip, adapters, &mut notes, preferred)
                } else {
                    None
                };
                Ok(BindPlan {
                    v4: Some(v4),
                    v6,
                    notes,
                })
            }
        };
    }

    // Adapter name.
    let list = match adapters() {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("nie można wylistować kart sieciowych ({e}) — nasłuch na All");
            tracing::warn!("{msg}");
            notes.push(msg);
            return Ok(BindPlan {
                v4: Some(Ipv4Addr::UNSPECIFIED),
                v6: use_ipv6.then(|| SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
                notes,
            });
        }
    };
    let Some(adapter) = find_named(preferred, &list) else {
        let msg = format!("nieznana karta sieciowa „{preferred}” — nasłuch na All");
        tracing::warn!("{msg}");
        notes.push(msg);
        return Ok(BindPlan {
            v4: Some(Ipv4Addr::UNSPECIFIED),
            v6: use_ipv6.then(|| SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
            notes,
        });
    };
    let v4 = match adapter.best_ipv4() {
        Some(ip) => ip,
        None => {
            let msg = format!(
                "karta „{}” nie ma adresu IPv4 — IPv4 nasłuchuje na All",
                adapter.name
            );
            tracing::warn!("{msg}");
            notes.push(msg);
            Ipv4Addr::UNSPECIFIED
        }
    };
    let v6 = if use_ipv6 {
        let v6 = adapter.best_ipv6();
        if v6.is_none() {
            let msg = format!(
                "karta „{}” nie ma adresu IPv6 — IPv6 nie jest nasłuchiwane",
                adapter.name
            );
            tracing::warn!("{msg}");
            notes.push(msg);
        }
        v6
    } else {
        None
    };
    Ok(BindPlan {
        v4: Some(v4),
        v6,
        notes,
    })
}

/// IPv6 bind for the adapter that owns IPv4 literal `ip`.
fn ipv6_for_owner(
    ip: IpAddr,
    adapters: impl FnOnce() -> io::Result<Vec<NetAdapter>>,
    notes: &mut Vec<String>,
    preferred: &str,
) -> Option<SocketAddrV6> {
    let found = match adapters() {
        Ok(list) => list.into_iter().find(|a| a.owns(ip)),
        Err(e) => {
            let msg = format!("nie można wylistować kart ({e}) — IPv6 nie jest nasłuchiwane");
            tracing::warn!("{msg}");
            notes.push(msg);
            return None;
        }
    };
    let v6 = found.as_ref().and_then(|a| a.best_ipv6());
    if v6.is_none() {
        let msg = format!(
            "brak adresu IPv6 na karcie z adresem {preferred} — IPv6 nie jest nasłuchiwane"
        );
        tracing::warn!("{msg}");
        notes.push(msg);
    }
    v6
}

#[cfg(windows)]
mod os {
    use super::NetAdapter;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
    };

    pub fn list_adapters() -> io::Result<Vec<NetAdapter>> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        for _ in 0..4 {
            // u64 storage → 8-byte alignment required by IP_ADAPTER_ADDRESSES_LH.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let head = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            // SAFETY: `buf` is at least `size` bytes, aligned, and outlives the walk below.
            let rc = unsafe {
                GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(head), &mut size)
            };
            if rc == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if rc == ERROR_NO_DATA.0 {
                return Ok(Vec::new());
            }
            if rc != NO_ERROR.0 {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let mut out = Vec::new();
            let mut cur = head as *const IP_ADAPTER_ADDRESSES_LH;
            while !cur.is_null() {
                // SAFETY: linked list filled by the OS inside `buf`.
                let a = unsafe { &*cur };
                out.push(convert(a));
                cur = a.Next;
            }
            return Ok(out);
        }
        Err(io::Error::other(
            "GetAdaptersAddresses: bufor ciągle za mały",
        ))
    }

    fn convert(a: &IP_ADAPTER_ADDRESSES_LH) -> NetAdapter {
        // SAFETY: string pointers come from the OS list; null-checked before use.
        let name = unsafe {
            if a.FriendlyName.is_null() {
                String::new()
            } else {
                a.FriendlyName.to_string().unwrap_or_default()
            }
        };
        let description = unsafe {
            if a.Description.is_null() {
                String::new()
            } else {
                a.Description.to_string().unwrap_or_default()
            }
        };
        let id = unsafe {
            if a.AdapterName.is_null() {
                String::new()
            } else {
                a.AdapterName.to_string().unwrap_or_default()
            }
        };
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        let mut ua = a.FirstUnicastAddress;
        while !ua.is_null() {
            // SAFETY: OS-owned list inside the same buffer.
            let u = unsafe { &*ua };
            match unsafe { sockaddr_ip(&u.Address) } {
                Some(IpAddr::V4(v4)) => ipv4.push(v4),
                Some(IpAddr::V6(v6)) => ipv6.push(v6),
                None => {}
            }
            ua = u.Next;
        }
        NetAdapter {
            name,
            description,
            id,
            ipv4,
            ipv6,
            ipv6_if_index: a.Ipv6IfIndex,
            is_up: a.OperStatus == IfOperStatusUp,
            is_loopback: a.IfType == IF_TYPE_SOFTWARE_LOOPBACK,
        }
    }

    /// SAFETY: `sa.lpSockaddr` must be null or point to a valid sockaddr of
    /// `iSockaddrLength` bytes.
    unsafe fn sockaddr_ip(sa: &SOCKET_ADDRESS) -> Option<IpAddr> {
        if sa.lpSockaddr.is_null() {
            return None;
        }
        let len = sa.iSockaddrLength as usize;
        let family = (*sa.lpSockaddr).sa_family;
        if family == AF_INET && len >= std::mem::size_of::<SOCKADDR_IN>() {
            let sin = &*(sa.lpSockaddr as *const SOCKADDR_IN);
            // S_addr is in network byte order; its in-memory bytes are a.b.c.d.
            let raw = sin.sin_addr.S_un.S_addr;
            return Some(IpAddr::V4(Ipv4Addr::from(raw.to_ne_bytes())));
        }
        if family == AF_INET6 && len >= std::mem::size_of::<SOCKADDR_IN6>() {
            let sin6 = &*(sa.lpSockaddr as *const SOCKADDR_IN6);
            return Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.u.Byte)));
        }
        None
    }
}

#[cfg(not(windows))]
mod os {
    use super::NetAdapter;
    use std::io;

    pub fn list_adapters() -> io::Result<Vec<NetAdapter>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "wyliczanie kart sieciowych nie jest jeszcze zaimplementowane na tej platformie",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eth() -> NetAdapter {
        NetAdapter {
            name: "Ethernet".into(),
            description: "Intel(R) Ethernet Connection".into(),
            id: "{1111-2222}".into(),
            ipv4: vec!["169.254.3.3".parse().unwrap(), "192.168.1.10".parse().unwrap()],
            ipv6: vec!["fe80::10".parse().unwrap(), "2001:db8::10".parse().unwrap()],
            ipv6_if_index: 7,
            is_up: true,
            is_loopback: false,
        }
    }

    fn wifi_down() -> NetAdapter {
        NetAdapter {
            name: "Wi-Fi".into(),
            description: "Wireless".into(),
            id: "{3333}".into(),
            ipv4: vec![],
            ipv6: vec!["fe80::20".parse().unwrap()],
            ipv6_if_index: 9,
            is_up: false,
            is_loopback: false,
        }
    }

    fn list() -> io::Result<Vec<NetAdapter>> {
        Ok(vec![eth(), wifi_down()])
    }

    fn never() -> io::Result<Vec<NetAdapter>> {
        panic!("adapters must not be enumerated for All / literals")
    }

    #[test]
    fn all_and_empty_bind_unspecified() {
        assert_eq!(resolve_bind_ip_with("All", never), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(resolve_bind_ip_with(" all ", never), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(resolve_bind_ip_with("", never), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn literal_ip_is_kept() {
        assert_eq!(
            resolve_bind_ip_with("192.168.1.77", never),
            "192.168.1.77".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            resolve_bind_ip_with("2001:db8::1", never),
            "2001:db8::1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn name_description_and_id_resolve_to_ipv4() {
        let want: IpAddr = "192.168.1.10".parse().unwrap();
        assert_eq!(resolve_bind_ip_with("Ethernet", list), want);
        assert_eq!(resolve_bind_ip_with("ethernet", list), want);
        assert_eq!(resolve_bind_ip_with("Intel(R) Ethernet Connection", list), want);
        assert_eq!(resolve_bind_ip_with("{1111-2222}", list), want);
    }

    #[test]
    fn unknown_or_ipv4less_adapter_falls_back_to_all() {
        assert_eq!(resolve_bind_ip_with("Bluetooth", list), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(resolve_bind_ip_with("Wi-Fi", list), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let err = || Err(io::Error::other("boom"));
        assert_eq!(resolve_bind_ip_with("Ethernet", err), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn plan_all_with_and_without_ipv6() {
        let p = resolve_bind_plan("All", false, never).unwrap();
        assert_eq!(p.v4, Some(Ipv4Addr::UNSPECIFIED));
        assert_eq!(p.v6, None);
        let p = resolve_bind_plan("All", true, never).unwrap();
        assert_eq!(p.v6.unwrap().ip(), &Ipv6Addr::UNSPECIFIED);
    }

    #[test]
    fn plan_ipv6_literal_requires_ipv6() {
        assert!(resolve_bind_plan("2001:db8::10", false, never).is_err());
        let p = resolve_bind_plan("fe80::10%7", true, never).unwrap();
        assert_eq!(p.v4, None);
        let v6 = p.v6.unwrap();
        assert_eq!(v6.ip(), &"fe80::10".parse::<Ipv6Addr>().unwrap());
        assert_eq!(v6.scope_id(), 7);
    }

    #[test]
    fn plan_name_picks_global_ipv6_and_link_local_fallback() {
        let p = resolve_bind_plan("Ethernet", true, list).unwrap();
        assert_eq!(p.v4, Some("192.168.1.10".parse().unwrap()));
        assert_eq!(p.v6.unwrap().ip(), &"2001:db8::10".parse::<Ipv6Addr>().unwrap());
        assert!(p.notes.is_empty());

        let p = resolve_bind_plan("Wi-Fi", true, list).unwrap();
        assert_eq!(p.v4, Some(Ipv4Addr::UNSPECIFIED));
        let v6 = p.v6.unwrap();
        assert_eq!(v6.ip(), &"fe80::20".parse::<Ipv6Addr>().unwrap());
        assert_eq!(v6.scope_id(), 9);
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn plan_ipv4_literal_finds_owner_ipv6() {
        let p = resolve_bind_plan("192.168.1.10", true, list).unwrap();
        assert_eq!(p.v4, Some("192.168.1.10".parse().unwrap()));
        assert_eq!(p.v6.unwrap().ip(), &"2001:db8::10".parse::<Ipv6Addr>().unwrap());
        let p = resolve_bind_plan("10.9.9.9", true, list).unwrap();
        assert_eq!(p.v6, None);
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn plan_unknown_name_binds_all_with_note() {
        let p = resolve_bind_plan("Nope", true, list).unwrap();
        assert_eq!(p.v4, Some(Ipv4Addr::UNSPECIFIED));
        assert_eq!(p.v6.unwrap().ip(), &Ipv6Addr::UNSPECIFIED);
        assert_eq!(p.notes.len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn os_enumeration_works() {
        let list = list_adapters().expect("GetAdaptersAddresses");
        // Every Windows box has a loopback pseudo-interface.
        assert!(list.iter().any(|a| a.is_loopback));
        assert!(list
            .iter()
            .filter(|a| a.is_loopback)
            .any(|a| a.ipv4.contains(&Ipv4Addr::LOCALHOST)));
    }
}
