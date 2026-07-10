# Zero-config folder → neural semantic search, in one binary

**Goal:** you have a folder of mixed files — Markdown postmortems, a plain-text
FAQ, NDJSON tickets, a CSV of plans — and you want it *searchable by meaning*
without writing a mapping, standing up an embedding service, or running a vector
database. Point one binary at the folder and ask it questions in plain language.

`xerj autoindex` walks the folder, sniffs each file's format, infers a schema,
and bulk-indexes it — turning prose bodies into `semantic_text` fields
automatically. When the server runs with the built-in **neural** embedder, those
bodies are embedded in-process by a real BERT model (all-MiniLM-L6-v2), so the
semantic search that follows is genuinely neural — no Python, no API keys, no
second system.

Runnable version: [`docs/examples/autoindex-semantic-search/autoindex_semantic.sh`](../examples/autoindex-semantic-search/autoindex_semantic.sh)
(self-contained shell; also at [`recipes/autoindex_semantic.sh`](../../recipes/autoindex_semantic.sh)).
The sample folder it indexes lives at [`demo/data/support-folder/`](../../demo/data/support-folder).

---

## 1. Start the server with the neural embedder

The only prerequisite for *real* semantics: build with the `neural` feature and
select it at startup. The model downloads once on first use, then runs from a
local cache (air-gap friendly via `embedding.local_model_dir`).

```bash
cd engine && cargo build --release -p xerj-server --features neural
target/release/xerj --insecure --data-dir ./data --embed-mode neural
```

Drop `--embed-mode neural` and everything below still works — it just uses the
built-in lexical embedder instead. Nothing else in the recipe changes.

## 2. Discover and index the folder — zero config

```bash
xerj autoindex ./demo/data/support-folder
```

```
autoindex: 5 files (0 MB) under demo/data/support-folder
phase A: sniffing + sampling 5 files…
phase A: 3 datasets inferred, 0 junk/skipped files
phase B: indexing 5 files with 8 workers

done in 0.1s — 3 datasets, 10 records live, 0 junk records
  ax-jsonl        4 docs      # tickets.ndjson  → structured records
  ax-csv          3 docs      # plans.csv       → structured records
  ax-txtprose     3 docs      # *.md + faq.txt  → semantic_text bodies
```

autoindex made a decision per file: the Markdown and text prose became an
`ax-txtprose` index with a **`semantic_text` body** (embedded on ingest); the
NDJSON and CSV became structured indices with typed, exactly-filterable fields.
`xerj autoindex map` prints the full schema and ready-to-send queries it inferred.

## 3. Search the discovered prose *by meaning*

The prose bodies are neural-embedded, so paraphrases with no shared vocabulary
still land on the right document.

```bash
curl -s -X POST localhost:9200/ax-txtprose/_search -H 'content-type: application/json' -d '{
  "query": { "semantic": { "field": "body",
    "query": "the site slowed down because we ran out of database connections", "k": 5 } },
  "size": 2 }'
```

```
    0.773  [incident-checkout-latency.md]  # Postmortem: checkout latency spike …
    0.715  [runbook-oncall.md]             # On-call runbook …
```

The postmortem never says "slowed down" or "site" — it talks about a payment
service "timing out" from "connection-pool exhaustion." Neural semantics bridges
the gap. (On the default lexical embedder this particular paraphrase would miss.)

## 4. The structured files stay exactly searchable

autoindex didn't force everything into vectors. The tickets kept their typed
fields, so exact filters and aggregations work with no embedding involved.

```bash
curl -s -X POST localhost:9200/ax-jsonl/_search -H 'content-type: application/json' -d '{
  "query": { "bool": { "filter": [
    { "term": { "status": "open" } }, { "term": { "priority": "high" } }
  ] } } }'
```

```
    Latency doubled for EU customers during peak hours   (team: platform)
```

---

## What you got, and from what

One binary, one command, and a folder of files nobody mapped by hand became:

- **prose you can search by meaning** — neural embeddings computed in-process,
- **structured records you can filter and aggregate exactly**, and
- **a machine-readable data map** (`xerj autoindex map`) describing all of it.

No embedding microservice, no vector database, no ETL. See
[Zero-config indexing](./zero-config-indexing.md) for autoindex internals
(format sniffing, resume/idempotency, hostile-input handling) and
[All-you-can-eat search](./all-way-search.md) for full-text + vector + hybrid on
a single hand-mapped corpus.
