// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a webhook may go (PR #55 review M1: a requester names the URL, so bezent must not become
//! its way into bezent's own network).
//!
//! - **At registration** ([`super::Webhooks::check_url`]): absolute, `https`, a host, no user
//!   information, no fragment; an IP literal or a denied host is refused there already. Names are
//!   not resolved then: what a name resolves to can change.
//! - **At send time** ([`super::Webhooks::vet`]), the guard: the host is resolved once, and every
//!   address it resolves to must be admitted, or nothing is sent. The connection is then made to
//!   those vetted addresses themselves (the client's resolution is pinned to them and resolves
//!   nothing else), so DNS cannot answer one address to the check and another to the connection.
//!   Redirects are off, proxy variables are ignored, the connection and the whole request have
//!   time limits, and the response body is never read.
//! - **Admitted:** a global unicast address ([`Reach::Global`]). Loopback, private (RFC 1918),
//!   unique-local and shared (CGNAT) addresses ([`Reach::Internal`]) only for a host on the
//!   allowlist, which holds only on a test store (the ingress refuses to start otherwise) and also
//!   lets that host take plain `http`. Never: unspecified, link-local (the cloud metadata
//!   endpoints among them, `169.254.169.254`, `169.254.170.2`, `fd00:ec2::254`), multicast,
//!   broadcast, documentation, benchmarking and reserved ranges, and the IPv4-mapped, NAT64 and
//!   6to4 forms of a refused IPv4 address ([`Reach::Never`]). Never either: bezent's own service
//!   addresses, the deny list ([`Deny`]), which wins over the allowlist.
//! - **What a requester sees:** only a coarse class (`address-not-allowed`, …),
//!   never an address or an error text.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

/// The classes a failed or refused attempt records; nothing finer reaches a requester.
pub mod class {
    /// The requester has no signing secret: nothing is sent unsigned.
    pub const NO_SECRET: &str = "no-secret";
    /// The stored URL does not open under this ingress's webhook key.
    pub const URL_UNOPENABLE: &str = "url-unopenable";
    /// The URL's scheme, user information or host is not allowed.
    pub const URL_NOT_ALLOWED: &str = "url-not-allowed";
    /// The host is, or resolves to, an address bezent does not post to, or does not resolve at
    /// all. One class for both, so the listing cannot tell a requester which internal names
    /// exist in bezent's resolver (PR #55 re-check S-d); the operator's log keeps the difference.
    pub const ADDRESS_NOT_ALLOWED: &str = "address-not-allowed";
    /// No answer in time.
    pub const TIMEOUT: &str = "timeout";
    /// No connection (TLS included).
    pub const CONNECT_FAILED: &str = "connect-failed";
    /// It answered with a status other than 2xx (the status is recorded beside).
    pub const HTTP_STATUS: &str = "http-status";
    /// Any other failure of the request.
    pub const REQUEST_FAILED: &str = "request-failed";
    /// The delivery does not match the log entry it points at (PR #55 review M2); never sent.
    pub const NOTICE_REFUSED: &str = "notice-refused";
}

/// What an address is, to a webhook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Global unicast: any URL may resolve to it.
    Global,
    /// Loopback, private, unique-local or shared (CGNAT): only an allowlisted host's.
    Internal,
    /// Never posted to.
    Never,
}

/// What `ip` is, to a webhook.
pub fn reach(ip: IpAddr) -> Reach {
    match ip {
        IpAddr::V4(v4) => reach_v4(v4),
        IpAddr::V6(v6) => reach_v6(v6),
    }
}

fn reach_v4(ip: Ipv4Addr) -> Reach {
    let [a, b, c, _] = ip.octets();
    if ip.is_loopback() || ip.is_private() || (a == 100 && (b & 0xc0) == 64) {
        return Reach::Internal;
    }
    let never = a == 0
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a >= 240
        || (a == 198 && (b & 0xfe) == 18)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99);
    if never { Reach::Never } else { Reach::Global }
}

fn reach_v6(ip: Ipv6Addr) -> Reach {
    let s = ip.segments();
    // IPv4 inside IPv6: mapped (::ffff:0:0/96), NAT64 (64:ff9b::/96), 6to4 (2002::/16).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return reach_v4(v4);
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return reach_v4(Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32));
    }
    if s[0] == 0x2002 {
        return reach_v4(Ipv4Addr::from(((s[1] as u32) << 16) | s[2] as u32));
    }
    if ip.is_loopback() {
        return Reach::Internal;
    }
    // The AWS metadata endpoint sits in the unique-local range: never, allowlisted or not.
    if ip == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254) {
        return Reach::Never;
    }
    if (s[0] & 0xfe00) == 0xfc00 {
        return Reach::Internal;
    }
    // Global unicast is 2000::/3, less the IETF protocol assignments (2001::/23: Teredo,
    // benchmarking, ORCHID) and documentation (2001:db8::/32, 3fff::/20). Everything else
    // (unspecified, IPv4-compatible, discard, link- and site-local, multicast, NAT64 local use)
    // is never.
    let global = (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && s[1] < 0x0200)
        && !(s[0] == 0x2001 && s[1] == 0x0db8)
        && (s[0] & 0xfff0) != 0x3ff0;
    if global { Reach::Global } else { Reach::Never }
}

/// One of bezent's own service addresses, never posted to (PR #55 review M1): an IP address or
/// network (`10.0.4.7`, `10.0.4.0/24`, `fd12::/16`), with or without a port (`127.0.0.1:8080`,
/// `[::1]:8080`), or a host name with or without a port (`core.internal`, `localhost:8080`).
#[derive(Clone, PartialEq, Eq)]
pub enum Deny {
    /// An address or network, any port unless one is given.
    Net {
        /// The network's address.
        addr: IpAddr,
        /// Its prefix length.
        prefix: u8,
        /// Only this port, when set.
        port: Option<u16>,
    },
    /// A host name, any port unless one is given.
    Host {
        /// The name, lower case, without a trailing dot.
        name: String,
        /// Only this port, when set.
        port: Option<u16>,
    },
}

impl fmt::Debug for Deny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net { addr, prefix, port } => write!(f, "{addr}/{prefix}:{port:?}"),
            Self::Host { name, port } => write!(f, "{name}:{port:?}"),
        }
    }
}

impl FromStr for Deny {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, String> {
        let text = raw.trim();
        let bad = || format!("not an address, network or host: {raw:?}");
        if let Ok(sa) = text.parse::<SocketAddr>() {
            let prefix = if sa.is_ipv4() { 32 } else { 128 };
            return Ok(Self::Net {
                addr: sa.ip(),
                prefix,
                port: Some(sa.port()),
            });
        }
        if let Some((addr, prefix)) = text.split_once('/') {
            let addr: IpAddr = addr.parse().map_err(|_| bad())?;
            let prefix: u8 = prefix.parse().map_err(|_| bad())?;
            let max = if addr.is_ipv4() { 32 } else { 128 };
            if prefix > max {
                return Err(bad());
            }
            return Ok(Self::Net {
                addr,
                prefix,
                port: None,
            });
        }
        if let Ok(addr) = text.parse::<IpAddr>() {
            let prefix = if addr.is_ipv4() { 32 } else { 128 };
            return Ok(Self::Net {
                addr,
                prefix,
                port: None,
            });
        }
        let (name, port) = match text.rsplit_once(':') {
            Some((name, port)) => (name, Some(port.parse::<u16>().map_err(|_| bad())?)),
            None => (text, None),
        };
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() || name.contains(['/', '[', ']', ' ', '@']) {
            return Err(bad());
        }
        Ok(Self::Host { name, port })
    }
}

impl Deny {
    /// Whether this entry covers `addr`.
    pub fn covers_addr(&self, addr: SocketAddr) -> bool {
        let Self::Net {
            addr: net,
            prefix,
            port,
        } = self
        else {
            return false;
        };
        if port.is_some_and(|p| p != addr.port()) {
            return false;
        }
        let ip = match addr.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        };
        match (net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(*prefix)).unwrap_or(0);
                (u32::from(*n) & mask) == (u32::from(a) & mask)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(*prefix)).unwrap_or(0);
                (u128::from(*n) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }

    /// Whether this entry covers the host name `host` at `port`.
    pub fn covers_host(&self, host: &str, port: u16) -> bool {
        let Self::Host { name, port: only } = self else {
            return false;
        };
        only.is_none_or(|p| p == port) && name.eq_ignore_ascii_case(host.trim_end_matches('.'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(ip: &str) -> Reach {
        reach(ip.parse().unwrap())
    }

    #[test]
    fn m1_global_addresses_only_and_internal_ones_for_the_allowlist() {
        for global in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700::1111",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "::ffff:8.8.8.8",
        ] {
            assert_eq!(r(global), Reach::Global, "{global}");
        }
        for internal in [
            "127.0.0.1",
            "127.8.9.10",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "::1",
            "fd12:3456::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
        ] {
            assert_eq!(r(internal), Reach::Internal, "{internal}");
        }
        for never in [
            "169.254.169.254",
            "169.254.170.2",
            "fd00:ec2::254",
            "0.0.0.0",
            "0.1.2.3",
            "::",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "198.18.0.1",
            "192.0.0.8",
            "2001:db8::1",
            "2001::1",
            "3fff::1",
            "100::1",
            "::127.0.0.1",
            "64:ff9b:1::1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
        ] {
            assert_eq!(r(never), Reach::Never, "{never}");
        }
    }

    #[test]
    fn m1_bezents_own_addresses_are_denied_by_address_network_port_or_name() {
        let sa = |s: &str| s.parse::<SocketAddr>().unwrap();
        let one: Deny = "10.0.4.7".parse().unwrap();
        assert!(one.covers_addr(sa("10.0.4.7:443")));
        assert!(one.covers_addr(sa("[::ffff:10.0.4.7]:80")));
        assert!(!one.covers_addr(sa("10.0.4.8:443")));
        let net: Deny = "10.0.4.0/24".parse().unwrap();
        assert!(net.covers_addr(sa("10.0.4.200:1")));
        assert!(!net.covers_addr(sa("10.0.5.1:1")));
        let port: Deny = "127.0.0.1:8080".parse().unwrap();
        assert!(port.covers_addr(sa("127.0.0.1:8080")));
        assert!(!port.covers_addr(sa("127.0.0.1:8081")));
        let v6: Deny = "[::1]:8080".parse().unwrap();
        assert!(v6.covers_addr(sa("[::1]:8080")));
        let all6: Deny = "fd00::/8".parse().unwrap();
        assert!(all6.covers_addr(sa("[fd12::1]:443")));
        let host: Deny = "Core.Internal.".parse().unwrap();
        assert!(host.covers_host("core.internal", 443));
        let host_port: Deny = "localhost:8080".parse().unwrap();
        assert!(host_port.covers_host("LOCALHOST", 8080));
        assert!(!host_port.covers_host("localhost", 9000));
        for bad in ["10.0.0.0/33", "", "a/b", "host:port", "user@host"] {
            assert!(bad.parse::<Deny>().is_err(), "{bad}");
        }
    }
}
