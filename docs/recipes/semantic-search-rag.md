# Semantic search / RAG retrieval with XERJ — no separate vector DB

**Goal:** you have a knowledge base (support docs, product FAQs, policy pages) and
you want an LLM to answer questions *grounded* in it. The retrieval half of RAG —
"find the passages that mean the same thing as the user's question" — normally
means standing up a separate vector database, an embedding service, and glue code
to keep them in sync with your search index.

With XERJ you don't. You map one field as `semantic_text`, ingest your docs with a
normal `_bulk`, and XERJ **embeds each document on ingest** with a built-in,
zero-config embedder. Then a `semantic` query retrieves by meaning from the same
index you already full-text search. One engine, one wire protocol (Elasticsearch
8.x), no extra moving parts.

This recipe indexes a tiny KB, retrieves passages with paraphrased questions
(different words, same meaning), assembles the context bundle you'd hand an LLM,
and is honest about where the built-in lexical embedder stops and a real neural
endpoint takes over.

---

## 1. Map one field as `semantic_text`

```bash
curl -X PUT localhost:9200/kb -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "title": { "type": "text" },
      "body":  { "type": "semantic_text" }
    }
  }
}'
```

`body` is now auto-embedded. On ingest XERJ vectorizes the field's text into a
companion HNSW-indexed vector field (`body_vector`, 384 dims by default, cosine
similarity) — you never call an embedding API yourself. No `dense_vector`
declaration, no dimension bookkeeping, no second datastore.

## 2. Ingest with a plain `_bulk`

Nothing special — the embedding happens under the hood:

```bash
curl -X POST localhost:9200/_bulk -H 'Content-Type: application/x-ndjson' --data-binary '
{"index":{"_index":"kb","_id":"kb-1"}}
{"title":"Rotating an API key","body":"To change your API credentials, open Settings, choose Security, and click Regenerate token. The old token stops working immediately."}
{"index":{"_index":"kb","_id":"kb-2"}}
{"title":"Refund policy","body":"Customers can request their money back within 30 days of purchase. Refunds are issued to the original payment method within five business days."}
'
```

## 3. Retrieve by meaning with the `semantic` query

The user's question rarely uses your doc's exact words. A `match` query on
`"get my money back after buying"` returns **nothing** — none of those tokens are
in the "Refund policy" doc analyzed the same way. The `semantic` query retrieves
it anyway:

```bash
curl localhost:9200/kb/_search -H 'Content-Type: application/json' -d '{
  "size": 2,
  "query": { "semantic": { "field": "body", "query": "how do I regenerate my API token", "k": 2 } }
}'
```

Real response (trimmed):

```json
{
  "hits": {
    "hits": [
      {
        "_id": "kb-1",
        "_score": 0.6625697,
        "_source": {
          "title": "Rotating an API key",
          "body": "To change your API credentials, open Settings, choose Security, and click Regenerate token. The old token stops working immediately.",
          "body_vector": [ 0.0, 0.0, 0.0, ... ]
        }
      }
    ]
  }
}
```

`field` is the `semantic_text` field, `query` is the natural-language question,
`k` is how many nearest passages to pull from the vector index. Add a `filter`
(any XERJ query) to constrain candidates, or a `boost` to weight it inside a
`bool`.

> **`_source` note.** The companion `body_vector` rides along in `_source` on
> semantic hits (source filtering is applied on the lexical path but the vector
> field is re-attached on the retrieval path). It's just noise for RAG — read the
> fields you want (`title`, `body`) and ignore it when you build the LLM prompt.

## 4. The RAG context bundle

Retrieval done, RAG is just "concatenate the top passages into the prompt." Pull
`k` hits and format them with their ids so the model can cite them:

```
User question: how do I regenerate my API token

Retrieved context:
[kb-1] Rotating an API key
To change your API credentials, open Settings, choose Security, and click
Regenerate token. The old token stops working immediately.

[kb-5] Rate limits
The API permits 600 requests per minute per key. Exceeding the throttle returns
HTTP 429; back off and retry after the reset window.
```

Feed that block to your LLM as grounding context alongside the question. That's
the whole retrieval side of RAG — no vector DB in the diagram.

## 5. Results — paraphrases land on the right doc

Running five paraphrased questions (none share the doc's full wording) against
the 5-doc KB, each returns the correct passage at rank 1:

```
Q: 'how do I regenerate my API token'          -> kb-1 Rotating an API key           OK
Q: 'can I get my money back after buying'       -> kb-2 Refund policy                  OK
Q: "I forgot my login and can't sign in"        -> kb-3 Resetting a forgotten password OK
Q: 'which file formats are supported for uploads' -> kb-4 Supported file formats        OK
Q: 'how many calls per minute am I allowed'     -> kb-5 Rate limits                    OK

RESULT: 5/5 semantic retrievals returned the right doc at rank 1
```

For contrast, the same paraphrase through a keyword `match`:

```
Q: 'get my money back after buying'
   match    top -> NO HITS
   semantic top -> kb-2 Refund policy
```

## Honest limitation: the built-in embedder is *lexical*, not neural

The zero-config embedder is a **lexical** model — it maps overlapping and related
vocabulary into vector space. It shines when the question and the answer share
*some* terms (a synonym here, a rephrase there), which covers a lot of real
support/FAQ traffic. It is **not** a neural sentence embedder and won't capture
deep semantics with no lexical bridge.

Concretely, from this same KB:

```
Q: 'how do I rotate my API key'
   semantic top -> kb-5 Rate limits   (expected kb-1)
```

The doc is *titled* "Rotating an API key," but only its **body** is embedded, and
the body never uses the word "rotate" — so a pure "rotate" paraphrase misses. Two
honest takeaways:

- **Embed the text that carries the meaning.** If titles matter, index them into
  the `semantic_text` field too (or add a second `semantic_text` field and combine
  with `hybrid`), so the vocabulary the user might use is actually in the vector.
- **For production-grade semantics, wire an external embeddings endpoint.** XERJ
  will call any OpenAI-compatible `/v1/embeddings` service. Set
  `embedding.default_endpoint` in config, or point a field at it in the mapping
  via `inference_endpoint` / `inference_id`. The `semantic` query and the ingest
  path are identical — you just get neural-quality vectors instead of the built-in
  lexical ones. (Anomaly-style continuous re-embedding isn't a thing here;
  embedding happens at ingest time.)

Everything above is `semantic_text` + the built-in embedder — no external service
was configured for this recipe.

---

## Reproduce it yourself

Start XERJ on its default port (`9200`) and run the example — no keys, no
external services, stdlib Python only:

```bash
# 1. Start a throwaway XERJ (ES-compat wire on :9200 by default)
xerj --insecure --data-dir ./data

# 2. In another shell, run the demo (honors $XERJ_URL, default http://localhost:9200)
python3 docs/examples/semantic-search-rag/rag_demo.py
```

Point it at a non-default host/port with `XERJ_URL=http://host:port` (the older
`BASE` variable still works as an alias). The script (stdlib only — `urllib` +
`json`, no pip) creates the index, bulk-loads the KB, runs the five semantic
retrievals with assertions, prints the RAG context bundle, shows the
keyword-vs-semantic contrast, demonstrates the lexical-embedder miss, scores the
ranking with `_rank_eval`, and exits non-zero if any of the five retrievals
regress — so it doubles as a CI gate.

The built-in embedder is deterministic (offline lexical model), so every run
prints the same numbers:

```
bulk -> errors=False items=5
...
Q: 'how do I regenerate my API token'  top -> kb-1 'Rotating an API key' score=0.6626 OK
Q: 'can I get my money back after buying'  top -> kb-2 'Refund policy' score=0.5771 OK
Q: "I forgot my login and can't sign in"  top -> kb-3 'Resetting a forgotten password' score=0.6710 OK
Q: 'which file formats are supported for uploads'  top -> kb-4 'Supported file formats for upload' score=0.5926 OK
Q: 'how many calls per minute am I allowed'  top -> kb-5 'Rate limits' score=0.6171 OK

RESULT: 5/5 semantic retrievals returned the right doc at rank 1
```

Every score above is emitted by this run (the top `kb-1` score is
`0.6625697016716003`, shown to 4 dp); nothing is hardcoded. Point `XERJ_URL` at
real Elasticsearch with a `semantic_text`/inference setup to compare.

---

## Measuring ranking quality with `_rank_eval`

Once you have relevance judgements (which docs *should* come back for a
query), the `_rank_eval` API scores your ranking with the standard IR
metrics — the same wire API as Elasticsearch. XERJ computes each metric for
real (they are **not** aliases for precision): `precision`, `recall`, `dcg`
(add `"normalize": true` for nDCG), `mean_reciprocal_rank`, and
`expected_reciprocal_rank`. An unknown metric name returns `400` rather than
a misleading number.

Scoring the regenerate-token ranking against this KB — `kb-1` is the one
relevant answer, so with it at rank 1 the ranking is perfect:

```bash
curl -sX POST localhost:9200/kb/_rank_eval -H 'content-type: application/json' -d '{
  "metric": { "dcg": { "k": 3, "normalize": true } },
  "requests": [
    {
      "id": "regen_token_q",
      "request": { "query": { "semantic": { "field": "body",
                    "query": "how do I regenerate my API token", "k": 3 } } },
      "ratings": [
        { "_id": "kb-1", "rating": 3 },
        { "_id": "kb-5", "rating": 0 },
        { "_id": "kb-2", "rating": 0 }
      ]
    }
  ]
}'
# → { "metric_score": 1.0, "details": { "regen_token_q": { "metric_score": 1.0, ... } } }
```

Swap `"dcg"` for `"precision"`, `"recall"`, or `"mean_reciprocal_rank"` to score
the *same* ranking a different way — each returns its own value. These are the
figures the demo actually measures against this KB:

```
dcg (nDCG@3)          metric_score=1.0
precision@3           metric_score=0.3333333333333333
recall@3              metric_score=1.0
mean_reciprocal_rank  metric_score=1.0
unknown-metric -> HTTP 400 (fail-loud, not a misleading number)
```

Track whichever metric your product cares about as you tune embeddings, `k`, and
hybrid weights.
