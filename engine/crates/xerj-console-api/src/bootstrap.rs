//! First-launch bootstrap.
//!
//! Two responsibilities:
//!
//! 1. **Master key.** Load (or generate-then-persist) a 32-byte secret used
//!    to HMAC session cookies, magic-link tokens, and connection-secret
//!    AEAD keys. Stored at `<data_dir>/.xerj_master_key` mode 0600, or
//!    overridden by the `XERJ_CONSOLE_KEY` env var (32 hex bytes).
//!
//! 2. **Bootstrap mode.** If `.xerj_users` has zero documents with
//!    `status = "active"`, mint a single-use 30-minute magic link with
//!    `purpose = "bootstrap"` and `role = "owner"`, store its sha256 in
//!    `.xerj_magic_links`, and print a bordered banner to stderr with
//!    the redeem URL the operator should open.
//!
//! Idempotent: if an active user already exists, this function is a no-op
//! beyond loading the master key.

use std::path::Path;

use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use xerj_engine::Engine;

use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::time::{now_epoch_ms, now_iso};

/// Result of the bootstrap routine, returned to the server's main()
/// so it can fold the master key into `ConsoleState` and decide what to
/// print on stdout.
pub struct BootstrapOutcome {
    pub master_key: [u8; 32],
    /// `Some(url)` if a fresh bootstrap link was minted. The caller is
    /// responsible for printing this — `bootstrap_if_needed` does its
    /// own stderr banner already, but tests want the URL programmatically.
    pub magic_link: Option<String>,
}

/// Run the full first-launch routine. Idempotent on subsequent boots.
///
/// `bind_url` is the URL the operator should browse to (e.g.
/// `http://localhost:9200`). The full link the SPA receives is
/// `{bind_url}/_xerj-console/setup#token=…`.
pub async fn run(
    engine: &Engine,
    data_dir: &Path,
    bind_url: &str,
) -> ConsoleResult<BootstrapOutcome> {
    let master_key = load_or_init_master_key(data_dir)?;

    indices::ensure_all(engine)?;

    let magic_link = if has_any_active_user(engine).await? {
        None
    } else {
        let url = mint_bootstrap_link(engine, bind_url).await?;
        print_bootstrap_banner(&url);
        Some(url)
    };

    Ok(BootstrapOutcome {
        master_key,
        magic_link,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Master key
// ─────────────────────────────────────────────────────────────────────────────

const MASTER_KEY_FILENAME: &str = ".xerj_master_key";
const MASTER_KEY_ENV: &str = "XERJ_CONSOLE_KEY";

fn load_or_init_master_key(data_dir: &Path) -> ConsoleResult<[u8; 32]> {
    // Env var wins — covers Kubernetes secret mounts.
    if let Ok(hex) = std::env::var(MASTER_KEY_ENV) {
        let bytes = hex_decode(hex.trim()).ok_or_else(|| {
            ConsoleApiError::Internal(format!("{MASTER_KEY_ENV}: not 64 hex chars"))
        })?;
        if bytes.len() != 32 {
            return Err(ConsoleApiError::Internal(format!(
                "{MASTER_KEY_ENV}: expected 32 bytes (64 hex), got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }

    let path = data_dir.join(MASTER_KEY_FILENAME);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        let trimmed = raw.trim();
        let bytes = hex_decode(trimmed).ok_or_else(|| {
            ConsoleApiError::Internal(format!(
                "{}: not valid hex (corrupt? regenerate by deleting and restarting)",
                path.display()
            ))
        })?;
        if bytes.len() != 32 {
            return Err(ConsoleApiError::Internal(format!(
                "{}: expected 32 bytes (64 hex), got {}",
                path.display(),
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }

    // First boot: generate, persist with mode 0600.
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    let hex = hex_encode(&key);
    std::fs::write(&path, &hex)?;
    set_owner_readable_only(&path)?;
    tracing::info!(path = %path.display(), "generated xerj-console master key");
    Ok(key)
}

#[cfg(unix)]
fn set_owner_readable_only(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(0o600);
    std::fs::set_permissions(p, perm)
}

#[cfg(not(unix))]
fn set_owner_readable_only(_p: &Path) -> std::io::Result<()> {
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// "Any active user?" predicate
// ─────────────────────────────────────────────────────────────────────────────

async fn has_any_active_user(engine: &Engine) -> ConsoleResult<bool> {
    let idx = engine
        .get_index(indices::USERS)
        .map_err(|e| ConsoleApiError::Internal(format!("open {}: {e}", indices::USERS)))?;

    // Build a `term: { status: "active" }` search with size: 1. Cheap on
    // a tiny system index — we only care whether any doc matches.
    let body = json!({
        "query": { "term": { "status": "active" } },
        "size": 1,
        "track_total_hits": true
    });

    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(format!("parse bootstrap probe: {e}")))?;

    let result = idx
        .search(&req)
        .await
        .map_err(|e| ConsoleApiError::Internal(format!("search {}: {e}", indices::USERS)))?;

    Ok(result.total.value > 0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Magic link minting
// ─────────────────────────────────────────────────────────────────────────────

/// Format of the token the operator pastes into a browser. Random 32 bytes,
/// URL-safe base64 (no padding) — always 43 chars. We never log or store
/// the token in plaintext; only its sha256 lands in `.xerj_magic_links`.
const TOKEN_BYTES: usize = 32;
const BOOTSTRAP_TTL_MS: i64 = 30 * 60 * 1000; // 30 minutes

async fn mint_bootstrap_link(engine: &Engine, bind_url: &str) -> ConsoleResult<String> {
    let idx = engine
        .get_index(indices::MAGIC_LINKS)
        .map_err(|e| ConsoleApiError::Internal(format!("open {}: {e}", indices::MAGIC_LINKS)))?;

    let token = random_url_token();
    let token_hash = sha256_hex(token.as_bytes());

    let now_ms = now_epoch_ms();
    let expires_ms = now_ms + BOOTSTRAP_TTL_MS;

    let doc: Value = json!({
        "purpose":     "bootstrap",
        "role":        "owner",
        "user_id":     null,
        "email":       null,
        "created_by":  "system",
        "created_at":  now_iso(),
        "expires_at":  crate::time::epoch_ms_to_iso(expires_ms),
        "used_at":     null,
        "used_from_ip": null
    });

    idx.create_document(token_hash.clone(), doc)
        .await
        .map_err(|e| ConsoleApiError::Internal(format!("write magic link: {e}")))?;

    Ok(format!(
        "{}/_xerj-console/setup#token={}",
        bind_url.trim_end_matches('/'),
        token
    ))
}

fn print_bootstrap_banner(url: &str) {
    let bar = "─".repeat(78);
    eprintln!();
    eprintln!("┌{bar}┐");
    eprintln!("│ {:<76} │", "XERJ CONSOLE  ·  first-launch setup");
    eprintln!("│ {:<76} │", "");
    eprintln!(
        "│ {:<76} │",
        "Open this link in your browser to claim the owner account by"
    );
    eprintln!(
        "│ {:<76} │",
        "enrolling a passkey.  Valid for 30 minutes.  Single use."
    );
    eprintln!("│ {:<76} │", "");
    // The URL can be longer than 76 chars; if so, print it on its own line.
    if url.len() <= 76 {
        eprintln!("│   {:<74} │", url);
    } else {
        eprintln!("├{bar}┤");
        eprintln!("  {url}");
        eprintln!("├{bar}┤");
    }
    eprintln!("│ {:<76} │", "");
    eprintln!(
        "│ {:<76} │",
        "Need a fresh link?  `xerj admin magic-link --role owner`"
    );
    eprintln!("└{bar}┘");
    eprintln!();
}

// ─────────────────────────────────────────────────────────────────────────────
// Tiny crypto helpers (dependency-light hex + sha256 + url-safe base64).
// ─────────────────────────────────────────────────────────────────────────────

fn random_url_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64_url_encode(&bytes)
}

pub(crate) fn sha256_hex(input: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(input);
    hex_encode(&h.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        let h = a.to_digit(16)?;
        let l = b.to_digit(16)?;
        out.push(((h << 4) | l) as u8);
    }
    Some(out)
}

fn base64_url_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use xerj_common::config::Config;

    fn engine_in(dir: &TempDir) -> Engine {
        let mut cfg = Config::default();
        cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
        Engine::new(cfg).expect("engine init")
    }

    #[tokio::test]
    async fn fresh_data_dir_emits_magic_link() {
        let dir = TempDir::new().unwrap();
        let engine = engine_in(&dir);

        let outcome = run(&engine, dir.path(), "http://localhost:9200")
            .await
            .expect("bootstrap");

        assert!(outcome.magic_link.is_some(), "fresh boot must mint a link");
        let url = outcome.magic_link.unwrap();
        assert!(url.starts_with("http://localhost:9200/_xerj-console/setup#token="));
        assert!(dir.path().join(".xerj_master_key").exists());
    }

    #[tokio::test]
    async fn bootstrap_idempotent_with_active_user() {
        let dir = TempDir::new().unwrap();
        let engine = engine_in(&dir);

        // First boot mints a link.
        let _ = run(&engine, dir.path(), "http://x").await.unwrap();

        // Simulate the operator finishing enrolment by writing an active
        // user. Real flow goes through /auth/passkey/finish in phase 2.
        let users = engine.get_index(indices::USERS).unwrap();
        users
            .create_document(
                "u1".into(),
                json!({
                    "email": "owner@example.com",
                    "role": "owner",
                    "status": "active",
                    "created_at": now_iso()
                }),
            )
            .await
            .unwrap();
        users.flush().await.unwrap();

        let outcome2 = run(&engine, dir.path(), "http://x").await.unwrap();
        assert!(
            outcome2.magic_link.is_none(),
            "second boot with an active user must not mint a new link"
        );
    }

    #[test]
    fn master_key_round_trip() {
        let dir = TempDir::new().unwrap();
        let k1 = load_or_init_master_key(dir.path()).unwrap();
        let k2 = load_or_init_master_key(dir.path()).unwrap();
        assert_eq!(k1, k2, "key must be stable across boots");
    }
}
