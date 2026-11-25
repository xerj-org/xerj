#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# knn-recall.sh — one-command reproduction of the kNN recall headline.
#
#   CLAIMED   unfiltered kNN on a full-precision cosine field (>=1,024 docs)
#             is HNSW-served with exact rescoring: "measured recall@10 1.00 on
#             the official bench query, 100-probe mean 0.976" — docs/WHY_XERJ.md:32,
#             ROADMAP.md:23, landing/docs/vectors.html, quoted with ES 8.13.4
#             at 0.937 on the same protocol.
#
# What this script does: starts a throwaway node on a PRIVATE port (93xx),
# indexes 50,000 generated 128-dim vectors (mulberry32 seed 0xC0FFEE, explicit
# ids — the exact generator and shape of demo/playbooks/bench-matrix.mjs
# knnBench), checks GET /_cat/ann so a silent fallback to the exact scan
# cannot pass for the HNSW path, then runs 100 probe queries of k=10,
# num_candidates=100 and scores each against a client-side brute-force cosine
# top-10 ground truth. Recall is MEASURED, never assumed — that is the whole
# point of the claim.
#
# It also verifies the second half of the claim — candidates are exact-
# rescored, so returned scores match the exact path — by comparing every
# engine _score against the client-side (1 + cos)/2 of the same document.
#
# Usage:
#   bash scripts/bench/knn-recall.sh
# Knobs: VECS=50000 DIMS=128 PROBES=100 K=10 CANDIDATES=100 PORT=<free 93xx port>
#         XERJ_BIN=<binary> KEEP=1
#         ANN_WAIT_S=120 (seconds to wait for GET /_cat/ann to report a covering,
#         non-stale graph; the run refuses to score recall if it never does)
# Requires: node >= 18, curl, a built server binary.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/repro.sh
source "$HERE/lib/repro.sh"

VECS="${VECS:-50000}"
DIMS="${DIMS:-128}"
PROBES="${PROBES:-100}"
K="${K:-10}"
CANDIDATES="${CANDIDATES:-100}"
ANN_WAIT_S="${ANN_WAIT_S:-120}"

repro_find_binary
repro_free_port 9334

repro_header "one-command repro: kNN recall@${K} (HNSW-served, exact-rescored)"
repro_claim  "recall@10 = 1.00 on the official bench query; 100-probe mean 0.976 / min 0.90 (docs/WHY_XERJ.md:32)"
repro_claim  "candidates exact-rescored: returned scores match the exact brute-force path"
repro_note   "ES 8.13.4 scored 0.937 mean / 0.70 min on the same protocol — the ES side needs an ES install"

repro_node_start -
trap repro_node_stop EXIT
repro_node_wait

BASE="http://127.0.0.1:$REPRO_PORT"
IDX="bench-vec"
SCRATCH="$(mktemp -d /tmp/xerj-bench-knn.XXXXXX)"
trap 'rm -rf "$SCRATCH"; repro_node_stop' EXIT
RESULT_FILE="$SCRATCH/knn.json"

curl -sf -XDELETE "$BASE/$IDX" >/dev/null 2>&1 || true
curl -sf -XPUT "$BASE/$IDX" -H 'content-type: application/json' --data-binary @- >/dev/null <<JSON
{ "mappings": { "properties": {
  "v": { "type": "dense_vector", "dims": $DIMS, "index": true, "similarity": "cosine" }
} } }
JSON

node - "$BASE" "$IDX" "$VECS" "$DIMS" "$PROBES" "$K" "$CANDIDATES" "$RESULT_FILE" "$ANN_WAIT_S" <<'NODE'
// Same generator, corpus shape and query shape as demo/playbooks/bench-matrix.mjs
// knnBench(): mulberry32(0xC0FFEE) uniforms mapped to [-1,1), explicit _ids so
// engine hits map back to generated vectors, cosine similarity, k=10,
// num_candidates=100. Extended from 1 probe to PROBES so the "100-probe mean"
// half of the claim is measured, not extrapolated from one draw.
const http = require('node:http');
const [base, idx, M, DIMS, PROBES, K, CAND, outFile, ANN_WAIT_S] = [process.argv[2], process.argv[3], +process.argv[4], +process.argv[5], +process.argv[6], +process.argv[7], +process.argv[8], process.argv[9], +process.argv[10]];
const port = +base.split(':')[2];
const agent = new http.Agent({ keepAlive: true, maxSockets: 256 });

function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rng = mulberry32(0xC0FFEE);
const vecs = new Array(M);
for (let i = 0; i < M; i++) {
  const v = new Array(DIMS);
  for (let k = 0; k < DIMS; k++) v[k] = rng() * 2 - 1;
  vecs[i] = v;
}

function req(method, path, body, ndjson) {
  const payload = body === undefined ? undefined : (ndjson ? body : JSON.stringify(body));
  const headers = { 'content-type': ndjson ? 'application/x-ndjson' : 'application/json' };
  if (payload !== undefined) headers['content-length'] = Buffer.byteLength(payload);
  return new Promise((resolve, reject) => {
    const r = http.request({ hostname: '127.0.0.1', port, path, method, headers, agent }, (res) => {
      let txt = ''; res.setEncoding('utf8');
      res.on('data', (c) => { txt += c; });
      res.on('end', () => {
        if (res.statusCode >= 400) return reject(new Error(`${method} ${path} -> ${res.statusCode}: ${txt.slice(0, 300)}`));
        resolve(txt ? JSON.parse(txt) : null);
      });
    });
    r.on('error', reject);
    if (payload !== undefined) r.write(payload);
    r.end();
  });
}

function cosine(a, b) {
  let d = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { d += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i]; }
  return d / (Math.sqrt(na) * Math.sqrt(nb) || 1);
}

(async () => {
  // bulk load in 5k-doc chunks through the same NDJSON wire the harness uses
  const CHUNK = 5000;
  for (let s = 0; s < M; s += CHUNK) {
    let nd = '';
    for (let i = s; i < Math.min(s + CHUNK, M); i++)
      nd += `{"index":{"_id":"${i}"}}\n` + JSON.stringify({ v: vecs[i] }) + '\n';
    const r = await req('POST', `/${idx}/_bulk`, nd, true);
    if (r && r.errors) throw new Error('bulk reported item errors');
    process.stderr.write(`  indexed ${Math.min(s + CHUNK, M)}/${M}\n`);
  }
  await req('POST', `/${idx}/_refresh`);

  // wait for the HNSW graph to cover the corpus (a stale/under-covered graph
  // silently degrades kNN to the exact scan — this is the check). If it never
  // gets there, stop: probes sent now would measure the exact-scan fallback
  // and print it as HNSW recall (#2075 item 12).
  // `coverage` is a COUNT of graphed docs (not a fraction), so "coverage >= 1"
  // passed as soon as one vector was in the graph. Ready means the node's own
  // `covered` verdict (graphed == vector-bearing docs, not stale), or, on a
  // build without that column, coverage reaching vector_docs; never while stale
  // or rebuilding.
  const yes = (v) => /^true$/i.test(String(v));
  const annReady = (a) => !yes(a.stale) && !yes(a.rebuilding) && (a.covered !== undefined
    ? yes(a.covered)
    : Number(a.vector_docs) > 0 && Number(a.coverage) >= Number(a.vector_docs));
  let ann = {};
  for (let i = 0; i < ANN_WAIT_S; i++) {
    const rows = await req('GET', `/_cat/ann/${idx}?format=json`);
    ann = (Array.isArray(rows) && rows[0]) || {};
    if (annReady(ann)) break;
    await new Promise((r) => setTimeout(r, 1000));
  }
  if (!annReady(ann)) {
    throw new Error(`ANN not ready after ${ANN_WAIT_S}s (GET /_cat/ann/${idx}: ${JSON.stringify(ann)}): ` +
      'kNN would be served by the exact-scan fallback, so recall is not measured');
  }

  const recalls = []; let maxScoreDelta = 0; const lat = [];
  for (let q = 0; q < PROBES; q++) {
    const qv = new Array(DIMS);
    for (let k = 0; k < DIMS; k++) qv[k] = rng() * 2 - 1;
    // client-side exact brute-force top-K ground truth
    const scored = new Array(M);
    for (let i = 0; i < M; i++) scored[i] = [i, cosine(qv, vecs[i])];
    scored.sort((a, b) => b[1] - a[1]);
    const exact = new Map(scored.slice(0, K).map(([i, c]) => [i, (1 + c) / 2]));
    const body = { knn: { field: 'v', query_vector: qv, k: K, num_candidates: CAND }, size: K };
    const t0 = performance.now();
    const r = await req('POST', `/${idx}/_search?request_cache=false`, body);
    lat.push(performance.now() - t0);
    const hits = (r.hits && r.hits.hits) || [];
    const ids = hits.map((h) => parseInt(h._id, 10));
    let hit = 0;
    ids.forEach((id, n) => {
      if (exact.has(id)) hit++;
      if (hits[n] && typeof hits[n]._score === 'number' && exact.has(id)) {
        maxScoreDelta = Math.max(maxScoreDelta, Math.abs(hits[n]._score - exact.get(id)));
      }
    });
    recalls.push(ids.length ? hit / K : 0);
  }
  agent.destroy();
  const mean = recalls.reduce((a, b) => a + b, 0) / recalls.length;
  const out = {
    vectors: M, dims: DIMS, probes: PROBES, k: K, num_candidates: CAND,
    recall_mean: mean, recall_min: Math.min(...recalls), recall_max: Math.max(...recalls),
    perfect_probes: recalls.filter((r) => r === 1).length,
    max_abs_score_delta_vs_exact: maxScoreDelta,
    latency_p50_ms: lat.sort((a, b) => a - b)[Math.floor(lat.length / 2)],
    ann,
  };
  require('node:fs').writeFileSync(outFile, JSON.stringify(out));
  process.stderr.write('done\n');
})().catch((e) => { console.error(String(e && e.message || e)); process.exit(1); });
NODE

python3 - "$RESULT_FILE" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
ann = r.get("ann") or {}
print()
print(f'  MEASURED  recall@{r["k"]} : {r["recall_mean"]:.3f} mean over {r["probes"]} probes '
      f'(min {r["recall_min"]:.3f}, max {r["recall_max"]:.3f}, {r["perfect_probes"]}/{r["probes"]} probes at 1.00)')
print(f'  MEASURED  score parity    : max |engine _score - client exact (1+cos)/2| = {r["max_abs_score_delta_vs_exact"]:.2e} '
      f'(claim: exact rescoring, scores match the exact path)')
print(f'  MEASURED  kNN latency     : p50 {r["latency_p50_ms"]:.2f} ms per probe, k={r["k"]}, num_candidates={r["num_candidates"]} '
      f'over {r["vectors"]} x {r["dims"]}-d vectors')
print(f'  MEASURED  ANN health      : coverage={ann.get("coverage")} stale={ann.get("stale")} nodes={ann.get("nodes")} '
      f'(GET /_cat/ann — proves the HNSW graph served this, not a silent exact-scan fallback)')
print('  note      50k x 128-d uniform random vectors reproduce the harness generator; recall on a')
print('            different distribution can differ — the claim is measured on THIS shape, as published')
PY
repro_env_box
repro_note "teardown: node stopped, throwaway data dir removed (KEEP=1 to keep)"
