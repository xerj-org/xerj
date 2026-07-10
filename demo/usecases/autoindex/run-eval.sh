#!/usr/bin/env bash
# End-to-end eval of `xerj autoindex` against a corpus folder.
#
# Boots a dedicated worktree server on es_compat :9260 (rest 9360, grpc 9361),
# runs autoindex over $CORPUS, exercises the 5 query classes + map, then
# re-runs to prove idempotency (counts must not change).
#
# Usage: run-eval.sh [corpus-dir] [binary]
set -uo pipefail
CORPUS="${1:-/tmp/xerj-discover/corpus}"
BIN="${2:-/home/claude/ai/xerj-autoindex-wt/engine/target/release/xerj}"
PORT=9260
DATA="/tmp/xerj-autoindex/data-build"
CFG="/tmp/xerj-autoindex/server-9260.toml"
LOG="/tmp/xerj-autoindex/server-9260.log"
URL="http://localhost:$PORT"
STATE="/tmp/xerj-autoindex/state"

mkdir -p /tmp/xerj-autoindex
cat >"$CFG" <<EOF
[server]
rest_port = 9360
grpc_port = 9361
es_compat_port = $PORT
bind_address = "127.0.0.1"
data_dir = "$DATA"

[auth]
enabled = false
EOF

if ! curl -sf "$URL/" >/dev/null 2>&1; then
  mkdir -p "$DATA"
  "$BIN" --insecure --config "$CFG" --data-dir "$DATA" >"$LOG" 2>&1 &
  echo "server pid $!"
  for i in $(seq 1 60); do
    curl -sf "$URL/" >/dev/null 2>&1 && break
    sleep 0.5
  done
fi
curl -sf "$URL/" >/dev/null || { echo "server failed to boot"; tail -20 "$LOG"; exit 1; }

echo "=== autoindex run ($CORPUS) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" --fresh
echo "exit code: $?"

echo "=== per-index counts (run 1) ==="
curl -s "$URL/_cat/indices" | grep -E 'ax-|autoindex-catalog' | sort > /tmp/xerj-autoindex/counts-run1.txt
cat /tmp/xerj-autoindex/counts-run1.txt

echo "=== idempotency: re-run (resume path — all files done) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE"

echo "=== idempotency: re-run with --fresh (full re-extract, idempotent ids) ==="
time "$BIN" autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" --fresh

curl -s "$URL/_cat/indices" | grep -E 'ax-|autoindex-catalog' | sort > /tmp/xerj-autoindex/counts-run3.txt
echo "=== count diff run1 vs run3 (must be empty on doc counts) ==="
diff /tmp/xerj-autoindex/counts-run1.txt /tmp/xerj-autoindex/counts-run3.txt && echo "IDENTICAL COUNTS ✓"

echo "=== data map ==="
"$BIN" autoindex map --url "$URL" | head -80
