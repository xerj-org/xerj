#!/usr/bin/env bash
# One-shot CI gate: boot xerj, seed, then run the deterministic checks.
#   1. smoke suite     (must be all-green / exit 0)
#   2. API liveness    (no 5xx across the read surface)
#   3. benchmark       (informational — prints throughput + latency)
#   4. index size      (informational — force-merged per-extension footprint)
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

echo "== 4. index size (informational) =="
# Force-merged 20k-doc pass with the DISK_SIZE_2026-07-09 corpus shape and a
# per-extension breakdown, so encoding changes land with before/after evidence
# in the PR logs. ci-test-profile numbers gate regressions only — they are
# never published cells (see benchmarks/index-size/README.md). Non-fatal for
# the same reason the benchmark is; a tmpfs WORK is refused by run.sh itself.
SIZE_WORK="$(mktemp -d)"
bash "$REPO/benchmarks/index-size/run.sh" \
  "$XERJ_BIN" "$SIZE_WORK" 9530 "${SIZE_DOCS:-20000}" || echo "(index-size non-fatal)"
rm -rf "$SIZE_WORK" 2>/dev/null || true

echo "== CI checks passed =="
