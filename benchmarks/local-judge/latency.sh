#!/bin/bash
# CPU latency of one rerank call over a 30-document window, per tier and thread count.
#
# usage: RUNS=<runs_dir> PAIR_SCORE=<path to pair_score> latency.sh <groups> <dataset> <tier> <threads...>
#
# Each group in D.test.w30.jsonl is one query's BM25 top-30, scored in ONE call — the
# shape of one `"rerank": {"provider": "local"}` search. pair_score times each call.
# A fresh output per run (no resume), and the load average before and after is
# recorded next to the percentiles, because on a shared machine it moves them.
set -u
: "${RUNS:?set RUNS to the runs directory}"
: "${PAIR_SCORE:?set PAIR_SCORE to the pair_score binary}"
G=$1; ds=$2; tier=$3; shift 3
mkdir -p "$RUNS/latency"
for t in "$@"; do
  o=$RUNS/latency/$ds.$tier.t$t
  rm -f "$o.logits.jsonl"
  l0=$(cut -d' ' -f1-3 /proc/loadavg)
  "$PAIR_SCORE" --tier "$tier" --threads "$t" --max-groups "$G" --in "$RUNS/$ds.test.w30.jsonl" \
    --out "$o.logits.jsonl" --summary "$o.summary.json" > /dev/null 2> "$o.err" || { echo "FAILED $o"; exit 1; }
  l1=$(cut -d' ' -f1-3 /proc/loadavg)
  python3 - "$o.summary.json" "$l0" "$l1" <<'PY'
import json, sys
p, l0, l1 = sys.argv[1:4]
s = json.load(open(p)); s["loadavg_before"] = l0; s["loadavg_after"] = l1
json.dump(s, open(p, "w"), indent=1)
print(f"{s['tier']:5s} t={s['threads']:2d} groups={s['groups']} p50={s['per_call_ms']['p50']:.0f}ms "
      f"p95={s['per_call_ms']['p95']:.0f}ms rss={s['peak_rss_mb']}MB load {l0} -> {l1}")
PY
done
