//! Shared-secret authentication for the cluster control transport.
//!
//! Every control frame on the cluster port carries an HMAC-SHA256 tag computed
//! over a domain-separated binding of:
//!
//! * a protocol context string (`hello` vs `frame` — domain separation),
//! * the wire version byte,
//! * the **per-connection challenge** the receiver generated and sent first,
//! * the sender's node id,
//! * the frame's position in the connection (`seq`, implicit — never on the wire),
//! * the frame payload.
//!
//! The challenge is what makes a captured frame useless: it is 32 fresh random
//! bytes chosen by the *receiver* for every accepted connection, so a recorded
//! frame replayed to the same node (or reflected at a different node) is
//! verified against a challenge it was never signed under and is rejected. The
//! implicit `seq` additionally pins each frame to its position, so frames
//! within a live connection cannot be reordered, dropped, or duplicated
//! without the tag failing.
//!
//! ## What this does *not* give you
//!
//! * **No confidentiality.** Frames are still plaintext JSON on the wire; the
//!   HMAC authenticates, it does not encrypt. Cluster traffic still belongs on
//!   a trusted network segment.
//! * **No per-node identity.** The secret is cluster-wide, so it proves
//!   "the peer knows the cluster secret", not "the peer is node X". A
//!   compromised member can impersonate any other member. Per-node identity
//!   needs mTLS or per-node keys, which is a larger design change.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

// ── Wire constants ────────────────────────────────────────────────────────────

/// Magic bytes that open the receiver's handshake.
pub const WIRE_MAGIC: &[u8; 8] = b"XERJCLUS";

/// Wire version. Bumped whenever the authenticated framing changes.
pub const WIRE_VERSION: u8 = 1;

/// Length of the per-connection challenge, in bytes.
pub const CHALLENGE_LEN: usize = 32;

/// Length of an HMAC-SHA256 tag, in bytes.
pub const TAG_LEN: usize = 32;

/// Minimum accepted length of a configured cluster secret, in characters.
///
/// Short secrets are brute-forceable offline from a single captured frame, so
/// they are rejected outright rather than accepted with a warning.
pub const MIN_SECRET_LEN: usize = 16;

/// Domain-separation context for the connection hello.
const CTX_HELLO: &[u8] = b"xerj-cluster/v1/hello";

/// Domain-separation context for a message frame.
const CTX_FRAME: &[u8] = b"xerj-cluster/v1/frame";

// ── Errors ────────────────────────────────────────────────────────────────────

/// Failure to build a [`ClusterSecret`] from operator-supplied material.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    /// No secret was supplied at all.
    #[error(
        "cluster authentication secret is empty — cluster mode refuses to run \
         an unauthenticated control transport"
    )]
    Empty,
    /// The secret was supplied but is too short to resist offline guessing.
    #[error("cluster authentication secret is too short ({got} chars, minimum {MIN_SECRET_LEN})")]
    TooShort {
        /// The length that was supplied.
        got: usize,
    },
}

// ── ClusterSecret ─────────────────────────────────────────────────────────────

/// A validated cluster-wide shared secret.
///
/// Constructing one is the *only* way to obtain frame tags, which is how the
/// transport is made structurally incapable of running unauthenticated: there
/// is no "no secret" variant to fall back to.
#[derive(Clone)]
pub struct ClusterSecret {
    key: Vec<u8>,
}

impl std::fmt::Debug for ClusterSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material, not even a prefix.
        f.write_str("ClusterSecret(<redacted>)")
    }
}

impl ClusterSecret {
    /// Validate and adopt an operator-supplied secret.
    ///
    /// Surrounding whitespace is trimmed (a trailing newline from
    /// `XERJ_CLUSTER_AUTH_SECRET=$(cat secret)` is a configuration accident,
    /// not key material), after which the secret must be at least
    /// [`MIN_SECRET_LEN`] characters.
    pub fn new(secret: &str) -> Result<Self, SecretError> {
        let trimmed = secret.trim();
        if trimmed.is_empty() {
            return Err(SecretError::Empty);
        }
        if trimmed.chars().count() < MIN_SECRET_LEN {
            return Err(SecretError::TooShort {
                got: trimmed.chars().count(),
            });
        }
        Ok(ClusterSecret {
            key: trimmed.as_bytes().to_vec(),
        })
    }

    /// Tag authenticating a connection hello (the sender's node id, bound to
    /// the receiver's challenge).
    pub fn hello_tag(&self, challenge: &[u8; CHALLENGE_LEN], sender: &str) -> [u8; TAG_LEN] {
        let mut mac = self.mac();
        mac.update(CTX_HELLO);
        mac.update(&[WIRE_VERSION]);
        mac.update(challenge);
        Self::update_lp(&mut mac, sender.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Tag authenticating one message frame.
    ///
    /// `seq` is the zero-based index of this frame within the connection. It is
    /// never transmitted: both ends count independently, so a replayed,
    /// dropped, or reordered frame lands on the wrong `seq` and fails.
    pub fn frame_tag(
        &self,
        challenge: &[u8; CHALLENGE_LEN],
        sender: &str,
        seq: u64,
        payload: &[u8],
    ) -> [u8; TAG_LEN] {
        let mut mac = self.mac();
        mac.update(CTX_FRAME);
        mac.update(&[WIRE_VERSION]);
        mac.update(challenge);
        Self::update_lp(&mut mac, sender.as_bytes());
        mac.update(&seq.to_be_bytes());
        Self::update_lp(&mut mac, payload);
        mac.finalize().into_bytes().into()
    }

    fn mac(&self) -> HmacSha256 {
        // HMAC accepts a key of any length; this cannot fail.
        HmacSha256::new_from_slice(&self.key).expect("HMAC accepts keys of any length")
    }

    /// Feed a length-prefixed field so that concatenation is unambiguous
    /// (`"ab" || "c"` must not tag the same as `"a" || "bc"`).
    fn update_lp(mac: &mut HmacSha256, bytes: &[u8]) {
        mac.update(&(bytes.len() as u64).to_be_bytes());
        mac.update(bytes);
    }
}

// ── Constant-time comparison ──────────────────────────────────────────────────

/// Compare a computed tag against a tag read off the wire in constant time.
///
/// A plain `==` on the byte arrays would short-circuit on the first differing
/// byte and leak, through timing, how many leading bytes an attacker guessed
/// correctly — enough to forge a tag byte-by-byte. Length is not secret, so the
/// length check may short-circuit.
#[must_use]
pub fn tags_match(expected: &[u8; TAG_LEN], actual: &[u8]) -> bool {
    if actual.len() != TAG_LEN {
        return false;
    }
    expected.ct_eq(actual).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a-perfectly-fine-cluster-secret";

    fn challenge(b: u8) -> [u8; CHALLENGE_LEN] {
        [b; CHALLENGE_LEN]
    }

    #[test]
    fn rejects_empty_and_short_secrets() {
        assert_eq!(ClusterSecret::new("").unwrap_err(), SecretError::Empty);
        assert_eq!(ClusterSecret::new("   \n").unwrap_err(), SecretError::Empty);
        assert_eq!(
            ClusterSecret::new("short").unwrap_err(),
            SecretError::TooShort { got: 5 }
        );
        assert!(ClusterSecret::new(SECRET).is_ok());
    }

    #[test]
    fn secret_debug_never_leaks_key_material() {
        let s = ClusterSecret::new(SECRET).unwrap();
        let rendered = format!("{s:?}");
        assert!(
            !rendered.contains("cluster-secret"),
            "Debug leaked key material: {rendered}"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn tag_binds_challenge_sender_seq_and_payload() {
        let s = ClusterSecret::new(SECRET).unwrap();
        let base = s.frame_tag(&challenge(1), "node-a", 0, b"payload");

        assert_ne!(base, s.frame_tag(&challenge(2), "node-a", 0, b"payload"));
        assert_ne!(base, s.frame_tag(&challenge(1), "node-b", 0, b"payload"));
        assert_ne!(base, s.frame_tag(&challenge(1), "node-a", 1, b"payload"));
        assert_ne!(base, s.frame_tag(&challenge(1), "node-a", 0, b"payloa!"));

        // Different secret → different tag.
        let other = ClusterSecret::new("an-entirely-different-secret").unwrap();
        assert_ne!(
            base,
            other.frame_tag(&challenge(1), "node-a", 0, b"payload")
        );

        // Same inputs → same tag.
        assert_eq!(base, s.frame_tag(&challenge(1), "node-a", 0, b"payload"));
    }

    #[test]
    fn hello_and_frame_contexts_are_separated() {
        let s = ClusterSecret::new(SECRET).unwrap();
        let hello = s.hello_tag(&challenge(7), "node-a");
        let frame = s.frame_tag(&challenge(7), "node-a", 0, b"");
        assert_ne!(hello, frame, "hello and frame tags must not be confusable");
    }

    #[test]
    fn length_prefixing_prevents_field_smuggling() {
        let s = ClusterSecret::new(SECRET).unwrap();
        // "ab" + payload "c"  must not collide with "a" + payload "bc".
        assert_ne!(
            s.frame_tag(&challenge(0), "ab", 0, b"c"),
            s.frame_tag(&challenge(0), "a", 0, b"bc")
        );
    }

    #[test]
    fn tags_match_accepts_equal_and_rejects_everything_else() {
        let s = ClusterSecret::new(SECRET).unwrap();
        let tag = s.frame_tag(&challenge(3), "n", 0, b"x");
        assert!(tags_match(&tag, &tag));

        let mut flipped = tag;
        flipped[TAG_LEN - 1] ^= 0x01;
        assert!(!tags_match(&tag, &flipped));

        // Truncated and over-long tags are rejected without panicking.
        assert!(!tags_match(&tag, &tag[..TAG_LEN - 1]));
        assert!(!tags_match(&tag, &[0u8; TAG_LEN + 1]));
        assert!(!tags_match(&tag, &[]));
    }
}
