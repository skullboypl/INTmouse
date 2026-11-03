//! Host / address helpers shared by the hub: IP literal parsing (IPv4 + IPv6
//! with optional `%scope`), socket address building and the inbound ACL.

use inputshare_core::ClientEntry;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};

/// Parse an IP literal as typed in settings: `192.168.1.5`, `fe80::1`,
/// `fe80::1%12`, `[2001:db8::5]`. Returns the IP and the IPv6 scope id (0 if none).
/// Hostnames return `None`.
pub fn parse_host_ip(host: &str) -> Option<(IpAddr, u32)> {
    let h = host.trim();
    let h = h
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(h);
    if let Ok(ip) = h.parse::<IpAddr>() {
        return Some((ip, 0));
    }
    let (ip, scope) = h.split_once('%')?;
    let ip = ip.parse::<std::net::Ipv6Addr>().ok()?;
    let scope = scope.parse::<u32>().ok()?;
    Some((IpAddr::V6(ip), scope))
}

/// Socket address for an IP-literal host (keeps the IPv6 scope id).
pub fn host_socket_addr(host: &str, port: u16) -> Option<SocketAddr> {
    let (ip, scope) = parse_host_ip(host)?;
    Some(match ip {
        IpAddr::V4(v4) => SocketAddr::new(IpAddr::V4(v4), port),
        IpAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope)),
    })
}

/// Same address, different port — unlike `SocketAddr::new(a.ip(), p)` this keeps
/// the IPv6 scope id (needed for link-local peers).
pub fn with_port(mut addr: SocketAddr, port: u16) -> SocketAddr {
    addr.set_port(port);
    addr
}

/// Unwrap IPv4-mapped IPv6 (`::ffff:a.b.c.d`) so comparisons work across families.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// Does settings host `host` denote `ip`? (IP literals only; scope id ignored.)
pub fn host_is_ip(host: &str, ip: IpAddr) -> bool {
    parse_host_ip(host)
        .map(|(h, _)| canonical_ip(h) == canonical_ip(ip))
        .unwrap_or(false)
}

/// Inbound ACL: empty list = anyone (password still applies); otherwise the peer
/// IP must equal one of the accepted IP literals (IPv4 or IPv6).
pub fn acl_allows(accepted: &[ClientEntry], ip: IpAddr) -> bool {
    accepted.is_empty() || accepted.iter().any(|c| host_is_ip(&c.host, ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn entry(host: &str) -> ClientEntry {
        ClientEntry {
            host: host.into(),
            tcp_port: 6555,
        }
    }

    #[test]
    fn parses_v4_v6_brackets_and_scope() {
        assert_eq!(
            parse_host_ip(" 10.0.0.2 "),
            Some((IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 0))
        );
        assert_eq!(
            parse_host_ip("[2001:db8::5]"),
            Some(("2001:db8::5".parse().unwrap(), 0))
        );
        assert_eq!(
            parse_host_ip("fe80::1%12"),
            Some(("fe80::1".parse().unwrap(), 12))
        );
        assert_eq!(parse_host_ip("my-pc"), None);
        assert_eq!(parse_host_ip("fe80::1%eth0"), None);
        assert_eq!(parse_host_ip("10.0.0.2%3"), None);
    }

    #[test]
    fn socket_addr_keeps_scope() {
        let a = host_socket_addr("fe80::1%7", 1046).unwrap();
        match a {
            SocketAddr::V6(v6) => {
                assert_eq!(v6.scope_id(), 7);
                assert_eq!(v6.port(), 1046);
            }
            _ => panic!("expected v6"),
        }
        let b = with_port(a, 6555);
        match b {
            SocketAddr::V6(v6) => {
                assert_eq!(v6.scope_id(), 7);
                assert_eq!(v6.port(), 6555);
            }
            _ => panic!("expected v6"),
        }
        assert_eq!(
            host_socket_addr("192.168.1.9", 1),
            Some("192.168.1.9:1".parse().unwrap())
        );
    }

    #[test]
    fn acl_empty_allows_everyone() {
        assert!(acl_allows(&[], "1.2.3.4".parse().unwrap()));
        assert!(acl_allows(&[], "2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn acl_non_empty_requires_match() {
        let list = vec![entry("192.168.1.10"), entry("2001:db8::7"), entry("fe80::2%4")];
        assert!(acl_allows(&list, "192.168.1.10".parse().unwrap()));
        assert!(!acl_allows(&list, "192.168.1.11".parse().unwrap()));
        assert!(acl_allows(&list, "2001:db8::7".parse().unwrap()));
        assert!(!acl_allows(&list, "2001:db8::8".parse().unwrap()));
        // Link-local entry matches regardless of the scope the socket reports.
        assert!(acl_allows(&list, "fe80::2".parse().unwrap()));
        // IPv4-mapped IPv6 peer matches the IPv4 entry.
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 168, 1, 10).to_ipv6_mapped());
        assert!(acl_allows(&list, mapped));
        // Hostname entries never match an inbound IP.
        assert!(!acl_allows(&[entry("my-pc")], Ipv6Addr::LOCALHOST.into()));
    }
}
