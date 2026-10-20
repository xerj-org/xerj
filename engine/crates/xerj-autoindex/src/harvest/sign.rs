//! Detached ed25519 signatures over a pack's SHA256SUMS — the publish half
//! of the pack format.
//!
//! Checksums (M1) prove a pack arrived intact; a signature proves WHO built
//! it. That distinction is the whole design: SHA256SUMS re-hashing catches
//! corruption and lazy tampering, but an attacker who rebuilds a pack
//! self-consistently produces valid checksums for malicious records — the
//! case `read_manifest`'s tests explicitly left to "a later milestone",
//! which is this file. Every prior-art vulnerability database surveyed at
//! design time (Trivy DB, Grype/vunnel, PrimeVul) ships unsigned; this is
//! the open lane, not table stakes.
//!
//! The trust model, stated plainly:
//!
//! - The **public** key travels out of band — committed next to the pack
//!   recipe in this repo, and on xerj.org — never inside the pack (a key
//!   shipped beside its own signature verifies nothing).
//! - The **seed** lives only in CI's secrets; `corpus build` never sees it.
//!   Signing is a separate `corpus sign` step so a local build and a
//!   published build differ by exactly one file.
//! - v1 is a single release key. Rotation is by re-publishing under a new
//!   key and updating the two public locations; consumers pin the file they
//!   fetched, and the README says so.
//!
//! Format, deliberately boring: `SHA256SUMS.sig` is the RAW 64-byte ed25519
//! signature of SHA256SUMS' exact bytes. No armored envelopes, no trusted
//! comments — a consumer with any ed25519 implementation can verify the
//! bytes without this crate. The `.sig` file is intentionally absent from
//! SHA256SUMS itself (it signs that file; covering it would be circular) —
//! `read_manifest` only re-hashes files the SUMS names, so the signature
//! rides in a pack dir or zip without breaking older readers.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ring::rand::SecureRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// The signature file name inside a pack directory.
pub const SIG_NAME: &str = "SHA256SUMS.sig";

/// Generate a fresh ed25519 keypair as (seed_hex, public_hex) — 32 bytes
/// each, lower-case hex. The seed is everything an attacker needs: it goes
/// to CI secrets and nowhere else; the public half is published.
pub fn generate_keypair() -> Result<(String, String)> {
    let mut seed = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut seed)
        .map_err(|_| anyhow::anyhow!("key generation failed (no entropy source?)"))?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("a fresh 32-byte seed is always valid — ring disagrees"))?;
    Ok((hex(&seed), hex(pair.public_key().as_ref())))
}

/// Sign `<pack>/SHA256SUMS` with a seed file, writing `<pack>/SHA256SUMS.sig`.
/// Refuses to sign a pack whose SUMS is missing (nothing to sign) and
/// overwrites a previous `.sig` — re-signing a rebuilt pack is the normal
/// publish step, and a stale signature would fail verification anyway.
pub fn sign_pack(pack_dir: &Path, seed_hex: &str) -> Result<()> {
    let sums = pack_dir.join("SHA256SUMS");
    let msg = std::fs::read(&sums).with_context(|| format!("read {}", sums.display()))?;
    let seed = parse_key(seed_hex, 32, "seed")?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("seed is not a valid ed25519 key"))?;
    let sig = pair.sign(&msg);
    let out = pack_dir.join(SIG_NAME);
    std::fs::write(&out, sig.as_ref()).with_context(|| format!("write {}", out.display()))?;
    println!(
        "signed {} ({} byte signature over SHA256SUMS)",
        out.display(),
        sig.as_ref().len()
    );
    Ok(())
}

/// Verify `<pack>/SHA256SUMS.sig` against a public-key file. The signature
/// chain is: sig → SHA256SUMS → every file the SUMS names, so verifying here
/// plus `read_manifest`'s re-hashing proves both integrity and origin.
/// `verify_pack` is called BEFORE the pack is materialized, so a failure
/// means nothing was indexed.
pub fn verify_pack(pack_dir: &Path, public_hex: &str) -> Result<()> {
    verify_sig_at(pack_dir, &pack_dir.join(SIG_NAME), public_hex)
}

/// Same verification with an explicit signature path — `corpus add --from
/// <pack.zip>` uses this with the LOOSE signature a release ships beside the
/// zip: the sig is never packed inside the zip (it is deliberately absent
/// from the SUMS it signs, and the signing step runs after the zip is
/// built), so a zip consumer's signature lives in a sibling file.
pub fn verify_sig_at(pack_dir: &Path, sig_path: &Path, public_hex: &str) -> Result<()> {
    let at = |what: &str| format!("{}: {what}", pack_dir.display());
    let sig = std::fs::read(sig_path).with_context(|| at("no SHA256SUMS.sig to verify"))?;
    if sig.len() != 64 {
        bail!(at(&format!(
            "{SIG_NAME} is {} bytes, expected a 64-byte ed25519 signature",
            sig.len()
        )));
    }
    let public = parse_key(public_hex, 32, "public key")?;
    let msg =
        std::fs::read(pack_dir.join("SHA256SUMS")).with_context(|| at("cannot read SHA256SUMS"))?;
    let key = UnparsedPublicKey::new(&ring::signature::ED25519, &public);
    key.verify(&msg, &sig).map_err(|_| {
        anyhow::anyhow!(at(
            "signature does not verify — the pack is not from the holder of the expected key, \
                 or SHA256SUMS changed after signing"
        ))
    })?;
    Ok(())
}

/// True when the pack dir carries a signature file (the `corpus add` hint:
/// "this pack is signed; pass --verify-sig" — presence is public metadata,
/// verification is the consumer's explicit choice).
pub fn pack_is_signed(pack_dir: &Path) -> bool {
    pack_dir.join(SIG_NAME).is_file()
}

/// Raw ed25519 signature (64 bytes, hex) of `bytes`, for the hub pack archive
/// where the signature is a separate file next to the archive. Same key format
/// and primitive as `sign_pack`; the only difference is the message is the
/// caller's buffer (the archive bytes) rather than a SHA256SUMS file, and the
/// output is hex so it can ride in a sibling text file.
pub fn sign_bytes(bytes: &[u8], seed_hex: &str) -> Result<String> {
    let seed = parse_key(seed_hex, 32, "seed")?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("seed is not a valid ed25519 key"))?;
    let sig = pair.sign(bytes);
    Ok(hex(sig.as_ref()))
}

/// Verify a hex ed25519 signature of `bytes` against a hex public key. Mirrors
/// `verify_sig_at`'s `UnparsedPublicKey` check; returns `Ok(())` on a good
/// signature and an `Err` otherwise. The signature is the 64 raw bytes as hex
/// (128 chars) that `sign_bytes` produced.
pub fn verify_bytes(bytes: &[u8], sig_hex: &str, public_hex: &str) -> Result<()> {
    let sig = parse_key(sig_hex, 64, "signature")?;
    let public = parse_key(public_hex, 32, "public key")?;
    let key = UnparsedPublicKey::new(&ring::signature::ED25519, &public);
    key.verify(bytes, &sig).map_err(|_| {
        anyhow::anyhow!(
            "signature does not verify — the archive is not from the holder of the expected key, \
             or the bytes changed after signing"
        )
    })?;
    Ok(())
}

// ── CLI: `xerj corpus keygen`, `xerj corpus sign` ───────────────────────────

const KEYGEN_USAGE: &str = "usage: xerj corpus keygen --out <prefix>

writes <prefix>.key (the 32-byte ed25519 seed, hex — SECRET) and <prefix>.pub
(the verifying key, hex — publish this). Keep the .key out of the repo and out
of logs; it goes to CI secrets and nowhere else.";

const SIGN_USAGE: &str = "usage: xerj corpus sign <pack-dir> --key <seed-file>

writes <pack-dir>/SHA256SUMS.sig, a detached ed25519 signature over the pack's
SHA256SUMS (which covers every pack file). Re-signing overwrites. The pack's
checksums stay untouched — signing is a publish step, never a rebuild.";

/// Entry point for `xerj corpus keygen|sign <args>` from the dispatcher.
pub fn run_sign_cli(sub: &str, args: &[String]) -> i32 {
    match sub {
        "keygen" => run_keygen(args),
        "sign" => run_sign(args),
        _ => unreachable!("dispatcher routes only keygen|sign here"),
    }
}

fn run_keygen(args: &[String]) -> i32 {
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{KEYGEN_USAGE}");
                return 0;
            }
            "--out" => match args.get(i + 1) {
                Some(v) => {
                    out = Some(PathBuf::from(v));
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus keygen: --out needs a path prefix\n\n{KEYGEN_USAGE}");
                    return 2;
                }
            },
            a => {
                eprintln!("xerj corpus keygen: unexpected argument '{a}'\n\n{KEYGEN_USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(prefix) = out else {
        eprintln!("xerj corpus keygen: --out is required\n\n{KEYGEN_USAGE}");
        return 2;
    };
    match generate_keypair() {
        Ok((seed, public)) => {
            let key_path = format!("{}.key", prefix.display());
            if Path::new(&key_path).exists() {
                eprintln!(
                    "xerj corpus keygen: {key_path} already exists — refusing to overwrite a key"
                );
                return 1;
            }
            // the documented first run names a directory that does not exist
            // yet (`--out tools/packs/keys/<name>`) — create it rather than
            // make the user mkdir by hand
            if let Some(parent) = Path::new(&key_path).parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "xerj corpus keygen: cannot create {}: {e}",
                        parent.display()
                    );
                    return 1;
                }
            }
            if let Err(e) = std::fs::write(&key_path, format!("{seed}\n")).and_then(|_| {
                std::fs::write(format!("{}.pub", prefix.display()), format!("{public}\n"))
            }) {
                eprintln!("xerj corpus keygen: cannot write key files: {e}");
                return 1;
            }
            println!("wrote {key_path} (SECRET — CI secret material, never commit)");
            println!(
                "wrote {}.pub (verifying key — publish next to the pack)",
                prefix.display()
            );
            0
        }
        Err(e) => {
            eprintln!("xerj corpus keygen: {e:#}");
            1
        }
    }
}

fn run_sign(args: &[String]) -> i32 {
    let mut pack: Option<PathBuf> = None;
    let mut key: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{SIGN_USAGE}");
                return 0;
            }
            "--key" => match args.get(i + 1) {
                Some(v) => {
                    key = Some(PathBuf::from(v));
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus sign: --key needs a seed file\n\n{SIGN_USAGE}");
                    return 2;
                }
            },
            a if a.starts_with('-') => {
                eprintln!("xerj corpus sign: unknown flag '{a}'\n\n{SIGN_USAGE}");
                return 2;
            }
            a => {
                if pack.is_some() {
                    eprintln!("xerj corpus sign: one <pack-dir>, not two\n\n{SIGN_USAGE}");
                    return 2;
                }
                pack = Some(PathBuf::from(a));
            }
        }
        i += 1;
    }
    let (Some(pack), Some(key)) = (pack, key) else {
        eprintln!(
            "xerj corpus sign: <pack-dir> and --key <seed-file> are both required\n\n{SIGN_USAGE}"
        );
        return 2;
    };
    let seed = match std::fs::read_to_string(&key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("xerj corpus sign: cannot read {}: {e}", key.display());
            return 1;
        }
    };
    match sign_pack(&pack, &seed) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("xerj corpus sign: {e:#}");
            1
        }
    }
}

// ── key encoding helpers ────────────────────────────────────────────────────

/// Decode hex of an exact byte length, with a name for the error message.
/// The keys are 32 raw bytes in hex (64 chars) — not PEM, not pkcs8 — so a
/// public key file is copy-pasteable in an issue thread and diffable in a
/// repo.
fn parse_key(hex: &str, want: usize, what: &str) -> Result<Vec<u8>> {
    let t = hex.trim();
    if t.len() != want * 2 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "{what} must be {want} bytes as {want}×2 hex chars (got {} chars)",
            t.len()
        );
    }
    (0..want)
        .map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).map_err(|e| anyhow::anyhow!("{e}")))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::pack;
    use crate::harvest::recipe::{Emit, Envelope, Identity, Merge, Suggest};
    use serde_json::{json, Map};
    use sha2::Digest;

    fn tiny_pack(dir: &Path) -> PathBuf {
        let recipe = crate::harvest::recipe::Recipe {
            name: "t".into(),
            description: String::new(),
            envelope: Envelope {
                id_from: vec!["id".into()],
                title_from: vec!["id".into()],
                body_join: vec![],
                defs_from: vec![],
                passthrough: false,
            },
            sources: vec![],
            identity: Identity {
                edges: vec![],
                canonical_source_order: vec![],
            },
            merge: Merge::default(),
            derived: vec![],
            emit: Emit { shards: 1 },
            suggest: Suggest { sample: 64 },
        };
        let mut rec = Map::new();
        rec.insert("id".into(), json!("a"));
        let records = vec![rec];
        let stats = pack::RunStats {
            envelopes: 1,
            per_source: vec![pack::SourceStat {
                slug: "s".into(),
                kind: "dir".into(),
                path: None,
                url: None,
                licence: "CC0-1.0".into(),
                watermark: None,
                unchanged: false,
                records: 1,
                new: 1,
                skipped: 0,
                pruned: 0,
            }],
        };
        let sugg = crate::harvest::suggest::analyze(&records, 64);
        pack::emit(dir, &recipe, "# r\n", records, &stats, &sugg)
            .unwrap()
            .dir
    }

    #[test]
    fn sign_then_verify_round_trips_on_a_real_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let pack_dir = tiny_pack(tmp.path());
        let (seed, public) = generate_keypair().unwrap();
        sign_pack(&pack_dir, &seed).unwrap();
        assert!(pack_is_signed(&pack_dir));
        // the signed pack still passes checksum verification (the .sig file
        // is not SUMS-covered, by design, and read_manifest ignores it)
        pack::read_manifest(&pack_dir).unwrap();
        verify_pack(&pack_dir, &public).unwrap();
        // and a second key does NOT verify it
        let (_, other) = generate_keypair().unwrap();
        assert!(verify_pack(&pack_dir, &other)
            .unwrap_err()
            .to_string()
            .contains("does not verify"));
    }

    #[test]
    fn a_tampered_sums_fails_verification() {
        // the whole point of signatures: a self-consistent REBUILD (fresh
        // checksums over changed records) has valid checksums, and only the
        // signature catches it.
        let tmp = tempfile::tempdir().unwrap();
        let pack_dir = tiny_pack(tmp.path());
        let (seed, public) = generate_keypair().unwrap();
        sign_pack(&pack_dir, &seed).unwrap();
        // replace a record file AND rewrite SUMS so checksums agree — the
        // scenario read_manifest cannot catch
        let sums = std::fs::read_to_string(pack_dir.join("SHA256SUMS")).unwrap();
        let fname = sums
            .lines()
            .find(|l| l.ends_with("recipe.toml"))
            .unwrap()
            .split_once("  ")
            .unwrap()
            .1
            .to_string();
        std::fs::write(pack_dir.join(&fname), "# tampered recipe\n").unwrap();
        let mut new_sums = String::new();
        for line in sums.lines() {
            if line.ends_with(&fname) {
                let d = sha2::Sha256::digest(b"# tampered recipe\n");
                new_sums.push_str(&hex(&d));
                new_sums.push_str("  ");
                new_sums.push_str(&fname);
                new_sums.push('\n');
            } else {
                new_sums.push_str(line);
                new_sums.push('\n');
            }
        }
        std::fs::write(pack_dir.join("SHA256SUMS"), new_sums).unwrap();
        pack::read_manifest(&pack_dir).unwrap(); // checksums pass…
        let err = verify_pack(&pack_dir, &public).unwrap_err().to_string();
        assert!(err.contains("does not verify"), "{err}");
    }

    #[test]
    fn verification_names_its_failure_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let pack_dir = tiny_pack(tmp.path());
        let (seed, public) = generate_keypair().unwrap();
        // unsigned pack
        assert!(verify_pack(&pack_dir, &public)
            .unwrap_err()
            .to_string()
            .contains("no SHA256SUMS.sig"));
        sign_pack(&pack_dir, &seed).unwrap();
        // truncated signature
        std::fs::write(pack_dir.join(SIG_NAME), vec![0u8; 10]).unwrap();
        assert!(verify_pack(&pack_dir, &public)
            .unwrap_err()
            .to_string()
            .contains("10 bytes"));
        // malformed public key
        sign_pack(&pack_dir, &seed).unwrap();
        assert!(verify_pack(&pack_dir, "not-hex!")
            .unwrap_err()
            .to_string()
            .contains("public key"));
    }

    #[test]
    fn a_seed_determines_one_public_key() {
        // the property keygen relies on: seed → keypair is deterministic, so
        // the .pub derived at keygen time is the same key `sign` uses later
        let (seed, public) = generate_keypair().unwrap();
        let pair =
            Ed25519KeyPair::from_seed_unchecked(&parse_key(&seed, 32, "seed").unwrap()).unwrap();
        assert_eq!(hex(pair.public_key().as_ref()), public);
    }

    #[test]
    fn hex_keys_are_validated_loudly() {
        for bad in ["", "zz", &"a".repeat(63), &"a".repeat(66)] {
            assert!(parse_key(bad, 32, "seed").is_err(), "{bad:?}");
        }
        assert!(parse_key(&"a".repeat(64), 32, "seed").is_ok());
    }

    #[test]
    fn sign_bytes_round_trips_over_a_raw_buffer() {
        // the hub-pack case: the signature is over the archive bytes, not a
        // SHA256SUMS file. A fresh key signs a buffer and the matching public
        // key verifies it — including embedded NULs, which an archive has.
        let (seed, public) = generate_keypair().unwrap();
        let data = b"hub pack archive bytes \x00\x01\x02 not a SHA256SUMS file";
        let sig = sign_bytes(data, &seed).unwrap();
        // raw 64-byte signature as lower-case hex (128 chars)
        assert_eq!(sig.len(), 128);
        assert!(sig
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        verify_bytes(data, &sig, &public).unwrap();
    }

    #[test]
    fn sign_bytes_rejects_a_flipped_byte() {
        // flipping one byte of the signed buffer must fail — this is the whole
        // reason the archive is signed rather than just checksummed.
        let (seed, public) = generate_keypair().unwrap();
        let data = b"hub pack archive bytes";
        let sig = sign_bytes(data, &seed).unwrap();
        let mut tampered = data.to_vec();
        tampered[0] ^= 0x01;
        let err = verify_bytes(&tampered, &sig, &public)
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not verify"), "{err}");
    }

    #[test]
    fn sign_bytes_rejects_a_foreign_public_key() {
        // a signature made by one key must not verify under an unrelated key.
        let (seed, _public) = generate_keypair().unwrap();
        let data = b"hub pack archive bytes";
        let sig = sign_bytes(data, &seed).unwrap();
        let (_, other) = generate_keypair().unwrap();
        assert!(verify_bytes(data, &sig, &other).is_err());
    }
}
