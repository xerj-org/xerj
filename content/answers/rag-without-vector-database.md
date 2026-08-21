---
title: "RAG without a vector database"
h1: "How do I build RAG without a vector database?"
description: "Build a RAG retrieval layer on one XERJ index. One hybrid request fuses BM25 and kNN with Reciprocal Rank Fusion, and no second service runs."
slug: "rag-without-vector-database"
cluster: "Hybrid retrieval: architecture"
question: "How do I build RAG without a vector database?"
intent: "how-to"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start one single-node XERJ, create an index with a semantic_text field, bulk-load the corpus, then run one hybrid request with fusion rrf and report the returned ids and scores before you write any retrieval code."
commands:
  - cmd: "curl -s -XPUT 'http://127.0.0.1:9440/eval' -H 'content-type: application/json' -d '{\"mappings\":{\"properties\":{\"doc_id\":{\"type\":\"keyword\"},\"title\":{\"type\":\"text\"},\"text\":{\"type\":\"semantic_text\"},\"tags\":{\"type\":\"keyword\"},\"ord\":{\"type\":\"long\"}}}}'"
    note: "Create the one index that holds both the text and the dense vector. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/eval/_mapping'"
    note: "Read back the mapping, including the companion vector field XERJ adds for you."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/v1/embedding/identity'"
    note: "Name the embedder in use. The capture returned backend lexical, 384 dimensions."
links_out:
  - "reciprocal-rank-fusion-when-to-use"
  - "vector-database-vs-full-text-search"
  - "xerj-vs-vector-database"
  - "/docs/recipes/hybrid-search"
faq:
  - q: "Does XERJ need a separate vector database?"
    a: "No. XERJ stores the dense vector beside the inverted index in one index, and one `_search` request queries both. Our capture ran with no second service."
  - q: "What is the hybrid request shape?"
    a: "The `hybrid` clause lives inside `query`, and it takes a `queries` list of `{query, weight}` pairs plus a `fusion` name. A top-level `hybrid` key returns 400."
  - q: "Which fusion strategies exist?"
    a: "Two work: `rrf` with a default k of 60, and `linear` weighted scores. The third variant, `learned`, is rejected with a 400 at parse time."
  - q: "Is the default embedder semantic?"
    a: "No. The default embedder is lexical feature hashing with no model. Run the node with `--embed-mode neural` to get a downloaded MiniLM-class embedder."
  - q: "Can I send a query string to kNN?"
    a: "No. XERJ rejects `query_vector_builder` with a 400 and needs a literal float array. Index the query text and read the companion vector back out of `_source`."
  - q: "How many nodes does this need?"
    a: "One. XERJ is single-node, with no replication, no sharding and no failover, so this RAG layer is one process on one host."
---

**TL;DR** — XERJ holds the inverted index and the dense vector in one index, so a RAG retrieval layer needs no second service. One `hybrid` request fuses a BM25 sub-query and a kNN sub-query with Reciprocal Rank Fusion. Our 20-document capture returned `d08`, `d04` and `d10`.

## One index, both signals

XERJ stores the text and the embedding in the same index, so a RAG pipeline reads from one place. XERJ embeds a `semantic_text` field at write time and adds a companion `<field>_vector` field beside it. The default embedder is lexical feature hashing, not a neural model, and `--embed-mode neural` is the opt-in that changes that.

Our capture created one index named `eval` with a `semantic_text` field called `text`, then bulk-loaded 20 labeled documents. The `_bulk` response reported `"errors": false` and 20 created items. `GET /v1/embedding/identity` answered `"backend": "lexical"` with `"dimensions": 384`.

```json
{"mappings":{"properties":{
  "doc_id":{"type":"keyword"},
  "title":{"type":"text"},
  "text":{"type":"semantic_text"},
  "tags":{"type":"keyword"},
  "ord":{"type":"long"}}}}
```

## The hybrid clause is a XERJ extension

The `hybrid` clause is an extension that XERJ adds to the Elasticsearch DSL, and it lives inside `query`. Each entry in `queries` is a `{query, weight}` pair, and `fusion` names the strategy.

The Elasticsearch habit of a top-level `knn` key beside `query` fuses too, but only conditionally. XERJ folds that pair into one `hybrid` query and applies Reciprocal Rank Fusion. It does so only when the request carries none of `aggs`, `aggregations`, `sort`, `collapse`, `search_after`, `rescore`, `highlight`, `min_score`, `explain` or `profile`.

Send any of those and the request falls back to a lexical `bool.should`, where the kNN half contributes nothing. Several `knn` clauses beside a `query` are refused with 400. Writing the `hybrid` clause yourself carries no such condition.

Our capture sent a top-level `hybrid` key on purpose and recorded the refusal: `"Unknown key for a START_OBJECT in [hybrid]."` with HTTP 400.

This request is the shape that works. The empty array marks where the 384 floats go; paste in the vector you read out of `_source`.

```json
{"query":{"hybrid":{"queries":[
  {"query":{"match":{"text":"how does the index recover after a restart"}},"weight":1.0},
  {"query":{"knn":{"field":"text_vector","k":3,"num_candidates":50,"query_vector":[]}},"weight":1.0}],
  "fusion":"rrf"}},"size":3}
```

## What the fused response returned

The fused response for the query `how does the index recover after a restart` returned three documents in this order. Scores come straight from the captured response.

| rank | id | title | fused score |
| --- | --- | --- | --- |
| 1 | `d08` | `Checkpoint journal replay` | 0.032522473 |
| 2 | `d04` | `Canine behaviour notes` | 0.032522473 |
| 3 | `d10` | `Bicycle commuting` | 0.031257633 |

The BM25 sub-query alone ranked `d04` first and the kNN sub-query alone ranked `d08` first. Reciprocal Rank Fusion gave `d08` and `d04` the same fused score, and the response broke the tie in favor of `d08`.

## Fusion strategies that exist

XERJ ships three fusion names and only two of them work.

| `fusion` value | status | behavior |
| --- | --- | --- |
| `rrf` | works | Reciprocal Rank Fusion with a default k of 60 |
| `linear` | works | weighted combination of normalized scores |
| `learned` | rejected | HTTP 400 at parse time; there is no learned or trained fusion |

On our judged set the two working strategies did not score the same. Precision at 3 was 0.4167 for `rrf` and 0.4583 for `linear`, against a BM25 baseline of 0.375, all on the lexical default embedder.

## No second service runs

A process inventory taken during the memory run found one XERJ process and nothing else. The inventory line reads `none of postgres/qdrant/redis/weaviate/milvus/chroma/elasticsearch/opensearch is running`. The node listened on three loopback ports only: 8430 native REST, 8431 gRPC, and 9430 Elasticsearch-compatible.

A separate 50 ms sampler watched the retrieval node for its whole life and counted 0 non-loopback peers across 42 samples covering 3.51 s. That sampler polls `/proc/net/tcp`, so a connection that opens and closes inside one 50 ms gap escapes it. State that limit whenever you cite the number.

XERJ is single-node. There is no replication, no sharding, no failover and no object-store snapshot destination, so this RAG layer is one process on one host.

## The kNN vector must be literal

XERJ rejects the Elasticsearch `query_vector_builder` idiom with HTTP 400. No query-time text-embedding endpoint exists either. The captured response body carries this reason.

```text
parse error: `knn` requires `query_vector` (or `vector`) as a float array
```

The supported route has two steps. Index the query text into a `semantic_text` field, then read the companion `<field>_vector` value back out of `_source` and paste it into the kNN clause. Our harness uses exactly that route.

The captured request carried all 384 floats, of which only 39 were non-zero on the lexical default.

## What this capture does not show

The corpus is 20 documents on one shared host, so these numbers size a design, not a production plan. The evaluation ran one request per query per mode, so the latency figures are context for the response and not a benchmark. The run's own summary file lists every miss.
