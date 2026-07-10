# All-you-can-eat search: one corpus, five ways to retrieve it

**Goal:** you have one set of documents and different questions need different
kinds of retrieval — an exact error code here, a vague paraphrase there, a
"find more like this" over there, and sometimes all of it scoped to one
category. Most stacks make you bolt a vector database onto a search engine and
keep two systems in sync. XERJ indexes each document **once** and lets you
retrieve it as full-text, as semantics, as raw vectors, or as any blend —
from the same index, over the same wire protocol.

This recipe builds a 16-article help-center KB where the `body` field is
`semantic_text` (so XERJ embeds it on ingest and stores the vector next to the
text) and then answers five questions, one per retrieval mode, so you can see
exactly what each one is for.

Runnable version: [`docs/examples/all-way-search/all_way_search.py`](../examples/all-way-search/all_way_search.py)
(stdlib-only Python 3; also at [`recipes/all_way_search.py`](../../recipes/all_way_search.py)).

> **Embedder matters for three of the five modes.** Semantic, hybrid, and
> filtered-semantic are only as good as the server's embedding backend. The
> default is the built-in **lexical** embedder (offline, deterministic, but not
> neural). For the results below, start the server with the built-in **neural**
> BERT embedder — one flag, no change to the mapping or any query:
>
> ```bash
> # needs a binary built with --features neural; model downloads once
> xerj --insecure --data-dir ./data --embed-mode neural
> ```

---

## 1. One index, one mapping

`body` is the only field that needs anything special — `semantic_text` tells
XERJ to embed it at ingest and keep the vector alongside the text. `category`
stays a `keyword` so we can filter on it exactly.

```bash
curl -X PUT localhost:9200/helpdesk -H 'Content-Type: application/json' -d '{
  "mappings": { "properties": {
    "title":    { "type": "text" },
    "body":     { "type": "semantic_text", "dimensions": 384 },
    "category": { "type": "keyword" }
  }}
}'
```

Bulk-ingest the articles with a normal `_bulk` — no embedding step on your side,
no second datastore. XERJ embeds every `body` as it lands and exposes the vector
as `body_vector` in `_source`.

---

## 2. Full-text (BM25) — exact words win

A user pastes an error string. Only literal term matching reliably lands on the
document that actually contains `429`.

```json
POST /helpdesk/_search
{ "query": { "match": { "body": "HTTP 429 Too Many Requests" } } }
```

```
── full-text (BM25)  → nails the literal token '429'
    6.323  [performance]  Handling rate limits
    2.517  [       data]  Import records in bulk
    2.064  [performance]  Reduce tail latency
```

## 3. Semantic — meaning wins, even with zero shared words

The user asks how to "change the credentials my app uses to authenticate." The
right doc — *Rotating an API key* — never contains "change" or "credentials";
its body talks about a "fresh secret token." Full-text misses it; semantics
doesn't. `semantic` embeds the query with the same backend used at ingest and
runs kNN over the stored vectors.

```json
POST /helpdesk/_search
{ "query": { "semantic": { "field": "body",
    "query": "how do I change the credentials my app uses to authenticate", "k": 5 } } }
```

```
── semantic (embeddings)  → finds 'Rotating an API key' by meaning
    0.763  [       auth]  Rotating an API key
    0.748  [       auth]  Enable two-factor authentication
    0.658  [       auth]  Reset a forgotten password
```

## 4. Vector kNN — "more like THIS document"

No query text at all. Read one document's stored embedding and use it as the
query vector — pure nearest-neighbour over the same vectors ingest produced.
Great for "related articles" and dedup. (The seed matches itself at 1.0, so we
drop it and show the rest.)

```json
POST /helpdesk/_search
{ "knn": { "field": "body_vector",
           "query_vector": [ /* body_vector of 'Enable two-factor authentication' */ ],
           "k": 5, "num_candidates": 16 } }
```

```
── vector (kNN, more-like-this)  → nearest by vector distance
    0.730  [   auth]  Reset a forgotten password
    0.725  [   auth]  Rotating an API key
    0.618  [billing]  Update your payment method
```

The account-security siblings rise to the top; vector neighbours aren't perfectly
categorical, which is exactly why the next two modes exist.

## 5. Hybrid (RRF) — literal *and* conceptual at once

"I keep getting 429 responses — how should my client back off and retry" is part
literal (`429`) and part concept (backing off). BM25 alone drifts to *Pagination*
(it owns the word "slow"); semantics alone spreads across performance docs.
Fusing both with Reciprocal Rank Fusion pulls the doc that satisfies **both** —
*Handling rate limits* — back to the top.

```json
POST /helpdesk/_search
{ "query": { "hybrid": { "queries": [
    { "query": { "match": { "body": "...429...back off and retry" } }, "weight": 1.0 },
    { "query": { "semantic": { "field": "body", "query": "...429...back off and retry", "k": 10 } }, "weight": 1.0 }
] } } }
```

```
── hybrid (BM25 + semantic, RRF)  → literal '429' + the concept of backing off
    0.033  [performance]  Handling rate limits
    0.032  [ deployment]  Roll back a bad release
    0.032  [performance]  Pagination for large result sets
```

## 6. Filtered — semantics scoped to an exact facet

Retrieve by meaning, but only within `category = billing`. Put the keyword
constraint in the semantic clause's **inline `filter`** — this is how XERJ ANDs
an exact filter with vector scoring in a single request.

```json
POST /helpdesk/_search
{ "query": { "semantic": {
    "field": "body", "query": "I want my money back", "k": 10,
    "filter": { "term": { "category": "billing" } }
} } }
```

```
── filtered (semantic ∩ keyword)  → only billing docs, ranked by meaning
    0.750  [billing]  Request a refund
    0.612  [billing]  Update your payment method
    0.576  [billing]  Understanding prorated charges
```

> **Tip — filtering vector results.** Scope a vector/semantic query with the
> clause's own `filter` (as above), or wrap an explicit `knn` in
> `bool { must: [knn…], filter: [term…] }`. A bare `post_filter` or a `knn.filter`
> sibling is **not** the reliable path for narrowing embedding-scored hits on
> this engine — use the inline `filter`.

---

## Why this is one system, not two

Every mode above read the **same rows** in the **same index**. There is no
vector database to provision, no embedding service to run (the model is in the
binary), and no sync job to keep them consistent — the vector and the text are
written together at ingest and queried together. Pick the retrieval mode per
question; the storage is shared.

The default embedder is lexical and honest about it; `--embed-mode neural` (or
`--embed-mode proxy` for an external model) upgrades the semantic/hybrid/filtered
modes with no change to your mapping or queries. See
[Semantic search & RAG](./semantic-search-rag.md) for the embedder trade-offs
and [Hybrid search](./hybrid-search.md) for fusion tuning.
