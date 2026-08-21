---
title: "How does XERJ combine BM25 and kNN?"
h1: "How does XERJ combine BM25 and kNN?"
description: "One hybrid request carries a BM25 sub-query and a kNN sub-query against one index, and Reciprocal Rank Fusion merges their ranked lists into a single result list."
slug: "how-xerj-combines-search"
cluster: "Hybrid search"
question: "How does XERJ combine BM25 and kNN?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, GET /_mapping on the target index to find the companion vector field, GET /v1/embedding/identity to learn which embedder the node loaded, then POST one hybrid request whose queries array holds a match sub-query and a knn sub-query with fusion rrf, and report the fused ranking beside the ranking each sub-query returns on its own."
commands:
  - cmd: "xerj autoindex ./docs --url http://127.0.0.1:9200 --prefix article-docs --progress plain"
    note: "Index the documentation into a named dataset with a semantic_text body field."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9200/article-docs-*/_mapping'"
    note: "Find the companion vector field name that the kNN sub-query has to name."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9200/v1/embedding/identity'"
    note: "Name the embedder before you read any fused ranking. A default node reports backend lexical."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/article-docs-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"hybrid\":{\"queries\":[{\"query\":{\"match\":{\"body\":\"how does the index recover after a restart\"}},\"weight\":1.0},{\"query\":{\"knn\":{\"field\":\"body_vector\",\"k\":3,\"num_candidates\":50,\"query_vector\":[]}},\"weight\":1.0}],\"fusion\":\"rrf\"}},\"size\":3}'"
    note: "The hybrid request itself. Fill query_vector with your own embedding; an empty array marks where the floats go."
links_out:
  - "reciprocal-rank-fusion-when-to-use"
  - "rag-without-vector-database"
  - "what-is-xerj"
  - "/docs/recipes/hybrid-search"
  - "/docs/playbooks/vector-search"
faq:
  - q: "How does XERJ combine BM25 and kNN?"
    a: "It sends both as sub-queries inside one `hybrid` clause against one index, then merges the two ranked lists with Reciprocal Rank Fusion. One request, one response, one result list."
  - q: "Does hybrid search need two indices?"
    a: "No. The inverted index and the dense vectors live in the same XERJ index, so both sub-queries read one index and no second service takes part."
  - q: "How are a BM25 score and a cosine score made comparable?"
    a: "They are not. Reciprocal Rank Fusion ignores both scores and uses rank position only, which is why the two scales never have to be normalized."
  - q: "What goes in the queries array?"
    a: "Any two or more sub-queries, each an object with a `query` and an optional `weight` that defaults to 1.0. A `match` and a `knn` is the usual pair."
  - q: "Do I have to supply the query vector myself?"
    a: "For a `knn` sub-query, yes. Send the floats in `query_vector`, using the same embedder the node indexed with, or the distances are meaningless."
  - q: "Will hybrid search find synonyms of my query terms?"
    a: "Not on a default node. The default embedder is lexical feature hashing, so fusing it with BM25 combines two lexical signals. Start the node with `--embed-mode neural` for paraphrase matching."
  - q: "Which fusion methods can I choose?"
    a: "`rrf` and `linear`. `rrf` uses rank position with a default k of 60; `linear` adds weighted scores and lets one signal dominate."
---

**TL;DR** — One `hybrid` request carries a BM25 sub-query and a kNN sub-query against a single index, and Reciprocal Rank Fusion merges their two ranked lists into one. Fusion reads rank position rather than score, so the BM25 and cosine scales never have to be normalized.

## Send both signals in one request

A `hybrid` clause takes a `queries` array, and every entry in it is an ordinary sub-query with an optional `weight`. XERJ runs each sub-query, collects each ranked list, and fuses the lists into the response.

```json
{"query":{"hybrid":{"queries":[
  {"query":{"match":{"body":"how does the index recover after a restart"}},"weight":1.0},
  {"query":{"knn":{"field":"body_vector","k":3,"num_candidates":50,"query_vector":[]}},"weight":1.0}],
  "fusion":"rrf"}},"size":3}
```

The empty array marks where the floats of your query vector go. Embed the query with the same embedder the node indexed with, or the kNN distances describe nothing.

## Both sub-queries read one index

The inverted index and the dense vectors live in the same XERJ index. A hybrid request therefore names one index and starts no second service. When `xerj autoindex` elects a `semantic_text` field for a prose column, it writes both.

Read `GET /<index>/_mapping` to find the companion vector field before you write the `knn` sub-query. The field name is what the sub-query has to carry. Guessing it returns an HTTP 400 rather than a silent empty result.

## Fusion uses rank position, not score

Reciprocal Rank Fusion scores each document as the sum of `weight / (k + rank)` over every list that returned it, with a default `k` of 60. A BM25 score and a cosine score are on different scales, and adding them would favour whichever list produces larger numbers.

Rank positions are comparable without normalization, which is the whole reason the fusion step exists. A document that appears in both lists collects a contribution from each, so agreement between the two signals moves it up.

The [Reciprocal Rank Fusion page](/answers/reciprocal-rank-fusion-when-to-use) works one fused score out by hand against a captured response.

## Which embedder the vector half uses

The vector sub-query searches whatever embedder the node started with. The default embedder is lexical feature hashing, so it matches terms and sub-words rather than meaning, and fusing it with BM25 combines two lexical signals.

Start the node with `--embed-mode neural` when you want the vector half to match a paraphrase. The route `GET /v1/embedding/identity` reports the backend the node actually loaded. Read it before you judge a ranking.

## When one signal alone is the better request

Send a plain `match` when the user typed an identifier, an error string or a quoted phrase. Exact terms are what BM25 is for. A fused list can push an exact match down behind a vector neighbour.

Send a plain `knn` when you already hold an embedding and the wording is expected to differ. Reach for `hybrid` when you do not know which of the two the query is. That is the common case for a question a person typed.

## What this page does not cover

The shapes above are a single-node configuration. XERJ has no replication and no failover, so nothing here describes multi-node behaviour.

This page publishes no latency, throughput or relevance measurement. It describes the request shape and the fusion rule, and a ranking quality claim would need a judged corpus that this page does not have.
