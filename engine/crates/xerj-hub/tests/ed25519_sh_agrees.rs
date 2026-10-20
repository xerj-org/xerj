//! The CI-side verifier (`tools/hub-packs/ed25519.sh`, openssl) and the client
//! (`verify_bytes`, ring) must agree on the key and signature formats, or the
//! workflows could prove a signature no client accepts (or refuse one every
//! client accepts). Checked with a throwaway keypair: the real seed lives only in
//! the `HUB_SIGNING_SEED` secret.
//!
//! Skipped (with a note) where `bash`, `openssl` or `xxd` is missing — the
//! workflows that use the script run on ubuntu, where all three are present.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use xerj_autoindex::harvest::sign::{generate_keypair, sign_bytes, verify_bytes};
use xerj_hub::HUB_PUBKEY_HEX;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/hub-packs/ed25519.sh")
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sh(args: &[&str]) -> (bool, String) {
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn ed25519_sh_and_verify_bytes_agree() {
    if !["bash", "openssl", "xxd"].iter().all(|t| have(t)) {
        eprintln!("skipped: needs bash, openssl and xxd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (seed, public) = generate_keypair().unwrap();
    let seed_file = dir.path().join("hub.key");
    std::fs::write(&seed_file, &seed).unwrap();

    // derive: the script gets the same public half from the seed as ring does.
    let (ok, derived) = sh(&["derive", seed_file.to_str().unwrap()]);
    assert!(ok, "derive failed");
    assert_eq!(
        derived, public,
        "openssl and ring derive different public keys"
    );

    // A signature from the client's signer verifies in the script…
    let msg = dir.path().join("catalogue.json");
    std::fs::write(&msg, br#"{"format_version":1}"#).unwrap();
    let sig = sign_bytes(&std::fs::read(&msg).unwrap(), &seed).unwrap();
    let sig_file = dir.path().join("catalogue.json.sig");
    std::fs::write(&sig_file, format!("{sig}\n")).unwrap();
    let (m, s) = (msg.to_str().unwrap(), sig_file.to_str().unwrap());
    assert!(
        sh(&["verify", &public, m, s]).0,
        "script rejects a good sig"
    );
    verify_bytes(&std::fs::read(&msg).unwrap(), &sig, &public).unwrap();

    // …and both refuse it under the compiled-in hub key and after a flipped byte.
    assert!(
        !sh(&["verify", HUB_PUBKEY_HEX, m, s]).0,
        "script accepts a test-key signature under HUB_PUBKEY_HEX"
    );
    let err = verify_bytes(&std::fs::read(&msg).unwrap(), &sig, HUB_PUBKEY_HEX).unwrap_err();
    assert!(
        format!("{err:#}").contains("does not verify"),
        "HUB_PUBKEY_HEX must parse as a key and fail only the signature check: {err:#}"
    );
    std::fs::write(&msg, br#"{"format_version":2}"#).unwrap();
    assert!(
        !sh(&["verify", &public, m, s]).0,
        "script accepts a flipped byte"
    );
    assert!(verify_bytes(&std::fs::read(&msg).unwrap(), &sig, &public).is_err());
}
