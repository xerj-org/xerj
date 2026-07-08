#!/usr/bin/env python3
"""passage_search.py — per-passage retrieval on long documents.

XERJ's ingest-time chunk-embedding pipeline: a ``semantic_text`` field
auto-embeds *each overlapping passage* of a long document, and a ``semantic``
query scores a document by its BEST-matching passage (max-sim) rather than a
single blurred whole-document vector. That is what lets a query about one topic
find a long document that only covers that topic in one section.

Real workload (deterministic, offline)
--------------------------------------
Index 40 real AI-engineering articles (``demo/data/ai_kb.ndjson``) as short
docs, PLUS one long "compendium" doc containing all 40 concatenated. Then run
each article's title as a query and ask: **can the long compendium — which
contains every topic as one passage — compete for the top ranks?**

* **per-passage** (``semantic_text`` + ``semantic``): the compendium's matching
  passage is scored undiluted, so it consistently lands near the top.
* **pooled** (``dense_vector`` + ``knn``): the compendium is a single vector
  averaging 40 topics, so any one query barely registers against it and it is
  buried below the 40 undiluted short docs.

Both arms use XERJ's own server-side embedder (the pooled baseline reuses
XERJ's ``<field>_vector`` read back out of ``_source``), so the only variable
is pooled-vs-per-passage. We report how often the compendium reaches the top-3.

Run:  python3 docs/examples/passage-retrieval/passage_demo.py
      (honors $XERJ_URL, default http://localhost:9200)
"""
import json
import os
import sys
import urllib.error
import urllib.request

URL = os.environ.get("XERJ_URL", "http://localhost:9200").rstrip("/")
HERE = os.path.dirname(os.path.abspath(__file__))
TOPK = 3


def _find_kb():
    """Locate demo/data/ai_kb.ndjson robustly.

    Honors $XERJ_KB if set; otherwise walks up from this file to the repo
    root that holds demo/data/ai_kb.ndjson (works no matter how deep the
    example lives under the tree)."""
    env = os.environ.get("XERJ_KB")
    if env:
        return env
    d = HERE
    for _ in range(8):
        cand = os.path.join(d, "demo", "data", "ai_kb.ndjson")
        if os.path.exists(cand):
            return cand
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    # Documented repo-relative fallback (docs/examples/<recipe>/ → repo root).
    return os.path.join(HERE, "..", "..", "..", "demo", "data", "ai_kb.ndjson")


KB = _find_kb()


def req(method, path, body=None, quiet=False):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(
        URL + path, data=data, headers={"Content-Type": "application/json"}, method=method
    )
    try:
        with urllib.request.urlopen(r) as resp:
            return json.load(resp)
    except urllib.error.HTTPError as e:
        if not quiet:
            sys.stderr.write(f"HTTP {e.code} {method} {path}\n{e.read().decode()[:500]}\n")
        raise


def recreate(index, mappings):
    try:
        req("DELETE", "/" + index, quiet=True)
    except urllib.error.HTTPError:
        pass
    req("PUT", "/" + index, {"mappings": {"properties": mappings}})


def rank_of(hits, doc_id):
    """1-based rank of doc_id in a hits list, or None if absent."""
    for i, h in enumerate(hits, 1):
        if h["_id"] == doc_id:
            return i
    return None


def main():
    with open(KB, encoding="utf-8") as fh:
        articles = [json.loads(line) for line in fh if line.strip()]
    if len(articles) < 10:
        sys.exit("need at least 10 KB articles")

    print(f"XERJ at {URL}")
    print(f"{len(articles)} short article docs + 1 long compendium of all {len(articles)}\n")

    recreate("kb-passages", {"body": {"type": "semantic_text"}})
    recreate("kb-pooled", {"vec": {"type": "dense_vector", "dims": 384, "similarity": "cosine"}})
    # Probe index: index a short string, read back XERJ's own embedding of it so
    # the pooled `knn` baseline is driven by the same server-side embedder.
    recreate("kb-probe", {"body": {"type": "semantic_text"}})

    def embed(text):
        req("PUT", "/kb-probe/_doc/q?refresh=true", {"body": text})
        return req("GET", "/kb-probe/_doc/q")["_source"]["body_vector"]

    # Short article docs into both arms. For a short (single-chunk) doc the
    # pooled companion IS the article's embedding, so the baseline is exact.
    for i, art in enumerate(articles):
        body = f"{art['title']}. {art['content']}"
        req("PUT", f"/kb-passages/_doc/a{i}?refresh=true", {"body": body})
        vec = req("GET", f"/kb-passages/_doc/a{i}")["_source"]["body_vector"]
        req("PUT", f"/kb-pooled/_doc/a{i}?refresh=true", {"vec": vec})

    # One long compendium containing every article as a passage.
    compendium = "\n\n".join(f"{a['title']}. {a['content']}" for a in articles)
    req("PUT", "/kb-passages/_doc/compendium?refresh=true", {"body": compendium})
    comp_src = req("GET", "/kb-passages/_doc/compendium")["_source"]
    n_passages = len(comp_src.get("body_vector_chunks", []))
    print(f"compendium embedded into {n_passages} passage vectors "
          f"(pooled into 1 whole-doc vector of {len(comp_src['body_vector'])} dims)\n")
    if n_passages < 5:
        sys.exit("FAIL: compendium did not produce multiple passage vectors")
    req("PUT", "/kb-pooled/_doc/compendium?refresh=true", {"vec": comp_src["body_vector"]})

    pp_topk = 0
    po_topk = 0
    pp_ranks = []
    po_ranks = []
    for art in articles:
        title = art["title"]
        sem = req("POST", "/kb-passages/_search", {
            "size": TOPK,
            "query": {"semantic": {"field": "body", "query": title, "k": 50}},
        })
        # Server-embed the query via the probe index for the pooled arm.
        qv = embed(title)
        pool = req("POST", "/kb-pooled/_search", {
            "size": TOPK,
            "knn": {"field": "vec", "query_vector": qv, "k": 50},
        })
        pr = rank_of(sem["hits"]["hits"], "compendium")
        qr = rank_of(pool["hits"]["hits"], "compendium")
        pp_ranks.append(pr)
        po_ranks.append(qr)
        pp_topk += pr is not None
        po_topk += qr is not None

    n = len(articles)
    pp_rate = pp_topk / n
    po_rate = po_topk / n
    print(f"compendium reached the top-{TOPK}:")
    print(f"  per-passage : {pp_topk}/{n}  ({pp_rate:.0%})")
    print(f"  pooled      : {po_topk}/{n}  ({po_rate:.0%})")

    if pp_rate < 0.75:
        sys.exit(f"FAIL: per-passage top-{TOPK} rate {pp_rate:.2f} < 0.75")
    if pp_rate <= po_rate:
        sys.exit(f"FAIL: per-passage ({pp_rate:.2f}) did not beat pooled ({po_rate:.2f})")
    print(f"\nOK: per-passage scoring let the long document compete on each of its "
          f"sections\n    ({pp_topk}/{n} top-{TOPK}); a single pooled vector managed "
          f"only {po_topk}/{n}.")


if __name__ == "__main__":
    main()
