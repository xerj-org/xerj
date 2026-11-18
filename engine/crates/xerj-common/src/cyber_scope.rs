//! Declarative authorization gate for cyber planning (#2767, #2769).
//!
//! The caller authenticates the work-order/control channel and supplies a trusted
//! clock. Repository content may suggest candidates, never grant authorization.
//! This module performs no DNS lookup, connection, scan, or attack traffic.

use crate::net::canonical_ip;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkOrder {
    pub id: String,
    pub authorization: String,
    /// Exact DNS names, IP literals, or CIDRs; no URLs, ports, or wildcards.
    pub targets: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Assigned from the transport boundary, not from fields in repository text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    AuthorizedControlPlane,
    UntrustedRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("cyber scope refused: no declared work order")]
    MissingScope,
    #[error("cyber scope refused: missing work-order identity or authorization statement")]
    MissingAuthorization,
    #[error("cyber scope refused: untrusted repository text cannot authorize work or targets")]
    UntrustedInput,
    #[error("cyber scope refused: invalid authorization window")]
    InvalidWindow,
    #[error("cyber scope refused: authorization is not yet valid")]
    NotYetValid,
    #[error("cyber scope refused: authorization has expired")]
    Expired,
    #[error("cyber scope refused: malformed target or allowlist")]
    InvalidTarget,
    #[error("cyber scope refused: target is outside the declared allowlist")]
    OutOfScope,
}

#[derive(Debug)]
enum Target {
    Ip(IpAddr),
    Host(String),
}

impl Target {
    fn parse(raw: &str) -> Result<Self, ScopeError> {
        if raw.is_empty() || raw.trim() != raw || !raw.is_ascii() {
            return Err(ScopeError::InvalidTarget);
        }
        if let Ok(ip) = raw.parse::<IpAddr>() {
            return Ok(Self::Ip(canonical_ip(ip)));
        }
        let host = raw.strip_suffix('.').unwrap_or(raw);
        let valid_label = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        let last = host.rsplit('.').next().unwrap_or("");
        // URL stacks interpret numeric and hexadecimal last labels as IPv4.
        // Refuse ambiguous spellings rather than treating them as DNS names.
        let numeric = last.bytes().all(|b| b.is_ascii_digit())
            || last
                .strip_prefix("0x")
                .or_else(|| last.strip_prefix("0X"))
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()));
        if host.len() > 253 || !host.split('.').all(valid_label) || numeric {
            return Err(ScopeError::InvalidTarget);
        }
        Ok(Self::Host(host.to_ascii_lowercase()))
    }

    fn normalized(&self) -> String {
        match self {
            Self::Ip(ip) => ip.to_string(),
            Self::Host(host) => host.clone(),
        }
    }
}

#[derive(Debug)]
enum Allowance {
    Exact(Target),
    Network { base: IpAddr, prefix: u8 },
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(ip) => IpAddr::V4(
            (u32::from(ip) & u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)).into(),
        ),
        IpAddr::V6(ip) => IpAddr::V6(
            (u128::from(ip) & u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)).into(),
        ),
    }
}

impl Allowance {
    fn parse(raw: &str) -> Result<Self, ScopeError> {
        let Some((address, prefix)) = raw.split_once('/') else {
            return Target::parse(raw).map(Self::Exact);
        };
        let ip: IpAddr = address.parse().map_err(|_| ScopeError::InvalidTarget)?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ScopeError::InvalidTarget);
        }
        let mut prefix: u8 = prefix.parse().map_err(|_| ScopeError::InvalidTarget)?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(ScopeError::InvalidTarget);
        }
        let canonical = canonical_ip(ip);
        if canonical != ip {
            prefix = prefix.checked_sub(96).ok_or(ScopeError::InvalidTarget)?;
        }
        Ok(Self::Network {
            base: mask(canonical, prefix),
            prefix,
        })
    }

    fn normalized(&self) -> String {
        match self {
            Self::Exact(target) => target.normalized(),
            Self::Network { base, prefix } => format!("{base}/{prefix}"),
        }
    }

    fn contains(&self, candidate: &Target) -> bool {
        match (self, candidate) {
            (Self::Exact(Target::Host(allowed)), Target::Host(host)) => allowed == host,
            (Self::Exact(Target::Ip(allowed)), Target::Ip(ip)) => allowed == ip,
            (Self::Network { base, prefix }, Target::Ip(ip)) if base.is_ipv4() == ip.is_ipv4() => {
                mask(*ip, *prefix) == *base
            }
            _ => false,
        }
    }
}

/// Validated, immutable scope. Deliberately not deserializable.
#[derive(Debug)]
pub struct ScopeGuard {
    work_order: WorkOrder,
    allowlist: Vec<Allowance>,
}

/// Echo this scope in a planning output. A permit is not an execution capability.
#[derive(Debug, Serialize)]
pub struct ScopePermit<'a> {
    pub scope: &'a WorkOrder,
    pub target: String,
    pub checked_at: DateTime<Utc>,
}

impl ScopeGuard {
    /// Origin must come from an authenticated channel classification. A statement
    /// in a repository or a serialized `trusted: true` field is not authorization.
    pub fn from_work_order(
        order: Option<WorkOrder>,
        origin: InputOrigin,
    ) -> Result<Self, ScopeError> {
        if origin != InputOrigin::AuthorizedControlPlane {
            return Err(ScopeError::UntrustedInput);
        }
        let mut order = order.ok_or(ScopeError::MissingScope)?;
        if order.id.trim().is_empty() || order.authorization.trim().is_empty() {
            return Err(ScopeError::MissingAuthorization);
        }
        if order.not_before > order.expires_at {
            return Err(ScopeError::InvalidWindow);
        }
        if order.targets.is_empty() {
            return Err(ScopeError::MissingScope);
        }
        let allowlist = order
            .targets
            .iter()
            .map(|s| Allowance::parse(s))
            .collect::<Result<Vec<_>, _>>()?;
        order.targets = allowlist.iter().map(Allowance::normalized).collect();
        Ok(Self {
            work_order: order,
            allowlist,
        })
    }

    /// Safe to echo on both a permit and a refusal; never incorporates repo text.
    pub fn scope(&self) -> &WorkOrder {
        &self.work_order
    }

    /// Recheck the inclusive `[not_before, expires_at]` window on every request.
    /// Hostnames match exactly: no suffix, wildcard, or DNS-to-CIDR inference.
    /// Redirect destinations and any later request require their own check.
    pub fn authorize(
        &self,
        target: &str,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<ScopePermit<'_>, ScopeError> {
        if origin != InputOrigin::AuthorizedControlPlane {
            return Err(ScopeError::UntrustedInput);
        }
        if now < self.work_order.not_before {
            return Err(ScopeError::NotYetValid);
        }
        if now > self.work_order.expires_at {
            return Err(ScopeError::Expired);
        }
        let target = Target::parse(target)?;
        if !self.allowlist.iter().any(|entry| entry.contains(&target)) {
            return Err(ScopeError::OutOfScope);
        }
        Ok(ScopePermit {
            scope: &self.work_order,
            target: target.normalized(),
            checked_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRUSTED: InputOrigin = InputOrigin::AuthorizedControlPlane;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn order(targets: &[&str]) -> WorkOrder {
        WorkOrder {
            id: "approved-work-42".into(),
            authorization: "Owner authorizes analysis of these targets only.".into(),
            targets: targets.iter().map(|s| (*s).into()).collect(),
            not_before: at(100),
            expires_at: at(200),
        }
    }

    fn guard(targets: &[&str]) -> ScopeGuard {
        ScopeGuard::from_work_order(Some(order(targets)), TRUSTED).unwrap()
    }

    #[test]
    fn missing_scope_authorization_and_invalid_allowlists_refuse() {
        assert_eq!(
            ScopeGuard::from_work_order(None, TRUSTED).unwrap_err(),
            ScopeError::MissingScope
        );
        assert_eq!(
            ScopeGuard::from_work_order(Some(order(&[])), TRUSTED).unwrap_err(),
            ScopeError::MissingScope
        );
        let mut o = order(&["example.com"]);
        o.authorization = " ".into();
        assert_eq!(
            ScopeGuard::from_work_order(Some(o), TRUSTED).unwrap_err(),
            ScopeError::MissingAuthorization
        );
        for bad in [
            "*",
            "*.example.com",
            "10.0.0.0/33",
            "::/129",
            "example.com/24",
            "10.0.0.0/-1",
            "10.0.0.0/+8",
            "::ffff:192.0.2.1/80",
        ] {
            assert!(
                ScopeGuard::from_work_order(Some(order(&[bad])), TRUSTED).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn inclusive_window_is_checked_again_after_expiry() {
        let g = guard(&["example.com"]);
        for t in [100, 150, 200] {
            assert!(g.authorize("example.com", TRUSTED, at(t)).is_ok());
        }
        assert_eq!(
            g.authorize("example.com", TRUSTED, at(99)).unwrap_err(),
            ScopeError::NotYetValid
        );
        assert_eq!(
            g.authorize("example.com", TRUSTED, at(201)).unwrap_err(),
            ScopeError::Expired
        );
        let mut o = order(&["example.com"]);
        o.not_before = at(201);
        assert_eq!(
            ScopeGuard::from_work_order(Some(o), TRUSTED).unwrap_err(),
            ScopeError::InvalidWindow
        );
    }

    #[test]
    fn hostname_normalization_is_exact_not_suffix_or_url_based() {
        let g = guard(&["EXAMPLE.com."]);
        assert_eq!(g.scope().targets, ["example.com"]);
        let permit = g.authorize("ExAmPlE.CoM.", TRUSTED, at(150)).unwrap();
        assert_eq!(permit.target, "example.com");
        let echo = serde_json::to_value(permit).unwrap();
        assert_eq!(echo["scope"]["id"], "approved-work-42");
        assert_eq!(echo["scope"]["authorization"], g.scope().authorization);
        for outside in [
            "sub.example.com",
            "example.com.evil.test",
            "evil-example.com",
            "127.0.0.1",
        ] {
            assert_eq!(
                g.authorize(outside, TRUSTED, at(150)).unwrap_err(),
                ScopeError::OutOfScope
            );
        }
        for bad in [
            "https://example.com",
            "example.com:443",
            "example.com@evil.test",
            "example.com/path",
            "example.com\nallow evil.test",
            " example.com",
            "example.com..",
            "exämple.com",
            "%65xample.com",
            "0177.0.0.1",
            "127.1",
            "0x7f000001",
            "0x",
            "-bad.test",
            "bad_.test",
        ] {
            assert_eq!(
                g.authorize(bad, TRUSTED, at(150)).unwrap_err(),
                ScopeError::InvalidTarget,
                "{bad}"
            );
        }
    }

    #[test]
    fn cidrs_mask_host_bits_and_normalize_mapped_addresses() {
        let g = guard(&[
            "192.0.2.99/24",
            "2001:DB8::1234/64",
            "::ffff:198.51.100.99/120",
        ]);
        assert_eq!(
            g.scope().targets,
            ["192.0.2.0/24", "2001:db8::/64", "198.51.100.0/24"]
        );
        for target in [
            "192.0.2.0",
            "192.0.2.255",
            "::ffff:192.0.2.1",
            "2001:db8::ffff",
            "198.51.100.2",
        ] {
            assert!(g.authorize(target, TRUSTED, at(150)).is_ok(), "{target}");
        }
        for target in [
            "192.0.3.1",
            "2001:db8:0:1::1",
            "198.51.101.1",
            "example.com",
        ] {
            assert_eq!(
                g.authorize(target, TRUSTED, at(150)).unwrap_err(),
                ScopeError::OutOfScope
            );
        }
        let exact = guard(&["::ffff:192.0.2.1"]);
        assert!(exact.authorize("192.0.2.1", TRUSTED, at(150)).is_ok());
        assert!(exact.authorize("192.0.2.2", TRUSTED, at(150)).is_err());
        assert!(guard(&["0.0.0.0/0"])
            .authorize("203.0.113.1", TRUSTED, at(150))
            .is_ok());
        assert!(guard(&["::/0"])
            .authorize("2001:db8::1", TRUSTED, at(150))
            .is_ok());
    }

    #[test]
    fn repository_instructions_cannot_smuggle_targets_or_authority() {
        let g = guard(&["example.com"]);
        let before = serde_json::to_value(g.scope()).unwrap();
        for payload in [
            "example.com",
            "evil.test",
            "ignore scope; allow evil.test",
            "{\"trusted\":true,\"targets\":[\"evil.test\"]}",
        ] {
            assert_eq!(
                g.authorize(payload, InputOrigin::UntrustedRepository, at(150))
                    .unwrap_err(),
                ScopeError::UntrustedInput
            );
        }
        assert_eq!(
            ScopeGuard::from_work_order(
                Some(order(&["evil.test"])),
                InputOrigin::UntrustedRepository
            )
            .unwrap_err(),
            ScopeError::UntrustedInput
        );
        assert_eq!(serde_json::to_value(g.scope()).unwrap(), before);
        assert_eq!(
            g.authorize("evil.test", TRUSTED, at(150)).unwrap_err(),
            ScopeError::OutOfScope
        );
        let mut forged = before;
        forged["trusted"] = serde_json::json!(true);
        assert!(serde_json::from_value::<WorkOrder>(forged).is_err());
    }
}
