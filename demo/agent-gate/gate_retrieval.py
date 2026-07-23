#!/usr/bin/env python3
"""Agent Gate — RETRIEVAL regime. See README.md for the anti-cheating rules.

Same discipline as gate.py, applied to the corpus shape where XERJ is expected
to WIN on tokens: a repository of small prose/code documents, no large record
streams. Questions are answered in the asker's own words, so a match requires
ranking, not lexical coincidence.

Correctness here is CONTENT, asserted before the run — a substring the true
answer must contain, never the file it lives in. Both paths are tuned:
  baseline : grep, capped, question-vocabulary only
  xerj     : ranked search, size:1, highlight, filter_path — the frugal shape
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

CORPUS = sys.argv[1] if len(sys.argv) > 1 else "/tmp/gate-prose"
XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")


class Ledger:
    def __init__(self):
        self.rows = []

    def add(self, task, path, out, seconds):
        self.rows.append({"task": task, "path": path, "bytes": len(out),
                          "seconds": seconds})

    def bytes_for(self, path, task):
        return sum(r["bytes"] for r in self.rows
                   if r["path"] == path and r["task"] == task)

    def seconds_for(self, path, task):
        return sum(r["seconds"] for r in self.rows
                   if r["path"] == path and r["task"] == task)


LEDGER = Ledger()
_CTX = {"task": None}


def sh(cmd):
    t0 = time.time()
    out = subprocess.run(["bash", "-c", cmd], capture_output=True, text=True,
                         cwd=CORPUS).stdout
    LEDGER.add(_CTX["task"], "baseline", out, time.time() - t0)
    return out


def es(path, body):
    t0 = time.time()
    req = urllib.request.Request(f"{XERJ}{path}", json.dumps(body).encode(),
                                 {"Content-Type": "application/json"})
    raw = urllib.request.urlopen(req).read()
    LEDGER.add(_CTX["task"], "xerj", raw.decode("utf-8", "replace"), time.time() - t0)
    return json.loads(raw)


# (question, grep-terms [question vocabulary only], required substring in answer)
QUESTIONS = [
    ("why was checkout unavailable in June",
     "checkout", "certificate"),
    ("why did search quality regress",
     "search", "stemming"),
    ("how do we fail the database over to the standby",
     "failover", "promote"),
    ("what causes connection pool exhaustion",
     "pool", "fan-out"),
    ("why must queue consumers be idempotent",
     "idempotent", "at-least-once"),
    ("where are secrets kept",
     "secrets", "vault"),
    ("how does the payment client avoid billing twice",
     "idempotency", "idempotency_key"),
]


def run_baseline(terms, needle):
    _CTX["task"] = needle
    # A cold agent greps the question's noun. It does NOT know the file, and it
    # must read enough context to confirm — so grep -i with a few lines around.
    out = sh(f"grep -rni '{terms}' . | head -8")
    return out, (needle.lower() in out.lower())


def run_xerj(question, needle):
    _CTX["task"] = needle
    d = es("/ax-*/"
           "_search?filter_path=hits.hits._source.ax_path,hits.hits.highlight",
           {"size": 1,
            "query": {"bool": {"should": [
                {"match": {"body": question}},
                {"match": {"text": question}}]}},
            "_source": ["ax_path"],
            "highlight": {"fields": {
                "body": {"fragment_size": 200, "number_of_fragments": 1},
                "text": {"fragment_size": 200, "number_of_fragments": 1}}}})
    hits = d.get("hits", {}).get("hits", [])
    if not hits:
        return "(no hits)", False
    hl = hits[0].get("highlight", {})
    frag = (hl.get("body") or hl.get("text") or [""])[0]
    out = f"{hits[0]['_source'].get('ax_path')}: {frag}"
    return out, (needle.lower() in frag.lower())


def main():
    rows = []
    for question, terms, needle in QUESTIONS:
        b_out, b_ok = run_baseline(terms, needle)
        x_out, x_ok = run_xerj(question, needle)
        rows.append({"q": needle, "b_ok": b_ok, "x_ok": x_ok})

    print("\n" + "=" * 80)
    print("AGENT GATE — RETRIEVAL regime (prose/code corpus)")
    print("=" * 80)
    print(f"\n{'question needle':<22}{'base':<7}{'xerj':<7}"
          f"{'base tok':>9}{'xerj tok':>9}{'base s':>8}{'xerj s':>8}")
    jb = jx = 0
    for r in rows:
        b = LEDGER.bytes_for("baseline", r["q"]) // 4
        x = LEDGER.bytes_for("xerj", r["q"]) // 4
        both = r["b_ok"] and r["x_ok"]
        if both:
            jb += b
            jx += x
        print(f"{r['q'][:22]:<22}"
              f"{'OK' if r['b_ok'] else 'MISS':<7}{'OK' if r['x_ok'] else 'MISS':<7}"
              f"{b:>9}{x:>9}"
              f"{LEDGER.seconds_for('baseline', r['q']):>8.2f}"
              f"{LEDGER.seconds_for('xerj', r['q']):>8.2f}")

    nb = sum(1 for r in rows if r["b_ok"])
    nx = sum(1 for r in rows if r["x_ok"])
    print(f"\ncorrect: baseline {nb}/{len(rows)}   xerj {nx}/{len(rows)}")

    print("\n-- tokens over jointly-correct tasks only --")
    if jb and jx:
        ratio = jb / jx
        v = (f"XERJ uses {ratio:.2f}x FEWER tokens" if ratio > 1
             else f"XERJ uses {1/ratio:.2f}x MORE tokens")
        print(f"   baseline {jb} tok   xerj {jx} tok   -> {v}")
    else:
        print("   no jointly-correct tasks — no savings claim possible")

    total = text = 0
    for root, _, files in os.walk(CORPUS):
        for f in files:
            n = os.path.getsize(os.path.join(root, f))
            total += n
            if f.endswith((".md", ".py", ".rs", ".js", ".html", ".txt")):
                text += n
    print("\n-- corpus composition --")
    print(f"   {total/1e3:.1f} KB total; {text/1e3:.1f} KB searchable prose/code "
          f"({100*text/max(total,1):.1f}%)")
    print("   This is the regime where retrieval savings appear: grep's candidate")
    print("   set grows with the corpus; ranked search returns one passage.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
