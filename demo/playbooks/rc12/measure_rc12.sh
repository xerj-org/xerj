#!/usr/bin/env bash
# rc.12 gate harness — measures BOTH axes the release must defend:
#   (1) on-disk bytes per artifact type for the same logical corpus
#   (2) query latency across the shapes rc.12 claims to improve
#
# Methodology follows demo/playbooks/DISK_SIZE_2026-07-09.md so the numbers are
# comparable to that baseline: deterministic corpus, single index, force-merged
# to one segment so we measure STEADY STATE rather than a transient many-segment
# ingest state (that conflation is what produced the bogus "XERJ ~2x ES disk"
# claim historically).
#
# Usage:
#   measure_rc12.sh <label> [docs]
# Writes results to demo/playbooks/rc12/results-<label>.json and prints a table.
#
# Run it once on the base commit and once after a change; compare the JSONs.
set -euo pipefail

LABEL="${1:?usage: measure_rc12.sh <label> [docs]}"
DOCS="${2:-100000}"
PORT="${XERJ_BENCH_PORT:-9400}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN="${XERJ_BIN:-$ROOT/engine/target/release/xerj}"
DATA="${XERJ_BENCH_DATA:-/tmp/claude-1001/xerj-rc12-bench/$LABEL}"
OUT="$ROOT/demo/playbooks/rc12/results-$LABEL.json"
IDX="rc12bench"

[ -x "$BIN" ] || { echo "no xerj binary at $BIN — build it first" >&2; exit 2; }
mkdir -p "$(dirname "$OUT")"

echo "==> rc.12 harness: label=$LABEL docs=$DOCS port=$PORT"
echo "    binary: $BIN"
echo "    data:   $DATA"

# ── fresh instance ───────────────────────────────────────────────────────────
# The listen ports come from the TOML config below (the same PORT, PORT+1,
# PORT+2 layout that `--port` claims; the config keeps all three explicit).
# Free the PORT, not just this label's data dir: a previous run under a
# DIFFERENT label still owns the socket, and killing only our own config path
# leaves it up. The new server then fails to bind and exits, while every
# subsequent request silently hits the OLD instance — which surfaces as a
# baffling 409 on index creation rather than as a bind error.
for pid in $(lsof -ti tcp:"$PORT" 2>/dev/null); do
  kill "$pid" 2>/dev/null || true
done
sleep 2
rm -rf "$DATA"; mkdir -p "$DATA"
cat > "$DATA/xerj.toml" <<EOF
[server]
bind_address   = "127.0.0.1"
es_compat_port = $PORT
rest_port      = $((PORT + 1))
grpc_port      = $((PORT + 2))
EOF
# XERJ_DISABLE_QUERY_CACHE=1 is MANDATORY, not optional. Without it the
# whole-result cache clones the answer for a repeated identical query body, and
# since this harness fires 40 identical repeats per shape, every number becomes
# the cache's latency instead of the engine's. A first run of this harness
# without it reported `took: 0` for all 17 shapes and nearly led to the
# conclusion that no query work was measurable at all. See
# demo/playbooks/CRITICAL_FINDING_read_perf_cache_mirage.md.
nohup env XERJ_DISABLE_QUERY_CACHE=1 \
  "$BIN" -c "$DATA/xerj.toml" --data-dir "$DATA" --insecure \
  > "$DATA/server.log" 2>&1 &
disown

URL="http://localhost:$PORT"
for _ in $(seq 1 120); do
  curl -fsS -m 3 "$URL/_cluster/health" 2>/dev/null | grep -q '"status":"\(green\|yellow\)"' && break
  sleep 1
done
curl -fsS -m 5 "$URL/_cluster/health" >/dev/null || { echo "server never came up" >&2; exit 1; }
echo "==> server up"

# ── corpus: deterministic, mixed-type (keyword + text + numeric + date + bool) ─
# Mirrors the 2026-07-09 baseline shape so artifact shares are comparable.
CORPUS="$DATA/corpus.ndjson"
python3 - "$DOCS" "$IDX" "$CORPUS" <<'PY'
import json, random, sys
n, idx, out = int(sys.argv[1]), sys.argv[2], sys.argv[3]

# The `body` text must have REALISTIC term diversity or the measurement is
# worthless: a small vocabulary makes postings compress absurdly well and makes
# every text query trivially fast, which flatters both axes. Real text is
# Zipfian over a large vocabulary, so we synthesise that explicitly.
# Seeded, so the corpus is byte-identical between runs and A/B is valid.
rng = random.Random(20261207)
VOCAB_N = 60000
STEMS = ("index segment posting token merge flush shard query filter score rank "
         "cache buffer stream codec vector cluster commit replica offset column "
         "record field parse encode decode compress bitmap ordinal dictionary").split()
VOCAB = [f"{STEMS[i % len(STEMS)]}{i//len(STEMS):04d}" for i in range(VOCAB_N)]
# Zipf weights: a few very common terms, a long tail of rare ones.
# Draw ONE large pool up front rather than sampling per token: per-call weighted
# sampling over a 60k vocabulary rebuilds the cumulative table every time and is
# far too slow at corpus scale. A rolling window over the pool preserves the
# distribution while making generation linear.
WEIGHTS = [1.0 / (i + 1) ** 1.07 for i in range(VOCAB_N)]
POOL_N = 2_000_003          # prime, so the rolling window does not resonate
POOL = rng.choices(VOCAB, weights=WEIGHTS, k=POOL_N)

MODELS = ["fable-5", "opus-5", "sonnet-5", "haiku-4-5"]
STATUS = ["ok", "error", "timeout", "throttled"]
REGION = ["us-east-1", "eu-west-2", "ap-south-1"]
with open(out, "w") as f:
    for i in range(n):
        # ~40 tokens drawn Zipfian — drives realistic positional postings and
        # gives phrase/match queries a real term-frequency distribution.
        off = (i * 41) % (POOL_N - 40)
        body = " ".join(POOL[off:off + 40])
        doc = {
            "doc_id": f"doc-{i:08d}",              # high-cardinality keyword
            "model": MODELS[i % len(MODELS)],       # low-cardinality keyword
            "status": STATUS[i % len(STATUS)],
            "region": REGION[i % len(REGION)],
            "session": f"sess-{i % 5000:05d}",
            "tenant": f"tenant-{i % 97:03d}",
            "body": body,                            # analyzed text
            "latency_ms": (i * 37) % 5000,           # integer-structured
            "context_tokens": 1000 + (i % 100000),   # monotonic-ish
            "cost_usd": round((i % 997) / 1000.0, 4),# float
            "retries": i % 5,                        # tiny range
            "score": round(((i * 13) % 1000) / 100.0, 2),
            "ts": 1700000000000 + i * 1000,          # epoch-ms, monotonic
            "cached": (i % 3 == 0),                  # boolean
        }
        f.write(json.dumps({"index": {"_index": idx, "_id": doc["doc_id"]}}) + "\n")
        f.write(json.dumps(doc) + "\n")
print(f"corpus: {n} docs -> {out}", file=sys.stderr)
PY

RAW_BYTES=$(python3 - "$CORPUS" <<'PY'
import json,sys
# raw = the source documents only, excluding bulk action lines
tot=0
with open(sys.argv[1]) as f:
    for i,l in enumerate(f):
        if i % 2 == 1: tot += len(l.encode())
print(tot)
PY
)
echo "==> corpus raw source bytes: $RAW_BYTES"

# ── mapping: explicit, so the measurement is not at the mercy of inference ────
curl -fsS -X PUT "$URL/$IDX" -H 'Content-Type: application/json' -d '{
  "mappings": {"properties": {
    "doc_id":{"type":"keyword"}, "model":{"type":"keyword"},
    "status":{"type":"keyword"}, "region":{"type":"keyword"},
    "session":{"type":"keyword"}, "tenant":{"type":"keyword"},
    "body":{"type":"text"},
    "latency_ms":{"type":"long"}, "context_tokens":{"type":"long"},
    "cost_usd":{"type":"double"}, "retries":{"type":"long"},
    "score":{"type":"double"}, "ts":{"type":"date"}, "cached":{"type":"boolean"}
  }}}' > /dev/null
echo "==> mapping created"

# ── ingest (timed) ───────────────────────────────────────────────────────────
INGEST_START=$(date +%s.%N)
split -l 20000 "$CORPUS" "$DATA/chunk-"
for c in "$DATA"/chunk-*; do
  curl -fsS -X POST "$URL/_bulk" -H 'Content-Type: application/x-ndjson' \
       --data-binary "@$c" > /dev/null
done
curl -fsS -X POST "$URL/$IDX/_refresh" > /dev/null
INGEST_END=$(date +%s.%N)
INGEST_S=$(python3 -c "print(round($INGEST_END-$INGEST_START,2))")
echo "==> ingest: ${INGEST_S}s"

# ── force-merge to steady state ──────────────────────────────────────────────
MERGE_START=$(date +%s.%N)
curl -fsS -X POST "$URL/$IDX/_forcemerge?max_num_segments=1" > /dev/null || true
curl -fsS -X POST "$URL/$IDX/_flush" > /dev/null || true
sleep 10
MERGE_END=$(date +%s.%N)
MERGE_S=$(python3 -c "print(round($MERGE_END-$MERGE_START,2))")
echo "==> force-merge: ${MERGE_S}s"

COUNT=$(curl -fsS "$URL/$IDX/_count" | sed 's/.*"count":\([0-9]*\).*/\1/')
echo "==> indexed docs: $COUNT"

# ── disk by artifact type ────────────────────────────────────────────────────
python3 - "$DATA" "$IDX" "$RAW_BYTES" "$COUNT" "$INGEST_S" "$MERGE_S" "$LABEL" "$OUT" <<'PY'
import json, os, sys, collections
data, idx, raw, count, ing, mrg, label, out = sys.argv[1:9]
by_ext = collections.Counter()
total = 0
for dirpath, _, files in os.walk(data):
    if "corpus.ndjson" in files or dirpath.endswith(data): pass
    for fn in files:
        if fn in ("corpus.ndjson","server.log") or fn.startswith("chunk-"): continue
        p = os.path.join(dirpath, fn)
        try: sz = os.path.getsize(p)
        except OSError: continue
        ext = fn.rsplit(".", 1)[-1] if "." in fn else "(noext)"
        by_ext[ext] += sz; total += sz
res = {
  "label": label, "docs": int(count), "raw_source_bytes": int(raw),
  "index_total_bytes": total,
  "index_over_raw": round(total / int(raw), 4) if int(raw) else None,
  "ingest_seconds": float(ing), "forcemerge_seconds": float(mrg),
  "by_artifact": {k: v for k, v in by_ext.most_common()},
}
json.dump(res, open(out, "w"), indent=2)
print(f"\n{'artifact':<12}{'bytes':>14}{'share':>9}")
for k, v in by_ext.most_common(12):
    print(f"{k:<12}{v:>14,}{v/total*100:>8.1f}%")
print(f"{'TOTAL':<12}{total:>14,}")
print(f"\nraw source : {int(raw):,} B")
print(f"index/raw  : {res['index_over_raw']}x")
PY

# ── query latency suite ──────────────────────────────────────────────────────
echo ""
echo "==> query latency (p50/p99 ms over 40 runs each)"
python3 - "$URL" "$IDX" "$OUT" <<'PY'
import json, sys, time, urllib.request
url, idx, out = sys.argv[1], sys.argv[2], sys.argv[3]
# Cheap point-lookup shapes were all sub-0.5ms at 100k docs — at that level HTTP
# and JSON overhead dominate and no engine change is measurable. So the suite
# deliberately includes EXPENSIVE shapes (large result sets, deep pages, high
# cardinality, phrase over diverse text) where engine work actually shows up.
QUERIES = {
 # --- cheap shapes: regression guards, not improvement targets ---
 "term_keyword_low_card": {"query":{"term":{"model":"opus-5"}}},
 "term_keyword_high_card":{"query":{"term":{"doc_id":"doc-00050000"}}},
 "range_date_narrow":     {"query":{"range":{"ts":{"gte":1700000000000,"lt":1700000050000}}}},
 # --- expensive shapes: where a 2x would have to come from ---
 "range_long_wide":       {"query":{"range":{"latency_ms":{"gte":0,"lt":4900}}}},
 "match_text_common":     {"query":{"match":{"body":"index0000"}}},
 "match_text_multi":      {"query":{"match":{"body":"index0000 segment0000 token0000"}}},
 "match_phrase":          {"query":{"match_phrase":{"body":"index0000 segment0000"}}},
 "bool_must_filter":      {"query":{"bool":{"must":[{"match":{"body":"index0000"}}],
                                            "filter":[{"term":{"status":"ok"}},
                                                      {"range":{"latency_ms":{"lt":2500}}}]}}},
 "deep_page":             {"from":9000,"size":100,
                           "query":{"range":{"latency_ms":{"gte":0,"lt":4900}}}},
 "large_page":            {"size":500,"query":{"match":{"body":"index0000"}}},
 "sort_by_numeric":       {"size":100,"sort":[{"latency_ms":"desc"}],
                           "query":{"match_all":{}}},
 # --- aggregations ---
 "agg_terms_low_card":    {"size":0,"aggs":{"m":{"terms":{"field":"model"}}}},
 "agg_terms_high_card":   {"size":0,"aggs":{"s":{"terms":{"field":"session","size":100}}}},
 "agg_stats_numeric":     {"size":0,"aggs":{"s":{"stats":{"field":"latency_ms"}}}},
 "agg_date_histogram":    {"size":0,"aggs":{"h":{"date_histogram":{"field":"ts",
                                                 "fixed_interval":"1h"}}}},
 # numeric histogram is still the brute-force O(N) path (task #35) — a named target
 "agg_histogram_numeric": {"size":0,"aggs":{"h":{"histogram":{"field":"latency_ms",
                                                 "interval":250}}}},
 "agg_nested_terms_stats":{"size":0,"aggs":{"m":{"terms":{"field":"model"},
                            "aggs":{"s":{"stats":{"field":"cost_usd"}}}}}},
 # --- the two read families READ_SCORECARD_2026-07-09.md leaves UNFIXED ---
 # Both still brute-scan and score every document: boosting ~163ms,
 # function_score ~157ms on 100k docs = 64.5% of that scorecard's total read
 # time. These are the largest remaining query-latency levers in the engine.
 "boosting":              {"query":{"boosting":{
                             "positive":{"match":{"body":"index0000"}},
                             "negative":{"match":{"body":"merge0000"}},
                             "negative_boost":0.2}}},
 "function_score":        {"query":{"function_score":{
                             "query":{"match":{"body":"index0000"}},
                             "functions":[{"filter":{"term":{"status":"ok"}},"weight":2}]}}},
 # phrase families improved 5-6x in that sweep but remain super-linear (~40ms)
 "match_phrase_prefix":   {"query":{"match_phrase_prefix":{"body":"index0000 segment"}}},
 "fuzzy_text":            {"query":{"fuzzy":{"body":{"value":"index0001","fuzziness":1}}}},
 "wildcard_text":         {"query":{"wildcard":{"body":"index00*"}}},
 "prefix_text":           {"query":{"prefix":{"body":"index00"}}},
 # --- pathological: served from the TEXT doc-values column ---
 # ES rejects both (fielddata disabled on text by default). XERJ answers them
 # from `.dv`, keyed by the WHOLE document body, and they are by three orders of
 # magnitude the slowest operations in this suite. If the "no doc-values for
 # text" lever lands, these must turn into ERRORS, not faster queries — an
 # `error` entry here is the SUCCESS condition, not a regression.
 "agg_terms_on_text":     {"size":0,"aggs":{"a":{"terms":{"field":"body","size":3}}}},
 "sort_on_text":          {"size":1,"sort":[{"body":"asc"}]},
}
def run(body):
    d = json.dumps(body).encode()
    r = urllib.request.Request(f"{url}/{idx}/_search", data=d,
                               headers={"Content-Type":"application/json"})
    t0 = time.perf_counter()
    with urllib.request.urlopen(r) as resp: resp.read()
    return (time.perf_counter()-t0)*1000
# Report the engine's own `took` alongside wall-clock: when wall-clock is
# sub-millisecond, transport dominates and `took` is the honest engine signal.
def took(body):
    d = json.dumps(body).encode()
    r = urllib.request.Request(f"{url}/{idx}/_search", data=d,
                               headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(r) as resp:
        return json.loads(resp.read()).get("took", -1)
results = {}
print(f"{'query':<26}{'p50':>9}{'p99':>9}{'took':>8}{'hits':>12}")
for name, body in QUERIES.items():
    try:
        for _ in range(5): run(body)           # warm
        ts = sorted(run(body) for _ in range(40))
        p50, p99 = ts[len(ts)//2], ts[int(len(ts)*0.99)-1]
        tk = took(body)
        d = json.dumps(body).encode()
        r = urllib.request.Request(f"{url}/{idx}/_search", data=d,
                                   headers={"Content-Type":"application/json"})
        with urllib.request.urlopen(r) as resp:
            tot = json.loads(resp.read()).get("hits",{}).get("total",{}).get("value",-1)
        results[name] = {"p50_ms": round(p50,3), "p99_ms": round(p99,3),
                         "took_ms": tk, "hits": tot}
        print(f"{name:<26}{p50:>9.2f}{p99:>9.2f}{tk:>8}{tot:>12,}")
    except Exception as e:
        results[name] = {"error": str(e)[:200]}
        print(f"{name:<26}{'ERROR':>9}  {str(e)[:60]}")
res = json.load(open(out)); res["queries"] = results
json.dump(res, open(out,"w"), indent=2)
print(f"\nwrote {out}")
PY

echo ""
echo "==> done. server still running on $PORT (data: $DATA)"
echo "    stop it with: pkill -f '$DATA/xerj.toml'"
