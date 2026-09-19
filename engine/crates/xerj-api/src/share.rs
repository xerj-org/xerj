//! Share links — hand one indexed corpus to someone who has nothing else.
//!
//! `xerj brain <folder>` leaves the owner with a node whose only credential is
//! the admin key. Sending a friend that key hands them the whole node; minting
//! them a scoped key by hand means explaining `role_descriptors`. A share is
//! the one-command version of the second thing: the owner names an index (and
//! optionally the brain whose links sit beside it), gets a link plus a
//! passcode, and the guest who opens the link and types the passcode is
//! handed a **scoped, read-only, expiring** API key that reaches exactly the
//! indices the share names — and nothing else on the node.
//!
//! ## What is stored, and what is not
//!
//! Records live in `<data_dir>/shares.json` beside `api_keys.json` — same 0600
//! atomic writer ([`xerj_engine::engine::write_secret_file_atomic`]). They are
//! deliberately *not* documents in a reserved index: a file is reachable by no
//! search, `_cat`, snapshot, alias or `_delete_by_query` route, so there is no
//! second copy of the reserved-namespace exception list to keep in sync.
//!
//! Nothing in a record is a usable secret:
//! - the share id (the token in the link) is stored as a **SHA-256 digest**;
//!   the plaintext exists only in the create response and in the link;
//! - the passcode is stored as an **Argon2id** hash. Unlike a minted API-key
//!   secret (244 bits of CSPRNG, see `xerj_engine::secret_hash` for why that
//!   one is a fast hash) a passcode is short and may have been chosen by a
//!   human, so an offline guess is a real threat and a slow hash is the right
//!   tool. It is verified once per claim, on a rate-limited route, so the cost
//!   never lands on the request path;
//! - the guest's API key is **minted at claim time**, one per claim, expiring
//!   when the share expires. The record keeps only the minted key *ids* so a
//!   revoke can invalidate every key the share ever produced. No credential
//!   is ever written to the share file.
//!
//! ## Who may do what
//!
//! | Route | Who |
//! |---|---|
//! | `POST /_share`, `GET /_share`, `DELETE /_share/{handle}` | superuser only (the admin key) — a scoped or unscoped key gets 403 |
//! | `POST /_share/claim` | **nobody in particular** — it is exempt from authentication (`auth::is_share_claim_path`) the way `/v1/metrics` is for the scrape token, and rate-limited per source address and per share instead |
//!
//! The claim route is the only unauthenticated door that hands out a
//! credential, so it is narrow on purpose: one exact path, `POST` only, a body
//! capped at [`MAX_CLAIM_BODY_BYTES`], a sliding-window throttle,
//! `Cache-Control: no-store` on every answer, and every outcome — success,
//! wrong passcode, exhausted, expired, revoked, unknown, throttled — in the
//! audit log with the source address.
//!
//! ## The share id is never part of a request path
//!
//! The claim body is `{id, passcode}`. The first cut put the id in the path
//! (`POST /_share/{id}/claim`), and the guest page's care to keep the id in the
//! URL *fragment* was undone one request later: a request path is exactly what
//! an access log records. With `logging.access_log = true` this node wrote the
//! id to `server.log` twice per claim, and any reverse proxy or tunnel in front
//! of it logged the same line (review of PR #947, reproduced 2026-09-19). A
//! body is in none of those logs. `DELETE /_share/{handle}` takes the public
//! handle, which opens nothing. It also accepts the full id, for an API caller
//! who kept only that — such a call puts the id in its own request path, which
//! is why `xerj share --revoke` sends only the handle.
//!
//! ## The two throttles, and why a tunnel does not break them
//!
//! - A claim against a **known** share is charged to that share's window
//!   ([`SHARE_PER_MINUTE`] / [`SHARE_PER_HOUR`]), from anywhere. This is the
//!   passcode lockout, and it does not depend on knowing who is asking.
//! - A claim against an **unknown** id is charged to the source address's
//!   window ([`IP_PER_MINUTE`] / [`IP_PER_HOUR`]). It bounds junk traffic and
//!   the audit lines it writes.
//!
//! They are deliberately *not* stacked. The first cut charged the source
//! window before looking the share up, which is fine on a directly exposed
//! node and wrong behind `xerj share --tunnel`: every request a tunnel (or any
//! reverse proxy on the same host) delivers arrives from `127.0.0.1`, so all
//! guests shared one source bucket, and ten junk requests a minute from anyone
//! who had found the hostname answered every real guest with `429` — a
//! one-line denial of service on the feature. The fix is not to believe
//! `X-Forwarded-For`: a direct client writes that header itself (#76 S5-4),
//! and an unconfigured node believes nobody. It is to stop the collapsed
//! bucket from mattering: a real share id is governed by its own window, which
//! no amount of junk can touch, and junk is governed by the source window,
//! where collapsing every tunnel client into one bucket only makes the bound
//! tighter.
//!
//! Operators who want each guest's real address — in the audit log and as the
//! junk-traffic key — declare the proxy the usual way,
//! `server.trusted_proxies = ["127.0.0.1", "::1"]`; then, and only then,
//! `X-Forwarded-For` is read, right to left (see [`ClaimSource`]). IPv6
//! sources are keyed by their /64, since one host is routinely handed a whole
//! /64 to rotate through.
//!
//! ## An open node cannot share
//!
//! With authentication off (`--insecure`, `auth.enabled = false`, or no admin
//! key) every request is [`Principal::Superuser`], so a "scoped, read-only"
//! guest key would restrict nothing. `POST /_share` and the claim route answer
//! `409` there rather than hand out a credential that promises a boundary the
//! node is not enforcing.
//!
//! ## What a guest can reach
//!
//! The minted key carries one role, named `share:<handle>`: `read` on the
//! share's `indices` plus, when a brain is named, `read` on that brain's edges
//! index (`authz::brain_edges_index`). Two things then confine it:
//!
//! 1. the index grants — the same per-index authorization every scoped key
//!    gets, in the middleware and again in the engine's index funnel;
//! 2. a **route allow-list** that applies to share guests only
//!    (`authz::guest_route_allowed`, keyed on [`GUEST_ROLE_PREFIX`]):
//!    `_search`, `_count`, `_msearch`, `_mget`, `_mapping`, `_field_caps` and
//!    `GET _doc/{id}` on the granted indices, and
//!    `GET /_graph/{brain}/ego|overview`. An ordinary scoped key is given a
//!    *filtered* view of `_cat`, `_cluster/*`, `_nodes` and friends because
//!    Kibana needs one; a guest is given none of it. No `scroll`, no `_pit`,
//!    no `_async_search` either — nothing that parks state on the owner's
//!    machine.
//!
//! The `autoindex-catalog` index is **never** granted, and cannot be named in a
//! share ([`CATALOG_INDEX`], refused by `validate_share_index`): it lists every
//! corpus on the node, the engine has no document-level security to filter it
//! with, and the guest page already receives the index name directly.
//!
//! Every response to a guest key is `Cache-Control: no-store`
//! (`authz::authz_middleware`): it is somebody's documents, on its way through
//! whatever proxy or tunnel the owner put in front of the node.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::{
    extract::{ConnectInfo, FromRequestParts, Path as AxumPath, State},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use xerj_engine::rbac::{Privilege, Role};

use crate::auth::Principal;
use crate::error::{EsErrorBody, EsErrorResponse, EsRootCause};
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Limits
// ─────────────────────────────────────────────────────────────────────────────

/// Default lifetime when the request names none.
pub const DEFAULT_EXPIRES_IN: &str = "24h";
/// Longest lifetime a share may be given.
pub const MAX_EXPIRES_MS: u64 = 30 * 86_400_000;
/// Shortest lifetime. Seconds are accepted so a test can watch one expire.
pub const MIN_EXPIRES_MS: u64 = 1_000;
/// `max_claims` bounds.
pub const DEFAULT_MAX_CLAIMS: u32 = 1;
pub const MAX_MAX_CLAIMS: u32 = 50;
/// A caller-supplied passcode must be at least this long.
pub const MIN_PASSCODE_CHARS: usize = 6;
pub const MAX_PASSCODE_CHARS: usize = 128;
pub const MAX_LABEL_CHARS: usize = 120;
/// How many indices one share may name.
pub const MAX_INDICES: usize = 32;
/// Largest body the unauthenticated claim route reads: a 32-hex id and a
/// passcode of at most [`MAX_PASSCODE_CHARS`] characters (four bytes each at
/// worst), with room for JSON around them. The router enforces it
/// (`router.rs`, the `/_share/claim` route) — the node-wide
/// `limits.max_body_bytes` is sized for bulk ingest, not for a caller who has
/// not authenticated.
pub const MAX_CLAIM_BODY_BYTES: usize = 4096;
/// `xerj autoindex`'s catalog: one row per corpus on the node — index names,
/// file counts, sample queries. Same value as
/// `xerj_autoindex::catalog::CATALOG_INDEX` (this crate does not depend on
/// that one; `xerj-server` has a test that the two agree).
pub const CATALOG_INDEX: &str = "autoindex-catalog";

/// Claim attempts per source address per minute / per hour.
pub const IP_PER_MINUTE: u32 = 10;
pub const IP_PER_HOUR: u32 = 100;
/// Claim attempts against one share per minute / per hour, from anywhere.
/// This is the lockout: a passcode has ~40 bits, and thirty guesses an hour
/// against it is a search that does not finish.
pub const SHARE_PER_MINUTE: u32 = 10;
pub const SHARE_PER_HOUR: u32 = 30;

/// Every guest key's single role is named `share:<handle>`. `authz` keys its
/// guest route allow-list on this prefix (`Principal::is_share_guest`).
pub const GUEST_ROLE_PREFIX: &str = "share:";

/// Hard ceiling on live rate windows. Past it, new source addresses share one
/// overflow bucket — throttled together rather than tracked without bound.
const MAX_WINDOWS: usize = 50_000;
/// Stale windows are swept every this-many charges, not on every call.
const PRUNE_EVERY: u64 = 64;

const WINDOW_MIN_MS: u64 = 60_000;
const WINDOW_HOUR_MS: u64 = 3_600_000;

/// The file name under the data directory.
const SHARES_FILE: &str = "shares.json";

// ─────────────────────────────────────────────────────────────────────────────
// Records
// ─────────────────────────────────────────────────────────────────────────────

/// One share, as persisted. The two secret-derived fields are private and
/// only ever hold digests — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareRecord {
    /// Public handle: the first 12 hex chars of `id_hash`. Safe to list and
    /// to print in a revoke command; it cannot be turned back into the link.
    pub handle: String,
    /// SHA-256 of the share id (the token carried in the link), lowercase hex.
    id_hash: String,
    /// Argon2id PHC string of the passcode.
    passcode_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Concrete index names the guest may read. Never patterns.
    pub indices: Vec<String>,
    /// The brain whose edges index is also granted, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain: Option<String>,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub max_claims: u32,
    #[serde(default)]
    pub claims: u32,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_ms: Option<u64>,
    /// Ids of every API key a claim minted, so a revoke can invalidate them.
    #[serde(default)]
    pub key_ids: Vec<String>,
}

impl ShareRecord {
    /// Every index the guest key is granted `read` on.
    pub fn granted_indices(&self) -> Vec<String> {
        let mut out = self.indices.clone();
        if let Some(brain) = &self.brain {
            out.push(crate::authz::brain_edges_index(brain));
        }
        out
    }

    /// Why this share cannot be claimed right now, if it cannot.
    fn unavailable(&self, now_ms: u64) -> Option<&'static str> {
        if self.revoked {
            Some("this share link has been revoked")
        } else if now_ms >= self.expires_ms {
            Some("this share link has expired")
        } else if self.claims >= self.max_claims {
            Some("this share link has been used up (max_claims reached)")
        } else {
            None
        }
    }

    /// The listing / response shape. Carries no hash.
    fn public_json(&self, now_ms: u64) -> Value {
        let status = if self.revoked {
            "revoked"
        } else if now_ms >= self.expires_ms {
            "expired"
        } else if self.claims >= self.max_claims {
            "exhausted"
        } else {
            "active"
        };
        json!({
            "handle": self.handle,
            "label": self.label,
            "index": self.indices.join(","),
            "indices": self.indices,
            "brain": self.brain,
            "created_at": rfc3339(self.created_ms),
            "expires_at": rfc3339(self.expires_ms),
            "max_claims": self.max_claims,
            "claims": self.claims,
            "claims_left": self.max_claims.saturating_sub(self.claims),
            "revoked": self.revoked,
            "status": status,
        })
    }
}

#[derive(Clone, Copy)]
struct RateWindow {
    count: u32,
    start_ms: u64,
}

/// The share store: records keyed by handle, plus the claim rate limiter.
pub struct ShareStore {
    path: PathBuf,
    records: DashMap<String, ShareRecord>,
    windows: DashMap<String, RateWindow>,
    charges: std::sync::atomic::AtomicU64,
}

/// A refused charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttled {
    /// Seconds until the window that refused rolls over.
    pub retry_after_secs: u64,
    /// True for the first refusal in that window only. The caller audits that
    /// one and stays quiet for the rest, so an unauthenticated flood cannot
    /// turn the audit log into its own denial of service.
    pub first_in_window: bool,
}

impl ShareStore {
    /// Load `<data_dir>/shares.json`, or start empty. A file that does not
    /// parse is logged and left alone rather than overwritten — the operator
    /// can inspect it; nothing in it could authenticate anyway.
    pub fn open(data_dir: &str) -> Arc<Self> {
        let path = Path::new(data_dir).join(SHARES_FILE);
        let records = DashMap::new();
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<ShareRecord>>(&bytes) {
                Ok(list) => {
                    for r in list {
                        records.insert(r.handle.clone(), r);
                    }
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "shares.json does not parse; starting with no shares")
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "shares.json unreadable; starting with no shares")
            }
        }
        Arc::new(Self {
            path,
            records,
            windows: DashMap::new(),
            charges: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn persist(&self) -> std::io::Result<()> {
        let mut list: Vec<ShareRecord> = self.records.iter().map(|r| r.value().clone()).collect();
        list.sort_by_key(|r| r.created_ms);
        let bytes = serde_json::to_vec_pretty(&list)?;
        xerj_engine::engine::write_secret_file_atomic(&self.path, &bytes)
    }

    fn find_by_id(&self, share_id: &str) -> Option<ShareRecord> {
        let (hash, handle) = hash_share_id(share_id);
        let rec = self.records.get(&handle)?;
        // Full-digest compare, in constant time, so a handle prefix collision
        // (or a caller who only knows the handle) never resolves to a record.
        if constant_time_eq(rec.id_hash.as_bytes(), hash.as_bytes()) {
            Some(rec.clone())
        } else {
            None
        }
    }

    /// Resolve `DELETE /_share/{id}` — accepts the full share id (what the
    /// create response returned) or the public handle (what the listing
    /// shows). Either is enough to revoke: revoking is superuser-only.
    fn find_for_revoke(&self, id_or_handle: &str) -> Option<String> {
        if let Some(rec) = self.find_by_id(id_or_handle) {
            return Some(rec.handle);
        }
        self.records.get(id_or_handle).map(|r| r.handle.clone())
    }

    /// Snapshot of every record, oldest first.
    pub fn list(&self) -> Vec<ShareRecord> {
        let mut list: Vec<ShareRecord> = self.records.iter().map(|r| r.value().clone()).collect();
        list.sort_by_key(|r| r.created_ms);
        list
    }

    /// Test seam: rewrite one record's expiry. Not reachable over HTTP.
    #[doc(hidden)]
    pub fn set_expiry_for_test(&self, handle: &str, expires_ms: u64) {
        if let Some(mut r) = self.records.get_mut(handle) {
            r.expires_ms = expires_ms;
        }
    }

    /// Charge one claim attempt against `key`. `Err` when over either window.
    /// Same sliding-window shape as the console's auth limiter
    /// (`xerj-console-api::auth::rate_limit`), re-implemented here because
    /// that one is bound to `ConsoleState` and this crate must not depend on
    /// the console.
    fn charge(
        &self,
        key: &str,
        per_minute: u32,
        per_hour: u32,
        now_ms: u64,
    ) -> Result<(), Throttled> {
        use std::sync::atomic::Ordering;
        if self
            .charges
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(PRUNE_EVERY)
        {
            self.windows
                .retain(|_, w| now_ms.saturating_sub(w.start_ms) < WINDOW_HOUR_MS);
        }
        // Bounded state for an unauthenticated route: once the table is full,
        // addresses it has not seen yet are throttled as one.
        // Only source keys collapse: share keys are bounded by the number of
        // shares, which only the superuser can create.
        let key = if key.starts_with("ip:")
            && self.windows.len() >= MAX_WINDOWS
            && !self.windows.contains_key(&format!("{key}:m"))
        {
            "ip:overflow"
        } else {
            key
        };
        for (suffix, window_ms, limit) in [
            ("m", WINDOW_MIN_MS, per_minute),
            ("h", WINDOW_HOUR_MS, per_hour),
        ] {
            let k = format!("{key}:{suffix}");
            let mut w = self.windows.entry(k).or_insert(RateWindow {
                count: 0,
                start_ms: now_ms,
            });
            if now_ms.saturating_sub(w.start_ms) > window_ms {
                *w = RateWindow {
                    count: 0,
                    start_ms: now_ms,
                };
            }
            w.count = w.count.saturating_add(1);
            if w.count > limit {
                return Err(Throttled {
                    retry_after_secs: (w.start_ms + window_ms).saturating_sub(now_ms) / 1000 + 1,
                    first_in_window: w.count == limit + 1,
                });
            }
        }
        Ok(())
    }
}

/// The rate-limit key for a claim source: the address itself for IPv4, the
/// enclosing /64 for IPv6. A single IPv6 host is routinely delegated a whole
/// /64 and can take a fresh address per request, so per-address buckets there
/// are no throttle at all. Anything that is not an address (`unknown`) is its
/// own bucket.
fn source_bucket(source: &str) -> String {
    match source.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => {
            let mut seg = v6.segments();
            seg[4..].fill(0);
            format!("{}/64", std::net::Ipv6Addr::from(seg))
        }
        _ => source.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Secrets
// ─────────────────────────────────────────────────────────────────────────────

/// A fresh share id: 128 bits from the OS CSPRNG, lowercase hex.
fn mint_share_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

/// `(full sha256 hex, 12-char handle)` for a share id.
fn hash_share_id(share_id: &str) -> (String, String) {
    let digest = Sha256::digest(share_id.as_bytes());
    let full = hex(&digest);
    let handle = full[..12].to_string();
    (full, handle)
}

/// Alphabet for generated passcodes: lowercase + digits with the four
/// look-alikes (`0/o`, `1/l`, plus `i`) removed, 31 symbols. Eight of them is
/// ~39.6 bits, which with [`SHARE_PER_HOUR`] guesses an hour is out of reach
/// online, and which is what the human on the other end has to type.
const PASSCODE_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// `xxxx-xxxx` from the alphabet above.
pub fn generate_passcode() -> String {
    let mut out = String::with_capacity(9);
    let mut rng = rand::rngs::OsRng;
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        // Rejection-free: 31 does not divide 256, so draw a u32 and take the
        // modulo of a much larger range; the bias is < 2^-24 per symbol.
        let n = rng.next_u32() as usize % PASSCODE_ALPHABET.len();
        out.push(PASSCODE_ALPHABET[n] as char);
    }
    out
}

fn hash_passcode(passcode: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(passcode.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Constant-time-by-construction: argon2's verifier compares the derived
/// key with `subtle`. Anything unparsable is a `false`, never a panic.
pub fn verify_passcode(presented: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(presented.as_bytes(), &parsed)
        .is_ok()
}

fn hex(bytes: &[u8]) -> String {
    const T: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(T[(b >> 4) as usize] as char);
        s.push(T[(b & 15) as usize] as char);
    }
    s
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─────────────────────────────────────────────────────────────────────────────
// Small helpers
// ─────────────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// `"30m"`, `"24h"`, `"7d"` (and `"90s"`) → milliseconds. `None` on anything
/// else, including a bare number: the unit is the whole point.
pub fn parse_expires_in(spec: &str) -> Option<u64> {
    let s = spec.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let n: u64 = num.trim().parse().ok()?;
    let per_ms: u64 = match unit.trim() {
        "d" => 86_400_000,
        "h" => 3_600_000,
        "m" => 60_000,
        "s" => 1_000,
        _ => return None,
    };
    n.checked_mul(per_ms)
}

/// Is `name` a concrete index a share may grant? Same alphabet the engine
/// accepts, minus anything that is a pattern or a list — a grant must name
/// exactly what it grants — and minus the reserved namespace, which is only
/// reachable through `brain`.
fn validate_share_index(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 {
        return Err("index name must be 1–255 characters".into());
    }
    if name.starts_with(crate::authz::RESERVED_INDEX_PREFIX) {
        return Err(format!(
            "index `{name}` is in the reserved .xerj-memory-* namespace; share a brain with `brain` instead"
        ));
    }
    // Every dot-index is the node's own: `.xerj_sessions`, `.xerj_api_tokens`,
    // `.xerj_passkeys` and `.xerj_magic_links` are credential stores, and
    // `.xerj_audit` records who connected from where. The check above named only
    // the memory namespace, so a share on any of these was accepted — found by
    // the first live run (2026-09-18), which minted a guest link onto the audit
    // log. A share grants a *user's* corpus, and no user index starts with a dot,
    // so the rule is the whole prefix rather than a list that has to be kept in
    // step with every system index added later.
    if name.starts_with('.') {
        return Err(format!(
            "index `{name}` is a system index and cannot be shared; a share names one of your own indices"
        ));
    }
    if name.starts_with('_') {
        return Err(format!("index `{name}` is not an index name"));
    }
    // The catalog describes EVERY corpus on the node. The docs said it was
    // "never granted" while `xerj share autoindex-catalog` minted a guest key
    // that read all of it (review of PR #947). A guest is handed one corpus;
    // the list of the others is not part of it, and there is no
    // document-level security to filter the list down.
    if name == CATALOG_INDEX {
        return Err(format!(
            "index `{name}` is the autoindex catalog: it describes every corpus on this \
             node, not one of them, so it cannot be shared. Share the corpus index itself \
             (`xerj autoindex map` lists them)"
        ));
    }
    for c in name.chars() {
        if c.is_whitespace()
            || matches!(
                c,
                '*' | ',' | '/' | '\\' | '?' | '"' | '<' | '>' | '|' | '#'
            )
        {
            return Err(format!(
                "index `{name}` contains `{c}`; a share names concrete indices, not patterns"
            ));
        }
        if c.is_ascii_uppercase() {
            return Err(format!("index `{name}` must be lowercase"));
        }
    }
    Ok(())
}

fn es_error(status: StatusCode, error_type: &str, reason: String) -> Response {
    let body = EsErrorResponse {
        error: EsErrorBody {
            root_cause: vec![EsRootCause {
                error_type: error_type.to_string(),
                reason: reason.clone(),
                resource_type: None,
                resource_id: None,
                index_uuid: None,
                index: None,
            }],
            error_type: error_type.to_string(),
            reason,
            resource_type: None,
            resource_id: None,
            index_uuid: None,
            index: None,
            request_id: None,
        },
        status: status.as_u16(),
        xerj_hint: None,
    };
    (status, Json(body)).into_response()
}

fn bad_request(reason: String) -> Response {
    es_error(
        StatusCode::BAD_REQUEST,
        "illegal_argument_exception",
        reason,
    )
}

/// Is this node enforcing authentication at all? Mirrors the open-mode test in
/// [`crate::auth::authenticate`] exactly: those are the conditions under which
/// every request is the superuser.
fn auth_is_enforced(state: &AppState) -> bool {
    let cfg = &state.config.auth;
    cfg.enabled && !cfg.admin_api_key.is_empty()
}

/// The `409` an open node gives instead of a share. See the module docs.
fn open_node_refusal() -> Response {
    es_error(
        StatusCode::CONFLICT,
        "illegal_state_exception",
        "this node runs with authentication off (--insecure, or auth.enabled = false), so \
         every request is already the superuser and a read-only guest key would restrict \
         nothing. Restart the node with authentication on (the default) to share an index"
            .into(),
    )
}

/// Nothing under `/_share` may be stored by a browser, a proxy or a CDN: the
/// create response carries the link and passcode, the claim response carries
/// a live credential, and an error body says whether a share exists.
/// `no-store` for HTTP/1.1 caches, `Pragma` for the HTTP/1.0 ones still found
/// in front of things.
pub(crate) fn no_store(mut resp: Response) -> Response {
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
    resp
}

/// Does the node believe forwarding headers from a proxy on this machine —
/// i.e. is loopback a declared `server.trusted_proxies` entry? Reported in the
/// create response so `xerj share --tunnel` can tell the owner whose address
/// the audit log will record.
fn loopback_is_trusted_proxy(state: &AppState) -> bool {
    xerj_common::net::TrustedProxies::parse(&state.config.server.trusted_proxies)
        .map(|t| t.contains(&IpAddr::from([127, 0, 0, 1])))
        .unwrap_or(false)
}

/// The management routes are superuser-only. The admin key is what
/// `xerj share` reads from `<data-dir>/admin.key`; a scoped key must not be
/// able to widen its own reach by minting a share, and an unscoped key must
/// not be able to hand the reserved namespace to a guest it cannot reach
/// itself.
fn require_superuser(principal: &Principal) -> Result<(), Response> {
    if principal.is_superuser() {
        Ok(())
    } else {
        Err(crate::authz::forbidden(
            principal,
            "_share",
            Privilege::SecurityAdmin,
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Claim source address
// ─────────────────────────────────────────────────────────────────────────────

/// The address a claim came from, for the rate limiter and the audit line.
///
/// The TCP peer, from `ConnectInfo` — the caller cannot forge it. Forwarding
/// headers are believed only when the peer is one of the operator's declared
/// `server.trusted_proxies`, walking `X-Forwarded-For` right-to-left past
/// other declared proxies (the same reading as the console's `client_ip`,
/// #76 S5-4). With no `ConnectInfo` at all — a router driven by `oneshot` in
/// a test — every caller shares one `unknown` bucket, which throttles rather
/// than exempts.
pub struct ClaimSource(pub String);

#[axum::async_trait]
impl FromRequestParts<AppState> for ClaimSource {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| xerj_common::net::canonical_ip(c.0.ip()));
        let Some(peer) = peer else {
            return Ok(ClaimSource("unknown".to_string()));
        };
        let trusted = xerj_common::net::TrustedProxies::parse(&state.config.server.trusted_proxies)
            .unwrap_or_else(|_| xerj_common::net::TrustedProxies::none());
        if !trusted.contains(&peer) {
            return Ok(ClaimSource(peer.to_string()));
        }
        let elements: Vec<&str> = parts
            .headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .collect();
        for element in elements.iter().rev() {
            match xerj_common::net::parse_forwarded_element(element) {
                Some(ip) => {
                    let ip: IpAddr = xerj_common::net::canonical_ip(ip);
                    if !trusted.contains(&ip) {
                        return Ok(ClaimSource(ip.to_string()));
                    }
                }
                None => break,
            }
        }
        Ok(ClaimSource(peer.to_string()))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// `POST /_share` — create a share. Superuser only.
///
/// Body: `{index: "name" | ["a","b"], brain?, expires_in?: "24h", max_claims?: 1,
/// passcode?, label?}`. Returns the share id, the console path to send, and
/// the passcode — the only time either plaintext is ever shown.
pub async fn create_share(
    State(state): State<AppState>,
    principal: Principal,
    body: Option<Json<Value>>,
) -> Response {
    no_store(create_share_inner(state, principal, body).await)
}

async fn create_share_inner(
    state: AppState,
    principal: Principal,
    body: Option<Json<Value>>,
) -> Response {
    if !auth_is_enforced(&state) {
        state.engine.audit.append(
            "share.create",
            principal.label(),
            "_share",
            "denied",
            "node has authentication off",
        );
        return open_node_refusal();
    }
    if let Err(resp) = require_superuser(&principal) {
        state
            .engine
            .audit
            .append("share.create", principal.label(), "_share", "denied", "");
        return resp;
    }
    let Some(Json(body)) = body else {
        return bad_request("a JSON body naming `index` is required".into());
    };

    // `index`: a string, a comma list, or an array. Deduplicated, order kept.
    let mut indices: Vec<String> = Vec::new();
    let raw = body.get("index").or_else(|| body.get("indices"));
    match raw {
        Some(Value::String(s)) => indices.extend(
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        ),
        Some(Value::Array(a)) => indices.extend(
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        ),
        Some(_) => return bad_request("`index` must be a string or an array of strings".into()),
        None => {}
    }
    let mut seen = HashSet::new();
    indices.retain(|i| seen.insert(i.clone()));
    let brain = match body.get("brain") {
        None | Some(Value::Null) => None,
        Some(Value::String(b)) if !b.trim().is_empty() => {
            let b = b.trim().to_string();
            if let Err(why) = crate::memory_api::validate_namespace(&b) {
                return bad_request(format!("`brain`: {why}"));
            }
            Some(b)
        }
        Some(_) => return bad_request("`brain` must be a string".into()),
    };
    if indices.is_empty() && brain.is_none() {
        return bad_request(
            "`index` (or `brain`) is required — a share must name what it grants".into(),
        );
    }
    if indices.len() > MAX_INDICES {
        return bad_request(format!("a share may name at most {MAX_INDICES} indices"));
    }
    // An alias is resolved NOW, to the indices it points at as the owner runs
    // the command, and the share records those. Two reasons. Authorization
    // resolves an alias to its backing indices and checks each of *those*
    // against the key's grants (`authz::authorize_expression`), so a grant on
    // the alias name alone mints a key that can read nothing — the first cut
    // accepted an alias here and produced exactly that. And a frozen list means
    // re-pointing the alias later cannot move, or widen, what a guest already
    // holds: the owner shared these indices, not whatever the name means next
    // week. The backing names go through the same validation, so an alias is
    // not a way to share a system index.
    let mut concrete: Vec<String> = Vec::with_capacity(indices.len());
    for name in &indices {
        if let Err(why) = validate_share_index(name) {
            return bad_request(why);
        }
        let backing = state
            .engine
            .aliases
            .get(name)
            .map(|e| e.value().clone())
            .filter(|b| !b.is_empty());
        let resolved = backing.unwrap_or_else(|| vec![name.clone()]);
        for index in resolved {
            if let Err(why) = validate_share_index(&index) {
                return bad_request(if index == *name {
                    why
                } else {
                    format!("alias `{name}` resolves to `{index}`: {why}")
                });
            }
            if state.engine.get_index(&index).is_err() {
                return es_error(
                    StatusCode::NOT_FOUND,
                    "index_not_found_exception",
                    format!("no such index [{index}]"),
                );
            }
            concrete.push(index);
        }
    }
    let mut seen = HashSet::new();
    concrete.retain(|i| seen.insert(i.clone()));
    if concrete.len() > MAX_INDICES {
        return bad_request(format!("a share may name at most {MAX_INDICES} indices"));
    }
    let indices = concrete;
    if let Some(b) = &brain {
        let edges = crate::authz::brain_edges_index(b);
        if state.engine.get_index(&edges).is_err() {
            return es_error(
                StatusCode::NOT_FOUND,
                "resource_not_found_exception",
                format!(
                    "no brain named [{b}] on this node (its edges index [{edges}] does not exist)"
                ),
            );
        }
    }

    let expires_spec = match body.get("expires_in") {
        None | Some(Value::Null) => DEFAULT_EXPIRES_IN.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            return bad_request("`expires_in` must be a duration string like \"24h\"".into())
        }
    };
    let Some(expires_in_ms) = parse_expires_in(&expires_spec) else {
        return bad_request(format!(
            "`expires_in`: cannot parse {expires_spec:?} — use e.g. \"30m\", \"24h\", \"7d\""
        ));
    };
    if !(MIN_EXPIRES_MS..=MAX_EXPIRES_MS).contains(&expires_in_ms) {
        return bad_request(format!(
            "`expires_in` must be between 1s and 30d (got {expires_spec})"
        ));
    }
    let max_claims = match body.get("max_claims") {
        None | Some(Value::Null) => DEFAULT_MAX_CLAIMS,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_MAX_CLAIMS as u64).contains(&n) => n as u32,
            _ => {
                return bad_request(format!(
                    "`max_claims` must be an integer from 1 to {MAX_MAX_CLAIMS}"
                ))
            }
        },
    };
    let (passcode, passcode_supplied) = match body.get("passcode") {
        None | Some(Value::Null) => (generate_passcode(), false),
        Some(Value::String(p)) => {
            let p = p.trim().to_string();
            let n = p.chars().count();
            if !(MIN_PASSCODE_CHARS..=MAX_PASSCODE_CHARS).contains(&n) {
                return bad_request(format!(
                    "`passcode` must be {MIN_PASSCODE_CHARS}–{MAX_PASSCODE_CHARS} characters"
                ));
            }
            (p, true)
        }
        Some(_) => return bad_request("`passcode` must be a string".into()),
    };
    let label = match body.get("label") {
        None | Some(Value::Null) => None,
        Some(Value::String(l)) => {
            let l = l.trim().to_string();
            if l.chars().count() > MAX_LABEL_CHARS {
                return bad_request(format!(
                    "`label` must be at most {MAX_LABEL_CHARS} characters"
                ));
            }
            if l.is_empty() {
                None
            } else {
                Some(l)
            }
        }
        Some(_) => return bad_request("`label` must be a string".into()),
    };

    // Argon2 is deliberately slow; keep it off the async worker.
    let pc = passcode.clone();
    let passcode_hash = match tokio::task::spawn_blocking(move || hash_passcode(&pc)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "argon2 could not hash the share passcode");
            return es_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "share_exception",
                "could not hash the passcode".into(),
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "passcode hashing task panicked");
            return es_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "share_exception",
                "could not hash the passcode".into(),
            );
        }
    };

    let share_id = mint_share_id();
    let (id_hash, handle) = hash_share_id(&share_id);
    let now = now_ms();
    let record = ShareRecord {
        handle: handle.clone(),
        id_hash,
        passcode_hash,
        label: label.clone(),
        indices: indices.clone(),
        brain: brain.clone(),
        created_ms: now,
        expires_ms: now + expires_in_ms,
        max_claims,
        claims: 0,
        revoked: false,
        revoked_ms: None,
        key_ids: Vec::new(),
    };
    state.shares.records.insert(handle.clone(), record.clone());
    if let Err(e) = state.shares.persist() {
        state.shares.records.remove(&handle);
        tracing::error!(error = %e, "could not persist shares.json");
        return es_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "share_exception",
            format!("could not persist the share: {e}"),
        );
    }
    state.engine.audit.append(
        "share.create",
        principal.label(),
        &handle,
        "ok",
        &format!(
            "indices={} brain={} max_claims={max_claims} expires_at={} passcode={}",
            indices.join(","),
            brain.as_deref().unwrap_or("-"),
            rfc3339(record.expires_ms),
            if passcode_supplied {
                "supplied"
            } else {
                "generated"
            }
        ),
    );
    state.engine.audit.sync_to_disk();

    let mut out = record.public_json(now);
    out["share_id"] = json!(share_id);
    out["url_path"] = json!(format!("/_xerj-console/share#{share_id}"));
    out["passcode"] = json!(passcode);
    out["claim_limiter"] = json!({
        "per_share_per_minute": SHARE_PER_MINUTE,
        "per_share_per_hour": SHARE_PER_HOUR,
        // Whose address a claim is attributed to: the TCP peer, or — when
        // loopback is a declared trusted proxy — the address a local reverse
        // proxy or tunnel forwarded.
        "source_address": if loopback_is_trusted_proxy(&state) { "forwarded" } else { "peer" },
    });
    Json(out).into_response()
}

/// `GET /_share` — list every share. Superuser only. No hashes, no ids.
pub async fn list_shares(State(state): State<AppState>, principal: Principal) -> Response {
    no_store(list_shares_inner(state, principal).await)
}

async fn list_shares_inner(state: AppState, principal: Principal) -> Response {
    if let Err(resp) = require_superuser(&principal) {
        state
            .engine
            .audit
            .append("share.list", principal.label(), "_share", "denied", "");
        return resp;
    }
    let now = now_ms();
    let shares: Vec<Value> = state
        .shares
        .list()
        .iter()
        .map(|r| r.public_json(now))
        .collect();
    state.engine.audit.append(
        "share.list",
        principal.label(),
        "_share",
        "ok",
        &format!("count={}", shares.len()),
    );
    Json(json!({ "shares": shares })).into_response()
}

/// `DELETE /_share/{id}` — revoke. Accepts the share id or its handle.
/// Invalidates every key a claim of this share minted, so a guest who has
/// already claimed loses access on their next request.
pub async fn revoke_share(
    State(state): State<AppState>,
    principal: Principal,
    AxumPath(id): AxumPath<String>,
) -> Response {
    no_store(revoke_share_inner(state, principal, id).await)
}

async fn revoke_share_inner(state: AppState, principal: Principal, id: String) -> Response {
    if let Err(resp) = require_superuser(&principal) {
        state
            .engine
            .audit
            .append("share.revoke", principal.label(), "_share", "denied", "");
        return resp;
    }
    let Some(handle) = state.shares.find_for_revoke(id.trim()) else {
        state.engine.audit.append(
            "share.revoke",
            principal.label(),
            "_share",
            "error",
            "unknown share",
        );
        return es_error(
            StatusCode::NOT_FOUND,
            "resource_not_found_exception",
            "unknown share".into(),
        );
    };
    let now = now_ms();
    let key_ids = {
        let Some(mut rec) = state.shares.records.get_mut(&handle) else {
            return es_error(
                StatusCode::NOT_FOUND,
                "resource_not_found_exception",
                "unknown share".into(),
            );
        };
        let already = rec.revoked;
        rec.revoked = true;
        if !already {
            rec.revoked_ms = Some(now);
        }
        rec.key_ids.clone()
    };
    let invalidated = match state.engine.invalidate_api_keys(&key_ids, now) {
        Ok((fresh, _previously)) => fresh.len(),
        Err(e) => {
            return crate::error::ApiError::new(xerj_common::XerjError::from(e)).into_response();
        }
    };
    if let Err(e) = state.shares.persist() {
        tracing::error!(error = %e, "could not persist shares.json after revoke");
    }
    state.engine.audit.append(
        "share.revoke",
        principal.label(),
        &handle,
        "ok",
        &format!("keys_invalidated={invalidated}"),
    );
    state.engine.audit.sync_to_disk();
    Json(json!({
        "handle": handle,
        "revoked": true,
        "keys_invalidated": invalidated,
    }))
    .into_response()
}

/// `POST /_share/claim` — the guest's one call. **Unauthenticated** (see the
/// module docs for why that is safe), rate-limited, audited.
///
/// Body: `{id, passcode}` — the share id travels in the body, never in the
/// path (module docs, "The share id is never part of a request path"). On
/// success: `{api_key, index, indices, brain, expires_at, label, claims_left}`
/// — `api_key` is ready for `Authorization: ApiKey <api_key>`. `index` is the
/// comma-joined list, which is a valid ES multi-index expression for
/// `/{index}/_search`.
pub async fn claim_share(
    State(state): State<AppState>,
    ClaimSource(source): ClaimSource,
    body: Option<Json<Value>>,
) -> Response {
    no_store(claim_share_inner(state, source, body).await)
}

/// The share id a claim body names. Anything that is not a string is the
/// empty id, which no share has: a body without one is answered like any other
/// unknown link — charged to the source window, audited, `404`.
fn claim_id(body: Option<&Value>) -> String {
    body.and_then(|b| b.get("id"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

async fn claim_share_inner(state: AppState, source: String, body: Option<Json<Value>>) -> Response {
    let subject = format!("guest@{source}");
    let now = now_ms();
    let id = claim_id(body.as_ref().map(|Json(b)| b));
    if !auth_is_enforced(&state) {
        return open_node_refusal();
    }
    // The lookup comes FIRST, and which window is charged depends on its
    // answer — see "The two throttles" in the module docs. It is a SHA-256 and
    // a map probe, so an unauthenticated caller buys nothing by forcing it.
    let Some(record) = state.shares.find_by_id(&id) else {
        // Junk: the source window is the only throttle there is. Behind a
        // tunnel every client shares the loopback bucket, which makes this
        // bound tighter, never looser — and it cannot lock a real guest out,
        // because a real share id never reaches this branch.
        let bucket = format!("ip:{}", source_bucket(&source));
        if let Err(t) = state
            .shares
            .charge(&bucket, IP_PER_MINUTE, IP_PER_HOUR, now)
        {
            if t.first_in_window {
                state.engine.audit.append(
                    "share.claim",
                    &subject,
                    "_share",
                    "denied",
                    "rate-limited (source); further refusals in this window are not logged",
                );
            }
            return too_many(t.retry_after_secs);
        }
        state
            .engine
            .audit
            .append("share.claim", &subject, "_share", "error", "unknown share");
        return es_error(
            StatusCode::NOT_FOUND,
            "resource_not_found_exception",
            "unknown share link".into(),
        );
    };
    let handle = record.handle.clone();
    // A real share: its own window, from anywhere. This is the passcode
    // lockout and it holds whoever is asking and however they got here.
    if let Err(t) = state.shares.charge(
        &format!("share:{handle}"),
        SHARE_PER_MINUTE,
        SHARE_PER_HOUR,
        now,
    ) {
        if t.first_in_window {
            state.engine.audit.append(
                "share.claim",
                &subject,
                &handle,
                "denied",
                "rate-limited (share); further refusals in this window are not logged",
            );
        }
        return too_many(t.retry_after_secs);
    }
    if let Some(why) = record.unavailable(now) {
        state
            .engine
            .audit
            .append("share.claim", &subject, &handle, "denied", why);
        return es_error(StatusCode::GONE, "share_unavailable", why.into());
    }
    let presented = body
        .as_ref()
        .and_then(|Json(b)| b.get("passcode"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let stored = record.passcode_hash.clone();
    let ok = if presented.is_empty() {
        false
    } else {
        tokio::task::spawn_blocking(move || verify_passcode(&presented, &stored))
            .await
            .unwrap_or(false)
    };
    if !ok {
        state
            .engine
            .audit
            .append("share.claim", &subject, &handle, "denied", "wrong passcode");
        state.engine.audit.sync_to_disk();
        return es_error(
            StatusCode::UNAUTHORIZED,
            "security_exception",
            "wrong passcode".into(),
        );
    }

    // Mint the guest's key: read-only over exactly the granted indices,
    // expiring with the share. Same key material shape as
    // `POST /_security/api_key` so the same auth path re-authenticates it.
    let granted = record.granted_indices();
    let roles = vec![Role::new(
        format!("{GUEST_ROLE_PREFIX}{handle}"),
        HashSet::from([Privilege::ReadIndex]),
        granted,
    )];
    let key_id = uuid::Uuid::new_v4().to_string();
    let raw_secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let api_key = crate::es_compat::base64_encode(&raw_secret);
    let encoded = crate::es_compat::base64_encode(&format!("{key_id}:{api_key}"));
    let key_record = xerj_engine::engine::ApiKeyRecord::new(
        format!("share:{handle}"),
        &api_key,
        now,
        Some(record.expires_ms),
        roles,
    );

    // Consume the claim first, under the record lock, so two racing claims of
    // a one-claim share cannot both succeed.
    let claims_left = {
        let Some(mut rec) = state.shares.records.get_mut(&handle) else {
            return es_error(
                StatusCode::NOT_FOUND,
                "resource_not_found_exception",
                "unknown share link".into(),
            );
        };
        if let Some(why) = rec.unavailable(now) {
            state
                .engine
                .audit
                .append("share.claim", &subject, &handle, "denied", why);
            return es_error(StatusCode::GONE, "share_unavailable", why.into());
        }
        rec.claims += 1;
        rec.key_ids.push(key_id.clone());
        rec.max_claims.saturating_sub(rec.claims)
    };
    if let Err(e) = state.engine.persist_api_key(key_id.clone(), key_record) {
        if let Some(mut rec) = state.shares.records.get_mut(&handle) {
            rec.claims = rec.claims.saturating_sub(1);
            rec.key_ids.retain(|k| k != &key_id);
        }
        return crate::error::ApiError::new(xerj_common::XerjError::from(e)).into_response();
    }
    if let Err(e) = state.shares.persist() {
        tracing::error!(error = %e, "could not persist shares.json after claim");
    }
    state.engine.audit.append(
        "share.claim",
        &subject,
        &handle,
        "ok",
        &format!("key_id={key_id} claims_left={claims_left}"),
    );
    state.engine.audit.sync_to_disk();

    Json(json!({
        "api_key": encoded,
        "index": record.indices.join(","),
        "indices": record.indices,
        "brain": record.brain,
        "expires_at": rfc3339(record.expires_ms),
        "label": record.label,
        "claims_left": claims_left,
    }))
    .into_response()
}

fn too_many(retry_after_secs: u64) -> Response {
    let mut resp = es_error(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_requests",
        format!("too many claim attempts; retry in {retry_after_secs}s"),
    );
    if let Ok(v) = header::HeaderValue::from_str(&retry_after_secs.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_in_parses_units_and_rejects_bare_numbers() {
        assert_eq!(parse_expires_in("24h"), Some(24 * 3_600_000));
        assert_eq!(parse_expires_in("30m"), Some(30 * 60_000));
        assert_eq!(parse_expires_in("7d"), Some(7 * 86_400_000));
        assert_eq!(parse_expires_in("90s"), Some(90_000));
        assert_eq!(parse_expires_in(" 2 d "), Some(2 * 86_400_000));
        assert_eq!(parse_expires_in("24"), None);
        assert_eq!(parse_expires_in("h"), None);
        assert_eq!(parse_expires_in("1w"), None);
        assert_eq!(parse_expires_in(""), None);
    }

    #[test]
    fn generated_passcodes_are_typeable_and_distinct() {
        let a = generate_passcode();
        let b = generate_passcode();
        assert_eq!(a.len(), 9);
        assert_eq!(&a[4..5], "-");
        for c in a.chars().filter(|c| *c != '-') {
            assert!(PASSCODE_ALPHABET.contains(&(c as u8)), "{a}");
        }
        assert_ne!(a, b);
    }

    #[test]
    fn passcode_hash_verifies_only_the_right_code_and_never_stores_plaintext() {
        let h = hash_passcode("k7mq-2xhd").unwrap();
        assert!(h.starts_with("$argon2id$"), "{h}");
        assert!(!h.contains("k7mq-2xhd"));
        assert!(verify_passcode("k7mq-2xhd", &h));
        assert!(!verify_passcode("k7mq-2xhe", &h));
        assert!(!verify_passcode("", &h));
        assert!(!verify_passcode("k7mq-2xhd", "not-a-phc-string"));
    }

    #[test]
    fn share_id_hashes_to_a_handle_that_cannot_reopen_the_share() {
        let id = mint_share_id();
        assert_eq!(id.len(), 32);
        let (full, handle) = hash_share_id(&id);
        assert_eq!(full.len(), 64);
        assert_eq!(handle.len(), 12);
        assert!(full.starts_with(&handle));
        assert!(!full.contains(&id));
    }

    #[test]
    fn index_validation_refuses_patterns_lists_and_the_reserved_namespace() {
        assert!(validate_share_index("ax-notes").is_ok());
        assert!(validate_share_index("logs-*").is_err());
        assert!(validate_share_index("a,b").is_err());
        assert!(validate_share_index(".xerj-memory-kb-edges").is_err());
        // The node's own indices, credential stores first. Each of these was
        // shareable before the dot-prefix rule.
        for system in [
            ".xerj_sessions",
            ".xerj_api_tokens",
            ".xerj_passkeys",
            ".xerj_magic_links",
            ".xerj_users",
            ".xerj_audit",
            ".anything-else",
        ] {
            assert!(
                validate_share_index(system).is_err(),
                "{system} must not be shareable"
            );
        }
        assert!(validate_share_index("_all").is_err());
        assert!(validate_share_index("Upper").is_err());
        assert!(validate_share_index("").is_err());
    }

    /// Review of PR #947: three public docs said the catalog is "never
    /// granted" while `validate_share_index` accepted it and a guest read
    /// every corpus's row.
    #[test]
    fn the_autoindex_catalog_cannot_be_shared() {
        let why = validate_share_index(CATALOG_INDEX).unwrap_err();
        assert!(why.contains("every corpus"), "{why}");
        assert!(why.contains("autoindex map"), "{why}");
        // A user's own index that merely resembles it is theirs to share.
        assert!(validate_share_index("autoindex-catalog-2").is_ok());
        assert!(validate_share_index("my-autoindex-catalog").is_ok());
    }

    /// The id comes out of the claim BODY. A body without a usable one is the
    /// empty id — never a panic, and never a record.
    #[test]
    fn the_claim_id_is_read_from_the_body() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(claim_id(Some(&json!({"id": id, "passcode": "x"}))), id);
        assert_eq!(claim_id(Some(&json!({"id": format!("  {id}\n")}))), id);
        for junk in [
            json!({}),
            json!({"passcode": "x"}),
            json!({"id": 7}),
            json!({"id": [id]}),
            json!({"id": {"$ne": ""}}),
            json!([id]),
            json!(id),
        ] {
            assert_eq!(claim_id(Some(&junk)), "", "{junk}");
        }
        assert_eq!(claim_id(None), "");
        // And the empty id is nobody's share.
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore::open(dir.path().to_str().unwrap());
        assert!(store.find_by_id("").is_none());
        const { assert!((MAX_PASSCODE_CHARS * 4 + 32 + 64) < MAX_CLAIM_BODY_BYTES) };
    }

    #[test]
    fn rate_windows_lock_out_after_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore::open(dir.path().to_str().unwrap());
        let now = 1_000_000;
        for _ in 0..SHARE_PER_MINUTE {
            store
                .charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
                .unwrap();
        }
        let refused = store
            .charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
            .unwrap_err();
        let retry = refused.retry_after_secs;
        assert!((1..=61).contains(&retry), "{retry}");
        // The first refusal in a window is the one that gets audited; the
        // flood behind it is not, or the audit log becomes the attack.
        assert!(refused.first_in_window);
        let again = store
            .charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
            .unwrap_err();
        assert!(!again.first_in_window);
        // A different share is untouched.
        store
            .charge("share:y", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
            .unwrap();
        // The minute window rolls over; the hour window still counts.
        //
        // A minute-throttled attempt returns before the hour window is
        // charged — it never reaches passcode verification, so it is not a
        // guess and does not spend the hourly guess budget. Filling the hour
        // therefore takes *accepted* charges, spread over fresh minute windows.
        let mut t = now;
        let mut accepted = SHARE_PER_MINUTE;
        while accepted < SHARE_PER_HOUR {
            t += WINDOW_MIN_MS + 1;
            for _ in 0..SHARE_PER_MINUTE {
                if accepted == SHARE_PER_HOUR {
                    break;
                }
                store
                    .charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, t)
                    .unwrap();
                accepted += 1;
            }
        }
        // The hourly budget is spent. A brand-new minute window does not help.
        t += WINDOW_MIN_MS + 1;
        assert!(t - now < WINDOW_HOUR_MS, "still inside the same hour");
        let retry = store
            .charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, t)
            .unwrap_err()
            .retry_after_secs;
        assert!(
            retry > 60,
            "locked out by the hour window, not the minute one: {retry}s"
        );
        // And the throttled attempts above never counted as guesses.
        assert_eq!(accepted, SHARE_PER_HOUR);
    }

    #[test]
    fn ipv6_sources_are_bucketed_by_their_64() {
        // One host, two addresses out of the /64 it was delegated: one bucket.
        assert_eq!(
            source_bucket("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
            source_bucket("2001:db8:1:2::1")
        );
        assert_eq!(source_bucket("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        // A different /64 is a different bucket.
        assert_ne!(
            source_bucket("2001:db8:1:3::1"),
            source_bucket("2001:db8:1:2::1")
        );
        // IPv4 and the no-transport bucket are left alone.
        assert_eq!(source_bucket("203.0.113.9"), "203.0.113.9");
        assert_eq!(source_bucket("unknown"), "unknown");
    }

    #[test]
    fn junk_traffic_cannot_spend_a_real_shares_budget() {
        // The tunnel case: every client arrives as 127.0.0.1. Exhaust that
        // source bucket the way a flood of made-up ids would…
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore::open(dir.path().to_str().unwrap());
        let now = 5_000_000;
        let junk = format!("ip:{}", source_bucket("127.0.0.1"));
        for _ in 0..IP_PER_MINUTE {
            store
                .charge(&junk, IP_PER_MINUTE, IP_PER_HOUR, now)
                .unwrap();
        }
        assert!(store
            .charge(&junk, IP_PER_MINUTE, IP_PER_HOUR, now)
            .is_err());
        // …and a real share's window is untouched: `claim_share` charges a
        // known id to `share:<handle>` only, never to the source bucket.
        for _ in 0..SHARE_PER_MINUTE {
            store
                .charge("share:abc123", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
                .unwrap();
        }
    }

    #[test]
    fn the_window_table_is_bounded_and_overflow_throttles_rather_than_exempts() {
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore::open(dir.path().to_str().unwrap());
        let now = 9_000_000;
        for i in 0..MAX_WINDOWS {
            store.windows.insert(
                format!("ip:filler-{i}:m"),
                RateWindow {
                    count: 1,
                    start_ms: now,
                },
            );
        }
        // New sources now share one bucket: ten of them, from ten "addresses",
        // and the eleventh is refused.
        for i in 0..IP_PER_MINUTE {
            store
                .charge(&format!("ip:new-{i}"), IP_PER_MINUTE, IP_PER_HOUR, now)
                .unwrap();
        }
        assert!(store
            .charge("ip:new-last", IP_PER_MINUTE, IP_PER_HOUR, now)
            .is_err());
        assert!(!store.windows.contains_key("ip:new-0:m"));
        // A share's own window is never collapsed into the overflow bucket.
        store
            .charge("share:real", SHARE_PER_MINUTE, SHARE_PER_HOUR, now)
            .unwrap();
        assert!(store.windows.contains_key("share:real:m"));
    }

    #[test]
    fn nothing_under_share_is_cacheable() {
        let resp = no_store(Json(json!({"api_key": "x"})).into_response());
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store, max-age=0"
        );
        assert_eq!(resp.headers().get(header::PRAGMA).unwrap(), "no-cache");
        // Errors too: a cached 404/410 says whether a share exists.
        let resp = no_store(too_many(5));
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store, max-age=0"
        );
        assert_eq!(resp.headers().get(header::RETRY_AFTER).unwrap(), "5");
    }

    #[test]
    fn store_round_trips_through_disk_without_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_str().unwrap();
        let store = ShareStore::open(data_dir);
        let share_id = mint_share_id();
        let (id_hash, handle) = hash_share_id(&share_id);
        store.records.insert(
            handle.clone(),
            ShareRecord {
                handle: handle.clone(),
                id_hash,
                passcode_hash: hash_passcode("secret-code").unwrap(),
                label: Some("friend".into()),
                indices: vec!["ax-notes".into()],
                brain: Some("notes".into()),
                created_ms: 1,
                expires_ms: 2,
                max_claims: 1,
                claims: 0,
                revoked: false,
                revoked_ms: None,
                key_ids: vec![],
            },
        );
        store.persist().unwrap();
        let raw = std::fs::read_to_string(dir.path().join(SHARES_FILE)).unwrap();
        assert!(!raw.contains(&share_id), "share id must not be on disk");
        assert!(!raw.contains("secret-code"), "passcode must not be on disk");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(SHARES_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let reloaded = ShareStore::open(data_dir);
        let rec = reloaded
            .find_by_id(&share_id)
            .expect("id resolves after reload");
        assert_eq!(rec.handle, handle);
        assert_eq!(
            rec.granted_indices(),
            vec![
                "ax-notes".to_string(),
                ".xerj-memory-notes-edges".to_string()
            ]
        );
        assert!(reloaded.find_by_id("0000").is_none());
        // The handle alone resolves for revoke, but never as a claim id.
        assert_eq!(
            reloaded.find_for_revoke(&handle).as_deref(),
            Some(handle.as_str())
        );
        assert!(reloaded.find_by_id(&handle).is_none());
    }
}
