# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-07-06 (against `v1.0.0-rc.1`, live-verified against `engine/target/release/xerj`).

## Shipping today (for context)

These are implemented and exercised by real API requests / the test suite / benchmarks:

- Elasticsearch REST wire compatibility (1,326 / 1,329 ES-YAML conformance cases).
- Full-text search (BM25) and the **26 publicly-documented query types** (`match_all`, `match_none`, `match`, `match_phrase`, `match_phrase_prefix`, `multi_match`, `term`, `terms`, `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`, `query_string`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`, `geo_distance`, `knn`, `semantic`, `hybrid`) — **all 26 parse and execute correctly on the live binary.**
- **~14 additional query types execute** beyond the documented 26: `combined_fields`, `match_bool_prefix`, `terms_set`, `intervals`, `function_score`, `script_score`, `distance_feature`, `rank_feature`, `geo_bounding_box`, `geo_polygon`, `geo_shape`, `more_like_this`, `pinned`, `wrapper`. The parser dispatches ~49 distinct type keys in total; roughly 38 run without a `400`. **Honest caveat:** not every dispatched type has correct ES semantics — several are stubs (see *Partial / in progress*), so "38 supported query types" describes the dispatch surface, not 38 types that are all semantically faithful.
- **Aggregations:** the 15 publicly-documented aggregations (`terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`, `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`, `missing`, `composite`) **plus ~15 more that execute with correct math** — `date_range`, `extended_stats`, `percentile_ranks`, `filters`, `geo_bounds`, `geo_centroid`, `geohash_grid`, `median_absolute_deviation`, `matrix_stats`, `rare_terms`, `adjacency_matrix`, `ip_range`, `global`, `sampler`, `diversified_sampler`, `top_hits` — and the full **pipeline family** (`avg_bucket`, `sum_bucket`, `max_bucket`, `min_bucket`, `stats_bucket`, `cumulative_sum`, `derivative`, `bucket_script`, `bucket_selector`, `moving_fn`). All verified live at `size:0`. (The README under-lists these — a docs gap, not a defect.)
- **Dense-vector kNN** (`knn` query and ES 8.x top-level `knn`): unfiltered kNN on a full-precision cosine field (≥1,024 docs) is served by a **persisted HNSW graph with exact rescoring** — measured recall@10 1.00 on the official bench query, 100-probe mean 0.976 (ES 8.13.4 same protocol: 0.937); filtered/nested kNN, non-cosine similarity, SQ8 fields, and small indexes run the exact brute-force scan (cosine mapped to `(1+cos)/2`). See "Landed since rc-2" below.
- **Hybrid search** — BM25 + kNN combined in a single request via the `hybrid` **query type**: `{"query":{"hybrid":{"queries":[{"query":…,"weight":…}, …],"fusion":"rrf|linear|learned"}}}`. RRF-fused union verified live. (See *Partial* for the ES-native top-level `query`+`knn` path, which does **not** fuse.)
- **Columnar storage** — the ZBS2 columnar block (per-column codec) with exactly **9 domain-aware encodings** (`BitsetEnum`, `DeltaTimestamp`, `PackedIp`, `UrlTemplate`, `Varint`, `Dictionary`, `RawString`, `Bitpacked`, `FixedPrecision`), ZSTD/LZ4 codecs, and SQ8 vector quantization — all real and wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` (Painless-style script writes applied) — all live-verified.
- **A single native binary** — ~23 MB (23,513,064 bytes) statically-linked, no JVM, sub-second cold start (readiness within ~100 ms).

## Landed in 1.0.0-rc.2

These three shipped in rc-2 (each conformance-gated at 1,326 / 1,329 and verified by real requests). Honest limitations are noted.

### 1. Auto-embed on ingest + a built-in embedder ✅ (rc-2)

`semantic_text` now works end to end with **zero external configuration**. Indexing a document into a `semantic_text` field auto-embeds its text (previously returned `405`), and the `semantic` query embeds the query text with the same embedder and runs kNN — no external service required. Live-verified: a `semantic_text` doc indexed with no embedder configured returned `201`, and a `semantic` query ranked the intended doc first. A configured external `/v1/embeddings` proxy is still used, at higher quality, when `embedding.default_endpoint` is set.

- **Limitation:** the **default** embedding mode is a deterministic **lexical** model (feature-hashed word unigrams + character trigrams, L2-normalized) — it captures vocabulary/sub-word overlap, not deep semantics. This is observable live: a vocabulary-sharing query out-scored a true paraphrase. Paraphrases that share vocabulary rank correctly; truly-synonymous text with no word overlap will not. For real neural semantics you have two drop-in upgrades with no mapping/query change: the built-in **neural** BERT embedder that ships in the binary (`--embed-mode neural`, downloads all-MiniLM-L6-v2 on first use — see "Neural embeddings" below), or the external `/v1/embeddings` **proxy** (`--embed-mode proxy` + `embedding.default_endpoint`).

### 2. Agent-memory REST API ✅ (rc-2)

A namespaced agent-memory API, backed by regular XERJ indices (reusing document + vector + BM25 + metadata-filter paths), working fully offline:
`POST /_memory/{ns}` (store), `POST /_memory/{ns}/_recall` (kNN by vector or BM25 by text, with optional metadata filter + `k`), `GET /_memory/{ns}` (list), `DELETE /_memory/{ns}/{id}` and `DELETE /_memory/{ns}` (forget / drop). Namespaces are physically isolated — live-verified: recall in an empty namespace returns `hits:[]`, text recall ranks the correct memory first, vector recall returns correct kNN order, and a `metadata.topic` term filter narrows correctly.

- **Limitation:** the text-recall field is `query` (the store uses `text`); a flat `{text:…}` or a bare filter silently degrades to `match_all`/errors — the filter must be a full ES clause (e.g. `{term:{"metadata.topic":…}}`). Recall is pure relevance (kNN/BM25); recency-blended scoring and semantic dedup from the older internal module are not applied. Single-node.

### 3. Anomaly detection (`_ml`) ✅ (rc-2)

A real statistical detector replaces the empty compat stubs:
`PUT /_ml/anomaly_detectors/{id}` (create: source index, time field, function `count|mean|min|max|sum`, bucket span, threshold), `GET` (fetch/list — returns real jobs), `POST /_ml/anomaly_detectors/{id}/_score` (buckets the source over time, builds a moving mean/stddev baseline, flags buckets deviating beyond the threshold with a normalized anomaly score), `DELETE`. Live-verified: a 500-value spike among 24 baseline buckets of ~10 was correctly flagged (`is_anomaly:true`, `anomaly_score:100`), and `DELETE` removed the job from subsequent `GET`s.

- **Limitation:** on-demand scoring only (`POST _score`) — no continuous datafeed scheduler, no forecasting, no influencers/model-plot, single-node config registry. When the baseline std_dev is 0 the z-score is a placeholder (`1000000`). `_cat/ml/datafeeds` and `_cat/ml/trained_models` remain valid empty stubs. (The continuous datafeed scheduler has since landed — see below.)

## Landed since rc-2 (on `main`, unreleased)

These shipped after rc-2 during the RC3 gap-closure and AI-use-case pass. Each is conformance-gated (full ES-compat YAML suite green) and ships a runnable recipe + docs.

### 4. Real scalar8 vector quantization (serving path) ✅

A `dense_vector` field can opt into **scalar8** (int8) quantization via `index_options.type: int8_hnsw`. The kNN *serving* path scores against 1-byte-per-dimension codes (≈4× smaller vector working set) while `_source` still returns the original float32 vectors. Live-verified on a 128-dim corpus: **recall@10 ≈ 0.99** vs exact float32, footprint 512 → 128 B/vec. Recipe: `recipes/vector_quantization.py`; guide: `docs/recipes/vector-quantization.md`.

- **Limitation:** `binary` (1-bit) is still rejected at startup rather than faked; scalar4/offload remain future work.

### 5. Continuous anomaly datafeeds (`_ml/datafeeds`) ✅

The datafeed scheduler that rc-2 lacked: `PUT/GET/DELETE /_ml/datafeeds/{id}` + `_start`/`_stop`, and `GET /_ml/anomaly_detectors/{job}/results/records`. A background task re-buckets a live index on a timer and appends newly-flagged anomaly records you poll — a second spike is detected with no second call. Live-verified end-to-end. Recipe: `recipes/anomaly_datafeed.py`; guide: `docs/recipes/continuous-anomaly-datafeeds.md`.

- **Limitation:** single-node scheduler; no forecasting/influencers.

### 6. Ingest-time chunk-embedding pipeline (per-passage vectors) ✅

Long `semantic_text` values are split into overlapping passages, embedded **per passage**, and the per-passage vectors persisted (in `<field>_vector_chunks`, only when a value spans >1 passage). A `semantic` query scores each document by its **best-matching passage** (max-sim) instead of a single pooled vector, so a long document competes on any one of its sections. Live-verified: on 40 articles + a compendium of all 40, the compendium reached top-3 for **98%** of single-topic queries with per-passage scoring vs **32%** pooled. Short single-passage values are byte-identical to before. Recipe: `recipes/passage_search.py`; guide: `docs/recipes/passage-retrieval.md`.

- **Limitation:** per-passage vectors are only as good as the active embedder. The default is lexical; switch to `--embed-mode neural` (built-in BERT) or `--embed-mode proxy` for neural-quality passage vectors — the chunk-embedding pipeline is backend-agnostic. A field that is *also* scalar8-quantized scores against the pooled vector (per-passage max-sim is exact-f32 only).

### 7. HNSW-served approximate kNN with exact rescoring ✅

Unfiltered `knn` (top-level or query form) is now served by a **persisted HNSW graph** instead of the exhaustive scan, with every candidate exact-rescored so returned `_score`s stay bit-identical to the brute path. Measured on the official bench cell (50k × 128-d, cosine, k=10): p50 23,325 ms → **1.87 ms** (ES 8.13.4: 1.57 ms — a tie), recall@10 **1.00** vs ES's 0.80; offline 100-probe recall@10 mean **0.976** / min 0.90 (ES same protocol: 0.937 / 0.70). `num_candidates` is honored as the beam width, floored at 800 to match ES's per-segment candidate semantics (ES's 1.5×k default applies when omitted). The graph is persisted at flush/refresh and reloaded at boot with field/freshness stamps; any ineligibility degrades to the exact scan — never to wrong results.

- **Limitation:** ANN serving covers **unfiltered, cosine, full-precision** kNN on indexes ≥1,024 docs only — filtered/nested kNN, `l2_norm`/`dot_product`/`max_inner_product`, SQ8-quantized fields, small indexes, and `semantic`-query vector scoring stay on the exact brute-force scan (recall 1.00, latency scales with vectors scanned). Recall on the ANN path is **measured, not guaranteed**, and `hits.total` can come back below `k`. A missing or stale graph snapshot serves brute until the next flush/refresh re-save; rebuild-from-WAL, HNSW tombstone compaction, and semantic-query routing through the graph are tracked follow-ups.

## Partial / in progress

### Query types that dispatch but are not yet semantically faithful

Counting these toward "supported query types" overstates correctness — they resolve without a `400` but do not implement ES semantics:

- **`percolate`** — hard-coded to `MatchNone` (`parser.rs:310`); always returns 0 hits.
- **`has_child` / `has_parent`** — return the inner-query hits **unfiltered** on an index with no join/parent-child mapping (no real join semantics); `nested` returns 0.
- **`span_term` / `span_or` / `span_not`** — return 0 hits **standalone**, even though `span_near` / `span_first` / `span_containing` using the same clauses return correct hits. Only composite span queries work.
- **`type`** — mapped to `MatchAll` (`parser.rs:330`).
- **`combined_fields`** — mapped to `multi_match cross_fields`; scoring is not exact. `rank_feature` passes through on plain fields (no `rank_feature` field type).

### Aggregations that are stubbed or silently degrade

- **`weighted_avg`** — returns **HTTP 200 with an embedded `{"error":"unsupported aggregation type 'weighted_avg'"}`** buried in the aggregations result instead of a value or a `400`. Silent-failure honesty gap; should `400`.
- **`scripted_metric`** — returns `{"value":null}`; scripts are not executed.
- **`significant_terms`** — returns empty `buckets` (no JLH/significance scoring produced).

### Hybrid / vector wire-compat

- The **ES-native top-level `{query, knn}`** body does **not** union the kNN hits (live: only the lexical match was returned; the best vector match was dropped). One-request BM25+kNN fusion works only through the explicit `hybrid` query type.
- `POST /{index}/_doc/{id}` returns `405` (only `PUT`/`GET`/`HEAD`/`DELETE` allowed); real ES accepts `POST` there. Minor wire-compat deviation.

### Distributed clustering maturity

- Embedded Raft (`raft.rs`, `replication.rs`, `transport.rs` — self-contained, no external raft crate) handles cluster metadata today, but the default run is **single-node** (`number_of_nodes:1`); multi-node sharding/replication hardening is ongoing.

### Log analytics data path

- The dedicated `xerj-logs` columnar module (delta-of-delta timestamps + dictionary strings) is declared as an engine dependency but **`xerj_logs::` is never invoked in non-test engine/server source** — effectively unwired. The runtime columnar path is `xerj-storage`'s ZBS2, and log-shaped analytics run through the generic ES aggregation suite (`date_histogram`, etc.). Wiring or removing the dead module is tracked work.

### Benchmark honesty (tracked docs fix)

- The **only reproducible** benchmark in the repo is `demo/playbooks/SCORECARD.md` / `BENCHMARK_VS_ES.md`: terms aggregation XERJ 1.34 ms vs ES 1.54 ms = **1.15×**; on-disk size XERJ 672.5 MB vs ES 806.7 MB = **1.20×** smaller. The website's headline perf claims (74× SIEM, 21× memory, 2.8× disk, 89× NGINX, 300× cold start, 56× binary) cite battle-report files (`SIEM_BATTLE_…`, `CLUSTER_BATTLE_…`, `HEAD_TO_HEAD_M3_…`) that **do not exist in the repo** and must be corrected or substantiated.

## Planned / not yet started

### Neural embeddings & richer ML

- A built-in **neural** embedder has **landed and ships in the default binary**: `--embed-mode neural` runs an in-process BERT sentence encoder (all-MiniLM-L6-v2, 384-dim) via `candle` — pure Rust, no Python, no external service, model auto-downloads on first use (air-gap friendly via `embedding.local_model_dir`). No rebuild and no separate artifact needed; the shipped binary is ~36 MB (a `--no-default-features` slim build without the neural backend is ~23 MB). **Remaining work:** (a) share one loaded model across all indices — today each `Index` holds its own lazily-loaded `NeuralHandle`, so a node serving several *semantic* indices can hold multiple copies of the weights in RAM (loads are lazy, so indices that never receive a semantic query never load it); (b) offer a larger/higher-quality default model option; (c) optionally pre-warm the model at startup so the one-time download happens at launch rather than on the first query.
- Forecasting for capacity/write-load signals (continuous `_ml` datafeeds have landed — see "Landed since rc-2"; the ingest-time per-passage chunk-embedding pipeline has also landed).

### Correctness of stubbed surface

- Real join / parent-child semantics for `has_child` / `has_parent` / `nested`.
- Standalone `span_term` / `span_or` / `span_not`; real `percolate`.
- `weighted_avg` and `scripted_metric` execution (and returning `400` for genuinely-unsupported aggs rather than a buried error/`null`).
- ES-native top-level `{query, knn}` fusion.

### Other tracked items

- **Distributed clustering maturity** — embedded Raft handles cluster metadata today; multi-node sharding/replication hardening is ongoing.
- **Broader aggregation coverage** — geo/IP/nested/join/span families are partially covered; see the conformance suite and `demo/playbooks/ES_COMPATIBILITY.md` for the current surface.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
