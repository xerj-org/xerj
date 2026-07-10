#!/usr/bin/env python3
"""All-you-can-eat search: ONE corpus, retrieved five different ways.

XERJ indexes a document once and lets you query it as full-text, as
semantics, as raw vectors, or as any blend — no second system, no
re-indexing. This recipe builds a small developer-support knowledge base
(inline below, so the data is right in front of you) and answers questions
with each retrieval mode so you can see exactly what each one is good at:

  1. full-text  (BM25)      — exact words, error codes, identifiers
  2. semantic   (neural)    — meaning, even with zero shared vocabulary
  3. vector     (kNN)       — "more like THIS document" via stored vectors
  4. hybrid     (RRF)       — BM25 + semantic fused, best of both
  5. filtered   (bool)      — semantics scoped to an exact keyword facet

Every document goes into ONE index whose `body` field is `semantic_text`:
XERJ embeds it at ingest and stores the vector alongside the text, so all
five modes read the same rows.

Run it:
    xerj --insecure --data-dir ./data &          # start XERJ
    python3 recipes/all_way_search.py

For REAL neural semantics (recommended — modes 2/4/5 get much sharper),
start the server with the built-in BERT embedder instead of the default
lexical one (needs a binary built with `--features neural`):

    xerj --insecure --data-dir ./data --embed-mode neural

Everything below is identical either way — you pick the backend once, at
the server, and never touch the mapping or the queries.

Optional env:
    XERJ_URL   (default http://localhost:9200)
"""

import json
import os
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")
INDEX = "helpdesk"

# ── The corpus: 16 short help-center articles across six exact-match
#    facets. A couple are deliberate "traps" that separate the modes:
#      • doc a3 is ABOUT changing an API key but its body never says
#        "rotate" or "change" — only semantics finds it from a paraphrase.
#      • doc p1 owns the exact token "429" — only BM25 nails that literally.
CORPUS = [
    # id,   category,      title,                                  body
    ("a1", "auth",        "Reset a forgotten password",
     "If you can no longer sign in, open the account recovery link we email you and choose a new passphrase to regain access."),
    ("a2", "auth",        "Enable two-factor authentication",
     "Add a second verification step with an authenticator app so a stolen password alone cannot unlock the account."),
    ("a3", "auth",        "Rotating an API key",
     "To replace a compromised credential, generate a fresh secret token in the dashboard and revoke the previous one; the old value stops working immediately."),
    ("b1", "billing",     "Update your payment method",
     "Change the card on file from Settings › Billing before the next renewal to avoid an interrupted subscription."),
    ("b2", "billing",     "Understanding prorated charges",
     "When you upgrade mid-cycle we bill only the remaining days at the new rate, so the first invoice after a plan change looks unusual but is correct."),
    ("b3", "billing",     "Request a refund",
     "Money back is available within thirty days of a charge; we return the funds to the original card, which can take a few business days to appear."),
    ("p1", "performance", "Handling rate limits",
     "Each token may issue 1000 requests per minute. Exceeding the quota returns HTTP 429 with a Retry-After header; back off and retry after the window resets."),
    ("p2", "performance", "Reduce tail latency",
     "Enable connection pooling and cache hot query results at the edge so the slowest requests speed up under load."),
    ("p3", "performance", "Pagination for large result sets",
     "Fetching millions of rows in one call is slow and memory-hungry; page through results with a cursor so each response stays small and fast."),
    ("d1", "deployment",  "Zero-downtime rolling deploys",
     "Ship a new version without an outage by draining connections from old instances only after the replacements pass their health checks."),
    ("d2", "deployment",  "Configure environment variables",
     "Inject secrets and per-stage settings at boot instead of baking them into the image, so the same artifact runs in staging and production."),
    ("d3", "deployment",  "Roll back a bad release",
     "If a new version misbehaves, redeploy the previous known-good build; traffic shifts back within seconds and no data is lost."),
    ("s1", "security",    "Encryption in transit and at rest",
     "Traffic is protected with TLS on the wire, and stored data is scrambled with keys you can rotate, so a leaked disk reveals nothing usable."),
    ("s2", "security",    "Audit logs for compliance",
     "Every privileged action is recorded in a tamper-evident trail you can export for reviewers who need to prove who did what and when."),
    ("x1", "data",        "Export your data",
     "Download everything you have stored as newline-delimited JSON from the export console; large accounts receive a signed link when the archive is ready."),
    ("x2", "data",        "Import records in bulk",
     "Load many rows at once by streaming an NDJSON file to the bulk endpoint instead of sending one request per record."),
]


def call(method, path, body=None):
    """One tiny HTTP helper instead of a client library."""
    data, headers = None, {}
    if body is not None:
        if isinstance(body, str):
            data, headers["Content-Type"] = body.encode(), "application/x-ndjson"
        else:
            data, headers["Content-Type"] = json.dumps(body).encode(), "application/json"
    req = urllib.request.Request(f"{XERJ}{path}", data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())


def show(label, hits, note=""):
    print(f"── {label}{'  ' + note if note else ''}")
    for h in hits:
        src = h["_source"]
        print(f"   {h['_score']:6.3f}  [{src['category']:>11}]  {src['title']}")
    print()


def main():
    # ── 1. One index. `body` is semantic_text (auto-embedded at ingest and
    #       stored as body_vector); `category` is keyword for exact facets.
    try:
        call("DELETE", f"/{INDEX}")
    except urllib.error.HTTPError:
        pass
    call("PUT", f"/{INDEX}", {
        "mappings": {"properties": {
            "title":    {"type": "text"},
            "body":     {"type": "semantic_text", "dimensions": 384},
            "category": {"type": "keyword"},
        }}
    })
    lines = []
    for cid, cat, title, body in CORPUS:
        lines.append(json.dumps({"index": {"_index": INDEX, "_id": cid}}))
        lines.append(json.dumps({"title": title, "body": body, "category": cat}))
    call("POST", "/_bulk?refresh=true", "\n".join(lines) + "\n")
    n = call("GET", f"/{INDEX}/_count")["count"]
    print(f"indexed {n} help-center articles into `{INDEX}` — one index, five ways to search it")
    print("(semantic/hybrid/filtered quality tracks the server's --embed-mode: "
          "lexical by default, sharpest with `--embed-mode neural`)\n")

    # ── 2. FULL-TEXT (BM25): literal tokens win. The user pasted an error
    #       string; only exact-term matching reliably surfaces the doc that
    #       actually contains "429".
    q = "HTTP 429 Too Many Requests"
    hits = call("POST", f"/{INDEX}/_search",
                {"query": {"match": {"body": q}}, "size": 3})["hits"]["hits"]
    print(f"Q1 (full-text): {q!r}")
    show("full-text (BM25)", hits, "→ nails the literal token '429'")

    # ── 3. SEMANTIC (neural): meaning wins with NO shared words. "change my
    #       credentials" shares no vocabulary with doc a3's body, which talks
    #       about a "fresh secret token" — semantics still finds it.
    q = "how do I change the credentials my app uses to authenticate"
    hits = call("POST", f"/{INDEX}/_search",
                {"query": {"semantic": {"field": "body", "query": q, "k": 5}},
                 "size": 3})["hits"]["hits"]
    print(f"Q2 (semantic): {q!r}")
    show("semantic (embeddings)", hits,
         "→ finds 'Rotating an API key' by meaning (neural mode: no shared words needed)")

    # ── 4. VECTOR (kNN): "more like THIS document." Read one doc's stored
    #       embedding and use it as the query vector — no query text at all,
    #       pure nearest-neighbour over the same vectors ingest produced.
    #       The seed matches itself at 1.0, so we drop it and show the rest.
    seed_id = "a2"                                    # Enable two-factor auth
    seed = call("GET", f"/{INDEX}/_doc/{seed_id}")
    seed_vec = seed["_source"]["body_vector"]
    hits = [h for h in call("POST", f"/{INDEX}/_search",
            {"knn": {"field": "body_vector", "query_vector": seed_vec,
                     "k": 5, "num_candidates": 16},
             "_source": ["title", "category"]})["hits"]["hits"]
            if h["_id"] != seed_id][:3]
    print(f"Q3 (vector kNN): more like → {seed['_source']['title']!r}")
    show("vector (kNN, more-like-this)", hits, "→ nearest by vector distance: the account-security siblings lead")

    # ── 5. HYBRID (RRF): fuse BM25 + semantic so a query that is partly
    #       literal ('429') and partly conceptual ('back off and retry')
    #       gets the best of both, ranked by Reciprocal Rank Fusion. BM25
    #       alone drifts to "Pagination" (it owns the word "slow"); the
    #       fusion pulls "Handling rate limits" back to the top.
    q = "I keep getting 429 responses — how should my client back off and retry"
    hits = call("POST", f"/{INDEX}/_search", {"query": {"hybrid": {"queries": [
        {"query": {"match": {"body": q}}, "weight": 1.0},
        {"query": {"semantic": {"field": "body", "query": q, "k": 10}}, "weight": 1.0},
    ]}}, "size": 3})["hits"]["hits"]
    print(f"Q4 (hybrid RRF): {q!r}")
    show("hybrid (BM25 + semantic, RRF)", hits, "→ literal '429' + the concept of backing off")

    # ── 6. FILTERED: semantics, but scoped to an exact keyword facet. Same
    #       meaning query, restricted to category=billing with an INLINE
    #       filter on the semantic clause (this is how XERJ ANDs a keyword
    #       constraint with vector scoring in a single request).
    q = "I want my money back"
    hits = call("POST", f"/{INDEX}/_search", {"query": {"semantic": {
        "field": "body", "query": q, "k": 10,
        "filter": {"term": {"category": "billing"}},
    }}, "size": 3})["hits"]["hits"]
    print(f"Q5 (semantic + filter category=billing): {q!r}")
    show("filtered (semantic ∩ keyword)", hits, "→ only billing docs, ranked by meaning")


if __name__ == "__main__":
    main()
