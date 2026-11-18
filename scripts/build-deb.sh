#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-deb.sh — pack the ALREADY-BUILT release binary as a .deb.
#
# Issue #810: ship a .deb alongside the tar.gz on GitHub releases. This is the
# "fairly simple" half of Christian Kastner's split — a release asset a Debian
# user can `dpkg -i`. It is NOT Debian archive packaging: nothing here makes
# any claim about xerj entering the Debian archive (that audit lives in
# docs/PACKAGING_DEBIAN.md).
#
# Design constraint, on purpose: the .deb packs the SAME file the tar.gz packs
# — the binary from the staging directory release.yml already built — so the
# two assets ship a bit-identical executable. That is why this is a plain
# `dpkg-deb --build` over a hand-written pkg root and NOT cargo-deb: cargo-deb
# would rebuild the crate on its own and the .deb could drift from the tar.gz
# the checksums and verify-release.sh vouch for.
#
# The runtime dependency claim is measured, not assumed. TLS is rustls+ring in
# every release target (engine workspace Cargo.toml), so the binary should need
# nothing beyond glibc — but "should" is not a Depends field. The script reads
# the ELF NEEDED entries, refuses to build if anything outside the glibc set
# appears (libssl.so.3 sneaking in via a feature change would otherwise ship a
# .deb that dies on first boot with a missing library), and derives the libc6
# floor from the newest GLIBC_* symbol version the binary actually references.
#
# Only the *-unknown-linux-gnu targets are packaged (→ amd64, arm64). The musl
# targets stay tar.gz-only: a static-musl .deb is unusual and nobody asked.
#
# Usage (exactly what release.yml runs, runnable locally the same way):
#   scripts/build-deb.sh \
#     --stage-dir dist/xerj-1.0.0-rc.76-x86_64-unknown-linux-gnu \
#     --target   x86_64-unknown-linux-gnu \
#     --version  1.0.0-rc.76 \
#     --out-dir  dist
#
# --stage-dir is the directory the "Stage artifacts (Unix)" step already
# created for the tar.gz: it must hold `xerj`, README.md and LICENSE. The .deb
# is named after the stage dir, so on a tag build (stage xerj-<ver>-<target>)
# the asset is xerj-<ver>-<target>.deb. On a main-push canary build the stage
# dir is named xerj-main-<target> while --version must still be a real
# workspace version ("main" is not a valid Debian version string), so the two
# are passed separately.
#
# Outputs: <out-dir>/<stage-name>.deb and <stage-name>.deb.sha256, in the same
# sha256sum format every other asset uses. Prints `dpkg-deb --info` for the
# build log, so a CI run records exactly what it shipped.
#
# Requires: dpkg-deb, binutils (readelf, objdump), sha256sum.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# Modes of script-created FILES must not depend on the caller's umask: under a
# 077 umask `copyright` would ship 0600 and be unreadable for non-root. (The
# directories are unaffected either way — dpkg-deb records every directory in
# data.tar as 0700 no matter what is on disk, and dpkg creates the real ones
# 0755 at install time; verified both halves against dpkg-deb 1.21.)
umask 022

STAGE_DIR=""
TARGET=""
VERSION=""
OUT_DIR="."

die() { echo "build-deb.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --stage-dir) STAGE_DIR="$2"; shift ;;
    --target)    TARGET="$2";    shift ;;
    --version)   VERSION="$2";   shift ;;
    --out-dir)   OUT_DIR="$2";   shift ;;
    -h|--help)   awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
    *)           die "unknown argument: $1" ;;
  esac
  shift
done

[ -n "$STAGE_DIR" ] || die "--stage-dir is required"
[ -n "$TARGET" ]    || die "--target is required"
[ -n "$VERSION" ]   || die "--version is required"
[ -x "$STAGE_DIR/xerj" ] || die "$STAGE_DIR/xerj is not an executable — stage the binary first"
[ -f "$STAGE_DIR/README.md" ] || die "$STAGE_DIR/README.md missing — the tar.gz stage step provides it"
[ -f "$STAGE_DIR/LICENSE" ]   || die "$STAGE_DIR/LICENSE missing — the tar.gz stage step provides it"
command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb not on PATH (run on the release runner, or install dpkg)"
for t in readelf objdump sha256sum; do
  command -v "$t" >/dev/null 2>&1 || die "$t not on PATH"
done

# Debian Architecture, mapped from the Rust triple. Deliberately exhaustive
# refuse-list: packaging a new target is a decision, not a default.
case "$TARGET" in
  x86_64-unknown-linux-gnu)  DEB_ARCH=amd64 ;;
  aarch64-unknown-linux-gnu) DEB_ARCH=arm64 ;;
  *) die "no Debian architecture mapping for target '$TARGET' (only the *-unknown-linux-gnu targets are packaged)" ;;
esac

# A Debian version string must start with a digit. Tag builds pass
# ${GITHUB_REF_NAME#v}; canary builds must pass the workspace version instead
# of the ref name (which is "main").
case "$VERSION" in
  [0-9]*) : ;;
  *) die "version '$VERSION' is not a valid Debian version (must start with a digit; canary builds pass the workspace version, not the ref name)" ;;
esac
# SemVer prereleases must sort before the final release in Debian too.
# A hyphen only inside +build metadata is not a prerelease separator.
DEB_VERSION="$VERSION"
if [[ "${VERSION%%+*}" == *-* ]]; then
  # Not ${VERSION/-/~}: bash tilde-expands the replacement to $HOME.
  DEB_VERSION="${VERSION%%-*}~${VERSION#*-}"
fi
dpkg --compare-versions "$DEB_VERSION" eq "$DEB_VERSION" 2>/dev/null \
  || die "version '$DEB_VERSION' rejected by dpkg --compare-versions"

# ── runtime dependencies, measured off the ELF we are about to pack ──────────
# readelf/objdump parse ELF of any architecture, so this works unchanged on
# the cross-built aarch64 binary.
NEEDED=$(readelf -d "$STAGE_DIR/xerj" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | sort -u || true)
GLIBC_FLOOR=$(objdump -T "$STAGE_DIR/xerj" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sort -Vu | tail -1 || true)
GLIBC_FLOOR="${GLIBC_FLOOR#GLIBC_}"

DEB_DEPENDS="libc6"
if [ -n "$GLIBC_FLOOR" ]; then
  DEB_DEPENDS="libc6 (>= $GLIBC_FLOOR)"
fi

# The allowlist is glibc itself, nothing else. Anything else in NEEDED means
# the binary grew a shared-library dependency the .deb's Depends does not
# express — the honest move is to fail the build, not to guess a mapping.
while IFS= read -r lib; do
  [ -z "$lib" ] && continue
  case "$lib" in
    libc.so.6|libm.so.6|libpthread.so.0|libdl.so.2|librt.so.1|ld-linux-*.so.*)
      # folded into libc6 on any glibc this floor can run on
      ;;
    libgcc_s.so.1)
      DEB_DEPENDS="$DEB_DEPENDS, libgcc-s1" ;;
    *)
      die "binary needs '$lib', which this .deb's Depends does not cover — add it or change the build (expected: glibc only; TLS is rustls, so libssl here would be a regression)" ;;
  esac
done <<< "$NEEDED"

echo "build-deb.sh: NEEDED libraries: ${NEEDED:-<none>}"
echo "build-deb.sh: newest GLIBC symbol version: ${GLIBC_FLOOR:-<none found>}"
echo "build-deb.sh: Depends: $DEB_DEPENDS"

# ── package root ─────────────────────────────────────────────────────────────
STAGE_NAME=$(basename "$STAGE_DIR")
PKG=$(mktemp -d "${TMPDIR:-/tmp}/xerj-deb-XXXXXX")
trap 'rm -rf "$PKG"' EXIT

mkdir -p "$PKG/DEBIAN" "$PKG/usr/bin" "$PKG/usr/share/doc/xerj"

# The exact file the tar.gz ships — same inode content, no strip, no rebuild.
cp "$STAGE_DIR/xerj" "$PKG/usr/bin/xerj"
cp "$STAGE_DIR/README.md" "$PKG/usr/share/doc/xerj/README.md"
cp "$STAGE_DIR/LICENSE"   "$PKG/usr/share/doc/xerj/LICENSE"

# Debian machine-readable copyright, pointing at the system copy of the
# Apache-2.0 text instead of pasting it a third time into /usr/share/doc.
cat > "$PKG/usr/share/doc/xerj/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: xerj
Source: https://github.com/xerj-org/xerj

Files: *
Copyright: xerj Contributors
License: Apache-2.0
 On Debian systems, the complete text of the Apache License 2.0 can be
 found at /usr/share/common-licenses/Apache-2.0.
EOF

INSTALLED_SIZE=$(du -sk "$PKG/usr" | cut -f1)

# Description continuation lines must start with a single space; the package
# description is the one place a Debian user meets before the binary, so it
# claims only what the binary does — search engine, one file, ES-compatible
# REST surface included.
cat > "$PKG/DEBIAN/control" <<EOF
Package: xerj
Version: $DEB_VERSION
Architecture: $DEB_ARCH
Maintainer: xerj Contributors <https://github.com/xerj-org/xerj>
Installed-Size: $INSTALLED_SIZE
Depends: $DEB_DEPENDS
Section: net
Priority: optional
Homepage: https://xerj.org
Built-Using: rustc ($TARGET)
Description: single-binary search engine for AI workloads
 XERJ is a search engine written from scratch in Rust that ships as one
 executable: full-text, vector and hybrid search over the same indices,
 built-in agent memory, and log analytics. It also speaks the Elasticsearch
 8.x REST protocol as a compatibility bridge, so existing ES clients work
 unmodified. This package contains the release binary built from
 https://github.com/xerj-org/xerj at version $VERSION.
EOF

mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/${STAGE_NAME}.deb"

# --root-owner-group: files land root:root without this script being root.
dpkg-deb --root-owner-group --build "$PKG" "$DEB" >/dev/null

( cd "$OUT_DIR" && sha256sum "$(basename "$DEB")" > "$(basename "$DEB").sha256" )

echo "build-deb.sh: built $DEB ($(du -h "$DEB" | cut -f1))"
dpkg-deb --info "$DEB"
