#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# glibc-baseline.sh — fail a linux-gnu release binary whose GLIBC_* symbol
# versions are newer than the floor we promise.
#
# ubuntu-latest became Ubuntu 26.04 (glibc 2.43) in November 2026. A gnu
# binary linked there references GLIBC_2.43 (libm: the new versions of
# acosf/sqrtf/…) and does not start on Debian 13 (glibc 2.41) or Ubuntu
# 24.04 (glibc 2.39). The release workflow builds the gnu targets on
# Ubuntu 22.04, whose glibc is 2.35, and this script is the gate that the
# floor did not creep back up: glibc keeps old symbol versions, so a binary
# whose newest requirement is 2.35 runs on 2.35 and on everything newer
# (Debian 12, Ubuntu 22.04, Debian 13, Ubuntu 24.04/26.04).
#
# Usage:
#   scripts/glibc-baseline.sh --self-test
#   scripts/glibc-baseline.sh --max 2.35 path/to/xerj
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

die() { echo "glibc-baseline.sh: $*" >&2; exit 1; }

# True (exit 0) when version $1 is strictly newer than $2. `sort -V` so
# 2.9 sorts before 2.35.
version_gt() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" != "$1" ]
}

# Newest GLIBC_x.y token on stdin, without the GLIBC_ prefix. Empty when
# the input names none.
newest_glibc() {
  grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' | sed 's/^GLIBC_//' | sort -Vu | tail -n1
}

self_test() {
  local newest
  newest=$(printf '%s\n' 'memcpy@GLIBC_2.17' 'sqrtf@GLIBC_2.35' 'GLIBC_2.34' | newest_glibc)
  [ "$newest" = "2.35" ] || die "self-test: newest was ${newest:-<empty>}, expected 2.35"
  version_gt 2.43 2.35 || die "self-test: 2.43 should be newer than 2.35"
  if version_gt 2.35 2.35; then
    die "self-test: equal versions must not compare newer"
  fi
  if version_gt 2.35 2.39; then
    die "self-test: 2.35 must not compare newer than 2.39"
  fi
  if version_gt 2.9 2.35; then
    die "self-test: 2.9 must not compare newer than 2.35"
  fi
  echo "glibc-baseline.sh: self-test ok"
}

MAX=""
BIN=""
SELF_TEST=0
while [ $# -gt 0 ]; do
  case "$1" in
    --max) MAX="${2:-}"; shift ;;
    --self-test) SELF_TEST=1 ;;
    -h|--help)
      awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
      exit 0
      ;;
    *)
      if [ -n "$BIN" ]; then
        die "unexpected argument: $1"
      fi
      BIN="$1"
      ;;
  esac
  shift
done

if [ "$SELF_TEST" -eq 1 ]; then
  self_test
  [ -z "$BIN" ] && exit 0
fi

[ -n "$MAX" ] || die "--max VERSION is required (for example 2.35)"
[ -n "$BIN" ] || die "binary path is required"
[ -f "$BIN" ] || die "not a file: $BIN"
command -v objdump >/dev/null 2>&1 || die "objdump is not on PATH (binutils)"

newest=$(objdump -T "$BIN" 2>/dev/null | newest_glibc || true)
[ -n "$newest" ] || die "no GLIBC_* symbols in $BIN — objdump failed, or this is not a glibc ELF"
if version_gt "$newest" "$MAX"; then
  die "$BIN requires GLIBC_${newest}, newer than the ${MAX} baseline. Build the gnu release on an older glibc (Ubuntu 22.04) so it starts on Debian 12/13 and Ubuntu 22.04+"
fi
echo "glibc-baseline.sh: $BIN newest GLIBC_${newest} (baseline ${MAX})"
