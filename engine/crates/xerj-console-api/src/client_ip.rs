//! Who the caller *is*, as opposed to who they claim to be (#76 S5-4).
//!
//! Every auth endpoint charges a per-IP rate-limit bucket and stamps a source
//! address into the audit log. Both used to be derived from `X-Forwarded-For`
//! — a header the caller writes. A rotating `x-forwarded-for: 1.2.3.$RANDOM`
//! therefore got a fresh quota on every request (unthrottled brute force of
//! magic links and the bootstrap claim) while honest clients, who send no such
//! header, stayed throttled. Exactly backwards.
//!
//! The address now comes from the transport: [`axum::extract::ConnectInfo`],
//! i.e. the TCP peer, which the caller cannot forge.
//!
//! ## Reverse proxies
//!
//! Behind a legitimate proxy the peer *is* the proxy, and keying purely on the
//! socket would collapse every user into one bucket — a self-inflicted DoS. So
//! forwarding headers are honoured, but only when the socket peer is a proxy
//! the operator declared in `server.trusted_proxies`. The list is empty by
//! default: **an unconfigured node believes nobody**.
//!
//! ## Reading the chain
//!
//! `X-Forwarded-For` is a comma-separated chain, appended to left-to-right.
//! The left end is whatever the original caller sent — attacker-controlled.
//! The *right-most* entry is the one your own outermost proxy just wrote, so
//! the chain is walked right-to-left, skipping entries that are themselves
//! declared proxies, and the first non-proxy address wins. A malformed
//! element stops the walk (we do not step over it into attacker-authored
//! text) and we fall back to the peer.

use std::net::{IpAddr, SocketAddr};

use axum::{
    async_trait,
    extract::{ConnectInfo, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use xerj_common::net::{canonical_ip, parse_forwarded_element, TrustedProxies};

/// The bucket key used when we cannot establish any address at all (no
/// `ConnectInfo` in the request extensions — e.g. a router driven directly by
/// `tower::ServiceExt::oneshot` in a test, or a future transport that does not
/// carry a peer). Failing to one shared bucket throttles rather than
/// exempts: strictly safer than believing a header.
pub const UNKNOWN_IP: &str = "unknown";

/// Resolve the caller's address.
///
/// `peer` is the real socket peer. Returns the address as a string because
/// that is what the rate-limit key and the audit record want; the value is
/// always either a canonical IP literal or [`UNKNOWN_IP`], never
/// caller-authored text.
pub fn resolve(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &TrustedProxies) -> String {
    let Some(peer) = peer.map(canonical_ip) else {
        // No transport identity. Header data cannot rescue this — it would
        // reintroduce the very spoof we are closing.
        return UNKNOWN_IP.to_string();
    };

    // The common case, and the whole point: an ordinary client's headers are
    // never consulted.
    if !trusted.contains(&peer) {
        return peer.to_string();
    }

    // Peer is a declared proxy — its forwarding headers may be believed.
    // Multiple `X-Forwarded-For` headers are equivalent to one comma-joined
    // header (RFC 7230 §3.2.2), so walk every value, right-to-left.
    let elements: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();

    if !elements.is_empty() {
        for element in elements.iter().rev() {
            match parse_forwarded_element(element) {
                Some(ip) => {
                    let ip = canonical_ip(ip);
                    if !trusted.contains(&ip) {
                        return ip.to_string();
                    }
                    // Another of our own proxies — keep walking left.
                }
                // Garbage or an obfuscated identifier. Stop: anything further
                // left is behind text we could not validate.
                None => return peer.to_string(),
            }
        }
        // Every hop in the chain was one of our own proxies; the original
        // client was never recorded.
        return peer.to_string();
    }

    // No forwarded chain — honour the single-valued `X-Real-IP` that nginx
    // and friends set. Same trust gate: only because the peer is declared.
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_forwarded_element)
    {
        return canonical_ip(ip).to_string();
    }

    peer.to_string()
}

/// Extractor form: the caller's address, already trust-resolved.
///
/// Use this in handlers instead of reading headers. It needs `ConnectInfo` in
/// the request extensions, which the server installs via
/// `into_make_service_with_connect_info::<SocketAddr>()` on both the plain and
/// the TLS listener.
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

impl ClientIp {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[async_trait]
impl<S> FromRequestParts<S> for ClientIp
where
    S: TrustedProxySource + Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip());
        Ok(ClientIp(resolve(
            peer,
            &parts.headers,
            state.trusted_proxies(),
        )))
    }
}

/// State that can say which proxies are trusted. Implemented by
/// `ConsoleState`; a trait so the extractor stays testable without one.
pub trait TrustedProxySource {
    fn trusted_proxies(&self) -> &TrustedProxies;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn trust(entries: &[&str]) -> TrustedProxies {
        TrustedProxies::parse(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    // ── The defect itself ────────────────────────────────────────────────

    /// #76 S5-4: an untrusted caller cannot move its own rate-limit bucket.
    #[test]
    fn spoofed_header_from_untrusted_peer_is_ignored() {
        let t = TrustedProxies::none();
        let peer = Some(ip("203.0.113.9"));
        let baseline = resolve(peer, &HeaderMap::new(), &t);
        assert_eq!(baseline, "203.0.113.9");

        for spoof in ["1.2.3.4", "1.2.3.5, 9.9.9.9", "  8.8.8.8  ", "2001:db8::1"] {
            assert_eq!(
                resolve(peer, &headers(&[("x-forwarded-for", spoof)]), &t),
                baseline,
                "x-forwarded-for: {spoof} must not change the key"
            );
        }
        assert_eq!(
            resolve(peer, &headers(&[("x-real-ip", "1.2.3.4")]), &t),
            baseline
        );
    }

    /// The brute-force shape from the finding: a rotating header used to mint
    /// a new bucket per request. Every rotation must now collapse to one key.
    #[test]
    fn rotating_spoof_yields_one_key() {
        let t = TrustedProxies::none();
        let peer = Some(ip("203.0.113.9"));
        let keys: std::collections::HashSet<String> = (0..64)
            .map(|n| {
                resolve(
                    peer,
                    &headers(&[("x-forwarded-for", &format!("1.2.3.{n}"))]),
                    &t,
                )
            })
            .collect();
        assert_eq!(keys.len(), 1, "rotating spoof produced {keys:?}");
    }

    /// Configuring a proxy does not make *other* peers trustworthy.
    #[test]
    fn peer_outside_the_trusted_set_is_still_ignored() {
        let t = trust(&["10.0.0.0/8"]);
        assert_eq!(
            resolve(
                Some(ip("203.0.113.9")),
                &headers(&[("x-forwarded-for", "1.2.3.4")]),
                &t
            ),
            "203.0.113.9"
        );
    }

    // ── The proxy case ───────────────────────────────────────────────────

    #[test]
    fn trusted_peer_has_its_forwarded_address_honoured() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "198.51.100.23")]),
                &t
            ),
            "198.51.100.23"
        );
    }

    /// The attacker owns the left end of the chain; only the right-most entry
    /// was written by our own proxy.
    #[test]
    fn rightmost_entry_wins_over_attacker_prefix() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "1.1.1.1, 2.2.2.2, 198.51.100.23")]),
                &t
            ),
            "198.51.100.23",
            "must not take the caller-authored left end"
        );
    }

    /// Two proxy hops: skip our own, take the first address neither of them.
    #[test]
    fn walks_left_past_our_own_proxies() {
        let t = trust(&["10.0.0.0/8"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "198.51.100.23, 10.0.0.9")]),
                &t
            ),
            "198.51.100.23"
        );
    }

    /// Split across repeated header lines — same chain semantics.
    #[test]
    fn repeated_headers_are_one_chain() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[
                    ("x-forwarded-for", "1.1.1.1"),
                    ("x-forwarded-for", "198.51.100.23"),
                ]),
                &t
            ),
            "198.51.100.23"
        );
    }

    /// An attacker behind our proxy can send garbage as the element to the
    /// left of the real one; we must not step over it and adopt their text.
    #[test]
    fn malformed_element_falls_back_to_the_peer() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "198.51.100.23, unknown")]),
                &t
            ),
            "10.0.0.7"
        );
    }

    #[test]
    fn all_hops_trusted_falls_back_to_the_peer() {
        let t = trust(&["10.0.0.0/8"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "10.0.0.4, 10.0.0.9")]),
                &t
            ),
            "10.0.0.7"
        );
    }

    #[test]
    fn trusted_peer_honours_x_real_ip_when_no_chain() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-real-ip", "198.51.100.23")]),
                &t
            ),
            "198.51.100.23"
        );
    }

    #[test]
    fn trusted_peer_with_no_headers_is_the_peer() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(Some(ip("10.0.0.7")), &HeaderMap::new(), &t),
            "10.0.0.7"
        );
    }

    // ── Edges ────────────────────────────────────────────────────────────

    /// No transport identity: one shared bucket, never a header-derived one.
    #[test]
    fn missing_connect_info_never_falls_back_to_headers() {
        for t in [TrustedProxies::none(), trust(&["0.0.0.0/0"])] {
            assert_eq!(
                resolve(None, &headers(&[("x-forwarded-for", "1.2.3.4")]), &t),
                UNKNOWN_IP
            );
        }
    }

    /// A dual-stack listener reports IPv4 peers as `::ffff:a.b.c.d`; the
    /// trusted-proxy entry is written in plain v4 form.
    #[test]
    fn v4_mapped_peer_matches_v4_trust_entry() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("::ffff:10.0.0.7")),
                &headers(&[("x-forwarded-for", "198.51.100.23")]),
                &t
            ),
            "198.51.100.23"
        );
        // …and the key for an ordinary v4-mapped client is the plain v4 form,
        // so one client never occupies two buckets.
        assert_eq!(
            resolve(Some(ip("::ffff:203.0.113.9")), &HeaderMap::new(), &t),
            "203.0.113.9"
        );
    }

    #[test]
    fn forwarded_entries_with_ports_are_accepted() {
        let t = trust(&["10.0.0.7"]);
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "198.51.100.23:44321")]),
                &t
            ),
            "198.51.100.23"
        );
        assert_eq!(
            resolve(
                Some(ip("10.0.0.7")),
                &headers(&[("x-forwarded-for", "[2001:db8::5]:443")]),
                &t
            ),
            "2001:db8::5"
        );
    }

    /// The socket port is not part of identity — otherwise every connection
    /// from one host would get its own quota.
    #[test]
    fn peer_port_is_not_part_of_the_key() {
        let t = TrustedProxies::none();
        let a: SocketAddr = "203.0.113.9:1111".parse().unwrap();
        let b: SocketAddr = "203.0.113.9:2222".parse().unwrap();
        assert_eq!(
            resolve(Some(a.ip()), &HeaderMap::new(), &t),
            resolve(Some(b.ip()), &HeaderMap::new(), &t)
        );
    }
}
