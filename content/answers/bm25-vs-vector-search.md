---
title: "BM25 vs vector search: keyword vs semantic, side by side"
h1: "BM25 vs vector search: which one should you use?"
description: "BM25 matches the words in a query. Vector search matches meaning. Type a query into a live toy, see where each one fails, and read recorded benchmark numbers."
slug: "bm25-vs-vector-search"
cluster: "Search quality"
question: "BM25 vs vector search: which one should you use?"
intent: "comparison"
published: "2026-10-30"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, index a folder of my docs with xerj autoindex, then write 10 test questions (5 with exact names or error strings, 5 written in other words) and report which ones BM25, knn and hybrid each answered in the top 3."
answer: "BM25 vs vector search: which should you use? BM25 scores the exact words a document shares with the query, so it is best for names, codes and error strings. Vector search compares meaning, so it finds paraphrases. Most search over mixed text works best with both, merged as hybrid search."
intent_group: "hybrid search"
queries:
  - "bm25 vs vector search"
  - "keyword vs semantic search"
  - "bm25 vs embeddings"
  - "lexical vs semantic search"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/docs/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"connection refused\"}},\"size\":5}'"
    note: "A BM25 query. No model needed."
links_out:
  - "what-is-hybrid-search"
  - "vector-database-vs-full-text-search"
  - "hybrid-search-quality-measured-ndcg"
  - "how-xerj-combines-search"
  - "rag-without-vector-database"
faq:
  - q: "What is the difference between BM25 and vector search?"
    a: "BM25 counts shared words and weighs rare words higher. Vector search turns text into numbers with a model and returns the closest texts by meaning, even with no shared words."
  - q: "Is vector search always better than BM25?"
    a: "No. On the recorded BEIR SciFact run BM25 scored 0.6572 nDCG@10 and MiniLM vectors 0.6764. That is close. BM25 is also better at exact tokens like IDs and error codes."
  - q: "Why does BM25 return no results?"
    a: "BM25 only scores documents that share a word with the query. In the NFCorpus run, 25 of 323 queries shared no word with any document, so BM25 returned nothing for them."
  - q: "Does BM25 need a model or GPU?"
    a: "No. BM25 is arithmetic over word counts. Vector search needs an embedding model to turn both the documents and every query into vectors."
  - q: "What are k1 and b in BM25?"
    a: "k1 controls how fast repeated words stop adding score. b controls how much long documents are penalized. XERJ uses k1 = 1.2 and b = 0.75."
  - q: "Can I use both?"
    a: "Yes. That is hybrid search: run both and merge the ranked lists, often with Reciprocal Rank Fusion."
---

**TL;DR** BM25 finds documents that share words with your query and favors rare words. Vector search finds documents with a similar meaning, even with no shared words. Use BM25 for exact names and error codes, vectors for paraphrases, and both together when queries mix the two.

```visual
bm25-vs-vector-map
```

## The short version

| | BM25 (keyword) | Vector search (semantic) |
| --- | --- | --- |
| Matches on | Shared words | Closeness of meaning |
| Needs a model | No | Yes, an embedder |
| Exact IDs, error strings | Strong | Can blur near tokens |
| Paraphrases, synonyms | Misses them | Strong |
| Empty results | Yes, when no word is shared | Never; always returns the nearest |
| Shows why a hit matched | Yes, you can see the words | Not directly |

## Try it

Type a query, or pick one. The left column is real BM25. The right column is a tiny toy, so you can see the idea without a model.

```visual
bm25-vs-vector-lab
```

What to look for:

- `connection refused` is exact text from line 1. BM25 gives it a clear lead.
- `network down` and `bot remembers last week` share no word with any line. BM25 returns nothing. The vector side still finds a sensible line.
- The vector side always returns 3 lines, even when the third is a poor match. A vector search has no natural "no result".

## How BM25 scores a line

For each query word that appears in a line, BM25 adds:

`idf × tf × (k1 + 1) / (tf + k1 × (1 - b + b × length / average length))`

- `tf` is how often the word appears in the line.
- `idf` is high for rare words and low for common ones. "refused" is in one line, so it counts more than "server", which is in two.
- `k1 = 1.2` stops a repeated word from adding score forever.
- `b = 0.75` gives shorter lines a small boost.

Those are the values in XERJ. The toy above uses the same formula, and on these six lines it gives the same scores as a real XERJ node, to 4 decimals.

## How vector search scores a line

A model reads each text and outputs a list of numbers, a vector. Texts with a similar meaning get vectors that point the same way. The query is turned into a vector too, and the search returns the lines whose vectors point closest, measured by cosine similarity.

The toy uses 5 numbers per word that we set by hand. Real models such as all-MiniLM-L6-v2 output 384 numbers per text and learn them from data.

## Recorded numbers

Two public BEIR sets, run on one XERJ node with all-MiniLM-L6-v2.

| Dataset | BM25 nDCG@10 | Vector nDCG@10 | Hybrid RRF | Queries with no BM25 hit |
| --- | --- | --- | --- | --- |
| SciFact, 300 queries | 0.6572 | 0.6764 | 0.6993 | 0 |
| NFCorpus, 323 queries | 0.3016 | 0.3291 | 0.3448 | 25 |

The 25 empty NFCorpus queries are single words such as "okra" or "Zoloft" that appear in no document. Only the vector side could answer them. The [nDCG page](/answers/hybrid-search-quality-measured-ndcg) has the method.

## Which should you pick?

- Logs, code, configs, ticket IDs: start with BM25.
- Support questions, notes, chat history: add vectors.
- An agent that asks both kinds of questions: use [hybrid search](/answers/what-is-hybrid-search).

Our [vector database vs full text search](/answers/vector-database-vs-full-text-search) page covers the storage side of the same choice.

## How to try it with XERJ

Use the Copy the prompt button and give it to your coding agent. It installs XERJ, indexes a folder and tests both kinds of question. BM25 works with no setup. Start the node with `--embed-mode neural` to get real vectors from all-MiniLM-L6-v2. The model downloads once, about 90 MB, and runs on your machine.

## Sources checked

All checked on 2026-10-30.

- BM25 formula and defaults: `engine/crates/xerj-fts/src/bm25.rs` in the XERJ repository.
- Benchmark numbers: `benchmarks/beir-hybrid/results/scifact.txt`, `nfcorpus.txt` and `nfcorpus-lexical-gap.txt`.
- Toy scores: a local XERJ v1.0.0-rc.97 node, 6 queries on the six lines above.
