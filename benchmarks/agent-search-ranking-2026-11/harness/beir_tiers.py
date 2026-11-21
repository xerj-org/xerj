"""#3007 BEIR arms: plain multi_match vs `xerj search` default vs `--all-terms-first`.

usage: beir_tiers.py <url> <dataset_dir> <index> <xerj_binary> <out_tsv> [arms]
"""
import collections, json, math, os, subprocess, sys, time, urllib.request

URL, ROOT, IDX, BIN, OUT = sys.argv[1:6]
ARMS = (sys.argv[6] if len(sys.argv) > 6 else "plain,default,tiered").split(",")
K = 10
PREFIX = IDX.split("-", 1)[0]


def post(path, body):
    r = urllib.request.Request(URL + path, data=json.dumps(body).encode(), method="POST",
                               headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(r, timeout=120).read())


queries = {}
for line in open(f"{ROOT}/queries.jsonl"):
    d = json.loads(line)
    queries[d["_id"]] = d["text"]
qrels = collections.defaultdict(dict)
for i, line in enumerate(open(f"{ROOT}/qrels/test.tsv")):
    if i == 0:
        continue
    q, d, s = line.rstrip("\n").split("\t")
    qrels[q][d] = int(s)


def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0


def plain(q):
    r = post(f"/{IDX}/_search", {"size": K, "_source": False,
                                 "query": {"multi_match": {"query": q, "fields": ["title", "text"]}}})
    return [h["_id"] for h in r["hits"]["hits"]], None


ENV = dict(os.environ, XERJ_DISABLE_FEEDBACK="true", XERJ_URL=URL)


def cli(q, extra):
    out = subprocess.run([BIN, "search", "--json", "--disable-feedback", "--url", URL,
                          "--prefix", PREFIX, "-k", str(K), *extra, q],
                         capture_output=True, text=True, env=ENV, timeout=120)
    if out.returncode != 0 and not out.stdout.strip().startswith("{"):
        return [], None
    try:
        body = json.loads(out.stdout)
    except json.JSONDecodeError:
        return [], None
    tiers = body.get("_xerj", {}).get("all_terms_first")
    return [h["_id"] for h in body.get("hits", {}).get("hits", [])], tiers


RUN = {"plain": plain, "default": lambda q: cli(q, []),
       "tiered": lambda q: cli(q, ["--all-terms-first"])}
res = {a: [] for a in ARMS}
lat = {a: [] for a in ARMS}
r1 = {a: 0 for a in ARMS}
second = 0
with open(OUT, "w") as f:
    f.write("qid\tarm\tnDCG@10\tR@1\tms\tquery\n")
    for qid, rel in qrels.items():
        q = queries[qid]
        for a in ARMS:
            t0 = time.time()
            ranked, tiers = RUN[a](q)
            ms = (time.time() - t0) * 1000
            s = ndcg10(ranked, rel)
            hit1 = 1 if ranked and rel.get(ranked[0], 0) > 0 else 0
            if a == "tiered" and tiers and tiers.get("any_term", 0) > 0:
                second += 1
            res[a].append(s); lat[a].append(ms); r1[a] += hit1
            f.write(f"{qid}\t{a}\t{s:.6f}\t{hit1}\t{ms:.1f}\t{q.replace(chr(9), ' ')}\n")
n = len(qrels)
print(f"index={IDX} queries={n} load={open('/proc/loadavg').read().split()[:3]}")
for a in ARMS:
    l = sorted(lat[a])
    print(f"{a:8s} nDCG@10={sum(res[a]) / n:.4f}  R@1={r1[a] / n:.4f}  "
          f"p50={l[n // 2]:.1f}ms p95={l[int(n * .95)]:.1f}ms")
if "tiered" in ARMS:
    print(f"tiered: queries that needed the any-term fill = {second}/{n}")
