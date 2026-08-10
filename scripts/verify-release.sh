#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# verify-release.sh — post-release check on the PUBLISHED artifacts.
#
# Not a build check. This downloads what a user actually downloads and asserts
# the things a green CI run cannot: that every archive has a checksum and
# matches it, that every binary reports the version its tag promises, and that
# the binary for this host boots on a clean data dir and answers a real search.
#
# It exists because v1.0.0-rc.10 shipped binaries that print "xerj v1.0.0-rc.9".
# The tag was cut at a commit where engine/Cargo.toml still held the old
# version: asset FILENAMES come from the git tag, the banner comes from
# CARGO_PKG_VERSION, and nothing made the two agree. Every test passed. The
# only way to see it is to run the artifact. Still reproducible today:
#
#   scripts/verify-release.sh v1.0.0-rc.10   ->  FAIL (version drift)
#   scripts/verify-release.sh v1.0.0-rc.13   ->  PASS
#
# Checks, in order:
#   1. every target we ship is present — the set is DECLARED below, not read off
#      the release page, so a release that lost a matrix leg fails instead of
#      quietly verifying the targets that did make it
#   2. every archive has a .sha256 companion, and matches it
#   3. every archive contains the binary + LICENSE + README. An archive that
#      matched its published checksum and still will not extract is a defect in
#      the release, so it FAILS rather than being skipped
#   4. every binary — including the ones this host cannot execute — carries the
#      tag's version in its startup banner, and carries no other version
#   5. the host-native binary: --version, boot on a clean data dir, health
#      green, index a doc, search it back, run a terms aggregation. Runs on
#      Linux and macOS hosts (both architectures). It is NOT run on a Windows
#      host — those archives are checksum- and version-checked like every
#      other target, but this script never boots the .exe.
#
# Anything this run could not check is counted and reported, never silently
# dropped, and the run then exits non-zero — a partially-checked release is not
# a verified release. That covers a target this host cannot unpack (no unzip
# for the Windows .zip) and a check-5 smoke (printed as "step 6" at run time)
# that never executed because this host has no runnable artifact. The one
# narrowing that still exits 0 is --no-smoke, because the operator asked for it.
#
# Usage:
#   scripts/verify-release.sh                  # latest release
#   scripts/verify-release.sh v1.0.0-rc.13     # a specific tag
#   scripts/verify-release.sh --keep           # keep the download dir
#   scripts/verify-release.sh --no-smoke       # checksums + versions only
#
# Requires: gh (authenticated), curl, tar, sha256sum|shasum, and unzip for the
# Windows archives.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="${XERJ_REPO:-xerj-org/xerj}"
TAG=""
KEEP=0
DO_SMOKE=1
WORKDIR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --keep)     KEEP=1 ;;
    --no-smoke) DO_SMOKE=0 ;;
    --repo)     REPO="$2"; shift ;;
    -h|--help)  awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
    -*)         echo "unknown flag: $1" >&2; exit 2 ;;
    *)          TAG="$1" ;;
  esac
  shift
done

BOLD=''; DIM=''; RED=''; GRN=''; YEL=''; RST=''
if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
  GRN=$(printf '\033[32m'); YEL=$(printf '\033[33m'); RST=$(printf '\033[0m')
fi

# Every target .github/workflows/release.yml publishes (its `matrix.include`).
# Declared, not discovered. A gate that verifies whatever the release page
# happens to hold reports a clean PASS for a release that is missing platforms:
# delete the two windows-msvc archives from a release and the rest of this
# script goes 6-for-6 green. Keep this list in step with release.yml — if the
# matrix gains or loses a target, this is the other half of that change.
EXPECTED_TARGETS="aarch64-apple-darwin
aarch64-pc-windows-msvc
aarch64-unknown-linux-gnu
aarch64-unknown-linux-musl
x86_64-apple-darwin
x86_64-pc-windows-msvc
x86_64-unknown-linux-gnu
x86_64-unknown-linux-musl"
TOTAL_TARGETS=$(printf '%s\n' "$EXPECTED_TARGETS" | wc -l | tr -d ' ')

FAILURES=0
# Targets that were present but could not be checked (archive not unpackable on
# this host). Counted separately from failures: not a defect in the release, but
# not a verification either, and the verdict must never round it up to PASS.
SKIPPED_TARGETS=0
# Set when step 6 did not execute anything for a reason the operator did NOT
# ask for (no runnable artifact for this host). --no-smoke does not set it.
SKIPPED_SMOKE=0
# Archives that were present and checksum-clean but would not extract. Already
# counted in FAILURES at step 4; tracked so step 5 does not ALSO count them as
# skipped targets and hand the operator the wrong remedy.
UNPACKABLE=""
pass() { printf '  %sPASS%s  %s\n' "$GRN" "$RST" "$1"; }
fail() { printf '  %sFAIL%s  %s\n' "$RED" "$RST" "$1"; FAILURES=$((FAILURES + 1)); }
warn() { printf '  %sSKIP%s  %s\n' "$YEL" "$RST" "$1"; }
step() { printf '\n%s%s%s\n' "$BOLD" "$1" "$RST"; }

cleanup() {
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null || true
  if [ "$KEEP" = 0 ] && [ -n "$WORKDIR" ] && [ -d "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  elif [ -n "$WORKDIR" ]; then
    printf '\n%skept: %s%s\n' "$DIM" "$WORKDIR" "$RST"
  fi
}
trap cleanup EXIT

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum   >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else echo "no sha256sum or shasum on PATH" >&2; exit 2
  fi
}

# The version a binary claims, read from its startup banner. Works on binaries
# this host cannot execute, which is the whole point — the drift we are hunting
# can just as easily land in the macOS or Windows artifact, and executing them
# is not an option on a Linux runner.
banner_versions() {
  LC_ALL=C grep -a -o -E 'xerj v[0-9][0-9A-Za-z.+-]* starting' "$1" 2>/dev/null \
    | sed 's/^xerj v//; s/ starting$//' | sort -u
}

free_port() {
  python3 - <<'PY' 2>/dev/null || echo "$((20000 + RANDOM % 20000))"
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

command -v gh >/dev/null 2>&1 || { echo "gh is required" >&2; exit 2; }

# ── resolve the tag ──────────────────────────────────────────────────────────
if [ -z "$TAG" ]; then
  TAG=$(gh release view --repo "$REPO" --json tagName --jq .tagName)
fi
EXPECTED="${TAG#v}"

printf '%srelease verification%s  %s  tag %s%s%s\n' "$BOLD" "$RST" "$REPO" "$BOLD" "$TAG" "$RST"

WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/xerj-verify-XXXXXX")
cd "$WORKDIR"

step "1. download every published asset"
gh release download "$TAG" --repo "$REPO" --dir "$WORKDIR" --clobber >/dev/null
ARCHIVES=$(find . -maxdepth 1 -type f \( -name '*.tar.gz' -o -name '*.zip' \) | sed 's|^\./||' | sort)
[ -n "$ARCHIVES" ] || { fail "no .tar.gz or .zip assets on $TAG"; exit 1; }
printf '  %s%s archives%s\n' "$DIM" "$(printf '%s\n' "$ARCHIVES" | wc -l | tr -d ' ')" "$RST"

step "2. every target we ship is present"
# Asserted against EXPECTED_TARGETS, never inferred from what was downloaded:
# one failed leg of the release matrix must fail this gate, not shrink it.
for t in $EXPECTED_TARGETS; do
  if printf '%s\n' "$ARCHIVES" | grep -qE -- "-${t}\.(tar\.gz|zip)\$"; then
    pass "$t"
  else
    fail "$t — NO archive published for this target"
  fi
done

step "3. checksums"
for a in $ARCHIVES; do
  if [ ! -f "$a.sha256" ]; then
    fail "$a — no .sha256 companion published"
    continue
  fi
  want=$(cut -d' ' -f1 < "$a.sha256")
  got=$(sha256_of "$a")
  if [ "$want" = "$got" ]; then
    pass "$a  ${got:0:16}…"
  else
    fail "$a — published $want, downloaded $got"
  fi
done

step "4. archive contents"
for a in $ARCHIVES; do
  d="unpack/${a%.tar.gz}"; d="${d%.zip}"
  mkdir -p "$d"
  # An archive that matched its published checksum and still will not extract is
  # a defect in the RELEASE, not a gap in this host's tooling — so it is a FAIL,
  # not a SKIP. Guarding the extractor also keeps `set -e` from ending the run
  # right here with the extractor's own error and no verdict at all, which is
  # the one way this script could stop short of the promise in its header that
  # everything it could not check is counted and reported.
  case "$a" in
    *.tar.gz)
      if ! tar -xzf "$a" -C "$d"; then
        fail "$a — matched its published checksum but did not extract"
        UNPACKABLE="$UNPACKABLE $a"
        continue
      fi ;;
    *.zip)
      if ! command -v unzip >/dev/null 2>&1; then
        warn "$a — unzip not on PATH, contents NOT checked"; continue
      elif ! unzip -q -o "$a" -d "$d"; then
        fail "$a — matched its published checksum but did not extract"
        UNPACKABLE="$UNPACKABLE $a"
        continue
      fi ;;
  esac
  # `|| true`: under `set -o pipefail`, find killed by SIGPIPE once head exits
  # returns 141 for the whole pipeline and `set -e` would abort the verifier
  # mid-run rather than report anything.
  bin=$(find "$d" -type f \( -name xerj -o -name xerj.exe \) | head -1 || true)
  missing=""
  [ -n "$bin" ] || missing="$missing binary"
  [ -n "$(find "$d" -type f -name LICENSE  | head -1 || true)" ] || missing="$missing LICENSE"
  [ -n "$(find "$d" -type f -name 'README*' | head -1 || true)" ] || missing="$missing README"
  if [ -n "$missing" ]; then fail "$a — missing:$missing"; else pass "$a  binary + LICENSE + README"; fi
done

step "5. version string matches the tag, on every target"
# The rc.10 check. A binary whose banner disagrees with its own filename is the
# defect; a binary carrying two different versions is a stale-artifact defect.
for a in $ARCHIVES; do
  d="unpack/${a%.tar.gz}"; d="${d%.zip}"
  target=$(printf '%s' "$a" | sed "s/^xerj-$EXPECTED-//; s/\.tar\.gz$//; s/\.zip$//")
  bin=$(find "$d" -type f \( -name xerj -o -name xerj.exe \) 2>/dev/null | head -1 || true)
  if [ -z "$bin" ]; then
    case " $UNPACKABLE " in
      *" $a "*)
        # Step 4 already counted this one as a FAIL: the archive is broken, and
        # the run is already failing closed for the right reason. Counting it a
        # second time as a skipped target would add the "install unzip" remedy
        # to the verdict, which is the wrong advice for a defective release.
        warn "$target — version NOT checked (archive did not extract, see step 4)" ;;
      *)
        # Step 4 could not unpack this archive (typically: no unzip for a
        # Windows .zip). Dropping it here would leave this section headed "on
        # every target" quietly checking fewer targets than it names.
        warn "$target — no binary unpacked, version NOT checked"
        SKIPPED_TARGETS=$((SKIPPED_TARGETS + 1)) ;;
    esac
    continue
  fi
  found=$(banner_versions "$bin" | tr '\n' ' ' | sed 's/ $//')
  if [ -z "$found" ]; then
    fail "$target — no version banner found in the binary"
  elif [ "$found" = "$EXPECTED" ]; then
    pass "$target  reports $EXPECTED"
  else
    fail "$target — tag says $EXPECTED, binary says: $found"
  fi
done

# ── 6. run the one we can actually run ───────────────────────────────────────
HOST_OS=$(uname -s); HOST_ARCH=$(uname -m); HOST_ARCH_RAW="$HOST_ARCH"
# `uname -m` and the Rust target triple disagree about the same CPU: an Apple
# Silicon Mac says `arm64` where the artifact is named `aarch64-apple-darwin`.
# Without this normalization the glob matched zero files on exactly the host a
# maintainer is most likely to verify a release from, step 6 was skipped, and
# the run still printed PASS — "everything green, nobody ran the artifact",
# which is the rc.10 shape this script exists to catch.
case "$HOST_ARCH" in
  arm64|armv8b|armv8l) HOST_ARCH=aarch64 ;;
  amd64)               HOST_ARCH=x86_64 ;;
esac
case "$HOST_OS" in
  Linux)  host_glob="*${HOST_ARCH}-unknown-linux-*" ;;
  Darwin) host_glob="*${HOST_ARCH}-apple-darwin*" ;;
  # Windows (MSYS/MinGW/Cygwin) and everything else: the .zip is checksum- and
  # version-checked like every other target, but this script does not boot the
  # .exe, so there is no host binary to run here.
  *)      host_glob="" ;;
esac

HOST_BIN=""
if [ -n "$host_glob" ]; then
  # shellcheck disable=SC2086
  HOST_BIN=$(find unpack -type f -name xerj -path "$host_glob" 2>/dev/null | head -1 || true)
fi

# Whether a live search was actually executed. The verdict line must not claim
# it when it did not happen: --no-smoke leaves this 0 and still passes, any
# other reason leaves it 0 and sets SKIPPED_SMOKE, which fails the run.
SMOKED=0

step "6. run the host-native artifact"
if [ "$DO_SMOKE" = 0 ]; then
  # The operator asked for this narrowing, so it does not fail the run — but
  # the verdict below still refuses to claim a live search it never ran.
  warn "--no-smoke — no binary was executed"
elif [ -z "$HOST_BIN" ]; then
  # Not "no artifact for this platform": on a Windows host the archive for it
  # was published, downloaded and version-checked by this very run. Say which
  # of the two actually happened, and count it — nobody asked for it.
  if [ -z "$host_glob" ]; then
    warn "this script does not boot a release binary on $HOST_OS/$HOST_ARCH_RAW (Linux and macOS hosts only) — the artifacts for it, if any, were still checksum- and version-checked above"
  else
    warn "no unpacked binary matched $host_glob — the archive for this host was not unpacked (see step 4), so nothing was executed"
  fi
  SKIPPED_SMOKE=1
else
  SMOKED=1
  chmod +x "$HOST_BIN"
  reported=$("$HOST_BIN" --version 2>&1 || true)
  if [ "$reported" = "xerj v$EXPECTED" ]; then
    pass "--version  ->  $reported"
  else
    fail "--version  ->  '$reported', expected 'xerj v$EXPECTED'"
  fi

  ES_PORT=$(free_port); REST_PORT=$(free_port); GRPC_PORT=$(free_port)
  mkdir -p smoke/data
  cat > smoke/xerj.toml <<EOF
[server]
rest_port = $REST_PORT
grpc_port = $GRPC_PORT
es_compat_port = $ES_PORT
bind_address = "127.0.0.1"
EOF
  "$HOST_BIN" --config "$WORKDIR/smoke/xerj.toml" --data-dir "$WORKDIR/smoke/data" \
      --insecure > "$WORKDIR/smoke/server.log" 2>&1 &
  SERVER_PID=$!

  B="http://127.0.0.1:$ES_PORT"
  health=""
  for _ in $(seq 1 60); do
    health=$(curl -s -m 2 "$B/_cluster/health" 2>/dev/null || true)
    [ -n "$health" ] && break
    kill -0 "$SERVER_PID" 2>/dev/null || break
    sleep 1
  done

  if [ -z "$health" ]; then
    fail "server never answered on :$ES_PORT — see $WORKDIR/smoke/server.log"
  else
    case "$health" in
      *'"status":"green"'*) pass "boots on a clean data dir, cluster green" ;;
      *) fail "cluster not green: $health" ;;
    esac

    curl -s -X POST "$B/xerj-verify/_doc/1?refresh=true" \
      -H 'Content-Type: application/json' \
      -d '{"title":"release verification","body":"published artifact booted and searched"}' \
      > smoke/index.json 2>&1 || true
    case "$(cat smoke/index.json)" in
      *'"result":"created"'*) pass "indexes a document" ;;
      *) fail "index failed: $(cat smoke/index.json)" ;;
    esac

    curl -s -X POST "$B/xerj-verify/_search" -H 'Content-Type: application/json' \
      -d '{"query":{"match":{"body":"published artifact"}}}' > smoke/search.json 2>&1 || true
    case "$(cat smoke/search.json)" in
      *'"total":{"value":1'*) pass "searches it back  ($(sed 's/.*"max_score":\([0-9.]*\).*/max_score \1/' smoke/search.json))" ;;
      *) fail "search returned no hit: $(head -c 300 smoke/search.json)" ;;
    esac

    curl -s -X POST "$B/xerj-verify/_search" -H 'Content-Type: application/json' \
      -d '{"size":0,"aggs":{"t":{"terms":{"field":"title.keyword"}}}}' > smoke/agg.json 2>&1 || true
    case "$(cat smoke/agg.json)" in
      *'"key":"release verification","doc_count":1'*) pass "terms aggregation buckets it" ;;
      *) fail "aggregation wrong: $(head -c 300 smoke/agg.json)" ;;
    esac
  fi

  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
fi

step "verdict"
# PASS is only ever printed for a release where every declared target was
# actually checked AND the host-native binary actually ran. --no-smoke narrows
# the verdict text because the operator asked for it; a skipped target, or a
# step 6 that silently never executed, is not something anyone asked for, so it
# fails closed rather than being rounded up into a PASS this run did not earn.
if [ "$FAILURES" -eq 0 ] && [ "$SKIPPED_TARGETS" -eq 0 ] && [ "$SKIPPED_SMOKE" -eq 0 ]; then
  if [ "$SMOKED" = 1 ]; then
    scope="checksums and versions verified on $TOTAL_TARGETS/$TOTAL_TARGETS targets, plus a live search"
  else
    scope="checksums and versions verified on $TOTAL_TARGETS/$TOTAL_TARGETS targets — NO binary was executed (--no-smoke), so nothing here says it runs"
  fi
  printf '  %sPASS%s  %s: %s\n' "$GRN" "$RST" "$TAG" "$scope"
  exit 0
fi

if [ "$SKIPPED_SMOKE" -gt 0 ]; then
  printf '  %sFAIL%s  %s: nothing was executed — no binary ran on this host and nobody asked for that (see SKIP at step 6); re-run on a host this release ships a runnable binary for, or pass --no-smoke to accept a checksums-and-versions-only check\n' \
    "$RED" "$RST" "$TAG"
fi
if [ "$SKIPPED_TARGETS" -gt 0 ]; then
  printf '  %sFAIL%s  %s: %s of %s target(s) NOT verified (see SKIP above) — install the missing tool (unzip, for the Windows .zip archives) and re-run; do not announce a partially checked release\n' \
    "$RED" "$RST" "$TAG" "$SKIPPED_TARGETS" "$TOTAL_TARGETS"
fi
if [ "$FAILURES" -gt 0 ]; then
  printf '  %sFAIL%s  %s: %s check(s) failed — do not announce this release\n' "$RED" "$RST" "$TAG" "$FAILURES"
fi
exit 1
