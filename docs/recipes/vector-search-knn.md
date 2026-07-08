# Vector similarity search (kNN) with XERJ

**Goal:** you have a set of items — products, articles, images, support
tickets — and each one is represented by an *embedding*: a fixed-length
list of floats. You want the classic operation behind recommendations,
"more like this", dedup, and semantic retrieval: **given one vector, find
the N most similar items.**

XERJ answers this natively. It stores a `dense_vector` field, builds an
HNSW graph over the vectors, and serves the Elasticsearch-8 top-level
`knn` query — so the exact same request bodies (and ES client libraries)
work unchanged. No sidecar vector database, no separate service.

This recipe uses **tiny, hand-made 4-dimensional vectors** so every result
is reproducible and you can see *why* each neighbor came back. In
production your vectors come from a real embedding model (768, 1024,
1536 dims…) — the API is identical.

---

## The toy feature space

Real embeddings are opaque. To make kNN legible, we hand-build vectors in
a 4-axis "feature space":

```
[ citrus , berry , engine , wheels ]
```

Fruits load on the fruit axes and are ~0 on the vehicle axes; vehicles do
the reverse. So a query sitting in the *citrus* corner should return
oranges and lemons first and **never** a truck. That's our recall check.

| id | name           | category | in_stock | embedding                  |
|----|----------------|----------|----------|----------------------------|
| 1  | Navel Orange   | fruit    | true     | `[0.90, 0.10, 0.00, 0.00]` |
| 2  | Meyer Lemon    | fruit    | true     | `[0.95, 0.05, 0.00, 0.00]` |
| 3  | Blood Orange   | fruit    | **false**| `[0.85, 0.20, 0.00, 0.00]` |
| 4  | Strawberry     | fruit    | true     | `[0.10, 0.92, 0.00, 0.00]` |
| 5  | Blueberry      | fruit    | true     | `[0.05, 0.95, 0.00, 0.00]` |
| 6  | Pickup Truck   | vehicle  | true     | `[0.00, 0.00, 0.90, 0.85]` |
| 7  | Sports Car     | vehicle  | false    | `[0.00, 0.00, 0.95, 0.80]` |
| 8  | Electric Sedan | vehicle  | true     | `[0.00, 0.05, 0.40, 0.88]` |

---

## 1. Map a `dense_vector` field

```bash
curl -X PUT http://localhost:9200/catalog -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "name":      { "type": "text" },
      "category":  { "type": "keyword" },
      "in_stock":  { "type": "boolean" },
      "embedding": { "type": "dense_vector", "dims": 4, "similarity": "cosine" }
    }
  }
}'
```

Two things matter:

- **`dims`** must equal the length of every vector you index. Indexing a
  wrong-length vector is silently skipped from the ANN scan.
- **`similarity`** defaults to `cosine` (magnitude-invariant — the right
  default for most normalized embeddings). Other options: `l2`,
  `dot_product`. Scores come back as ES-normalized cosine in `[0, 1]`:
  identical direction ≈ `1.0`, orthogonal ≈ `0.5`.

Load the rows with the Bulk API (`{"index":{...}}` + source line pairs),
adding `?refresh=true` so they're searchable immediately.

---

## 2. Plain kNN — "find items most like a citrus fruit"

Put the query vector right in the citrus corner and ask for the 3 nearest:

```bash
curl -X POST http://localhost:9200/catalog/_search -H 'Content-Type: application/json' -d '{
  "knn": { "field": "embedding", "query_vector": [0.92, 0.08, 0.0, 0.0], "k": 3, "num_candidates": 10 }
}'
```

Real response (trimmed):

```json
{
  "hits": { "hits": [
    { "_id": "1", "_score": 0.9998569, "_source": { "name": "Navel Orange", "category": "fruit" } },
    { "_id": "2", "_score": 0.9997083, "_source": { "name": "Meyer Lemon",  "category": "fruit" } },
    { "_id": "3", "_score": 0.9948,    "_source": { "name": "Blood Orange", "category": "fruit" } }
  ] }
}
```

The three nearest are **exactly** the three citrus fruits, in order of
closeness, and no vehicle leaked in. `k` is how many neighbors you want
back; `num_candidates` is how many the graph search explores before
picking the top `k` (bigger = higher recall, more work).

Query the *other* corner and the space cleanly separates:

```bash
curl -X POST http://localhost:9200/catalog/_search -H 'Content-Type: application/json' -d '{
  "knn": { "field": "embedding", "query_vector": [0.0, 0.0, 0.9, 0.85], "k": 2, "num_candidates": 10 }
}'
# → Pickup Truck (1.0), Sports Car (0.9992) — only vehicles
```

---

## 3. kNN + a filter — "…but only what's in stock"

This is the request people actually get wrong. To constrain a kNN search
(price range, in-stock, tenant, language…), **wrap the `knn` in a `bool`
and put the constraint in a sibling `filter` clause.** `bool.filter` runs
as a **pre-filter**: XERJ narrows the candidate set first, *then* ranks
the survivors by similarity.

```bash
curl -X POST http://localhost:9200/catalog/_search -H 'Content-Type: application/json' -d '{
  "size": 3,
  "query": {
    "bool": {
      "must":   [ { "knn": { "field": "embedding", "query_vector": [0.92, 0.08, 0.0, 0.0], "k": 3, "num_candidates": 10 } } ],
      "filter": [ { "term": { "in_stock": true } } ]
    }
  }
}'
```

Real response (trimmed):

```
1  0.9999  Navel Orange   in_stock=True
2  0.9997  Meyer Lemon    in_stock=True
4  0.5969  Strawberry     in_stock=True
```

Blood Orange (id 3) is the 3rd-closest citrus but is **out of stock**, so
it drops out and a farther *in-stock* item (a berry) takes the slot. The
filter genuinely restricted the pool — it didn't just re-score.

> **Heads-up on the ES-8 shorthand.** Elasticsearch also lets you inline
> the filter *inside* the knn clause: `"knn": { …, "filter": {…} }`. XERJ
> parses that form but **currently ignores the inlined filter** on the
> top-level-`knn` path (it returns unfiltered neighbors). Until that's
> wired up, use the `bool` + `filter` wrapper shown above — it's the
> reliable way to get pre-filtered kNN today, and it's also how XERJ's own
> agent-memory recall builds filtered vector queries internally.

---

## Reproduce it yourself

Boot XERJ on its default port and run the self-checking script — stdlib
only, no pip, no network calls:

```bash
# 1. start a throwaway node (default ES-compat port 9200)
./engine/target/release/xerj --insecure --data-dir ./data

# 2. in another shell, from the repo root:
python3 docs/examples/vector-search-knn/knn_demo.py
```

The script reads its server URL from `XERJ_URL` (default
`http://localhost:9200`); the older `BASE` variable still works as an
alias. Point it at any node with `XERJ_URL=http://host:port python3 …`.

It creates the index, bulk-loads the 8-row catalog, runs all three
searches, and — beyond the pass/fail assertions — computes **recall@k
against a brute-force exact-cosine ground truth built in-process**, so
the "the result is exact" claim below is measured, not asserted. Exact
tail from a real run:

```
=== kNN k=3, query ~citrus ===
   ('1', 0.9999, 'Navel Orange', 'fruit', True)
   ('2', 0.9997, 'Meyer Lemon', 'fruit', True)
   ('3', 0.9948, 'Blood Orange', 'fruit', False)
  exact top-3 (brute-force cosine): ['1', '2', '3']
  recall@3 = 1.000 (3/3 of the true nearest returned)
  OK: top-3 are exactly the citrus fruits, no vehicles leaked in

=== kNN + bool.filter in_stock:true ===
   ('1', 0.9999, 'Navel Orange', 'fruit', True)
   ('2', 0.9997, 'Meyer Lemon', 'fruit', True)
   ('4', 0.5969, 'Strawberry', 'fruit', True)
  OK: out-of-stock Blood Orange excluded; only in-stock neighbors returned

=== kNN k=2, query ~vehicle ===
   ('6', 1.0, 'Pickup Truck', 'vehicle', True)
   ('7', 0.9992, 'Sports Car', 'vehicle', False)
  exact top-2 (brute-force cosine): ['6', '7']
  recall@2 = 1.000
  OK: vehicle query returns only vehicles

MEASURED recall vs brute-force exact across queries: 1.000 (exact)
ALL ASSERTIONS PASSED
```

The scores and recall are **deterministic for this 8-row corpus** —
every run prints the same numbers (`recall@3 = recall@2 = 1.000`). The
cosine scores above are the ES-normalized values `(1 + cos)/2`; unrounded
they are `0.99985695`, `0.99970835`, `0.99479961` for the citrus top-3.

---

## How the search works: HNSW

XERJ indexes vectors in an **HNSW** graph (Hierarchical Navigable Small
World) — the same family of ANN index Elasticsearch/Lucene use. Instead
of comparing your query against every stored vector (O(N), fine for our
8-row demo, painful at millions), HNSW builds a layered proximity graph
and greedily hops toward the nearest region, giving roughly O(log N)
search that stays fast as the corpus grows. XERJ builds with
`ef_construction = 200` for high recall; at query time `num_candidates`
controls the search-time breadth (the ANN analog of `ef_search`) — raise
it to trade latency for recall, lower it for speed.

Because it's *approximate*, ANN can in principle miss a true neighbor.
For a corpus this small the result is **exact** — the script above
measures **recall@k = 1.000** against a brute-force cosine ground truth
on every query, confirming HNSW returned the identical top-k. The knobs
above only start to matter at scale.

---

## Raw kNN vs. `semantic_text`: which do I use?

Both end up doing vector search. The difference is **who makes the
vectors.**

- **Raw `dense_vector` + `knn` (this recipe)** — *you* supply the
  embeddings, on both the ingest side and the query side. Use it when you
  already run an embedding model (OpenAI, a local sentence-transformer,
  a CLIP image encoder, your own recommender) and just need fast storage
  and nearest-neighbor retrieval. Maximum control; works for any modality,
  not just text.

- **`semantic_text` field + `semantic` query** — XERJ embeds text
  **for** you at ingest time and at query time, so you send plain strings
  and never touch a vector. Great when your data is text and you want
  semantic search with zero embedding plumbing.
  *Honest caveat:* the built-in embedder is **lexical, not neural** —
  excellent for demos and keyword-semantic overlap, but for
  production-grade semantic quality point XERJ at an external
  OpenAI-compatible `/v1/embeddings` endpoint
  (`embedding.default_endpoint`), after which both ingest and query use
  that higher-quality model.

Rule of thumb: **already have embeddings → raw `knn`. Have text and want
it embedded for you → `semantic_text`.** Need to blend keyword relevance
(BM25) with vector similarity in one request? Reach for the `hybrid`
query.

---

## Gotchas

- **`dims` must match your vectors exactly** — a length mismatch means the
  doc is skipped from the scan, not an error.
- **Filter a kNN with `bool` + `filter`**, not the inlined `knn.filter`
  (currently ignored on the top-level path — see §3).
- **`k` vs `num_candidates`**: `k` = results returned; `num_candidates` =
  how many the graph explores first. `num_candidates >= k`; raise it if
  recall matters.
- Scores are **ES-normalized cosine in `[0,1]`** (≈1.0 identical, ≈0.5
  orthogonal), not raw cosine in `[-1,1]`.
