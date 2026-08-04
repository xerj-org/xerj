#!/usr/bin/env bash
# End-to-end eval of `xerj autoindex` against a corpus folder.
#
# Boots a dedicated server on es_compat $PORT (rest PORT+100, grpc PORT+101),
# runs autoindex over $CORPUS, then re-runs twice to prove idempotency — the
# resume path (all files done) and a full --fresh re-extract — and requires the
# per-index doc counts to be byte-identical to run 1. Ends with the data map.
# The shared autoindex-catalog is excluded from the comparison: every run
# appends its own run record, so a growing catalog is the design, not drift.
#
# Nothing here is bound to one machine: every path is an env override with a
# default under the repo or $TMPDIR, so the same script gates CI and drives a
# local eval over a large corpus.
#
# Usage: run-eval.sh [corpus-dir] [binary]
#
# Env overrides:
#   XERJ_AUTOINDEX_CORPUS  corpus folder   (default ${TMPDIR:-/tmp}/xerj-discover/corpus)
#   XERJ_BIN               xerj binary     (default <repo>/engine/target/release/xerj)
#   XERJ_AUTOINDEX_PORT    es-compat port  (default 9260)
#   XERJ_AUTOINDEX_WORK    work dir        (default ${TMPDIR:-/tmp}/xerj-autoindex)
#
# Exits non-zero if autoindex fails or idempotency breaks.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"

CORPUS="${1:-${XERJ_AUTOINDEX_CORPUS:-${TMPDIR:-/tmp}/xerj-discover/corpus}}"
BIN="${2:-${XERJ_BIN:-$REPO/engine/target/release/xerj}}"
PORT="${XERJ_AUTOINDEX_PORT:-9260}"
WORK="${XERJ_AUTOINDEX_WORK:-${TMPDIR:-/tmp}/xerj-autoindex}"
DATA="$WORK/data-build"
CFG="$WORK/server-$PORT.toml"
LOG="$WORK/server-$PORT.log"
URL="http://localhost:$PORT"
STATE="$WORK/state"
PREFIX="ax-eval"
BRAIN="eval"

[ -x "$BIN" ] || { echo "xerj binary not found at $BIN"; exit 1; }
[ -d "$CORPUS" ] || { echo "corpus folder not found at $CORPUS"; exit 1; }
mkdir -p "$WORK"
cat >"$CFG" <<EOF
[server]
rest_port = $((PORT + 100))
grpc_port = $((PORT + 101))
es_compat_port = $PORT
bind_address = "127.0.0.1"
data_dir = "$DATA"

[auth]
enabled = false
EOF

if ! curl -sf "$URL/" >/dev/null 2>&1; then
  mkdir -p "$DATA"
  "$BIN" --insecure --config "$CFG" --data-dir "$DATA" >"$LOG" 2>&1 &
  # Recorded so a caller (CI) can stop the server it made us start.
  echo $! > "$WORK/server.pid"
  echo "server pid $!"
  for _ in $(seq 1 60); do
    curl -sf "$URL/" >/dev/null 2>&1 && break
    sleep 0.5
  done
fi
curl -sf "$URL/" >/dev/null || { echo "server failed to boot"; tail -20 "$LOG"; exit 1; }

FAIL=0

echo "=== autoindex run ($CORPUS) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" --prefix "$PREFIX" --brain "$BRAIN"
RC=$?
echo "exit code: $RC"
# 0 complete, 3 completed-with-junk (unreadable files recorded, never fatal).
if [ "$RC" != "0" ] && [ "$RC" != "3" ]; then
  echo "autoindex exited $RC (expected 0 or 3)"
  FAIL=1
fi

# Name + doc count only. The _cat row also carries on-disk size, which a
# --fresh re-extract legitimately changes (segments are rewritten), and the
# catalog is excluded outright because every run appends its own run record —
# a growing catalog is the design, not drift. Columns of the _cat row:
# green(1) open(2) name(3) uuid(4) pri(5) rep(6) docs(7).
counts() { curl -s "$URL/_cat/indices" | awk -v prefix="$1-" '$3 ~ ("^" prefix) {sub("^" prefix, "", $3); print $3, $7}' | sort; }

echo "=== per-index counts (run 1) ==="
curl -s "$URL/_cat/indices" | grep -E 'ax-|autoindex-catalog' | sort
counts "$PREFIX" > "$WORK/counts-run1.txt"

echo "=== idempotency: re-run (resume path — all files done) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" --prefix "$PREFIX" --brain "$BRAIN"
RC=$?
if [ "$RC" != "0" ] && [ "$RC" != "3" ]; then
  echo "resume run exited $RC (expected 0 or 3)"
  FAIL=1
fi

echo "=== idempotency: re-run with --fresh (full re-extract, idempotent ids) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" --prefix "$PREFIX" --brain "$BRAIN" --fresh
RC=$?
if [ "$RC" != "0" ] && [ "$RC" != "3" ]; then
  echo "--fresh re-extract exited $RC (expected 0 or 3)"
  FAIL=1
fi

counts "$PREFIX" > "$WORK/counts-run3.txt"
echo "=== doc-count diff run1 vs run3 (must be empty) ==="
cat "$WORK/counts-run3.txt"
if diff "$WORK/counts-run1.txt" "$WORK/counts-run3.txt"; then
  echo "IDENTICAL COUNTS ✓"
else
  echo "doc counts drifted across re-runs — autoindex is not idempotent"
  FAIL=1
fi

echo "=== data map ==="
# Captured, then headed — piping straight into `head` closes stdout under the
# writer, and the map aborts on the resulting EPIPE (a core dump that hides
# whether `map` itself worked).
"$BIN" autoindex map --url "$URL" > "$WORK/map.md"
MAP_RC=$?
head -80 "$WORK/map.md"
if [ "$MAP_RC" != "0" ]; then
  echo "autoindex map exited $MAP_RC"
  FAIL=1
fi

[ "$FAIL" = 0 ] && echo "RESULT: PASS" || echo "RESULT: FAIL"
exit "$FAIL"
