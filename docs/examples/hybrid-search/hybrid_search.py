#!/usr/bin/env python3
"""
Hybrid search on XERJ — keyword (BM25) + vector (kNN) fused in ONE query.

Scenario: a help-desk search box. A user types "reset password".
We show that:
  * pure BM25 misses a highly relevant article that shares no keywords
    with the query ("Regain entry to a locked-out account"),
  * pure kNN (top-k) misses a keyword-exact article whose topic vector
    is far from the query ("Password complexity and rotation policy"),
  * a single `hybrid` query surfaces BOTH — no second system, no
    client-side result stitching.

Vectors here are hand-authored 4-dim "topic" vectors so the demo is
deterministic. In production you'd fill dense_vector fields with a real
embedding model (or use XERJ's semantic_text auto-embedding).
Topic axes: [auth_recovery, security_policy, cooking, generic]
"""
import json
import sys
import urllib.request

BASE = "http://localhost:9485"
INDEX = "helpdesk"


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(
        BASE + path, data=data, method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(r) as resp:
        return json.loads(resp.read().decode())


def ids(res):
    return [h["_id"] for h in res["hits"]["hits"]]


def table(res):
    for h in res["hits"]["hits"]:
        print(f"    {h['_id']:4}  score={h['_score']:.4f}  {h['_source']['title']}")


# ---- 1. fresh index: a text field + a dense_vector field -----------------
try:
    req("DELETE", f"/{INDEX}")
except urllib.error.HTTPError:
    pass

req("PUT", f"/{INDEX}", {
    "mappings": {
        "properties": {
            "title": {"type": "text"},
            "vec":   {"type": "dense_vector", "dims": 4, "similarity": "cosine"},
        }
    }
})

# ---- 2. corpus: title (for BM25) + topic vector (for kNN) ----------------
DOCS = [
    ("d1", "Reset your password",                     [1.00, 0.15, 0.0, 0.10]),
    ("d2", "Regain entry to a locked-out account",    [0.95, 0.10, 0.0, 0.15]),
    ("d3", "Password complexity and rotation policy", [0.15, 1.00, 0.0, 0.10]),
    ("d4", "How to bake sourdough bread",             [0.00, 0.00, 1.0, 0.10]),
    ("d5", "Change your account password",            [0.85, 0.25, 0.0, 0.20]),
]

bulk = ""
for did, title, vec in DOCS:
    bulk += json.dumps({"index": {"_index": INDEX, "_id": did}}) + "\n"
    bulk += json.dumps({"title": title, "vec": vec}) + "\n"

r = urllib.request.Request(
    BASE + "/_bulk", data=bulk.encode(), method="POST",
    headers={"Content-Type": "application/x-ndjson"},
)
with urllib.request.urlopen(r) as resp:
    assert not json.loads(resp.read())["errors"], "bulk had errors"
req("POST", f"/{INDEX}/_refresh")

# The user's intent, in both modalities:
QUERY_TEXT = "reset password"
QUERY_VEC = [1.0, 0.20, 0.0, 0.10]        # "auth recovery" intent

# ---- 3a. Pure keyword (BM25) ---------------------------------------------
kw = req("POST", f"/{INDEX}/_search", {
    "size": 3,
    "query": {"match": {"title": QUERY_TEXT}},
})
print("Pure BM25  (match 'reset password'):")
table(kw)
kw_ids = ids(kw)

# ---- 3b. Pure vector (kNN, top-3) ----------------------------------------
# knn scores every doc that has the field by cosine; `size` is the cutoff the
# user actually sees, so size:3 == "show me the 3 nearest".
vec = req("POST", f"/{INDEX}/_search", {
    "size": 3,
    "query": {"knn": {"field": "vec", "query_vector": QUERY_VEC, "k": 3}},
})
print("\nPure kNN   (top-3 nearest topic vectors):")
table(vec)
vec_ids = ids(vec)

# ---- 3c. Hybrid — both, fused with Reciprocal Rank Fusion ----------------
hyb = req("POST", f"/{INDEX}/_search", {
    "size": 5,
    "query": {
        "hybrid": {
            "queries": [
                {"query": {"match": {"title": QUERY_TEXT}},                     "weight": 1.0},
                {"query": {"knn": {"field": "vec", "query_vector": QUERY_VEC, "k": 3}}, "weight": 1.0},
            ],
            "fusion": "rrf"
        }
    },
})
print("\nHybrid     (BM25 + kNN, RRF fusion):")
table(hyb)
hyb_ids = ids(hyb)

# ---- 4. Prove the point --------------------------------------------------
print("\n--- assertions ---")
# d2 is relevant (account recovery) but shares NO keywords -> BM25 can't see it
assert "d2" not in kw_ids, f"expected BM25 to miss d2, got {kw_ids}"
print("PASS  BM25 alone MISSES d2 (locked-out account: no shared keywords)")

# d3 has the keyword 'password' but its topic vector is far -> kNN top-3 drops it
assert "d3" not in vec_ids, f"expected kNN top-3 to miss d3, got {vec_ids}"
print("PASS  kNN alone MISSES d3 (password policy: topic vector too far)")

# hybrid recovers BOTH in one request
assert "d2" in hyb_ids and "d3" in hyb_ids, f"hybrid should surface d2 AND d3, got {hyb_ids}"
print("PASS  Hybrid surfaces BOTH d2 and d3 in a single query")

# and the exact double-match still ranks first
assert hyb_ids[0] == "d1", f"expected d1 first in hybrid, got {hyb_ids}"
print("PASS  d1 (keyword + vector match) still ranks #1 under fusion")

print("\nBM25 ids  :", kw_ids)
print("kNN  ids  :", vec_ids)
print("Hybrid ids:", hyb_ids)
print("\nOK")
