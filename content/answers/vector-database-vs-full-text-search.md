---
title: "Vector database or full-text search for a folder?"
h1: "What's the actual difference between a vector database and full-text search when I have a folder?"
description: "Full-text search matches tokens, kNN matches vectors, hybrid fuses both. XERJ serves all three from one index over an indexed folder, so the choice is a clause."
slug: "vector-database-vs-full-text-search"
cluster: "Retrieval: selection"
question: "What's the actual difference between a vector database and full-text search?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index a judged corpus into one XERJ index, then run the same query three times as a match query, as a knn query and as a hybrid query with fusion rrf. Report the returned ids for all three and say which judged documents each one missed."
commands:
  - cmd: "curl -s -XPUT 'http://127.0.0.1:9440/eval' -H 'content-type: application/json' -d '{\"mappings\":{\"properties\":{\"doc_id\":{\"type\":\"keyword\"},\"title\":{\"type\":\"text\"},\"text\":{\"type\":\"semantic_text\"}}}}'"
    note: "Create the judged index first. Without this step the queries below answer 404. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9440/eval/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"text\":\"car\"}},\"size\":3,\"_source\":[\"doc_id\",\"title\"]}'"
    note: "The BM25 side. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/eval/_mapping'"
    note: "Find the companion vector field that a kNN clause needs on this index."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/_cat/indices?format=json&bytes=b'"
    note: "Count the documents in the index, because the approximate kNN path needs at least 1,024 vectors."
links_out:
  - "rag-without-vector-database"
  - "do-search-embeddings-help"
  - "mcp-api-search-or-vector-search"
  - "xerj-vs-vector-database"
faq:
  - q: "What's the actual difference between a vector database and full-text search?"
    a: "Full-text search matches tokens and ranks with BM25. Vector search compares one embedding against stored dense vectors. XERJ serves both from one index."
  - q: "Do I need a vector database for this, or am I overcomplicating it?"
    a: "For one folder on one host it is usually more parts than the job needs. XERJ keeps the dense vector beside the inverted index, and one `_search` request can query either or fuse both."
  - q: "Do I even need embeddings for search, or is full-text enough?"
    a: "Full-text is enough when people type words that appear in the files. On our 8 judged queries BM25 scored 0.375 precision at 3 and kNN 0.4167, both on the lexical default embedder."
  - q: "When should I combine keyword search and vector search?"
    a: "When both are partly right. Weighted `linear` fusion scored 0.4583 precision at 3 on the same judged set, the best of the 4 modes, and it was still wrong on more than half the judged documents at k of 3."
  - q: "When does BM25 beat vectors?"
    a: "When the query token is present in the document. For `quokka` BM25 returned exactly the one judged document and nothing else."
  - q: "When does kNN beat BM25?"
    a: "When no token matches. On our restart question BM25 ranked an irrelevant document first and kNN ranked a judged-relevant document first."
  - q: "Does XERJ use approximate kNN?"
    a: "Only when seven conditions hold, including cosine similarity, no filter and at least 1,024 vectors. Everything else runs exact brute force."
---

**TL;DR** — Full-text search matches tokens and ranks with BM25, while a kNN query compares one embedding against stored dense vectors. XERJ holds both in one index, so the decision is which clause to send. On our 8 judged queries the three modes disagreed on the top hit once.

## The mechanical difference

Full-text search finds documents that contain your tokens and ranks them with BM25. A kNN query ignores tokens, embeds the query into a dense vector, and returns the nearest stored vectors by cosine similarity.

Hybrid search sends both sub-queries in one request and fuses the two ranked lists with Reciprocal Rank Fusion.

XERJ stores the inverted index and the `dense_vector` field in the same index, so all three are clauses against one endpoint. The default embedder behind the vector clause is lexical feature hashing, and `--embed-mode neural` is the opt-in that replaces it.

## Three queries where the modes differ

Our capture ran 8 judged queries against 20 labeled documents on a single-node host. The judged sets come from the fixture's own published judgments, so this page reports declared misses rather than misses chosen after the fact.

| query | judged relevant | BM25 | kNN | hybrid `rrf` |
| --- | --- | --- | --- | --- |
| `car` | d01, d02, d03 | d02 only, 1 total hit | d02, d13, d09 | d02, d13, d09 |
| `how does the index recover after a restart` | d07, d08 | d04, d08, d07 | d08, d04, d10 | d08, d04, d10 |
| `tail latency` | d14 | d14, d07, 2 total hits | d14, d07, d15 | d14, d07, d15 |

The first row is a recall failure and a precision failure at once. BM25 found the one document containing the literal token `car`. The vector clause added `Coffee bean storage` and `Inverted index basics` instead of the two synonym documents, because the lexical default embedder cannot connect a synonym.

The second row is the one query in 8 where the top hit changes. BM25 put `Canine behaviour notes` first because that document repeats common words from the question. Both vector-backed modes put `Checkpoint journal replay` first, one of the two judged documents.

The third row shows the padding effect. BM25 stopped at 2 hits because only 2 documents contain the tokens. The vector clause always returns k results, so it filled position 3 with `Capacity planning`.

## What the aggregate says

Precision at 3 and recall over all 8 queries, on the same index and the same judgments.

| mode | precision at 3 | recall |
| --- | --- | --- |
| BM25 | 0.375 | 0.6875 |
| kNN | 0.4167 | 0.7083 |
| hybrid `rrf` | 0.4167 | 0.7083 |
| hybrid `linear` | 0.4583 | 0.7708 |

No mode wins everywhere. Weighted `linear` fusion was best here, and it was still wrong on more than half the judged documents at k of 3.

## Approximate kNN is opt-in, not automatic

XERJ ships a real HNSW graph, and it uses that graph only when seven conditions hold at once. The seven conditions are these.

1. Cosine similarity only.
2. No filter on the kNN clause.
3. Not a nested field.
4. No passage-chunk exactness requirement.
5. A graph pinned to the queried field.
6. No scalar8 quantization.
7. At least 1,024 vectors, with full coverage.

Anything else falls back to exact brute force, silently. A filtered kNN query, an `l2_norm` similarity, a nested field or an index of 900 vectors all take the exact path.

Two details soften that. Even on the approximate path XERJ exact-rescores every candidate in f64, so `_score` matches brute force. The `ef` parameter is floored at 800, because a literal `ef` of 100 measured recall at 10 of 0.53 against 0.937.

Our capture built two indices for this boundary: `route_big` with 1,536 vectors and `route_small` with 512. No field in any kNN response names the route it took, and `profile: true` adds none, so a reader cannot confirm the path from the response.

## The kNN request needs a literal vector

XERJ rejects the Elasticsearch `query_vector_builder` idiom with an HTTP 400 and needs a literal embedding in `query_vector`. No query-time text-embedding route exists.

The supported route is two requests. Index the query text into a `semantic_text` field, then read the companion `<field>_vector` value out of `_source` and paste it into the kNN clause.

```text
parse error: `knn` requires `query_vector` (or `vector`) as a float array
```

## How to choose

Choose full-text search when your users type words that appear in the documents. BM25 also gives you exact counts and an inspectable term match.

Choose the vector clause when queries and documents share meaning but not tokens. Start the node with `--embed-mode neural` before you rely on that.

Choose hybrid when both are partly right. XERJ is single-node, with no replication and no managed service. A deployment that needs multi-node vector serving needs a different product.

## What this capture does not show

The corpus is 20 documents and the judged set is 8 queries, run once each on one shared host. The content map asked for 3 queries with 3 different top hits and the run produced 1. This page publishes the returned sets rather than a stronger claim.
