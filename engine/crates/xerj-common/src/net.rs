//! Network trust primitives — who is allowed to tell us who the client is.
//!
//! Xerj derives the caller's identity (per-IP auth rate-limit bucket,
//! audit-log source address) from the TCP peer address, which a caller
//! cannot forge. Behind a legitimate reverse proxy the peer *is* the proxy,
//! which would collapse every user into one bucket, so the proxy's
//! `X-Forwarded-For` header has to be honoured — but only when the socket
//! peer is a proxy the operator has actually declared.
//!
//! [`TrustedProxies`] is that declaration: a parsed set of addresses / CIDR
//! blocks. It is **empty by default**, and an empty set trusts nothing, so a
//! node that has not been configured for a proxy ignores forwarding headers
//! entirely.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One entry of the trusted-proxy list: a base address plus a prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    base: IpAddr,
    prefix_len: u8,
}

impl Cidr {
    /// Parse `"10.0.0.7"`, `"10.0.0.0/8"`, `"::1"`, `"fd00::/8"`.
    ///
    /// A bare address is treated as a host route (`/32` for v4, `/128` for
    /// v6). The prefix length must fit the address family.
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty entry".to_string());
        }
        let (addr_str, prefix_str) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let base: IpAddr = addr_str
            .parse()
            .map_err(|_| format!("`{addr_str}` is not an IP address"))?;
        let max_len: u8 = match base {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix_len = match prefix_str {
            None => max_len,
            Some(p) => {
                let n: u8 = p
                    .parse()
                    .map_err(|_| format!("`{p}` is not a prefix length"))?;
                if n > max_len {
                    return Err(format!("prefix /{n} exceeds /{max_len} for `{addr_str}`"));
                }
                n
            }
        };
        Ok(Self { base, prefix_len })
    }

    fn contains(&self, ip: &IpAddr) -> bool {
        match (self.base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                prefix_eq(&base.octets(), &ip.octets(), self.prefix_len)
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                prefix_eq(&base.octets(), &ip.octets(), self.prefix_len)
            }
            // Families never cross: peers are canonicalised (v4-mapped v6
            // folded down to v4) before they get here, so a v4 peer is only
            // ever matched against v4 entries.
            _ => false,
        }
    }
}

/// Do `a` and `b` agree on their first `bits` bits?
fn prefix_eq(a: &[u8], b: &[u8], bits: u8) -> bool {
    let whole = (bits / 8) as usize;
    if a[..whole] != b[..whole] {
        return false;
    }
    let rem = bits % 8;
    if rem == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - rem);
    a[whole] & mask == b[whole] & mask
}

/// Fold an IPv4-mapped IPv6 address (`::ffff:1.2.3.4`) down to plain IPv4.
///
/// A dual-stack listener reports IPv4 peers in mapped form, so without this
/// a `10.0.0.0/8` entry would never match a real 10.x proxy.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// The operator-declared set of reverse proxies whose forwarding headers may
/// be believed. Empty means "trust nothing" — the safe default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    entries: Vec<Cidr>,
}

impl TrustedProxies {
    /// A set that trusts nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// Parse a config list. Fails on the first malformed entry — a typo in a
    /// trust boundary must be loud, never silently "trusts nothing" (which
    /// would look like it worked) nor silently "trusts everything".
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let mut parsed = Vec::with_capacity(entries.len());
        for e in entries {
            parsed.push(Cidr::parse(e).map_err(|why| format!("`{e}`: {why}"))?);
        }
        Ok(Self { entries: parsed })
    }

    /// True when nothing is trusted (the default).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Is `ip` one of the declared proxies?
    pub fn contains(&self, ip: &IpAddr) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let ip = canonical_ip(*ip);
        self.entries.iter().any(|e| e.contains(&ip))
    }
}

/// Parse one element of an `X-Forwarded-For` chain into an address.
///
/// Handles the shapes proxies actually emit: a bare address, a bracketed
/// IPv6 literal with a port (`[2001:db8::1]:443`), and an IPv4 address with
/// a port (`1.2.3.4:5678`). Returns `None` for anything else — including
/// RFC 7239 obfuscated identifiers (`_hidden`) and `unknown`, which carry no
/// address and must not be silently skipped over.
pub fn parse_forwarded_element(raw: &str) -> Option<IpAddr> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // `[v6]:port` or `[v6]`
    if let Some(rest) = s.strip_prefix('[') {
        let (inner, _) = rest.split_once(']')?;
        return inner.parse::<Ipv6Addr>().ok().map(IpAddr::V6);
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some(ip);
    }
    // `1.2.3.4:5678` — only valid for v4; a bare v6 with a colon-port is
    // ambiguous and proxies are required to bracket it.
    if let Some((host, port)) = s.rsplit_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(v4) = host.parse::<Ipv4Addr>() {
                return Some(IpAddr::V4(v4));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn empty_set_trusts_nothing() {
        let t = TrustedProxies::none();
        assert!(t.is_empty());
        for candidate in ["127.0.0.1", "10.0.0.1", "::1", "8.8.8.8"] {
            assert!(
                !t.contains(&ip(candidate)),
                "{candidate} must not be trusted"
            );
        }
    }

    #[test]
    fn host_entry_matches_exactly_one_address() {
        let t = TrustedProxies::parse(&["10.0.0.7".to_string()]).unwrap();
        assert!(t.contains(&ip("10.0.0.7")));
        assert!(!t.contains(&ip("10.0.0.8")));
        assert!(!t.contains(&ip("10.0.0.6")));
    }

    #[test]
    fn cidr_entry_matches_the_block_and_nothing_else() {
        let t = TrustedProxies::parse(&["10.1.2.0/24".to_string()]).unwrap();
        assert!(t.contains(&ip("10.1.2.0")));
        assert!(t.contains(&ip("10.1.2.255")));
        assert!(!t.contains(&ip("10.1.3.0")));
        assert!(!t.contains(&ip("10.1.1.255")));
    }

    #[test]
    fn non_byte_aligned_prefix_masks_correctly() {
        let t = TrustedProxies::parse(&["192.168.4.0/22".to_string()]).unwrap();
        assert!(t.contains(&ip("192.168.4.1")));
        assert!(t.contains(&ip("192.168.7.254")));
        assert!(!t.contains(&ip("192.168.8.1")));
        assert!(!t.contains(&ip("192.168.3.255")));
    }

    #[test]
    fn ipv6_entries_work() {
        let t = TrustedProxies::parse(&["fd00::/8".to_string(), "::1".to_string()]).unwrap();
        assert!(t.contains(&ip("fd12::9")));
        assert!(t.contains(&ip("::1")));
        assert!(!t.contains(&ip("2001:db8::1")));
    }

    #[test]
    fn families_do_not_cross() {
        let v4_only = TrustedProxies::parse(&["0.0.0.0/0".to_string()]).unwrap();
        assert!(v4_only.contains(&ip("8.8.8.8")));
        assert!(!v4_only.contains(&ip("2001:db8::1")));

        let v6_only = TrustedProxies::parse(&["::/0".to_string()]).unwrap();
        assert!(v6_only.contains(&ip("2001:db8::1")));
        assert!(!v6_only.contains(&ip("8.8.8.8")));
    }

    #[test]
    fn v4_mapped_v6_peer_matches_a_v4_entry() {
        let t = TrustedProxies::parse(&["10.0.0.0/8".to_string()]).unwrap();
        assert!(t.contains(&ip("::ffff:10.4.5.6")));
        assert!(!t.contains(&ip("::ffff:11.4.5.6")));
    }

    #[test]
    fn malformed_entries_are_rejected_loudly() {
        for bad in [
            "not-an-ip",
            "10.0.0.0/33",
            "::1/129",
            "10.0.0.0/abc",
            "",
            "example.com",
        ] {
            assert!(
                TrustedProxies::parse(&[bad.to_string()]).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn forwarded_elements_parse_the_shapes_proxies_emit() {
        assert_eq!(parse_forwarded_element(" 1.2.3.4 "), Some(ip("1.2.3.4")));
        assert_eq!(parse_forwarded_element("1.2.3.4:5678"), Some(ip("1.2.3.4")));
        assert_eq!(
            parse_forwarded_element("2001:db8::1"),
            Some(ip("2001:db8::1"))
        );
        assert_eq!(
            parse_forwarded_element("[2001:db8::1]:443"),
            Some(ip("2001:db8::1"))
        );
        assert_eq!(
            parse_forwarded_element("[2001:db8::1]"),
            Some(ip("2001:db8::1"))
        );
    }

    #[test]
    fn forwarded_elements_that_carry_no_address_are_none() {
        for bad in [
            "",
            "  ",
            "unknown",
            "_hidden",
            "1.2.3.4.5",
            "[bogus]",
            "evil",
        ] {
            assert_eq!(parse_forwarded_element(bad), None, "`{bad}` must not parse");
        }
    }
}
