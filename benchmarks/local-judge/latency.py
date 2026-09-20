"""CPU latency of ONE local rerank call, swept over window size, tier and thread width.

usage: latency.py <runs_dir> <pair_score> <dataset> <out.json> [groups] [tiers] [windows] [threads]
       defaults: groups=25  tiers=small,base  windows=10,20,30,50,100  threads=0

A `window` of N is N documents in one `"rerank": {"window": N}` call, so the timed unit
here is exactly the unit a search pays for. The pair files are grouped by query and hold
the BM25 top-30 plus the rest of the top-100 (`make_pairs.py`), so a window of N is the
first N documents of that query's BM25 list — the same text, the same clip, the same
`JudgeRuntime` the server scores with.

What this does NOT measure: throughput under load. Every run here is one process with the
machine otherwise idle, because a latency number taken next to seven other scorers is a
queueing number. The load average is recorded before and after each sweep point so a run
taken on a busy machine is visible as such rather than quoted as if it were not.
Standard library only."""
import json, os, subprocess, sys, time

runs, pair_score, ds, out = sys.argv[1:5]
groups = int(sys.argv[5]) if len(sys.argv) > 5 else 25
tiers = (sys.argv[6] if len(sys.argv) > 6 else "small,base").split(",")
windows = [int(w) for w in (sys.argv[7] if len(sys.argv) > 7 else "10,20,30,50,100").split(",")]
threads = [int(t) for t in (sys.argv[8] if len(sys.argv) > 8 else "0").split(",")]

# One query's candidates, in BM25 order: the top-30 window file first, then the
# rest file, which holds the documents ranked 31..100 for the wide windows.
cands = {}
for part in ("w30", "rest"):
    path = os.path.join(runs, f"{ds}.test.{part}.jsonl")
    if not os.path.exists(path):
        continue
    for line in open(path):
        r = json.loads(line)
        cands.setdefault(r["g"], []).append(line)
qids = sorted(cands)[:groups]


def loadavg():
    return open("/proc/loadavg").read().split()[:3]


report = {"dataset": ds, "groups": groups, "cores": os.cpu_count(), "points": []}
for w in windows:
    pairs = os.path.join(runs, f"latency.{ds}.w{w}.jsonl")
    with open(pairs, "w") as f:
        for q in qids:
            f.writelines(cands[q][:w])
    for tier in tiers:
        for t in threads:
            stem = os.path.join(runs, "latency", f"{ds}.{tier}.w{w}.t{t}")
            os.makedirs(os.path.dirname(stem), exist_ok=True)
            for suffix in (".logits.jsonl", ".summary.json"):
                if os.path.exists(stem + suffix):
                    os.remove(stem + suffix)  # never resume a latency run
            before = loadavg()
            started = time.time()
            proc = subprocess.run(
                [pair_score, "--tier", tier, "--threads", str(t), "--in", pairs,
                 "--out", stem + ".logits.jsonl", "--summary", stem + ".summary.json"],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            if proc.returncode != 0:
                raise SystemExit(f"pair_score failed for {tier} w{w} t{t}: {proc.stderr[-600:]}")
            s = json.load(open(stem + ".summary.json"))
            point = {"window": w, "tier": tier, "threads_requested": t,
                     "threads_used": s["threads"], "calls": s["groups"],
                     "p50_ms": s["per_call_ms"]["p50"], "p95_ms": s["per_call_ms"]["p95"],
                     "max_ms": s["per_call_ms"]["max"], "load_ms": s["load_ms"],
                     "peak_rss_mb": s["peak_rss_mb"], "real_tokens": s["real_tokens"],
                     "truncated_pairs": s["truncated_pairs"],
                     "wall_s": round(time.time() - started, 1),
                     "loadavg_before": before, "loadavg_after": loadavg()}
            report["points"].append(point)
            print(f"{tier:5s} window={w:3d} threads={s['threads']:2d} "
                  f"p50={point['p50_ms']:8.0f}ms p95={point['p95_ms']:8.0f}ms "
                  f"rss={point['peak_rss_mb']:5d}MB load {before[0]}->{point['loadavg_after'][0]}",
                  flush=True)
    os.remove(pairs)
json.dump(report, open(out, "w"), indent=1)
