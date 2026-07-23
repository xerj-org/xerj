#!/usr/bin/env python3
"""Agent Gate — honest measurement of agent benefit. See README.md.

Two paths answer the same tasks:
  baseline : ordinary shell tooling (grep/awk/python/sqlite3)
  xerj     : the running engine over an autoindexed copy

Every byte either path would put into an agent's context is counted. Savings
are computed ONLY over tasks both paths answered correctly AND agreed on.

Each task returns (display, answer):
  display — what would enter the agent's context
  answer  — the single canonical value under test, or None when the task is
            judged by content rather than by a value

Correctness is decided two ways, never by which file an answer came from:
  * content tasks — an explicit assertion on the text, declared before the run
  * value tasks   — the two paths must INDEPENDENTLY produce the same value;
                    disagreement fails BOTH rather than trusting either
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

CORPUS = sys.argv[1] if len(sys.argv) > 1 else "/tmp/gate-corpus"
XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")


class Truncated(Exception):
    """A partial result presented as complete. Scored wrong, never fast."""


class Ledger:
    def __init__(self):
        self.rows = []

    def add(self, task, path, out, seconds):
        self.rows.append({"task": task, "path": path, "bytes": len(out),
                          "seconds": seconds})

    def bytes_for(self, path, task):
        return sum(r["bytes"] for r in self.rows
                   if r["path"] == path and r["task"] == task)

    def calls_for(self, path, task):
        return sum(1 for r in self.rows
                   if r["path"] == path and r["task"] == task)

    def seconds_for(self, path, task):
        return sum(r["seconds"] for r in self.rows
                   if r["path"] == path and r["task"] == task)


LEDGER = Ledger()
_CTX = {"task": None}


def sh(cmd):
    """Baseline step. Commands may use ONLY vocabulary from the question."""
    t0 = time.time()
    out = subprocess.run(["bash", "-c", cmd], capture_output=True, text=True,
                         cwd=CORPUS).stdout
    LEDGER.add(_CTX["task"], "baseline", out, time.time() - t0)
    return out


def es(path, body, filter_path=None):
    """A competent agent trims the response envelope; the baseline commands are
    written competently, so these must be too. Without `filter_path` every
    query drags ~150 bytes of `took`/`_shards`/`max_score` scaffolding that no
    agent needs — 185 bytes to deliver a single integer."""
    if filter_path:
        sep = "&" if "?" in path else "?"
        path = f"{path}{sep}filter_path={filter_path}"
    return _es(path, body)


def _es(path, body):
    t0 = time.time()
    req = urllib.request.Request(f"{XERJ}{path}", json.dumps(body).encode(),
                                 {"Content-Type": "application/json"})
    raw = urllib.request.urlopen(req).read()
    LEDGER.add(_CTX["task"], "xerj", raw.decode("utf-8", "replace"), time.time() - t0)
    d = json.loads(raw)
    if d.get("timed_out") is True:
        raise Truncated("engine reported timed_out=true — partial result")
    return d


def total_docs(index):
    req = urllib.request.Request(f"{XERJ}/{index}/_count")
    return json.load(urllib.request.urlopen(req))["count"]


TASKS = []


def tail_number(text):
    """The value after the final colon. Stripping all non-digits instead once
    turned 'u4242 events: 2' into '42422' and produced a phantom disagreement —
    the extractor must not be able to invent an answer."""
    m = re.search(r":\s*([\d.]+)\s*$", text.strip())
    return m.group(1) if m else None


def task(name, kind, question, check=None):
    def deco(fn):
        TASKS.append({"name": name, "kind": kind, "question": question,
                      "check": check, "fn": fn})
        return fn
    return deco


def has(*needles):
    def check(text):
        low = text.lower()
        return all(n.lower() in low for n in needles)
    return check


# ── tasks ────────────────────────────────────────────────────────────────────

@task("lookup", "lookup", "how many log events does user u4242 have?")
def _lookup(path):
    if path == "baseline":
        out = sh("python3 -c \"import json;n=0\n"
                 "for l in open('logs/app.jsonl'):\n"
                 "  if json.loads(l)['uid']=='u4242': n+=1\n"
                 "print('u4242 events:',n)\"")
        return out, tail_number(out)
    d = es("/ax-logs-app/_search",
           {"size": 0, "track_total_hits": True, "query": {"term": {"uid": "u4242"}}},
           filter_path="hits.total.value")
    n = d["hits"]["total"]["value"]
    return f"u4242 events: {n}", str(n)


@task("aggregate", "aggregate", "how many ERROR events per service?")
def _aggregate(path):
    if path == "baseline":
        out = sh("python3 -c \"import json,collections\n"
                 "c=collections.Counter()\n"
                 "for l in open('logs/app.jsonl'):\n"
                 "  d=json.loads(l)\n"
                 "  if d['level']=='ERROR': c[d['svc']]+=1\n"
                 "print(sorted(c.items()))\"")
        nums = re.findall(r"\d+", out)
        return out, str(sum(int(n) for n in nums))
    d = es("/ax-logs-app/_search",
           {"size": 0, "track_total_hits": True, "query": {"term": {"level": "ERROR"}},
            "aggs": {"s": {"terms": {"field": "svc", "size": 20}}}},
           filter_path="hits.total.value,aggregations.s.buckets.key,"
                       "aggregations.s.buckets.doc_count")
    buckets = d["aggregations"]["s"]["buckets"]
    # Coverage cross-check: buckets must account for every matching doc.
    if buckets and d["hits"]["total"]["value"] != sum(b["doc_count"] for b in buckets):
        raise Truncated("terms agg did not cover all matching docs")
    return (json.dumps({b["key"]: b["doc_count"] for b in buckets}),
            str(sum(b["doc_count"] for b in buckets)))


@task("concept", "concept", "why did search relevance drop?", has("stemming"))
def _concept(path):
    q = "why did search relevance drop"
    if path == "baseline":
        # Question vocabulary only — no prior knowledge of the answer's location.
        return sh(f"grep -rni 'relevance' . | head -8"), None
    d = es("/ax-docs,ax-code,ax-web/_search",
           {"size": 2, "query": {"bool": {"should": [
               {"match": {"body": q}}, {"match": {"text": q}}]}},
            "_source": ["ax_path"],
            "highlight": {"fields": {
                "body": {"fragment_size": 160, "number_of_fragments": 1},
                "text": {"fragment_size": 160, "number_of_fragments": 1}}}})
    out = []
    for h in d["hits"]["hits"]:
        hl = h.get("highlight", {})
        frag = (hl.get("body") or hl.get("text") or [""])[0]
        out.append(f"{h['_source'].get('ax_path')}: {frag}")
    return "\n".join(out), None


@task("join", "join", "how many enterprise customers have an open ticket?")
def _join(path):
    if path == "baseline":
        out = sh("python3 -c \"import sqlite3\n"
                 "c=sqlite3.connect('crm.db')\n"
                 "q='select count(distinct t.user_id) from tickets t join customers c'\n"
                 "q+=' on c.user_id=t.user_id where c.plan=\\\"enterprise\\\"'\n"
                 "q+=' and t.status=\\\"open\\\"'\n"
                 "print('enterprise with open ticket:',list(c.execute(q))[0][0])\"")
        return out, tail_number(out)
    # XERJ has no JOIN: the id set must round-trip through the client. That
    # cost is real and is deliberately counted against XERJ here.
    # No JOIN exists, so one side must round-trip through the client. Fetch the
    # SMALLER side (enterprise is ~5% of customers) and return keys only. The
    # remaining cost is inherent to the missing join and is counted honestly.
    ids = es("/ax-customers/_search",
             {"size": 0, "query": {"term": {"plan": "enterprise"}},
              "aggs": {"u": {"terms": {"field": "user_id", "size": 50000}}}},
             filter_path="aggregations.u.buckets.key")
    users = [b["key"] for b in ids["aggregations"]["u"]["buckets"]]
    d = es("/ax-tickets/_search",
           {"size": 0, "query": {"bool": {"filter": [
               {"term": {"status": "open"}}, {"terms": {"user_id": users}}]}},
            "aggs": {"u": {"cardinality": {"field": "user_id"}}}},
           filter_path="aggregations.u.value")
    n = d["aggregations"]["u"]["value"]
    return f"enterprise with open ticket: {n}", str(n)


@task("drilldown", "drilldown", "how many 502s did /api/checkout serve?")
def _drilldown(path):
    if path == "baseline":
        # NOTE: the obvious `logs/*.log` glob silently skips
        # access-2026-05-31.log.gz. The gate caught that as a disagreement
        # (394 vs 530) — a blind spot in the BASELINE, not in the engine.
        # Compressed input has to be handled explicitly; the engine handles it
        # transparently. Counted honestly here.
        out = sh("{ cat logs/access-*.log; zcat logs/access-*.log.gz; } | "
                 "grep ' 502 ' | grep -c '/api/checkout'")
        return out, out.strip() or None
    d = es("/ax-logs/_search",
           {"size": 0, "track_total_hits": True,
            "query": {"bool": {"filter": [{"term": {"status": 502}},
                                          {"term": {"path": "/api/checkout"}}]}}},
           filter_path="hits.total.value")
    n = d["hits"]["total"]["value"]
    return f"/api/checkout 502s: {n}", str(n)


@task("orient", "orient", "what datasets exist and what are their fields?",
      has("ax-logs"))
def _orient(path):
    if path == "baseline":
        return sh("ls -R . | head -30; head -c 300 logs/app.jsonl; "
                  "head -2 orders_eu.csv"), None
    d = es("/autoindex-catalog/_search",
           {"size": 20, "query": {"term": {"doc_kind": "dataset"}},
            "_source": ["index_name", "record_count", "time_field"]},
           filter_path="hits.hits._source")
    return json.dumps([h["_source"] for h in d["hits"]["hits"]]), None


# ── run ──────────────────────────────────────────────────────────────────────

def main():
    results = []
    for t in TASKS:
        row = {"task": t["name"], "kind": t["kind"]}
        answers = {}
        for path in ("baseline", "xerj"):
            _CTX["task"] = t["name"]
            try:
                display, answer = t["fn"](path)
                answers[path] = answer
                ok = t["check"](display) if t["check"] else True
                row[path] = {"ok": ok, "err": None}
            except Truncated as e:
                answers[path] = None
                row[path] = {"ok": False, "err": f"TRUNCATED: {e}"}
            except Exception as e:
                answers[path] = None
                row[path] = {"ok": False, "err": f"ERROR: {type(e).__name__}: {e}"}

        b, x = answers.get("baseline"), answers.get("xerj")
        if b is not None and x is not None:
            row["agree"] = (b == x)
            # Disagreement fails BOTH — the gate never picks a winner.
            if not row["agree"]:
                row["baseline"]["ok"] = row["xerj"]["ok"] = False
                row["disagreement"] = f"baseline={b!r} xerj={x!r}"
        else:
            row["agree"] = None  # content-judged task
        results.append(row)

    print("\n" + "=" * 82)
    print("AGENT GATE — correctness first; savings only where both paths are right")
    print("=" * 82)
    print(f"\n{'task':<12}{'kind':<11}{'base':<7}{'xerj':<7}{'agree':<8}"
          f"{'base tok':>9}{'xerj tok':>9}{'base s':>8}{'xerj s':>8}")
    jb = jx = 0
    for r in results:
        b = LEDGER.bytes_for("baseline", r["task"]) // 4
        x = LEDGER.bytes_for("xerj", r["task"]) // 4
        both = r["baseline"]["ok"] and r["xerj"]["ok"]
        if both:
            jb += b
            jx += x
        ag = {True: "yes", False: "NO", None: "n/a"}[r["agree"]]
        print(f"{r['task']:<12}{r['kind']:<11}"
              f"{'OK' if r['baseline']['ok'] else 'BAD':<7}"
              f"{'OK' if r['xerj']['ok'] else 'BAD':<7}{ag:<8}{b:>9}{x:>9}"
              f"{LEDGER.seconds_for('baseline', r['task']):>8.2f}"
              f"{LEDGER.seconds_for('xerj', r['task']):>8.2f}")

    nb = sum(1 for r in results if r["baseline"]["ok"])
    nx = sum(1 for r in results if r["xerj"]["ok"])
    print(f"\ncorrect: baseline {nb}/{len(results)}   xerj {nx}/{len(results)}")
    for r in results:
        if r.get("disagreement"):
            print(f"   DISAGREEMENT on {r['task']}: {r['disagreement']}"
                  f"  -> both scored wrong, no winner assumed")
        for p in ("baseline", "xerj"):
            if r[p]["err"]:
                print(f"   {r['task']}/{p}: {r[p]['err'][:140]}")

    print("\n-- savings, counted ONLY over tasks BOTH paths answered correctly --")
    if jb and jx:
        ratio = jb / jx
        verdict = (f"XERJ uses {ratio:.2f}x FEWER tokens" if ratio > 1
                   else f"XERJ uses {1 / ratio:.2f}x MORE tokens")
        print(f"   baseline {jb} tok    xerj {jx} tok    -> {verdict}")
    else:
        print("   no jointly-correct tasks — no savings claim is possible")

    total = text = 0
    for root, _, files in os.walk(CORPUS):
        for f in files:
            n = os.path.getsize(os.path.join(root, f))
            total += n
            if f.endswith((".md", ".py", ".rs", ".js", ".html", ".txt")):
                text += n
    print("\n-- corpus composition (decides the result more than the engine does) --")
    print(f"   {total/1e6:.1f} MB total; {text/1e6:.3f} MB searchable prose/code "
          f"({100*text/max(total,1):.2f}%)")
    print("   Retrieval savings scale with the PROSE fraction; analytics savings")
    print("   scale with record count. A report that hides this is not a report.\n")

    broken = [r for r in results
              if r["baseline"]["err"] or r["xerj"]["err"]]
    if broken:
        print(f"GATE UNTRUSTWORTHY — {len(broken)} task(s) produced no verifiable answer.")
        return 1
    print("GATE OK — the report is trustworthy. This says nothing about who won.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
