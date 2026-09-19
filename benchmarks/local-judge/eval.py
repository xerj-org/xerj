"""nDCG@10 for every local-rerank arm, and the calibration of the probability it reports.

usage: eval.py <beir_root> <runs_dir> <out.json>

Reads, per dataset D in {scifact, nfcorpus, fiqa}:
  <runs_dir>/D.test.first_stage.json          BM25 / hybrid top-100 from a live node (first_stage.py)
  <runs_dir>/D.test.<tier>.{w30,rest}.logits.jsonl   raw logits from `pair_score`
  <runs_dir>/D.fit.first_stage.json and D.fit.<tier>.w30.logits.jsonl   held-out queries, for the fit

An arm whose pairs were not all scored is reported as missing, never computed on a subset.
Standard library only."""
import json, os, random, sys
from beirlib import qrels, ndcg10, sigmoid, ece, reliability, platt_fit, temperature_fit, nll

ROOT, RUNS, OUT = sys.argv[1:4]
DATASETS = ["scifact", "nfcorpus", "fiqa"]
FIT_SPLIT = {"scifact": "train", "nfcorpus": "dev", "fiqa": "dev"}
TIERS = ["small", "base", "large"]
WINDOWS = [30, 100]


def logits(ds, split, tier):
    table = {}
    for part in ("w30", "rest"):
        path = os.path.join(RUNS, f"{ds}.{split}.{tier}.{part}.logits.jsonl")
        if os.path.exists(path):
            for line in open(path):
                r = json.loads(line)
                table[(r["g"], r["id"])] = r["logits"][0]
    return table


def rerank(first, table, qid, w):
    head = first[:w]
    if any((qid, d) not in table for d in head):
        return None
    order = sorted(range(len(head)), key=lambda i: (-table[(qid, head[i])], i))
    return [head[i] for i in order] + first[w:]


def mean(xs):
    return sum(xs) / len(xs) if xs else 0.0


def paired_bootstrap(a, b, n=2000, seed=5):
    """95% interval for mean(a) - mean(b) over the same queries."""
    rng, k, diffs = random.Random(seed), len(a), []
    for _ in range(n):
        idx = [rng.randrange(k) for _ in range(k)]
        diffs.append(sum(a[i] - b[i] for i in idx) / k)
    diffs.sort()
    return diffs[int(n * .025)], diffs[int(n * .975)]


report = {"datasets": {}, "calibration": {}}
fit_rows = {t: [] for t in TIERS}       # pooled held-out (logit, label)
test_rows = {t: {} for t in TIERS}      # per dataset test (logit, label), BM25 top-30 window

for ds in DATASETS:
    fs_path = os.path.join(RUNS, f"{ds}.test.first_stage.json")
    if not os.path.exists(fs_path):
        continue
    run, rel = json.load(open(fs_path)), qrels(os.path.join(ROOT, ds), "test")
    qids = sorted(run["bm25"])
    per_q = {"bm25": [ndcg10(run["bm25"][q], rel[q]) for q in qids],
             "hybrid": [ndcg10(run["hybrid"][q], rel[q]) for q in qids]}
    arms = {"bm25": mean(per_q["bm25"]), "hybrid rrf": mean(per_q["hybrid"])}
    intervals = {}
    for tier in TIERS:
        table = logits(ds, "test", tier)
        if not table:
            continue
        for stage in ("bm25", "hybrid"):
            for w in WINDOWS:
                ranked = [rerank(run[stage][q], table, q, w) for q in qids]
                name = f"{stage} top-{w} -> local {tier}"
                if any(r is None for r in ranked):
                    arms[name] = None
                    continue
                scores = [ndcg10(r, rel[q]) for r, q in zip(ranked, qids)]
                arms[name] = mean(scores)
                lo, hi = paired_bootstrap(scores, per_q["hybrid"])
                intervals[name] = {"vs_hybrid": mean(scores) - arms["hybrid rrf"], "ci95": [lo, hi]}
        test_rows[tier][ds] = [(table[(q, d)], 1 if rel[q].get(d, 0) > 0 else 0)
                               for q in qids for d in run["bm25"][q][:30] if (q, d) in table]
    report["datasets"][ds] = {"queries": len(qids), "docs": run.get("docs"), "ndcg@10": arms,
                              "vs_hybrid": intervals, "first_stage_latency_ms": run.get("latency_ms")}

    fit_path = os.path.join(RUNS, f"{ds}.fit.first_stage.json")
    if os.path.exists(fit_path):
        frun, frel = json.load(open(fit_path)), qrels(os.path.join(ROOT, ds), FIT_SPLIT[ds])
        for tier in TIERS:
            table = logits(ds, "fit", tier)
            fit_rows[tier] += [(table[(q, d)], 1 if frel[q].get(d, 0) > 0 else 0)
                               for q in sorted(frun["bm25"]) for d in frun["bm25"][q][:30] if (q, d) in table]

for tier in TIERS:
    if not fit_rows[tier] or not test_rows[tier]:
        continue
    t = temperature_fit(fit_rows[tier])
    a, b = platt_fit(fit_rows[tier])
    maps = {"raw": (1.0, 0.0), "temperature": (t, 0.0), "platt": (a, b)}
    entry = {"temperature": {"scale": t, "T": 1.0 / t if t else None}, "platt": {"scale": a, "bias": b},
             "fit_pairs": len(fit_rows[tier]), "fit_positive_rate": mean([y for _, y in fit_rows[tier]]),
             "fit_nll": {k: nll(fit_rows[tier], s_, b_) for k, (s_, b_) in maps.items()},
             "test": {}}
    pooled = []
    for ds, rows in test_rows[tier].items():
        pooled += rows
        entry["test"][ds] = {"pairs": len(rows), "positive_rate": mean([y for _, y in rows])}
        for k, (s_, b_) in maps.items():
            entry["test"][ds]["ece_" + k] = ece([(sigmoid(s_ * x + b_), y) for x, y in rows])
            entry["test"][ds]["nll_" + k] = nll(rows, s_, b_)
    entry["test"]["pooled"] = {"pairs": len(pooled), "positive_rate": mean([y for _, y in pooled])}
    for k, (s_, b_) in maps.items():
        cal = [(sigmoid(s_ * x + b_), y) for x, y in pooled]
        entry["test"]["pooled"]["ece_" + k] = ece(cal)
        entry["test"]["pooled"]["nll_" + k] = nll(pooled, s_, b_)
        entry["test"]["pooled"]["reliability_" + k] = reliability(cal)
    report["calibration"][tier] = entry

# Issue #940: hybrid RRF breaks ties differently across restarts. Every repeat of the
# hybrid arm (a fresh node process each time) is scored here so the spread is reported,
# not one lucky draw.
for ds in DATASETS:
    reps = sorted(f for f in os.listdir(RUNS) if f.startswith(f"{ds}.test.hybrid.rep") and f.endswith(".json"))
    base = os.path.join(RUNS, f"{ds}.test.first_stage.json")
    if not reps or not os.path.exists(base):
        continue
    rel = qrels(os.path.join(ROOT, ds), "test")
    runs = [json.load(open(base))["hybrid"]] + [json.load(open(os.path.join(RUNS, f)))["hybrid"] for f in reps]
    qids = sorted(runs[0])
    scores = [mean([ndcg10(r[q], rel[q]) for q in qids]) for r in runs]
    top10_same = mean([1.0 if all(r[q][:10] == runs[0][q][:10] for r in runs) else 0.0 for q in qids])
    top100_same_set = mean([1.0 if all(set(r[q]) == set(runs[0][q]) for r in runs) else 0.0 for q in qids])
    report["datasets"][ds]["hybrid_repeats"] = {
        "runs": len(runs), "ndcg@10": scores, "min": min(scores), "max": max(scores),
        "queries_with_identical_top10": top10_same, "queries_with_identical_top100_set": top100_same_set}

json.dump(report, open(OUT, "w"), indent=1)

for ds, d in report["datasets"].items():
    print(f"\n== {ds}: {d['queries']} test queries, {d['docs']} docs")
    for arm, v in d["ndcg@10"].items():
        extra = ""
        if arm in d["vs_hybrid"]:
            i = d["vs_hybrid"][arm]
            extra = f"   vs hybrid {i['vs_hybrid']:+.4f}  [95% {i['ci95'][0]:+.4f}, {i['ci95'][1]:+.4f}]"
        print(f"  {arm:34s} {'not scored' if v is None else format(v, '.4f')}{extra}")
for ds, d in report["datasets"].items():
    h = d.get("hybrid_repeats")
    if h:
        print(f"\n== {ds}: hybrid over {h['runs']} node restarts: nDCG@10 " + ", ".join(f"{x:.4f}" for x in h["ndcg@10"])
              + f"; identical top-10 on {h['queries_with_identical_top10']:.1%} of queries")
for tier, c in report["calibration"].items():
    p = c["test"]["pooled"]
    print(f"\n== calibration, local {tier}: temperature scale={c['temperature']['scale']:.4f}; "
          f"Platt scale={c['platt']['scale']:.4f} bias={c['platt']['bias']:.4f} "
          f"(fit on {c['fit_pairs']} held-out pairs, {c['fit_positive_rate']:.3f} positive)")
    for ds, t in c["test"].items():
        print(f"  {ds:9s} pairs={t['pairs']:6d} positive={t['positive_rate']:.3f}  ECE raw={t['ece_raw']:.4f}  "
              f"temperature={t['ece_temperature']:.4f}  Platt={t['ece_platt']:.4f}")
