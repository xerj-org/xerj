#!/usr/bin/env python3
"""Which BEIR test queries get ZERO BM25 hits, and is that the corpus or the engine?

    python3 zero_hits.py http://localhost:9560 nfcorpus /path/to/beir/nfcorpus

For every zero-hit query it tokenises the query the way the `standard`
analyzer does (lowercased word tokens) and counts, straight from corpus.jsonl
and without asking the engine, how many documents contain each token as a whole
token, and how many contain its naive singular. A query whose tokens are all
absent from the corpus is a legitimate zero; anything else is an engine bug.
Read-only: `_search` only.
"""
import json
import re
import sys

from common import bm25_query, http, load_beir

url, index, data_dir = sys.argv[1], sys.argv[2], sys.argv[3]
queries, qrels = load_beir(data_dir)
word = re.compile(r"[A-Za-z0-9]+")
docs = [json.loads(l) for l in open(data_dir + "/corpus.jsonl")]
doc_tokens = [set(word.findall((d.get("title", "") + " " + d.get("text", "")).lower())) for d in docs]

empty = []
for qid in qrels:
    r = http(url, "POST", f"/{index}/_search", {"size": 1, "_source": False,
             "track_total_hits": True, "query": bm25_query(queries[qid])})
    if "_err" in r:
        raise SystemExit(f"{qid}: {r}")
    if not r["hits"]["hits"]:
        empty.append(qid)

print(f"test queries={len(qrels)}  zero-hit={len(empty)}")
legit = stem_only = engine_bug = 0
for qid in empty:
    terms = [t.lower() for t in word.findall(queries[qid])]
    exact = {t: sum(1 for s in doc_tokens if t in s) for t in terms}
    singular = {t: sum(1 for s in doc_tokens if t.endswith("s") and t[:-1] in s) for t in terms}
    if any(exact.values()):
        verdict = "ENGINE BUG: a query token IS in the corpus"
        engine_bug += 1
    elif any(singular.values()):
        verdict = "legitimate under `standard`; a stemming analyzer would match the singular"
        stem_only += 1
    else:
        verdict = "legitimate: no query token occurs in the corpus"
        legit += 1
    print(f"{qid:11s} {queries[qid]!r:22s} relevant={len(qrels[qid]):3d} "
          f"docs_with_token={exact} docs_with_singular={singular}  -> {verdict}")
print(f"absent={legit}  singular-only={stem_only}  engine-bug={engine_bug}")
