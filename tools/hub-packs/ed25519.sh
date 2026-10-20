#!/usr/bin/env bash
# Ed25519 checks for the hub pipeline, with nothing but `openssl` and `xxd` (both on
# every GitHub runner), so CI can prove a signature without building a verifier.
#
#   ed25519.sh derive <seed-file>             print the public key (64 hex) of a seed
#   ed25519.sh verify <pub-hex> <file> <sig>  exit 0 iff <sig> signs <file> under <pub>
#
# Formats are those of xerj-autoindex's harvest/sign.rs: a seed is 32 bytes as 64 hex
# characters; a public key likewise; a detached signature is the 64 raw bytes as 128 hex
# characters (what `hub build-pack` writes beside the archive, and what the catalogue's
# `.sig` holds).  Whitespace around any of them is ignored.
#
# Why this exists: pack-publish.yml proves in the publishing run that its signing secret
# still matches the COMMITTED public key, so a rotated secret fails there instead of
# shipping a pack no client can verify.  hub-packs.yml needs the same proof against the
# key clients embed (HUB_PUBKEY_HEX), and against the packs and catalogue it produces.
set -euo pipefail

die() { echo "ed25519.sh: $*" >&2; exit 2; }

# DER prefixes for a raw Ed25519 key (RFC 8410): PKCS#8 private (+32-byte seed) and
# SubjectPublicKeyInfo public (+32-byte key).
PKCS8_PREFIX=302e020100300506032b657004220420
SPKI_PREFIX=302a300506032b6570032100

hex_only() { tr -d '[:space:]' | tr 'A-F' 'a-f'; }

case "${1:-}" in
  derive)
    [ $# -eq 2 ] || die "usage: derive <seed-file>"
    seed=$(hex_only < "$2")
    [[ $seed =~ ^[0-9a-f]{64}$ ]] || die "the seed must be 64 hex characters"
    tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
    printf '%s%s' "$PKCS8_PREFIX" "$seed" | xxd -r -p > "$tmp"
    openssl pkey -inform DER -in "$tmp" -pubout -outform DER | tail -c 32 | xxd -p -c 64
    ;;
  verify)
    [ $# -eq 4 ] || die "usage: verify <pub-hex> <file> <sig-file>"
    pub=$(printf '%s' "$2" | hex_only)
    [[ $pub =~ ^[0-9a-f]{64}$ ]] || die "the public key must be 64 hex characters"
    [ -f "$3" ] || die "no such file: $3"
    sig=$(hex_only < "$4")
    [[ $sig =~ ^[0-9a-f]{128}$ ]] || die "the signature must be 128 hex characters: $4"
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT
    printf '%s%s' "$SPKI_PREFIX" "$pub" | xxd -r -p > "$dir/pub.der"
    printf '%s' "$sig" | xxd -r -p > "$dir/sig.bin"
    openssl pkeyutl -verify -pubin -inkey "$dir/pub.der" -keyform DER -rawin \
      -in "$3" -sigfile "$dir/sig.bin" >/dev/null
    echo "signature verifies: $3"
    ;;
  *) die "usage: ed25519.sh derive <seed-file> | verify <pub-hex> <file> <sig-file>" ;;
esac
