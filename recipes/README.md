# XERJ recipes — real AI apps, real workloads

A handful of small, dependency-free programs that use XERJ the way an AI
application or agent actually would. Each recipe is a single Python file
(or a self-contained shell script) using only the standard library, runs
against a stock XERJ binary, and uses **real data that ships in this repo**
— no mock servers, no API keys, no `pip install`.

They all work **fully offline**: XERJ has a built-in deterministic
embedder, so `semantic_text` fields, `semantic` queries, and agent memory
all function with zero external services. Where a recipe can *optionally*
use a real neural embedding model or an LLM, it says so and reads the
endpoint from an environment variable.

## Run them

```bash
# 1. get a binary (or `cargo build --release -p xerj-server`)
curl -fsSL https://xerj.org/get | sh

# 2. start XERJ
xerj --insecure --data-dir ./data &

# 3. run any recipe
python3 recipes/semantic_search.py
python3 recipes/all_way_search.py   # BM25 + semantic + vector + hybrid + filter
python3 recipes/passage_search.py
python3 recipes/rag_app.py
python3 recipes/memory_agent.py     # run twice — memory persists
python3 recipes/log_anomaly.py
python3 recipes/anomaly_datafeed.py
python3 recipes/vector_quantization.py

# self-contained (starts its own neural server): folder → semantic search
recipes/autoindex_semantic.sh
```

For **real neural** semantics in the semantic/hybrid recipes, start the
server with `--embed-mode neural` (build with `--features neural`); the
default binary uses the lexical embedder. `autoindex_semantic.sh` does this
for you.

Every recipe honors `XERJ_URL` (default `http://localhost:9200`).

## The recipes

| File | What it shows | Real workload |
|---|---|---|
| [`semantic_search.py`](semantic_search.py) | BM25 vs semantic vs hybrid (RRF) retrieval on one `semantic_text` field | `demo/data/ai_kb.ndjson` — 40 hand-written AI-engineering articles |
| [`all_way_search.py`](all_way_search.py) | **One corpus, five ways:** full-text (BM25), semantic, vector kNN (more-like-this), hybrid (RRF), and semantic-scoped-by-keyword-filter — all from one index | 16-article help-center KB (inline in the script) |
| [`autoindex_semantic.sh`](autoindex_semantic.sh) | **Zero-config folder → neural semantic search:** `xerj autoindex` a mixed folder (MD/TXT/NDJSON/CSV), then search the discovered prose by meaning; structured files stay exactly filterable. Starts its own `--embed-mode neural` server | `demo/data/support-folder/` — 5 mixed-format files |
| [`passage_search.py`](passage_search.py) | **Per-passage** retrieval on long docs: `semantic_text` auto-embeds every overlapping chunk, so a long document competes on any one of its sections (98% top-3 vs 32% for a single pooled vector) | Same 40 articles + a long "compendium" of all 40 |
| [`rag_app.py`](rag_app.py) | A full RAG pipeline: HTML→text→chunk→index→hybrid-retrieve→answer, with a retrieval eval and citations | XERJ's own docs (`landing/docs/*.html`) — ~96 chunks over 23 pages |
| [`memory_agent.py`](memory_agent.py) | A triage agent with **persistent** long-term memory via the `/_memory` API: recall similar past incidents, decide, remember | Real OpenSSH attack capture (`engine/demo-data/ssh_one.ndjson`, logpai/loghub) |
| [`log_anomaly.py`](log_anomaly.py) | Statistical anomaly detection (`_ml`) finding brute-force spikes over a moving baseline, then attributing the worst hour | Same real SSH capture (655k events) |
| [`anomaly_datafeed.py`](anomaly_datafeed.py) | A **continuous** `_ml` datafeed: start it once, then a background scorer re-buckets the live index every few seconds and appends new anomaly records you poll — a second spike is detected with no second call | Synthetic per-minute CPU series with two injected spikes |

## What each one exercises in the engine

- **semantic_text auto-embedding** — documents embed at ingest with no
  config; `semantic` queries embed the question the same way.
- **ingest-time chunk-embedding** — long `semantic_text` values are split
  into overlapping passages and embedded per-passage; `semantic` scores a
  document by its best-matching passage (max-sim), not a pooled average.
- **hybrid queries** — `bool`/`match` fused with `semantic`/`knn` via
  Reciprocal Rank Fusion in one query tree.
- **Agent-Memory API** — `POST /_memory/{ns}`, `_recall`, backed by a
  real per-namespace index, so memory survives restarts.
- **`_ml` anomaly detection** — `PUT /_ml/anomaly_detectors/{id}` +
  `_score`: date-histogram buckets scored against a moving mean/stddev.
- **`_ml` continuous datafeeds** — `PUT /_ml/datafeeds/{id}` + `_start`/`_stop`
  and `GET /_ml/anomaly_detectors/{job}/results/records`: a background task
  re-scores a live index on a timer and stores newly-flagged buckets.
- **ordinary ES aggregations** — `terms`/`range` used alongside the AI
  features (attribution in the anomaly recipe).

## Honest notes

- The **default** built-in embedder is **lexical** (feature-hashing over
  word + character n-grams, L2-normalized), not neural. It captures
  vocabulary overlap well and is perfect for offline demos and CI, but it
  won't match true synonyms. For real neural semantics — with no change to
  any recipe — start the server with a different `--embed-mode`: `neural`
  (built-in in-process BERT, all-MiniLM-L6-v2, downloads once on first use;
  build with `--features neural`) or `proxy` (any OpenAI-compatible
  embeddings endpoint, set `[embedding] default_endpoint` in `xerj.toml`).
- `memory_agent.py` recalls with **text (BM25) relevance** scores
  (unbounded, higher = better) by default. Add `"semantic": true` to the
  `_recall` body to have XERJ embed the query server-side and recall by
  vector similarity (cosine, 0–1) with no client-side embedding, or pass
  your own `vector` for BYO-embedding kNN.
- The SSH capture is real production honeypot-style traffic, so it is
  dominated by attacks — which is exactly why the anomaly detector lights
  up. That is the real signal, not an artifact.
