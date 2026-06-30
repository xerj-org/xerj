#!/usr/bin/env bash
# One-shot CI gate: boot xerj, seed, then run the deterministic checks.
#   1. smoke suite     (must be all-green / exit 0)
#   2. API liveness    (no 5xx across the read surface)
#   3. benchmark       (informational — prints throughput + latency)
#
# Expects a built binary. Set XERJ_BIN to override (default: engine release build).
# No model / LLM needed — safe to run in GitHub Actions.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
XERJ_BIN="${XERJ_BIN:-$REPO/engine/target/release/xerj}"
DATA="$(mktemp -d)"
CORPUS_SRC="$REPO/demo/data/extras/chat-events.ndjson"
PORT_ES=9200

cleanup() {
  kill "${XERJ_PID:-}" 2>/dev/null || true
  wait "${XERJ_PID:-}" 2>/dev/null || true   # let xerj release the data dir
  rm -rf "$DATA" 2>/dev/null || true          # never let cleanup fail the gate
}
trap cleanup EXIT

echo "== boot xerj =="
"$XERJ_BIN" --insecure --data-dir "$DATA" >"$DATA/server.log" 2>&1 &
XERJ_PID=$!
for _ in $(seq 1 80); do
  curl -fs -m1 "localhost:$PORT_ES/_cluster/health" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -fs -m2 "localhost:$PORT_ES/_cluster/health" >/dev/null

echo "== seed bench index =="
awk '{print "{\"index\":{\"_index\":\"bench\"}}"; print}' "$CORPUS_SRC" > "$DATA/bulk.ndjson"
curl -fs -XPOST "localhost:$PORT_ES/_bulk" -H 'content-type: application/x-ndjson' --data-binary @"$DATA/bulk.ndjson" >/dev/null
curl -fs -XPOST "localhost:$PORT_ES/bench/_refresh" >/dev/null

echo "== 1. smoke suite =="
node "$HERE/run.mjs"

echo "== 2. API liveness =="
node "$HERE/liveness.mjs"

echo "== 3. benchmark (informational) =="
node "$HERE/bench.mjs" "${BENCH_DOCS:-100000}" || echo "(benchmark non-fatal)"

echo "== CI checks passed =="
