# Hybrid search: keyword + vector in one query

## The problem

You run a help-desk search box. A user types **"reset password"**. Two
things are true at once:

- The best answer might share **no keywords** with the query. An article
  titled *"Regain entry to a locked-out account"* is exactly what they
  need, but BM25 never sees it — there's no lexical overlap.
- A keyword-exact article might be **off-topic**. *"Password complexity
  and rotation policy"* contains "password," so BM25 loves it, but a
  semantic ranker knows it's about policy, not recovery.

Keyword search (BM25) is precise on exact terms and useless on synonyms.
Vector search (kNN) captures meaning but drops exact-term matches whose
embeddings drift. The usual "fix" is to run **two systems** — a lexical
engine and a vector DB — then stitch and re-rank the results in your
application. That's two deployments, two writes on every ingest, and a
fusion step you have to build and maintain yourself.

## Why XERJ

XERJ indexes the text field and the `dense_vector` field in the **same
index**, and fuses BM25 and kNN in a **single `hybrid` query**. One
request goes out, one ranked list comes back — already fused with
Reciprocal Rank Fusion (RRF). No second datastore, no client-side
re-ranking.

## The solution

### 1. One index, two modalities

A `text` field for BM25 and a `dense_vector` field for kNN:

```json
PUT /helpdesk
{
  "mappings": {
    "properties": {
      "title": { "type": "text" },
      "vec":   { "type": "dense_vector", "dims": 4, "similarity": "cosine" }
    }
  }
}
```

> The vectors in this recipe are hand-authored 4-dim *topic* vectors so
> the demo is deterministic and you can see exactly why each result
> ranks where it does. In production you fill `vec` with a real embedding
> model — or skip the vectors entirely and use a `semantic_text` field,
> which auto-embeds on ingest. (XERJ's built-in embedder is lexical, good
> for demos and keyword-semantic overlap; point `inference_id` at an
> external `/v1/embeddings` endpoint for neural-quality vectors.)

### 2. The corpus

Each doc has a title (for BM25) and a topic vector (for kNN). Topic axes
are `[auth_recovery, security_policy, cooking, generic]`:

| id | title | vector | keyword "password"? | near query vector? |
|----|-------|--------|:--:|:--:|
| d1 | Reset your password | `[1.00, 0.15, 0, 0.10]` | yes | yes |
| d2 | Regain entry to a locked-out account | `[0.95, 0.10, 0, 0.15]` | **no** | yes |
| d3 | Password complexity and rotation policy | `[0.15, 1.00, 0, 0.10]` | yes | **no** |
| d4 | How to bake sourdough bread | `[0.00, 0.00, 1.0, 0.10]` | no | no |
| d5 | Change your account password | `[0.85, 0.25, 0, 0.20]` | yes | yes |

The user's intent in both modalities: text `"reset password"`, vector
`[1.0, 0.20, 0.0, 0.10]` (an "auth recovery" direction).

### 3. Watch each modality fail on its own

**Pure BM25** — `{"match": {"title": "reset password"}}`:

```
d1  score=0.5754  Reset your password
d3  score=0.2877  Password complexity and rotation policy
d5  score=0.2877  Change your account password
```

It **misses d2** entirely — "Regain entry to a locked-out account" has no
shared keywords, so it can never be a BM25 hit.

**Pure kNN** — `{"knn": {"field": "vec", "query_vector": [...], "k": 3}}`,
showing the 3 nearest:

```
d1  score=0.9994  Reset your password
d2  score=0.9971  Regain entry to a locked-out account
d5  score=0.9942  Change your account password
```

It surfaces d2 (great!) but **misses d3** — the password-policy article's
topic vector points at "security_policy," far from the query, so it falls
below the top-3 cutoff.

### 4. Hybrid recovers both — one query

```json
POST /helpdesk/_search
{
  "size": 5,
  "query": {
    "hybrid": {
      "queries": [
        { "query": { "match": { "title": "reset password" } }, "weight": 1.0 },
        { "query": { "knn": { "field": "vec",
                              "query_vector": [1.0, 0.2, 0.0, 0.1],
                              "k": 3 } }, "weight": 1.0 }
      ],
      "fusion": "rrf"
    }
  }
}
```

Real response (trimmed to `title`):

```json
{
  "hits": {
    "total": { "value": 5, "relation": "eq" },
    "max_score": 0.03278,
    "hits": [
      { "_id": "d1", "_score": 0.03278, "_source": { "title": "Reset your password" } },
      { "_id": "d5", "_score": 0.03200, "_source": { "title": "Change your account password" } },
      { "_id": "d3", "_score": 0.03150, "_source": { "title": "Password complexity and rotation policy" } },
      { "_id": "d2", "_score": 0.01613, "_source": { "title": "Regain entry to a locked-out account" } },
      { "_id": "d4", "_score": 0.01538, "_source": { "title": "How to bake sourdough bread" } }
    ]
  }
}
```

**d2** (only kNN found it) and **d3** (only BM25 found it) are now *both*
on the page, and **d1** — the doc that matched on *both* keyword and
vector — is still ranked #1. That's the whole point: fusion rewards
agreement between the two signals and still admits the strong
single-signal hits that either method alone would have dropped.

## How the fusion works

`"fusion": "rrf"` uses Reciprocal Rank Fusion: each sub-query contributes
`weight / (k + rank)` for a doc (default `k = 60`), summed across
sub-queries. It ranks by *position*, not raw score, so BM25's `~0.5`
scores and kNN's `~0.99` cosines never have to be normalized onto a
common scale — a persistent headache when you fuse two systems yourself.

Other `fusion` options:

- `"rrf"` or `{"type": "rrf", "k": 40}` — rank-based, the safe default.
- `"linear"` — weighted sum of the raw sub-query scores (use `weight` to
  tilt toward keyword or vector; requires comparable score scales).

Per-query `weight` lets you bias the blend, e.g. `1.5` on the `match`
sub-query to favor exact terms.

## Run it

No dependencies beyond Python 3 stdlib. Point it at a running XERJ:

```bash
# start XERJ (insecure, local data dir) on port 9485
xerj --insecure --data-dir /tmp/xerj-hybrid --config <(printf '[server]\nes_compat_port = 9485\n')

# in another shell
python3 docs/examples/hybrid-search/hybrid_search.py
```

Expected tail:

```
--- assertions ---
PASS  BM25 alone MISSES d2 (locked-out account: no shared keywords)
PASS  kNN alone MISSES d3 (password policy: topic vector too far)
PASS  Hybrid surfaces BOTH d2 and d3 in a single query
PASS  d1 (keyword + vector match) still ranks #1 under fusion

BM25 ids  : ['d1', 'd3', 'd5']
kNN  ids  : ['d1', 'd2', 'd5']
Hybrid ids: ['d1', 'd3', 'd5', 'd2', 'd4']

OK
```

## Notes & limits

- **`knn` in a query scores every doc that has the field**, ranked by
  cosine; `size` (or `from`+`size`) is the cutoff the user sees. So `k:3`
  plus `size:3` means "show the 3 nearest." For a hard candidate cap
  independent of `size`, keep `k` small — it's carried on the sub-query.
- **Score scales differ by fusion.** RRF returns small fused scores
  (`~0.01–0.03` here) — compare docs *within* a response, not against
  BM25/cosine absolutes.
- **Tie ordering is not guaranteed** for docs with near-identical fused
  scores (d3 and d5 differ by `0.00005` here), so exact positions among
  ties can vary run to run. Set membership — the point of this recipe —
  is stable.
- **Vector quality is your embeddings' job.** This recipe supplies
  vectors directly for a deterministic demo. For real semantic recall,
  generate `dense_vector` values with a production embedding model, or
  use a `semantic_text` field with an external `inference_id`.
