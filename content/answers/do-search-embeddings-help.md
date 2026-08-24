---
title: "When are neural embeddings worth turning on?"
h1: "When should I turn on neural embeddings, and when is the lexical default enough?"
description: "Embeddings only helped when we opted into the neural embedder. On XERJ's lexical default, kNN promoted coffee storage above the synonyms for the query car."
slug: "do-search-embeddings-help"
cluster: "Embeddings: selection"
question: "Do I even need embeddings for search, or is full-text enough?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index a small judged corpus into one XERJ index, run each query as BM25 and as kNN on the default node, then restart the node with --embed-mode neural and run the same queries again. Report precision at 3 and every miss for all three modes."
commands:
  - cmd: "curl -s -XPUT 'http://127.0.0.1:9440/eval' -H 'content-type: application/json' -d '{\"mappings\":{\"properties\":{\"doc_id\":{\"type\":\"keyword\"},\"title\":{\"type\":\"text\"},\"text\":{\"type\":\"semantic_text\"}}}}'"
    note: "Create the judged index first. Without this step the queries below answer 404. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/v1/embedding/identity'"
    note: "Ask the node which embedder it uses. The default answer is backend lexical, 384 dimensions."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9440/eval/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"text\":\"car\"}},\"size\":3,\"_source\":[\"doc_id\",\"title\"]}'"
    note: "Run the BM25 baseline for one query before you compare anything."
  - cmd: "xerj --insecure --data-dir ./xerj-neural --embed-mode neural"
    note: "Start a node on the opt-in neural embedder with its own data directory. Stop the default node first, because both bind the same ports."
links_out:
  - "local-embeddings-without-openai-api"
  - "xerj-vs-vector-database"
  - "vector-database-vs-full-text-search"
  - "reduce-embedding-api-cost"
faq:
  - q: "Do I even need embeddings for search, or is full-text enough?"
    a: "Full-text is enough until the queries stop sharing tokens with the files. XERJ's default embedder is lexical feature hashing, and on our judged set it never recovered a synonym BM25 missed."
  - q: "When should I turn on neural embeddings?"
    a: "When paraphrases matter more than exact wording. On 8 judged queries, precision at 3 moved from 0.4167 to 0.6667 and recall from 0.7083 to 1.0 after `--embed-mode neural`."
  - q: "When should I combine keyword search and vector search?"
    a: "When both are partly right. The `hybrid` clause fuses a BM25 sub-query and a kNN sub-query, and weighted `linear` fusion was the strongest lexical-mode result at 0.4583."
  - q: "My embedding API bill is too high. How do I cut it?"
    a: "Run the embedder on the host. The default is lexical feature hashing inside the binary and calls nothing, and `--embed-mode neural` runs a MiniLM-class model on the CPU, so neither path bills per request."
  - q: "What is XERJ's default embedder?"
    a: "Signed feature hashing over word unigrams and character trigrams, into 384 dimensions. It has no model, no training and no semantics."
  - q: "What does the neural embedder cost?"
    a: "A downloaded MiniLM-class model, which occupied 88 MB in our machine's cache, CPU-only execution, and about 15 docs/s on short strings. It also makes an autoindex state non-resumable."
  - q: "Is BM25 enough on its own?"
    a: "BM25 alone scored 0.375 precision at 3 and 0.6875 recall on our set. It found every literal token and missed every synonym."
---

**TL;DR** — Embeddings help only when you opt into XERJ's neural embedder. The default embedder is lexical feature hashing, and on 8 judged queries it scored 0.4167 precision at 3 against BM25's 0.375. The opt-in flag `--embed-mode neural` moved the same measurement to 0.6667 and recall to 1.0.

## The honest answer

You buy synonym reach with embeddings, and XERJ's default embedder does not deliver it. The default embedder uses feature hashing over word unigrams and character trigrams, packed into 384 dimensions, with no model and no training. Pass `--embed-mode neural` at startup to load a downloaded MiniLM-class model instead.

Both modes answer the same `semantic` and `knn` requests, so the request shape gives no warning about which one is loaded. Ask the node directly with `GET /v1/embedding/identity`.

```json
{"data":{"backend":"lexical","dimensions":384,
  "semantic_contract":"semantic_text-derived-vector.v1","resumable":true}}
```

## What the default embedder did with synonyms

Our capture indexed 20 labeled documents into one index and ran 8 judged queries in five retrieval modes. On the query `car`, the default embedder ranked `Car servicing checklist` first, `Coffee bean storage` second and `Inverted index basics` third. Neither judged synonym appeared: not `Automobile maintenance schedule`, and not `Vehicle inspection rules`.

That result is not weak semantics. An earlier two-document probe on a default node scored `a canine barked loudly` at 0.5169 and `the automobile is red` at 0.5000 for the query `car`. The unrelated sentence outranked the exact synonym.

The same pattern appears twice more in our run. On `dog` the default embedder put `Car servicing checklist` in position 2. On `bicycle` it returned `Capacity planning` and `Backup verification`, and it missed `Cycling safety`.

## Every judged query, all three modes

Document ids are the fixture's own labels, and the full titles and misses are in the run summaries.

| query | judged relevant | BM25 top 3 | lexical kNN top 3 | neural kNN top 3 |
| --- | --- | --- | --- | --- |
| `car` | d01, d02, d03 | d02 | d02, d13, d09 | d01, d02, d03 |
| `automobile` | d01, d02, d03 | d01 | d01, d03, d11 | d01, d03, d02 |
| `dog` | d04, d05, d06 | d05 | d05, d02, d04 | d05, d04, d06 |
| `how does the index recover after a restart` | d07, d08 | d04, d08, d07 | d08, d04, d10 | d07, d08, d09 |
| `bicycle` | d10, d11 | d10 | d10, d15, d16 | d10, d11, d01 |
| `espresso grind size` | d12 | d12 | d12, d13, d05 | d12, d13, d17 |
| `tail latency` | d14 | d14, d07 | d14, d07, d15 | d14, d07, d15 |
| `quokka` | d17 | d17 | d17, d03, d09 | d17, d06, d08 |

## The measured difference

Same corpus, same queries, same judgments, k of 3. The only change is the embedder the node started with.

| mode | precision at 3, lexical default | recall, lexical | precision at 3, neural | recall, neural |
| --- | --- | --- | --- | --- |
| BM25 | 0.375 | 0.6875 | 0.375 | 0.6875 |
| `semantic` | 0.4167 | 0.7083 | 0.6667 | 1.0 |
| kNN | 0.4167 | 0.7083 | 0.6667 | 1.0 |
| hybrid `rrf` | 0.4167 | 0.7083 | 0.6667 | 1.0 |
| hybrid `linear` | 0.4583 | 0.7708 | 0.6667 | 1.0 |

BM25 is identical in both columns, which is the control this comparison needs. Every vector-backed mode gained. On the lexical default the best result came from weighted `linear` fusion, not from the vectors alone.

## What the neural embedder costs

The neural model is `all-MiniLM-L6-v2`, downloaded from HuggingFace Hub on first use and cached after that. The code pins the device to the CPU, and throughput is a listed weakness at about 15 docs/s on short strings.

The neural embedder also changes an operational property. `GET /v1/embedding/identity` reported `"resumable": false` under neural mode, with the reason that XERJ cannot content-address the model. Use a fresh autoindex state after you change the embedder.

## How to decide

Answer three questions in order.

1. Do your users type synonyms that your documents never contain?
2. Do you index few enough documents that about 15 docs/s is acceptable?
3. Can you accept a downloaded model with no XERJ-pinned checksum?

If all three answers are yes, start the node with `--embed-mode neural`. If any answer is no, run BM25 with weighted `linear` fusion. Treat the default vectors as a small tiebreaker, not a meaning layer.

## What this capture does not show

The judged set is 8 queries over 20 documents on one shared single-node host, and the content map asked for 12. Do not carry any quality conclusion past this fixture. Both run summaries publish every returned document and every miss, including the queries where the neural embedder also missed.
