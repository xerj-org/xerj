"""Score every results/*.json the same way and write summary.csv + summary.json.

A question is a hit@k when a returned memory among the top k contains the
answer string (case-insensitive, word boundaries). For update and distractor
questions, "correct first" means the first returned memory that contains
either the answer or the stale/wrong answer contains the answer (and the
answer appears in the top k). Stdlib only.
"""
import json, os, re, glob, statistics, csv

HERE = os.path.dirname(os.path.abspath(__file__))
ds = json.load(open(os.path.join(HERE, "dataset.json")))
QS = {q["id"]: q for q in ds["questions"]}

def has(text, s):
    return re.search(r"(?<![\w-])" + re.escape(s.lower()) + r"(?![\w-])", (text or "").lower()) is not None

def first_rank(hits, s):
    for i, h in enumerate(hits):
        if has(h, s):
            return i + 1
    return None

def pct(a):
    return round(100.0 * sum(a) / len(a), 1) if a else None

def p(xs, q):
    xs = sorted(xs)
    if not xs:
        return None
    i = min(len(xs) - 1, max(0, int(round(q / 100.0 * (len(xs) - 1)))))
    return round(xs[i], 2)

rows = []
per_q = []
for f in sorted(glob.glob(os.path.join(HERE, "results", "*.json"))):
    r = json.load(open(f))
    k = r["k"]
    by_type = {}
    h1_all, h5_all = [], []
    upd_ok, dis_ok = [], []
    for qr in r["questions"]:
        q = QS[qr["id"]]
        hits = qr["hits"]
        ra = first_rank(hits, q["answer"])
        h1 = ra == 1
        hk = ra is not None and ra <= k
        h1_all.append(h1); h5_all.append(hk)
        by_type.setdefault(q["type"], []).append((h1, hk))
        ok = None
        if q["stale"]:
            rs = first_rank(hits, q["stale"])
            ok = ra is not None and (rs is None or ra <= rs)
            (upd_ok if q["type"] == "update" else dis_ok).append(ok)
        per_q.append({"system": r["system"], "config": r["config"], "id": q["id"], "type": q["type"],
                      "answer_rank": ra, "hit1": h1, "hitk": hk, "correct_first": ok})
    qms = [x["ms"] for x in r["questions"]]
    row = {
        "system": r["system"], "config": r["config"],
        "version": r["meta"].get("version"), "commit": r["meta"].get("commit"),
        "llm": r["meta"].get("llm") or "none", "embedder": r["meta"].get("embedder") or "none",
        "hit_at_1_pct": pct(h1_all), "hit_at_5_pct": pct(h5_all),
    }
    for t in ("exact", "paraphrase", "update", "distractor"):
        v = by_type.get(t, [])
        row[f"{t}_hit5_pct"] = pct([b for _, b in v])
    row["update_newest_wins_pct"] = pct(upd_ok)
    row["distractor_correct_first_pct"] = pct(dis_ok)
    row["write_ms_p50"] = p(r["write_ms"], 50)
    row["write_ms_p95"] = p(r["write_ms"], 95)
    row["query_ms_p50"] = p(qms, 50)
    row["query_ms_p95"] = p(qms, 95)
    row["ingest_total_s"] = r["ingest_total_s"]
    row["llm_calls_per_write"] = r["meta"].get("llm_calls_per_write")
    row["run_at"] = r["run_at"]
    rows.append(row)

os.makedirs(os.path.join(HERE, "out"), exist_ok=True)
if rows:
    with open(os.path.join(HERE, "out", "summary.csv"), "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        w.writeheader(); w.writerows(rows)
    with open(os.path.join(HERE, "out", "per_question.csv"), "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=list(per_q[0].keys()))
        w.writeheader(); w.writerows(per_q)
    json.dump(rows, open(os.path.join(HERE, "out", "summary.json"), "w"), indent=1)
for r in rows:
    print(f'{r["system"]:<22}{r["config"]:<22} h@1 {r["hit_at_1_pct"]:>5}  h@5 {r["hit_at_5_pct"]:>5}  ex {r["exact_hit5_pct"]:>5} para {r["paraphrase_hit5_pct"]:>5} upd-new {r["update_newest_wins_pct"]:>5} dis {r["distractor_correct_first_pct"]:>5}  w50 {r["write_ms_p50"]:>8} q50 {r["query_ms_p50"]:>8} q95 {r["query_ms_p95"]:>8}')
