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
//! | `POST /_share`, `GET /_share`, `DELETE /_share/{id}` | superuser only (the admin key) — a scoped or unscoped key gets 403 |
//! | `POST /_share/{id}/claim` | **nobody in particular** — it is exempt from authentication (`auth::is_share_claim_path`) the way `/v1/metrics` is for the scrape token, and rate-limited per source address and per share instead |
//!
//! The claim route is the only unauthenticated door that hands out a
//! credential, so it is narrow on purpose: one exact path shape, `POST` only,
//! a fixed-size body, a per-IP *and* per-share sliding window, and every
//! outcome — success, wrong passcode, exhausted, expired, revoked, unknown,
//! throttled — lands in the audit log with the source address.
//!
//! ## What a guest can reach
//!
//! The minted key carries one role: `read` on the share's `indices` plus, when
//! a brain is named, `read` on that brain's edges index
//! (`authz::brain_edges_index`). That is what unlocks `/{index}/_search`,
//! `_mget`, `_count`, and `/_graph/{brain}/ego|overview` for the guest, and
//! what `authz` denies everywhere else. The `autoindex-catalog` index is **not**
//! granted: it lists every corpus on the node, the engine has no document-level
//! security to filter it with, and the guest page already receives the index
//! name directly.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
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

/// Claim attempts per source address per minute / per hour.
pub const IP_PER_MINUTE: u32 = 10;
pub const IP_PER_HOUR: u32 = 100;
/// Claim attempts against one share per minute / per hour, from anywhere.
/// This is the lockout: a passcode has ~40 bits, and thirty guesses an hour
/// against it is a search that does not finish.
pub const SHARE_PER_MINUTE: u32 = 10;
pub const SHARE_PER_HOUR: u32 = 30;

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
                Err(e) => tracing::error!(path = %path.display(), error = %e, "shares.json does not parse; starting with no shares"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::error!(path = %path.display(), error = %e, "shares.json unreadable; starting with no shares"),
        }
        Arc::new(Self {
            path,
            records,
            windows: DashMap::new(),
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
        self.records
            .get(id_or_handle)
            .map(|r| r.handle.clone())
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

    /// Charge one claim attempt against `key`. `Err(retry_after_secs)` when
    /// over either window. Same sliding-window shape as the console's auth
    /// limiter (`xerj-console-api::auth::rate_limit`), re-implemented here
    /// because that one is bound to `ConsoleState` and this crate must not
    /// depend on the console.
    fn charge(&self, key: &str, per_minute: u32, per_hour: u32, now_ms: u64) -> Result<(), u64> {
        for (suffix, window_ms, limit) in [("m", WINDOW_MIN_MS, per_minute), ("h", WINDOW_HOUR_MS, per_hour)] {
            let k = format!("{key}:{suffix}");
            let mut w = self.windows.entry(k).or_insert(RateWindow { count: 0, start_ms: now_ms });
            if now_ms.saturating_sub(w.start_ms) > window_ms {
                *w = RateWindow { count: 0, start_ms: now_ms };
            }
            w.count += 1;
            if w.count > limit {
                let retry = (w.start_ms + window_ms).saturating_sub(now_ms) / 1000 + 1;
                return Err(retry);
            }
        }
        if self.windows.len() > 10_000 {
            self.windows.retain(|_, w| now_ms.saturating_sub(w.start_ms) < WINDOW_HOUR_MS);
        }
        Ok(())
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
    if name.starts_with('_') {
        return Err(format!("index `{name}` is not an index name"));
    }
    for c in name.chars() {
        if c.is_whitespace() || matches!(c, '*' | ',' | '/' | '\\' | '?' | '"' | '<' | '>' | '|' | '#') {
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
    es_error(StatusCode::BAD_REQUEST, "illegal_argument_exception", reason)
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

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
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
    if let Err(resp) = require_superuser(&principal) {
        state.engine.audit.append("share.create", principal.label(), "_share", "denied", "");
        return resp;
    }
    let Some(Json(body)) = body else {
        return bad_request("a JSON body naming `index` is required".into());
    };

    // `index`: a string, a comma list, or an array. Deduplicated, order kept.
    let mut indices: Vec<String> = Vec::new();
    let raw = body.get("index").or_else(|| body.get("indices"));
    match raw {
        Some(Value::String(s)) => indices.extend(s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from)),
        Some(Value::Array(a)) => indices.extend(a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)),
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
        return bad_request("`index` (or `brain`) is required — a share must name what it grants".into());
    }
    if indices.len() > MAX_INDICES {
        return bad_request(format!("a share may name at most {MAX_INDICES} indices"));
    }
    for name in &indices {
        if let Err(why) = validate_share_index(name) {
            return bad_request(why);
        }
        let exists = state.engine.get_index(name).is_ok() || state.engine.aliases.contains_key(name);
        if !exists {
            return es_error(
                StatusCode::NOT_FOUND,
                "index_not_found_exception",
                format!("no such index [{name}]"),
            );
        }
    }
    if let Some(b) = &brain {
        let edges = crate::authz::brain_edges_index(b);
        if state.engine.get_index(&edges).is_err() {
            return es_error(
                StatusCode::NOT_FOUND,
                "resource_not_found_exception",
                format!("no brain named [{b}] on this node (its edges index [{edges}] does not exist)"),
            );
        }
    }

    let expires_spec = match body.get("expires_in") {
        None | Some(Value::Null) => DEFAULT_EXPIRES_IN.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return bad_request("`expires_in` must be a duration string like \"24h\"".into()),
    };
    let Some(expires_in_ms) = parse_expires_in(&expires_spec) else {
        return bad_request(format!("`expires_in`: cannot parse {expires_spec:?} — use e.g. \"30m\", \"24h\", \"7d\""));
    };
    if !(MIN_EXPIRES_MS..=MAX_EXPIRES_MS).contains(&expires_in_ms) {
        return bad_request(format!("`expires_in` must be between 1s and 30d (got {expires_spec})"));
    }
    let max_claims = match body.get("max_claims") {
        None | Some(Value::Null) => DEFAULT_MAX_CLAIMS,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_MAX_CLAIMS as u64).contains(&n) => n as u32,
            _ => return bad_request(format!("`max_claims` must be an integer from 1 to {MAX_MAX_CLAIMS}")),
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
                return bad_request(format!("`label` must be at most {MAX_LABEL_CHARS} characters"));
            }
            if l.is_empty() { None } else { Some(l) }
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
            if passcode_supplied { "supplied" } else { "generated" }
        ),
    );
    state.engine.audit.sync_to_disk();

    let mut out = record.public_json(now);
    out["share_id"] = json!(share_id);
    out["url_path"] = json!(format!("/_xerj-console/share#{share_id}"));
    out["passcode"] = json!(passcode);
    Json(out).into_response()
}

/// `GET /_share` — list every share. Superuser only. No hashes, no ids.
pub async fn list_shares(State(state): State<AppState>, principal: Principal) -> Response {
    if let Err(resp) = require_superuser(&principal) {
        state.engine.audit.append("share.list", principal.label(), "_share", "denied", "");
        return resp;
    }
    let now = now_ms();
    let shares: Vec<Value> = state.shares.list().iter().map(|r| r.public_json(now)).collect();
    state.engine.audit.append("share.list", principal.label(), "_share", "ok", &format!("count={}", shares.len()));
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
    if let Err(resp) = require_superuser(&principal) {
        state.engine.audit.append("share.revoke", principal.label(), "_share", "denied", "");
        return resp;
    }
    let Some(handle) = state.shares.find_for_revoke(id.trim()) else {
        state.engine.audit.append("share.revoke", principal.label(), "_share", "error", "unknown share");
        return es_error(StatusCode::NOT_FOUND, "resource_not_found_exception", "unknown share".into());
    };
    let now = now_ms();
    let key_ids = {
        let Some(mut rec) = state.shares.records.get_mut(&handle) else {
            return es_error(StatusCode::NOT_FOUND, "resource_not_found_exception", "unknown share".into());
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

/// `POST /_share/{id}/claim` — the guest's one call. **Unauthenticated**
/// (see the module docs for why that is safe), rate-limited, audited.
///
/// Body: `{passcode}`. On success: `{api_key, index, indices, brain,
/// expires_at, label, claims_left}` — `api_key` is ready for
/// `Authorization: ApiKey <api_key>`. `index` is the comma-joined list, which
/// is a valid ES multi-index expression for `/{index}/_search`.
pub async fn claim_share(
    State(state): State<AppState>,
    ClaimSource(source): ClaimSource,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<Value>>,
) -> Response {
    let subject = format!("guest@{source}");
    let now = now_ms();
    let id = id.trim().to_string();
    // Unknown ids are throttled per source only; a real share is throttled
    // per share as well, so a distributed guesser still runs into the
    // per-share ceiling.
    if let Err(retry) = state.shares.charge(&format!("ip:{source}"), IP_PER_MINUTE, IP_PER_HOUR, now) {
        state.engine.audit.append("share.claim", &subject, "_share", "denied", "rate-limited (source)");
        return too_many(retry);
    }
    let Some(record) = state.shares.find_by_id(&id) else {
        state.engine.audit.append("share.claim", &subject, "_share", "error", "unknown share");
        return es_error(StatusCode::NOT_FOUND, "resource_not_found_exception", "unknown share link".into());
    };
    let handle = record.handle.clone();
    if let Err(retry) = state.shares.charge(&format!("share:{handle}"), SHARE_PER_MINUTE, SHARE_PER_HOUR, now) {
        state.engine.audit.append("share.claim", &subject, &handle, "denied", "rate-limited (share)");
        return too_many(retry);
    }
    if let Some(why) = record.unavailable(now) {
        state.engine.audit.append("share.claim", &subject, &handle, "denied", why);
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
        state.engine.audit.append("share.claim", &subject, &handle, "denied", "wrong passcode");
        state.engine.audit.sync_to_disk();
        return es_error(StatusCode::UNAUTHORIZED, "security_exception", "wrong passcode".into());
    }

    // Mint the guest's key: read-only over exactly the granted indices,
    // expiring with the share. Same key material shape as
    // `POST /_security/api_key` so the same auth path re-authenticates it.
    let granted = record.granted_indices();
    let roles = vec![Role::new(
        format!("share:{handle}"),
        HashSet::from([Privilege::ReadIndex]),
        granted,
    )];
    let key_id = uuid::Uuid::new_v4().to_string();
    let raw_secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
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
            return es_error(StatusCode::NOT_FOUND, "resource_not_found_exception", "unknown share link".into());
        };
        if let Some(why) = rec.unavailable(now) {
            state.engine.audit.append("share.claim", &subject, &handle, "denied", why);
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
        assert!(validate_share_index("_all").is_err());
        assert!(validate_share_index("Upper").is_err());
        assert!(validate_share_index("").is_err());
    }

    #[test]
    fn rate_windows_lock_out_after_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore::open(dir.path().to_str().unwrap());
        let now = 1_000_000;
        for _ in 0..SHARE_PER_MINUTE {
            store.charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, now).unwrap();
        }
        let retry = store.charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, now).unwrap_err();
        assert!(retry >= 1 && retry <= 61, "{retry}");
        // A different share is untouched.
        store.charge("share:y", SHARE_PER_MINUTE, SHARE_PER_HOUR, now).unwrap();
        // The minute window rolls over; the hour window still counts.
        let later = now + WINDOW_MIN_MS + 1;
        store.charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, later).unwrap();
        for _ in 0..(SHARE_PER_HOUR - SHARE_PER_MINUTE - 1) {
            let _ = store.charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, later + 1);
        }
        assert!(store.charge("share:x", SHARE_PER_MINUTE, SHARE_PER_HOUR, later + WINDOW_MIN_MS + 2).is_err());
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
            let mode = std::fs::metadata(dir.path().join(SHARES_FILE)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let reloaded = ShareStore::open(data_dir);
        let rec = reloaded.find_by_id(&share_id).expect("id resolves after reload");
        assert_eq!(rec.handle, handle);
        assert_eq!(rec.granted_indices(), vec!["ax-notes".to_string(), ".xerj-memory-notes-edges".to_string()]);
        assert!(reloaded.find_by_id("0000").is_none());
        // The handle alone resolves for revoke, but never as a claim id.
        assert_eq!(reloaded.find_for_revoke(&handle).as_deref(), Some(handle.as_str()));
        assert!(reloaded.find_by_id(&handle).is_none());
    }
}
