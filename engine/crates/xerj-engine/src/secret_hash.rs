//! Salted fast hash for **machine-generated** credentials (issue #201).
//!
//! # Why not Argon2/scrypt/bcrypt
//!
//! Those are password hashes. Their whole job is to be slow, because a human
//! password carries maybe 20–40 bits of entropy and an offline attacker with
//! the hash can otherwise walk the whole space. A xerj API-key secret is not a
//! password: `POST /_security/api_key` mints it from two v4 UUIDs
//! (`es_compat::security_create_api_key`), i.e. **244 bits** of CSPRNG output
//! that no human ever chooses, types or reuses. Brute-forcing that is not
//! "expensive", it is arithmetically out of reach regardless of how fast the
//! hash is — so a slow hash buys nothing against the only attack it defends,
//! and costs a great deal: this comparison runs on **every authenticated
//! request**, and Argon2id at any recommended parameterisation is tens of
//! milliseconds, which would turn a sub-millisecond search into a
//! password-hash benchmark and hand an attacker a trivial CPU-exhaustion DoS
//! (send garbage credentials, make the node hash them).
//!
//! So: one SHA-256 over `salt || secret`. That is the same call Elasticsearch
//! makes for API keys — its default `xpack.security.authc.api_key.hashing.
//! algorithm` is `SSHA256`, salted SHA-256, not one of the PBKDF2/bcrypt
//! options it offers for *user* passwords (approach read from
//! `x-pack/plugin/security/.../ApiKeyService.java:170-174` and
//! `x-pack/plugin/core/.../authc/support/Hasher.java:418-447`; ES is
//! AGPL/Elastic-licensed and APPROACH-ONLY here — no code was copied, the Rust
//! below is ours and the encoding is different).
//!
//! # Why salt at all, if the secret is already unguessable
//!
//! Two reasons, both cheap: a per-record salt means two nodes that somehow
//! minted the same secret do not produce the same stored string, and it kills
//! any precomputation (a shared rainbow table over, say, a leaked generator's
//! output space) before it starts. 16 bytes of salt costs 32 hex characters.
//!
//! # Encoding
//!
//! `$ssha256$<32 hex salt chars>$<64 hex digest chars>` — self-describing, so
//! a future algorithm can be added without a migration flag day and old
//! records keep verifying. [`is_usable_hash`] is the discriminator the load
//! path uses to tell a credential this build can actually check apart from a
//! pre-#201 plaintext secret — or from a corrupt record that can never
//! authenticate at all.

use sha2::{Digest, Sha256};

/// Scheme tag. Present on every string [`hash_secret`] produces.
const SSHA256_PREFIX: &str = "$ssha256$";

/// Salt length in bytes.
const SALT_LEN: usize = 16;

/// Hash `secret` under a freshly generated salt.
///
/// The salt comes from `Uuid::new_v4`, i.e. the `getrandom` CSPRNG — the same
/// source the secret itself is minted from. (122 of the 128 bits are random;
/// the six version/variant bits are fixed. A salt needs uniqueness, not
/// entropy, so that is ample.)
pub fn hash_secret(secret: &str) -> String {
    let salt = uuid::Uuid::new_v4().into_bytes();
    debug_assert_eq!(salt.len(), SALT_LEN);
    encode(&salt, &digest(&salt, secret))
}

/// Constant-time check of `presented` against a string produced by
/// [`hash_secret`].
///
/// Returns `false` — never panics, never errors — for anything it does not
/// recognise: a truncated record, a hand-edited file, a plaintext secret left
/// over from before #201. "Unrecognised" must mean "denied", so a mangled
/// store cannot become an authentication bypass.
pub fn verify_secret(presented: &str, stored: &str) -> bool {
    let Some((salt, expected)) = decode(stored) else {
        return false;
    };
    constant_time_eq(&digest(&salt, presented), &expected)
}

/// Is `stored` a hash this build can actually verify a secret against?
///
/// The discriminator the load path uses
/// (`Engine::load_persisted_api_keys` → `ApiKeyRecord::migrate_from_plaintext`)
/// and the same one `ApiKeyRecord::verify_secret` fails closed on, so the two
/// cannot disagree about what counts as a credential.
///
/// It is a **full parse**, not a scheme-tag check, and that distinction is
/// load-bearing: `"$ssha256$truncated"` carries the tag but decodes to
/// nothing, so [`verify_secret`] denies it against every input. A prefix test
/// would call that record "already hashed" and restore it — leaving a key
/// that `GET /_security/api_key` lists as live while nothing can ever
/// authenticate as it, which is exactly the accept-then-ignore shape issue
/// #204 tracks. "Tagged" is not "usable"; only "decodes" is.
pub fn is_usable_hash(stored: &str) -> bool {
    decode(stored).is_some()
}

// ── internals ────────────────────────────────────────────────────────────────

fn digest(salt: &[u8], secret: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(secret.as_bytes());
    h.finalize().into()
}

fn encode(salt: &[u8], digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(SSHA256_PREFIX.len() + SALT_LEN * 2 + 1 + 64);
    out.push_str(SSHA256_PREFIX);
    push_hex(&mut out, salt);
    out.push('$');
    push_hex(&mut out, digest);
    out
}

fn decode(stored: &str) -> Option<(Vec<u8>, [u8; 32])> {
    let body = stored.strip_prefix(SSHA256_PREFIX)?;
    let (salt_hex, digest_hex) = body.split_once('$')?;
    if salt_hex.len() != SALT_LEN * 2 || digest_hex.len() != 64 {
        return None;
    }
    let salt = from_hex(salt_hex)?;
    let digest_bytes = from_hex(digest_hex)?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&digest_bytes);
    Some((salt, digest))
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;
    for b in bytes {
        // Writing into a String is infallible.
        let _ = write!(out, "{b:02x}");
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Length-independent-only constant-time comparison. Both operands here are
/// fixed-size digests, so length never varies in practice; the loop exists so
/// a match/mismatch takes the same time regardless of *where* it differs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let stored = hash_secret("s3cr3t");
        assert!(is_usable_hash(&stored));
        assert!(verify_secret("s3cr3t", &stored));
        assert!(!verify_secret("s3cr3T", &stored));
        assert!(!verify_secret("", &stored));
    }

    #[test]
    fn the_secret_is_not_recoverable_from_the_encoding() {
        let stored = hash_secret("a-very-recognisable-secret");
        assert!(!stored.contains("a-very-recognisable-secret"));
        assert_eq!(stored.len(), SSHA256_PREFIX.len() + 32 + 1 + 64);
    }

    #[test]
    fn salt_is_per_call() {
        let a = hash_secret("same");
        let b = hash_secret("same");
        assert_ne!(a, b, "two hashes of the same secret must differ");
        assert!(verify_secret("same", &a) && verify_secret("same", &b));
    }

    /// Anything we do not recognise denies. A plaintext leftover must never
    /// verify against itself — that would silently re-create the bug #201 is
    /// about, with the plaintext comparison hiding behind the hash API.
    #[test]
    fn unrecognised_encodings_deny() {
        for stored in [
            "",
            "plaintext-secret",
            "$ssha256$",
            "$ssha256$deadbeef$cafe",
            "$ssha256$00000000000000000000000000000000",
            "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA",
        ] {
            assert!(
                !verify_secret("plaintext-secret", stored),
                "unrecognised stored form {stored:?} must not verify"
            );
            // The two must agree exactly: anything that can never verify is
            // also not a usable hash, so the load path drops it instead of
            // restoring a key nothing can authenticate as. Three of these
            // carry the `$ssha256$` tag, which is why "usable" is a full
            // decode and not a prefix test.
            assert!(
                !is_usable_hash(stored),
                "{stored:?} can never verify, so it must not count as a usable hash"
            );
        }
    }

    /// A record whose digest hex is valid but whose salt is wrong must fail —
    /// i.e. the salt is really mixed in, not decorative.
    #[test]
    fn the_salt_participates() {
        let stored = hash_secret("secret");
        let (salt, digest) = decode(&stored).expect("decodes");
        let mut other_salt = salt.clone();
        other_salt[0] ^= 0xFF;
        let tampered = encode(&other_salt, &digest);
        assert!(!verify_secret("secret", &tampered));
    }
}
