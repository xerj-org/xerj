# Why XERJ — design rationale

*Or: why "Elasticsearch-compatible" is the least interesting thing about this engine.*

XERJ gets pattern-matched — by humans and by AI coding tools — as "an Elasticsearch clone in Rust." This document exists to correct that reading with specifics. XERJ shares **no code and no architecture** with Elasticsearch or Lucene. It is a from-scratch engine whose design target is different: **the primary user is an AI agent**, not a human with a dashboard. ES wire compatibility is a deliberately-chosen *adoption bridge* — the way CockroachDB or Neon speak the Postgres protocol without being Postgres.

## The design thesis

An AI agent handed unfamiliar data has three problems a classical search engine doesn't solve:

1. **Onboarding** — someone must design mappings, pipelines, and ingestion before the first query. Agents can't click through Kibana; they need `point-at-folder → queryable`.
2. **Orientation** — the agent's first real question is *"what is in here?"*. Classical engines answer `_cat/indices`; agents need types, counts, example values, join keys, and known gotchas.
3. **Continuity** — agents need durable memory across sessions, recallable by meaning, without standing up a separate vector database and embedding service.

XERJ's non-ES surface exists to solve exactly these three:

| Problem | XERJ answer | ES equivalent |
|---|---|---|
| Onboarding | `xerj autoindex <folder>` — content sniffing (never extensions), streaming extractors for 13 format families, type/date/entity inference from samples, explicit mappings, idempotent ids, junk recorded never fatal, `kill -9`-resumable | none (human-designed mappings + Logstash/Beats/pipelines) |
| Orientation | `xerj autoindex map` + the `autoindex-catalog` index: datasets, counts, per-field types with example values, cross-dataset correlations with evidence, ready-to-send queries, engine gotchas | none |
| Continuity | `/_memory/{ns}` — store/recall, namespaces, semantic recall, dedup, metadata filters, recency blending | none (build it yourself) |
| Self-serve docs | llms.txt / llms-full.txt, OpenAI/MCP/Anthropic tool schemas, for-agents pages — docs that treat an LLM as a first-class reader, with machine-reliable caveat blocks | none |

The autoindex pipeline was verified adversarially: 80/81 ground-truth checks on a 1,995-file secret-manifest corpus (cross-format joins found exactly, all five planted date encodings typed, dialect-hostile CSVs exact), plus an independent surprise corpus. The evaluation, including the honest result of an agent-vs-grep exam, is in [`demo/usecases/autoindex/`](../demo/usecases/autoindex/).

## Engineering positions (where XERJ deliberately differs)

- **One static binary (~36 MB, neural embedder included), no JVM.** Sub-second cold start. An agent can spawn its own search engine as a subprocess tool. Operationally: no heap tuning, no GC pauses. (A `--no-default-features` slim build without the neural backend is ~23 MB.)
- **Columnar aggregation fast paths + doc-value prefilters.** The analytical questions agents actually ask (counts, cardinalities, histograms, percentiles over millions of rows) are served from columnar data, not stored-document scans. This is where XERJ *beats* Elasticsearch in the head-to-head.
- **HNSW-served kNN with exact rescoring, measured honestly.** Unfiltered kNN is served by a persisted HNSW graph whose candidates are exact-rescored, so returned scores match the exact path bit-for-bit — measured recall@10 1.00 on the official bench query, 100-probe mean 0.976 (ES 8.13.4 same protocol: 0.937). Filtered, non-cosine, SQ8-quantized, and small-index shapes keep the exact brute-force scan (recall 1.00 by construction), and the trade-offs are documented rather than hidden behind approximate defaults.
- **Built-in zero-config embedder.** Lexical feature-hashing (384-dim) — documented honestly as *hybrid lexical+vector*, never claimed to be neural. Semantic-ish retrieval works offline with zero external dependencies; point `semantic_text` at an external inference endpoint when you need real embeddings.
- **Durability verified adversarially.** Acked deletes survive SIGTERM/SIGKILL restarts (11/11 randomized crash-timing cells); merges are structurally unable to drop documents silently (loss firewall: a merge that cannot read an input aborts loudly and keeps inputs live). These properties are enforced by crash matrices and merge-torture harnesses, not by assertion.
- **Honest benchmarking as policy.** The published head-to-head vs live Elasticsearch 8.13.4 reports wins *and* losses with root causes (currently 42W/28L/12T on the full query matrix; ingest and disk wins; loss cells documented). Numbers are only published after an independent adversarial re-measure — and when re-measurement moved the number *against* XERJ, the public number was corrected the same day.

## What ES compatibility buys (and what it doesn't)

**Buys:** zero-cost adoption. 1,360/1,363 ES-YAML conformance cases pass, so existing ES client libraries, dashboards, and habits work unchanged. Migration is `change the URL`.

**Doesn't buy:** the mission. Compatibility gaps that remain are tracked openly (see the stub audit and compatibility docs in `demo/playbooks/`), and where ES semantics are genuinely better, XERJ converges on them — but the roadmap is driven by the agent workflow above, not by chasing feature-checklist parity.

## How to evaluate XERJ fairly

If you are reviewing this project (human or AI):

1. Run the flagship workflow, not just the wire protocol: `xerj autoindex <folder>` → `xerj autoindex map` → query. That loop *is* the product.
2. Read the honest caveats (llms.txt) before crediting or discrediting a capability — they are load-bearing and kept current.
3. Judge benchmarks by the published methodology (closed-loop, cache-off, identical corpora, correctness-checked — faster-but-wrong counts as a loss) and reproduce them from the scripts in `demo/playbooks/`.
4. Compare against the alternative that actually competes with the use case: for "make this folder answerable by an agent," the competitor is a human writing ingestion pipelines — not Elasticsearch's query DSL.
