#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# query-latency-size0.sh — one-command reproduction of the query-latency
# headline ("server round-trip, size:0 over 300k docs, keep-alive client").
#
#   CLAIMED   0.126 ms average round-trip for a size:0 search over 300k docs
#             through Node's core http keep-alive client, against 1.61 ms for
#             the SAME request through Node's global fetch (undici) — the
#             ~1.5 ms is client overhead that swamps sub-millisecond server
#             times. demo/playbooks/SCORECARD.md methodology note and
#             demo/playbooks/bench-matrix.mjs:47-58; quoted on
#             landing/benchmarks/elasticsearch.html ("Server round-trip,
#             size:0 over 300k docs, keep-alive client: 0.126 ms, 2.2x").
#
# What this script does: starts a throwaway node on a PRIVATE port (93xx)
# with the query cache off (XERJ_DISABLE_QUERY_CACHE=1 — the scorecard's
# uncached-execution rule), indexes 300,000 synthetic telemetry docs, force-
# merges to one segment and settles 3 s (the scorecard's read-index-state
# rule), then drives the SAME size:0 body through BOTH clients with the
# harness's own open-loop pacer (fixed 200 req/s cadence, sleep+spin to the
# intended instant, hot event loop; service time + scheduling backlog, i.e.
# coordinated-omission corrected).
#
# Usage:
#   bash scripts/bench/query-latency-size0.sh
# Knobs: DOCS=300000 ITERS=1200 RATE=200 PORT=<free 93xx port> XERJ_BIN=... KEEP=1
# Requires: node >= 18, curl, python3, a built server binary.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/repro.sh
source "$HERE/lib/repro.sh"

DOCS="${DOCS:-300000}"
BATCH="${BATCH:-10000}"
ITERS="${ITERS:-1200}"
RATE="${RATE:-200}"
WARMUP="${WARMUP:-60}"
CORPUS="$REPO_ROOT/demo/data/extras/chat-events.ndjson"

repro_find_binary
repro_free_port 9322

repro_header "one-command repro: size:0 query latency over ${DOCS} docs"
repro_claim  "0.126 ms avg, size:0 over 300k, Node http keep-alive client (scorecard run 2026-07)"
repro_claim  "1.61 ms avg for the SAME request through Node's global fetch/undici — client overhead, not server time"
repro_note   "this run measures the XERJ side only; the 2.2x column needs ES (demo/playbooks/bench-matrix.mjs)"

if [ ! -f "$CORPUS" ]; then
  echo "corpus $CORPUS missing — run scripts/bench/ingest-docs-per-second.sh's generator or restore demo/data/extras/" >&2
  exit 1
fi

repro_node_start - XERJ_DISABLE_QUERY_CACHE=1
trap repro_node_stop EXIT
repro_node_wait

BASE="http://127.0.0.1:$REPRO_PORT"
IDX="bench-lat"

curl -sf -XDELETE "$BASE/$IDX" >/dev/null 2>&1 || true
curl -sf -XPUT "$BASE/$IDX" -H 'content-type: application/json' --data-binary @- >/dev/null <<'JSON'
{ "mappings": { "properties": {
  "@timestamp": { "type": "date" },
  "model": { "type": "keyword" }, "intent": { "type": "keyword" }, "status": { "type": "keyword" },
  "tenant": { "type": "keyword" }, "top_doc": { "type": "keyword" }, "cache_hit": { "type": "boolean" },
  "prompt_tokens": { "type": "integer" }, "context_tokens": { "type": "integer" }, "completion_tokens": { "type": "integer" },
  "latency_ms": { "type": "integer" }, "cost_usd": { "type": "double" }
} } }
JSON

# ── load the corpus with curl _bulk (same client the harness uses for load) ─
SCRATCH="$(mktemp -d /tmp/xerj-bench-lat.XXXXXX)"
trap 'rm -rf "$SCRATCH"; repro_node_stop' EXIT
python3 - "$CORPUS" "$SCRATCH" "$DOCS" "$BATCH" <<'PY'
import os, sys
corpus, scratch, total, batch = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
raw = open(corpus).read().splitlines()
n, written, b = len(raw), 0, 0
while written < total:
    lines = []
    for _ in range(min(batch, total - written)):
        lines.append('{"index":{}}'); lines.append(raw[written % n]); written += 1
    open(os.path.join(scratch, f"batch_{b:04d}.ndjson"), "w").write("\n".join(lines) + "\n")
    b += 1
PY
t0=$(date +%s)
for f in "$SCRATCH"/batch_*.ndjson; do
  curl -s -XPOST "$BASE/$IDX/_bulk" -H 'content-type: application/x-ndjson' --data-binary @"$f" >/dev/null
done
curl -sf -XPOST "$BASE/$IDX/_refresh" >/dev/null
count="$(curl -sf "$BASE/$IDX/_count" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')"
echo "indexed $count docs in $(( $(date +%s) - t0 ))s"

# read-index state rule from the harness: force-merge to one segment, settle 3s
curl -sf -XPOST "$BASE/$IDX/_forcemerge?max_num_segments=1" >/dev/null
sleep 3

# ── the measurement: same body through BOTH clients, harness pacer ─────────
RESULT="$(node - "$BASE" "$IDX" "$ITERS" "$RATE" "$WARMUP" <<'NODE'
// Mirrors demo/playbooks/bench-matrix.mjs timed(): open-loop fixed cadence,
// sleep+spin pacer (SPIN_MS=1.5) to the intended instant, hot event loop so
// responses are processed the moment they arrive, latency = service time +
// scheduling backlog (end - min(intended, actualStart)).
const http = require('node:http');
const [base, idx, ITERS, RATE, WARMUP] = [process.argv[2], process.argv[3], +process.argv[4], +process.argv[5], +process.argv[6]];
const SPIN_MS = 1.5;
const PATH = `/${idx}/_search?request_cache=false`;
const BODY = JSON.stringify({ query: { match_all: {} }, size: 0, track_total_hits: true });

const agent = new http.Agent({ keepAlive: true, maxSockets: 256 });
// A non-2xx answer is not a latency sample: an error page is fast and would
// flatter the number (#2075 item 11). Any failed request stops the run.
const ok = (client, status) => {
  if (status < 200 || status > 299) throw new Error(`${client}: POST ${PATH} -> HTTP ${status}; failed requests are not latency samples, nothing measured`);
  return status;
};
const keepAliveReq = () => new Promise((resolve, reject) => {
  const r = http.request({ hostname: '127.0.0.1', port: +base.split(':')[2], path: PATH,
    method: 'POST', headers: { 'content-type': 'application/json', 'content-length': Buffer.byteLength(BODY) }, agent },
    (res) => {
      let n = 0; res.on('data', (c) => { n += c.length; });
      res.on('end', () => { try { resolve(ok('keep-alive', res.statusCode)); } catch (e) { reject(e); } });
    });
  r.on('error', reject); r.end(BODY);
});
const fetchReq = async () => { const r = await fetch(base + PATH, { method: 'POST', headers: { 'content-type': 'application/json' }, body: BODY }); await r.text(); return ok('fetch', r.status); };

function pct(a, p) { const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))]; }

async function measure(send) {
  for (let i = 0; i < WARMUP; i++) await send();
  const lat = new Array(ITERS); const tasks = new Array(ITERS);
  let hot = true; (function loop() { if (hot) setImmediate(loop); })();
  const t0 = performance.now();
  for (let i = 0; i < ITERS; i++) {
    const intended = t0 + (i / RATE) * 1000;
    tasks[i] = (async () => {
      const wait = intended - performance.now();
      if (wait > SPIN_MS) await new Promise((r) => setTimeout(r, wait - SPIN_MS));
      while (performance.now() < intended) { /* spin */ }
      const start = performance.now();
      await send();
      lat[i] = performance.now() - Math.min(intended, start);
    })();
  }
  await Promise.all(tasks); hot = false;
  const mean = lat.reduce((a, b) => a + b, 0) / lat.length;
  return { mean, p50: pct(lat, 50), p99: pct(lat, 99), max: Math.max(...lat) };
}

(async () => {
  const ka = await measure(keepAliveReq);
  const fe = await measure(fetchReq);
  agent.destroy();
  const out = {
    keepalive: ka, fetch: fe, iters: ITERS, rate: RATE,
    ratio_fetch_over_keepalive: +(fe.mean / ka.mean).toFixed(2),
  };
  console.log(JSON.stringify(out));
})().catch((e) => { console.error(String(e)); process.exit(1); });
NODE
)"

RESULT_FILE="$SCRATCH/latency.json"
printf '%s\n' "$RESULT" >"$RESULT_FILE"
python3 - "$RESULT_FILE" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
ka, fe = r["keepalive"], r["fetch"]
print()
print(f'  MEASURED  keep-alive client : {ka["mean"]:.3f} ms avg (p50 {ka["p50"]:.3f} / p99 {ka["p99"]:.3f} / max {ka["max"]:.3f})')
print(f'  MEASURED  fetch/undici      : {fe["mean"]:.3f} ms avg (p50 {fe["p50"]:.3f} / p99 {fe["p99"]:.3f} / max {fe["max"]:.3f})')
print(f'  MEASURED  client overhead   : {fe["mean"] - ka["mean"]:.3f} ms ({r["ratio_fetch_over_keepalive"]}x) — the published point: the client, not the server')
print(f'  note      {r["iters"]} timed iterations at {r["rate"]} req/s open-loop, request_cache=false, query cache off, 1 merged segment')
print('  note      absolute ms is hardware-bound — THIS box is slower than the scorecard box; what')
print('            reproduces anywhere is the ordering and gap (keep-alive << fetch), not the exact value')
PY
repro_env_box
repro_note "teardown: node stopped, throwaway data dir removed (KEEP=1 to keep)"