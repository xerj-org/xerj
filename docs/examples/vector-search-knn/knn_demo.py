#!/usr/bin/env python3
"""
Vector similarity search (kNN) against a live XERJ node.

Use case: you have items (products, docs, images) each represented as an
embedding vector, and you want "find the N most similar to this one".

This script uses tiny, hand-made 4-dimensional vectors so the result is
100% reproducible and you can eyeball why each neighbor was returned.
The 4 dims are a toy "feature space":

    [ citrus , berry , engine , wheels ]

- Fruits score high on the fruit axes, ~0 on the vehicle axes.
- Vehicles score high on engine/wheels, ~0 on the fruit axes.

So a query near the "citrus" corner should return oranges/lemons first,
and never a truck. That's the recall check.

No external deps: Python 3 stdlib only (urllib + json).
"""
import json
import urllib.request
import urllib.error

BASE = "http://localhost:9484"
INDEX = "catalog"


def req(method, path, body=None):
    data = None
    headers = {}
    if body is not None:
        if isinstance(body, str):          # ndjson (bulk)
            data = body.encode()
            headers["Content-Type"] = "application/x-ndjson"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode())


def hits(resp):
    """(id, score, name, category, in_stock) for each hit, in ranked order."""
    out = []
    for h in resp["hits"]["hits"]:
        s = h["_source"]
        out.append((h["_id"], round(h["_score"], 4), s["name"], s["category"], s["in_stock"]))
    return out


# ── 1. Fresh index with a dense_vector field ───────────────────────
req("DELETE", "/" + INDEX)
status, _ = req("PUT", "/" + INDEX, {
    "mappings": {
        "properties": {
            "name":      {"type": "text"},
            "category":  {"type": "keyword"},
            "in_stock":  {"type": "boolean"},
            # dims MUST match the vectors you index. similarity defaults to
            # cosine (magnitude-invariant) — the right choice for most
            # normalized embeddings. Other options: l2, dot_product.
            "embedding": {"type": "dense_vector", "dims": 4, "similarity": "cosine"},
        }
    }
})
print("create index:", status)

# ── 2. Index a tiny catalog. Vectors are [citrus, berry, engine, wheels]
catalog = [
    ("1", "Navel Orange",      "fruit",   True,  [0.90, 0.10, 0.00, 0.00]),
    ("2", "Meyer Lemon",       "fruit",   True,  [0.95, 0.05, 0.00, 0.00]),
    ("3", "Blood Orange",      "fruit",   False, [0.85, 0.20, 0.00, 0.00]),
    ("4", "Strawberry",        "fruit",   True,  [0.10, 0.92, 0.00, 0.00]),
    ("5", "Blueberry",         "fruit",   True,  [0.05, 0.95, 0.00, 0.00]),
    ("6", "Pickup Truck",      "vehicle", True,  [0.00, 0.00, 0.90, 0.85]),
    ("7", "Sports Car",        "vehicle", False, [0.00, 0.00, 0.95, 0.80]),
    ("8", "Electric Sedan",    "vehicle", True,  [0.00, 0.05, 0.40, 0.88]),
]
lines = []
for _id, name, cat, stock, vec in catalog:
    lines.append(json.dumps({"index": {"_index": INDEX, "_id": _id}}))
    lines.append(json.dumps({"name": name, "category": cat, "in_stock": stock, "embedding": vec}))
status, resp = req("POST", "/_bulk?refresh=true", "\n".join(lines) + "\n")
print("bulk:", status, "errors=", resp.get("errors"))

# ── 3. Plain kNN: "find items most like a citrus fruit" ──────────────
query_vec = [0.92, 0.08, 0.00, 0.00]        # sits right in the citrus corner
status, resp = req("POST", "/%s/_search" % INDEX, {
    "knn": {"field": "embedding", "query_vector": query_vec, "k": 3, "num_candidates": 10}
})
print("\n=== kNN k=3, query ~citrus ===")
knn_hits = hits(resp)
for h in knn_hits:
    print("  ", h)

top3_ids = [h[0] for h in knn_hits]
top3_cats = [h[3] for h in knn_hits]
# RECALL CHECK: the 3 nearest MUST be the 3 citrus fruits (ids 1,2,3),
# and NO vehicle should appear.
assert set(top3_ids) == {"1", "2", "3"}, top3_ids
assert all(c == "fruit" for c in top3_cats), top3_cats
# Cosine similarity of near-identical direction should be ~1.0.
assert knn_hits[0][1] > 0.99, knn_hits[0]
print("  OK: top-3 are exactly the citrus fruits, no vehicles leaked in")

# ── 4. kNN + filter: same query, but only items in stock ─────────────
# To combine kNN with a filter, wrap the knn in a `bool` and put the
# filter in a sibling `filter` clause. bool.filter runs as a PRE-filter:
# XERJ restricts the candidate set, THEN ranks by vector similarity.
#
# "Blood Orange" (id 3) is the 3rd-closest citrus but is OUT of stock, so
# with the filter it drops out and a farther in-stock item (a berry)
# takes its place — proving the filter runs, not just the ranker.
#
# NOTE: the ES-8 shorthand `knn: {..., "filter": {...}}` is parsed but the
# filter is currently ignored on this path — use the bool wrapper below.
status, resp = req("POST", "/%s/_search" % INDEX, {
    "size": 3,
    "query": {
        "bool": {
            "must": [
                {"knn": {"field": "embedding", "query_vector": query_vec,
                         "k": 3, "num_candidates": 10}}
            ],
            "filter": [{"term": {"in_stock": True}}],
        }
    },
})
print("\n=== kNN + bool.filter in_stock:true ===")
filtered = hits(resp)
for h in filtered:
    print("  ", h)
filtered_ids = [h[0] for h in filtered]
assert all(h[4] is True for h in filtered), filtered           # every hit in stock
assert "3" not in filtered_ids, filtered_ids                   # out-of-stock citrus excluded
assert filtered_ids[:2] == ["1", "2"], filtered_ids            # two in-stock citrus rank first
print("  OK: out-of-stock Blood Orange excluded; only in-stock neighbors returned")

# ── 5. Query the other corner to show the space really separates ───────
status, resp = req("POST", "/%s/_search" % INDEX, {
    "knn": {"field": "embedding", "query_vector": [0.0, 0.0, 0.9, 0.85], "k": 2, "num_candidates": 10}
})
print("\n=== kNN k=2, query ~vehicle ===")
veh = hits(resp)
for h in veh:
    print("  ", h)
assert all(h[3] == "vehicle" for h in veh), veh
print("  OK: vehicle query returns only vehicles")

print("\nALL ASSERTIONS PASSED")
