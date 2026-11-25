"""usage: compare.py <base.json> <new.json> [iterations] [seed]

Pairs two code_eval.py (or `xerj eval --json`) reports by qid. Per set and
metric: mean delta, wins/losses/ties, and a two-sided paired randomization
(sign-flip) p-value with a seeded PRNG, so the p-values repeat."""
import collections
import json
import random
import sys

a = json.load(open(sys.argv[1]))
b = json.load(open(sys.argv[2]))
iters = int(sys.argv[3]) if len(sys.argv) > 3 else 10000
seed = int(sys.argv[4]) if len(sys.argv) > 4 else 42
qa = {r["qid"]: r for r in a["queries"]}
qb = {r["qid"]: r for r in b["queries"]}
common = sorted(set(qa) & set(qb))
sets = collections.defaultdict(list)
for q in common:
    sets[q[0]].append(q)
    sets["all"].append(q)


def p_value(diffs):
    obs = abs(sum(diffs))
    if obs == 0:
        return 1.0
    rng = random.Random(seed)
    hits = 0
    for _ in range(iters):
        s = sum(d if rng.random() < 0.5 else -d for d in diffs)
        if abs(s) >= obs - 1e-12:
            hits += 1
    return (hits + 1) / (iters + 1)


for name, qs in sorted(sets.items()):
    out = {"n": len(qs)}
    for metric in ("ndcg", "recall", "mrr"):
        diffs = [qb[q][metric] - qa[q][metric] for q in qs]
        out[metric] = {
            "base": round(sum(qa[q][metric] for q in qs) / len(qs), 4),
            "new": round(sum(qb[q][metric] for q in qs) / len(qs), 4),
            "delta": round(sum(diffs) / len(qs), 4),
            "w/l/t": "%d/%d/%d"
            % (sum(d > 0 for d in diffs), sum(d < 0 for d in diffs), sum(d == 0 for d in diffs)),
            "p": round(p_value(diffs), 4),
        }
    print(name, json.dumps(out))
