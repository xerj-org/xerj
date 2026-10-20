#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# debian-rust-coverage.sh — how much of xerj's Rust dependency tree is already
# packaged in Debian?
#
# Issue #810's second half. A package enters the Debian ARCHIVE only if every
# build dependency is itself in Debian (Christian Kastner's split: the .deb
# release asset is "fairly simple"; archive inclusion is a function of how
# much of the tree Debian already carries). This script measures that number
# instead of guessing it, and docs/PACKAGING_DEBIAN.md records the result.
#
# Method:
#   * third-party crates read from engine/Cargo.lock (workspace xerj-* crates
#     excluded — they would be built from this source package);
#   * compared against the SOURCE packages of Debian unstable
#     (dists/unstable/main/source/Sources.gz), where the Debian Rust team
#     packages each crate as rust-<crate> with '_' mapped to '-'
#     (serde_derive → rust-serde-derive, per Debian policy on '_' in package
#     names);
#   * a crate counts as PRESENT if the source package exists. PRESENCE ONLY:
#     the packaged version may be older than the one Cargo.lock pins, and this
#     script does not compare versions — that deeper audit is what "archive
#     inclusion not pursued yet" means in the doc.
#
# Usage:
#   scripts/debian-rust-coverage.sh                 # fetch index, print report
#   scripts/debian-rust-coverage.sh --verbose       # also list missing crates
#   scripts/debian-rust-coverage.sh --index F       # reuse a saved Sources.gz
#   scripts/debian-rust-coverage.sh --lock F        # another Cargo.lock
#
# Requires: curl, gzip. Network unless --index is given.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
LOCK="${REPO_ROOT}/engine/Cargo.lock"
INDEX=""
VERBOSE=0
SOURCES_URL="http://deb.debian.org/debian/dists/unstable/main/source/Sources.gz"

while [ $# -gt 0 ]; do
  case "$1" in
    --lock)   LOCK="$2";   shift ;;
    --index)  INDEX="$2";  shift ;;
    --verbose) VERBOSE=1 ;;
    -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

[ -f "$LOCK" ] || { echo "no such Cargo.lock: $LOCK" >&2; exit 2; }

# Third-party crate names: every [[package]] name that is not a workspace
# xerj-* crate. awk state machine, not grep -A, because the entry body is
# multi-line and the name must be tied to its [[package]] stanza.
CRATES=$(awk '
  /^\[\[package\]\]$/ { in_pkg = 1; next }
  in_pkg && /^name = "/ { gsub(/"/, "", $3); print $3; in_pkg = 0; next }
  /^$/ { in_pkg = 0 }
' "$LOCK" | grep -v '^xerj-' | sort -u)
TOTAL=$(printf '%s\n' "$CRATES" | wc -l | tr -d ' ')

if [ -n "$INDEX" ]; then
  [ -f "$INDEX" ] || { echo "no such index file: $INDEX" >&2; exit 2; }
  DEB_PACKAGES=$(gzip -dc "$INDEX" | sed -n 's/^Package: //p' | sort -u)
else
  command -v curl >/dev/null 2>&1 || { echo "curl is required to fetch $SOURCES_URL" >&2; exit 2; }
  TMPIDX=$(mktemp "${TMPDIR:-/tmp}/xerj-sources-XXXXXX.gz")
  trap 'rm -f "$TMPIDX" 2>/dev/null' EXIT   # JOIN is not set yet; its trap comes below
  echo "fetching $SOURCES_URL" >&2
  curl -sSf -m 300 -o "$TMPIDX" "$SOURCES_URL"
  DEB_PACKAGES=$(gzip -dc "$TMPIDX" | sed -n 's/^Package: //p' | sort -u)
fi

[ -n "$DEB_PACKAGES" ] || { echo "no Package: entries parsed from the index — refusing to report an empty denominator" >&2; exit 2; }

# Membership via ONE awk pass over both lists, not a shell loop of per-crate
# greps. That is not a style choice: the loop form was measured giving
# run-to-run varying counts on an idle machine (sporadic short reads through
# 654 sequential `printf | grep` pipelines), and a measurement tool that
# reports a different number each run is worse than no tool. The awk join is
# deterministic and order-independent.
JOIN=$(mktemp "${TMPDIR:-/tmp}/xerj-coverage-XXXXXX")
trap 'rm -f "${TMPIDX:-}" "$JOIN" "$JOIN.pkgs" "$JOIN.crates" 2>/dev/null' EXIT
printf '%s\n' "$DEB_PACKAGES" > "$JOIN.pkgs"
printf '%s\n' "$CRATES" > "$JOIN.crates"
RESULT=$(awk '
  NR == FNR { pkgs[$0] = 1; next }
  {
    name = $0
    gsub(/_/, "-", name)
    if (pkgs["rust-" name]) print "P\t" $0; else print "M\t" $0
  }
' "$JOIN.pkgs" "$JOIN.crates")
PRESENT=$(printf '%s\n' "$RESULT" | grep -c '^P' | tr -d ' ')
MISSING=$(printf '%s\n' "$RESULT" | sed -n 's/^M\t//p')

MISSING_COUNT=$((TOTAL - PRESENT))
PCT=$(awk -v p="$PRESENT" -v t="$TOTAL" 'BEGIN { if (t == 0) print "n/a"; else printf "%.1f", 100 * p / t }')

if [ -n "$INDEX" ]; then
  INDEX_LINE="index: $INDEX (local copy)"
else
  INDEX_LINE="index: $SOURCES_URL"
fi

echo "third-party crates in $LOCK:            $TOTAL unique names"
echo "packaged in Debian unstable (rust-*):   $PRESENT ($PCT%)"
echo "NOT in Debian unstable:                 $MISSING_COUNT"
echo "$INDEX_LINE — $(date -u +%Y-%m-%d)"
echo "presence only; packaged versions were NOT compared against Cargo.lock pins"
if [ "$VERBOSE" = 1 ]; then
  printf 'missing: %s\n' "$MISSING" | tr ' ' '\n' | sed '/^$/d' | sed 's/^/  /'
fi
