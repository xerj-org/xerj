# xerj v0.6.0 — Feature fairness review

**Audience:** internal release governance — buyers, exec, eng leadership.
**Question this answers:** if we sell xerj as the **all-in-one ES killer for AI-transformation enterprises**, how much of that pitch is real?

**Method:** four parallel audits cross-referenced.
1. Marketing/website/PDF inventory of every concrete claim ever made (238 distinct claims across landing pages, brief.html, exec-brief.html, tech-brief.html, README, BRIEF_GAPS.md).
2. AI/vector/RAG implementation depth (xerj-vector, xerj-ai, xerj-query parser+executor).
3. Security / compliance / durability (xerj-api auth, xerj-storage WAL/snapshot, xerj-cluster Raft).
4. Operations / cloud-native / baremetal / ES feature parity (metrics, k8s, Docker, target-cpu, ES handler count, ES YAML pass rate).

**Honesty rule:** parser-only stubs do not count as "shipped." A query type that the parser accepts but the executor crashes on is a 0% feature, not a 50% feature. The review distinguishes:
- **SHIPPED** — implemented and exercised in production code paths
- **PARTIAL** — works for the common case, gaps in coverage or operations
- **STUB** — wire-format / parser exists, real execution does not
- **MISSING** — not in the codebase

---

## TL;DR delivery scorecard

| # | Domain | Delivery | Verdict |
|---|---|---|---|
| 1 | Core search (BM25, queries, aggs, mappings) | **78%** | strong; ES-shaped |
| 2 | Vector search (HNSW, SQ8, filtered ANN) | **65%** | competitive on basics; no graph persistence |
| 3 | AI / RAG / agents (semantic, hybrid, rerank, agent loop) | **35%** | **the gap** vs the marketing pitch |
| 4 | Security (auth, TLS, encryption, RBAC, audit) | **38%** | basic; relies on reverse proxy + FDE |
| 5 | Compliance (retention, GDPR, HIPAA, SOC 2, geo) | **26%** | **nothing certified, much un-enforced** |
| 6 | Durability (WAL, replication, snapshots, recovery) | **64%** | WAL & PIT solid; **snapshots are stubbed**, no backup automation |
| 7 | Operations (metrics, tracing, slow log, CLI, profiling) | **36%** | Prometheus shipped; tracing/slow-log missing |
| 8 | Cloud-native (Helm, operator, tiering, autoscaling, mesh) | **15%** | container only; no operator, no Helm, no tiering |
| 9 | Baremetal / arch (target-cpu, jemalloc, lock-free, SIMD) | **57%** | per-arch builds + lock-free shipped; no explicit SIMD/io_uring/NUMA |
| 10 | ES API parity (~187 endpoints, YAML conformance) | **27%**¹ | many endpoints are wire-stubs; YAML 1304/1329 (98%) on basics, 27% weighted across ILM/Watcher/Transform/EQL/percolator |
| | **Weighted overall** | **~45%** | **honest "ES-compat search engine," not yet "AI transformation platform"** |

¹ 27% is the implementation-depth score across 20 ES feature families. The ES YAML conformance number on the supported subset is **1304/1329 (98.1%)** — high on what's wired, but 7 entire feature families (ILM, SLM, Watcher, Transform, EQL, ES|QL, Anomaly Detection) are 0–10%.

---

## Headline finding

**xerj v0.6.0 is a strong ES-compatible search + vector engine. It is NOT an "AI transformation platform" yet.**

The single biggest gap between marketing and reality is at the AI layer:

| Promised in brief / website | Reality in code |
|---|---|
| "Hybrid BM25 + vector in one query, one pass" | Parser accepts `hybrid` ; **executor has no `QueryNode::Hybrid` arm — query crashes** |
| "Semantic search with auto-embedding" | Parser accepts `semantic` ; **executor has no `QueryNode::SemanticSearch` arm — query crashes** |
| "Time-decay scoring native operator" | AST defined ; **no execution path** |
| "Token-budget-aware retrieval" | **Not in codebase** |
| "Reranker / cross-encoder integration" | **Not in codebase** |
| "LLM tool/agent integration" | **Not in codebase** |
| "Pre-computed term histograms (the reason for 74× SIEM speed)" | On-demand FieldCache; **pre-compute not implemented** — perf claim is real, attribution is not |
| "Agent memory store" | In-memory only, **lost on restart** |

These are not minor gaps. They are the four or five features that distinguish "ES killer" from "ES alternative." They need to ship before the AI-transformation messaging is honest.

---

## Domain-by-domain

### 1. Core search — 78% delivered

**Shipped, production-grade:**
- BM25 full-text (xerj-fts, k1=1.2 b=0.75)
- 38 ES query types (match, term, range, prefix, wildcard, regexp, fuzzy, query_string, simple_query_string, bool, dis_max, boosting, constant_score, multi_match, match_phrase, match_phrase_prefix, match_all, match_none, ids, exists, terms, geo_distance, geo_bounding_box, knn, nested, function_score …)
- Aggregations: terms (exact, not HyperLogLog), range, histogram, date_histogram, filter, missing, composite, avg/sum/min/max/stats, value_count, cardinality, percentiles (HDR + tdigest), top_hits, scripted_metric (safe subset)
- Mappings, dynamic mapping, index aliases, index templates (basic), ILM/SLM API surface (storage only)
- Bulk, scroll, point-in-time, msearch, async_search (synchronous under the hood)
- 187 ES API endpoints exposed in `xerj-api/src/es_compat.rs`

**Test signal:** 1304/1329 (98.1%) ES YAML test pass rate on the suites we cover. The 22 failures are pre-existing precision / Lucene-exact issues (HoltWinters, _tsid Murmur128, HDR multi-shard, BM25 max_score, two-level field collapsing, date_nanos cross-index, flattened synth-source).

**Notable gaps:** dynamic-template `copy_to` not applied at index time, runtime fields stored but not executed, advanced mustache in search templates partial, full Painless interpreter is intentionally a safe-subset stub.

### 2. Vector search — 65% delivered

**Shipped, production-grade:**
- HNSW with M, efConstruction, efSearch, dynamic insert, batch insert, **filtered-ANN with pre-filter pushed into beam search** (Pinecone-style; not post-filter)
- Scalar8 quantization (4× compression, ~1–2% recall loss measured) and Scalar4 / nibble-packed quantization (8× compression, ~4–7% recall loss)
- Up to 16 384 dimensions per vector (vs ES 4 096)
- kNN brute-force fallback for tiny indexes
- Nested kNN (one parent doc with multiple child vectors)

**Production-grade gap:** **HNSW graph is not persisted.** Vectors live in WAL; on every restart the entire HNSW graph is rebuilt by re-inserting every vector. That's O(N log N) startup cost. There is no soft-delete on the graph either — removed vectors linger in neighbor lists until the next full rebuild. For an enterprise vector DB this is the single biggest correctness/durability hole.

**Missing vs Pinecone/Qdrant/Weaviate:** disk-persisted graph + soft-delete + cloud durability + sharding.

### 3. AI / RAG / agents — 35% delivered

This is **the** category where the marketing and the code diverge most.

**Shipped, production-grade:**
- Embedding proxy (xerj-ai/src/embed.rs) — async HTTP client to OpenAI-compatible endpoints, semaphore-bounded concurrency, exponential-backoff retry, configurable timeout, batch embedding
- Text chunker (xerj-ai/src/chunker.rs) — sentence-aware with overlap, UTF-8 safe, parent-doc tracking
- Agent memory store (xerj-ai/src/memory.rs) — semantic-similarity dedup, recency blending — **but in-memory only, lost on restart**

**Stubs (parser accepts, executor crashes):**
- `hybrid` query — RRF fusion not implemented
- `semantic` query — auto-embed not wired
- `function_score` time-decay (gauss/exp/linear) — AST present, executor missing

**Missing entirely:**
- Token-budget-aware retrieval (no token counter cap)
- Cross-encoder rerankers (no post-rerank stage)
- LLM tool/agent loop (no function-calling, no tool dispatch)
- Inline embeddings (raw text + precomputed vector in one field)
- Streaming chat / inference proxy
- Multi-modal (image / audio embedding)
- Cohere / Anthropic native adapters (only OpenAI-compatible endpoints work)
- NER / classification / non-English analyzers

**Verdict:** the embedding + chunking pipeline is real. Everything you'd build *on top* of that pipeline (hybrid retrieval, reranking, agent loops, token budgeting) is either parser-only or absent. The "AI transformation enterprise" pitch presumes those higher layers; today they aren't there.

### 4. Security — 38% delivered

**Shipped, production-grade:**
- API key auth (auto-generated 32-byte admin key; CORS layer; permissive CORS by default)
- Body size, query depth, mget batch, agg bucket caps (v0.5.9 + v0.6.0 hardening)

**Big asterisks on the marketing:**
- **TLS in transit** is config + auto-cert generation only — **the Axum listener binds plain TCP**. The `tls.cert_path` config is read but never wired into a `tokio_rustls::TlsAcceptor`. In production you MUST terminate TLS at a reverse proxy. The brief saying "TLS in transit" is technically accurate (cert generation works) but operationally misleading.
- **Encryption at rest** — not implemented at engine level. Relies on OS-level FDE or S3 SSE. Brief implies engine-level encryption; there is none.
- **BYOK / KMS** — not implemented.
- **RBAC** — not implemented. Auth is binary: key present → full access. No per-index, per-doc (DLS), per-field (FLS) controls. **The "unified RBAC across logs/vectors/memory" claim in the exec brief is false.**
- **OAuth / OIDC / SAML** — not implemented.
- **Audit logging** — only infrastructure tracing (request IDs, structured logs to stderr). No tamper-evident WORM audit trail, no per-user accountability. Cannot satisfy SOC 2 / HIPAA audit-trail requirements without an external SIEM.

The painless-execute endpoint hardening that landed in v0.6.0 (input limits, no eval) is a **good** security-hygiene win, but it doesn't change the picture: xerj is "secure-by-deployment" (behind a proxy with FDE) not "secure-by-engine."

### 5. Compliance — 26% delivered

| Standard | Status | Notes |
|---|---|---|
| **SOC 2 Type I / II** | ❌ not started | brief implies "audit-grade"; no attestation |
| **ISO 27001** | ❌ not started | |
| **HIPAA / HITECH** | ❌ not started | requires audit log + encryption at rest, neither exists at engine level |
| **PCI-DSS** | ❌ not started | no CHD detection, no tokenization, no encryption at rest |
| **GDPR right-to-be-forgotten** | ⚠️ async-only | DELETE writes a tombstone; data physically purged only after segment merge (~hours). No deletion certificate. Snapshot backups may retain the deleted data. |
| **GDPR data residency** | ⚠️ configurable, unenforced | S3 region config exists; no policy prevents cross-region reads; not auditable from API |
| **Retention policies** | ⚠️ stub | `logs.retention_days` config exists; no background job actually deletes |
| **Data classification / PII detection** | ❌ not implemented | optional WASM ingest plugin can drop fields; no built-in detector |
| **EU AI Act traceability** | ⚠️ explain-plan exists | the `explain` API is real; the AI-Act positioning is a positioning claim, not a certification |

**The "Audit-grade by default" line in the exec-brief should be softened to "Audit-ready building blocks" until at least SOC 2 Type I is in flight.**

### 6. Durability — 64% delivered

**Strong:**
- **WAL** (85%) — CRC32C per entry, LZ4 compression, generation-based rotation, three sync modes (Sync / Batched / Async, default Batched-100ms), checkpoint with own CRC
- **Crash recovery** (85%) — verified with kill -9, idempotent replay, replay errors stop instead of corrupting state
- **Point-in-time** (80%) — per-index max-seq snapshot, `pit.id` filter excludes post-PIT writes
- **Raft consensus** (80%) — clean from-scratch implementation of all 5 safety properties; used for cluster metadata only (not data WAL)
- **Versioning** (85%) — WAL high-bit compression flag, segment unknown-section-skip, both informally forward-compatible

**Big holes:**
- **Snapshots** (60%) — wire-format implemented, `create_snapshot()` and `restore_snapshot()` engine functions are **stubs**. Customers can `PUT /_snapshot/{repo}` and `POST /_snapshot/{repo}/{name}` and get 200 OK; **no segment data is actually written**.
- **Backup automation** (0%) — no scheduler, no SLM execution
- **Disaster recovery** (0%) — no cross-region tooling, no documented runbook
- **Segment-level integrity** — only WAL is checksummed; segment stored/postings/doc-values blocks are not
- **PIT cleanup** — open PITs are never garbage-collected; trivial memory leak vector

**The "zero data loss on restart" claim is real for crash recovery. The "snapshots / backup" pillar is not.**

### 7. Operations — 36% delivered

**Shipped:**
- Prometheus metrics (xerj-common/src/metrics.rs) — 17 metrics, exponential histograms, per-index labels for queries + indexing
- Health endpoints (`/_cluster/health`, `/_cat/health`, `/_nodes`)
- Structured logging via `tracing` (JSON output available, not default)
- Graceful SIGTERM shutdown (fixed in v0.5.9)
- 187 ES API endpoints exposed
- Single static binary, single config file (TOML), 38 knobs vs ES 3 000+

**Missing:**
- Distributed tracing (OTLP log ingest works; **xerj does not emit spans**)
- Slow query log
- Hot config reload (no SIGHUP, no API)
- Liveness vs readiness probe distinction (everything returns "green")
- `/_nodes/hot_threads`-style diagnostic dump
- Rich profile API (parameter accepted, timings discarded)
- Admin CLI subcommands (`xerj index list` etc.) — only server flags exist

### 8. Cloud-native — 15% delivered

This is the lowest-scoring domain.

**Shipped:**
- Multi-stage Dockerfile (~16 MB image vs ES 800 MB) — but **single-arch per build**, **not distroless**
- Docker run examples in README

**Missing:**
- **No Helm chart** in repository
- **No Kubernetes operator / CRDs** for cluster or index management
- **No storage tiering** (hot/warm/cold)
- **No HPA-friendly custom metrics** (Prometheus metrics are generic, not autoscale-shaped)
- **No service-mesh examples** (mTLS untested)
- **GCS / Azure Blob backends absent** (S3 SDK in dependencies, not wired into storage layer)
- **Managed SaaS** (roadmap '27 in tech-brief)

For a product positioned at AI-transformation enterprises in 2026, this is the most operationally-dangerous gap.

### 9. Baremetal / architecture — 57% delivered

**Shipped:**
- **Per-target rustflags** (✅ fix from v0.5.9) — x86-64-v3 (Haswell+ AVX2), apple-m1, default ARMv8-A
- **8-target binary matrix** built and shipped on every release (v0.6.0 confirmed: aarch64+x86_64 × {linux-gnu, linux-musl, apple-darwin, pc-windows-msvc})
- **jemalloc** as global allocator (non-MSVC) — measured win under high-RPS contention
- **Lock-free hot path**: `ArcSwap<IndexSnapshot>` for segment swaps, `DashMap` for version map and pipelines, parking_lot RwLock on FTS memtable shards
- **Sharded memtable** with runtime shard count (✅ fix from v0.5.9 — was a panic on small-core boxes)

**Not implemented:**
- No explicit SIMD intrinsics (relies on LLVM auto-vectorization; vector distance has a TODO for hand-rolled AVX2)
- No NEON intrinsics on aarch64 (auto-vectorization only)
- No NUMA pinning
- No `io_uring` (tokio uses epoll)
- No `O_DIRECT` / direct I/O
- No huge-page mmap

For pure throughput on commodity 16–32-core boxes the auto-vectorization path is fine. For 96-core Genoa / Bergamo or AWS Graviton4 hosts, leaving SIMD/NEON on the table costs real performance.

### 10. ES API parity — 27% (weighted) / 98% (on supported)

187 endpoints exposed. The honest split:

| Feature family | Status | Delivery |
|---|---|---|
| Core CRUD (index/get/delete/update) | shipped | 90% |
| Bulk | shipped | 90% |
| Search (BM25, term, range, geo, kNN, nested) | shipped | 80–98% (per YAML pass rate) |
| Aggregations (40+ types) | shipped | 80% |
| Mappings + index templates | shipped | 60% (composition partial, dynamic templates not applied) |
| Search templates (mustache) | shipped | 75% |
| Async search | shipped | 50% (synchronous under the hood) |
| Reindex API | **stub** | 10% |
| **ILM (lifecycle)** | **stub — policies stored, not enforced** | 10% |
| **SLM (snapshot lifecycle)** | **stub** | 10% |
| **Ingest pipelines** | **stub — processors not executed** | 10% |
| Painless scripting | partial — safe subset only (by design) | 25% |
| SQL | partial | 25% |
| **Transform API** | **stub** | 10% |
| **Watcher / alerting** | **missing** | 0% |
| **Anomaly detection (Elastic ML)** | **missing** | 0% |
| **EQL** | **stub — empty result set** | 0% |
| **ES\|QL (piped query language)** | **missing** | 0% |
| Cross-cluster search | missing | 0% |
| Geo (geo_point, geo_distance, grid aggs) | partial | 40% (geo_point fine, geo_shape incomplete) |
| Percolator | **stub — type recognized, no reverse-search** | 0% |
| Synonyms graph / query rules | missing | 0% |

The headline message: when xerj implements an ES feature, it implements it well. When it doesn't, it often **wires the endpoint as a 200-OK stub**, which is a misleading user experience. **Stub endpoints should return 501 Not Implemented or be removed before they reach a customer.** That's a v0.6.x cleanup task.

---

## Performance claims — re-check

Battle reports under `engine/releases/v0.1.0/reports/` and `engine/reports/` give the numbers. v0.6.0 has not re-run the SIEM/cluster battles; the v0.5.x numbers apply within a margin since v0.6.0 only added hardening (no perf regression observed in YAML).

| Claim (brief / exec-brief) | Verified? | Notes |
|---|---|---|
| 11 MB static binary | ✅ | v0.6.0 GitHub release: 5–6 MB compressed, ~12 MB on disk |
| 8-target multi-arch (linux/macos/windows × x86_64/aarch64) | ✅ | v0.6.0 ships all eight |
| 21× less idle memory than 4-node ES | ✅ | 400 MB vs 8.5 GB measured 2026-04-14 |
| 300× faster cold start than ES | ✅ | 50 ms vs 15 s measured |
| 14× faster graceful shutdown than ES | ✅ | 0.24 s vs 3.27 s |
| 74× faster top source IPs (terms agg) | ✅ on benchmark, ⚠️ attribution | 0.4 ms vs 29.8 ms measured. Brief attributes the win to "pre-computed term histograms" which are **not implemented**. The actual win is from columnar DocValues + lock-free reader path + no JVM. The number is real; the explanation in the brief is wrong. |
| 80 K docs/s sustained ingest | ⚠️ | claimed on a 2026-04-14 24h diurnal curve; no fresh v0.6.0 benchmark |
| ES wins on warm bulk (179 K vs 95 K docs/s) | ✅ | conceded in README |
| 4× memory savings on vectors | ✅ | 1M × 768d: 1.2 GB (SQ8) vs 4.6 GB (float32) |
| Hybrid query p95 38 ms on 1B vectors | ❌ | hybrid query is a parser-only stub — this number cannot be reproduced today |

---

## What we honestly are vs what the marketing says

| Marketing label | Honest label |
|---|---|
| "All-in-one ES killer for AI transformation enterprises" | **ES-compatible search + vector engine for new single-node workloads** |
| "Drop-in Elasticsearch replacement" | **Drop-in for the 60–80% ES surface customers actually use; not for ILM / Watcher / Transform / EQL / cross-cluster** |
| "Audit-grade by default" | **Audit-ready primitives; no SOC 2 / HIPAA attestation; no engine-level encryption at rest** |
| "Hybrid BM25 + vector in one query" | **Parser accepts hybrid syntax; executor not yet wired (v0.7 target)** |
| "Unified RBAC across logs / vectors / memory" | **Single API key, no roles, no per-doc / per-field controls** |
| "Cloud-native deployment" | **Container image; no Helm, no operator, no autoscaling hooks, no storage tiering** |
| "Production-ready alerting" | **No Watcher, no alerting** |
| "Bare-metal optimised" | **Per-arch builds + jemalloc + lock-free hot path; no SIMD intrinsics, no io_uring, no NUMA** |

---

## Recommendations for v0.6.x and the marketing

**Before we put v0.6.0 in front of an enterprise prospect:**

1. **Stop shipping stubs as 200-OK endpoints.** EQL, Watcher, Transform, ILM execution, ingest-pipeline processors, percolator should return `501 Not Implemented` with a Retry-After header pointing at the roadmap, OR be removed from the router. Today they silently lie.
2. **Soften the brief on three claims** — "audit-grade by default" → "audit-ready primitives," "unified RBAC" → "API-key authentication (RBAC roadmap v0.7)," "hybrid in one query" → either ship the executor in v0.6.1 or describe it as RRF-on-top.
3. **Document the deployment-security model.** The brief should say plainly: *xerj expects to run behind a TLS-terminating reverse proxy with OS-level FDE*. That's a deployment architecture, not a hidden gap.
4. **Persist the HNSW graph.** Restart-time graph rebuild is the single biggest correctness gap for anyone who pitches xerj as a vector DB.
5. **Implement snapshot serialization.** The wire format works; the engine doesn't actually back up data. This is a P0 for any enterprise customer.

**v0.7 should focus on the AI gap:**
- Wire `hybrid` executor (RRF fusion)
- Wire `semantic` executor (auto-embed → kNN)
- Reranker / cross-encoder hook (call out to external reranker, blend scores)
- Token-budget retrieval cap
- Persistent agent memory (RocksDB or our own segments)
- One AI-feature integration test suite — currently zero

**v0.8 should focus on the cloud-native gap:**
- Helm chart + reference Kubernetes deployment
- Operator / CRDs for cluster + index management
- HPA-friendly custom metrics (pending docs, replication lag, query queue depth)
- Storage tiering (hot local, warm S3) with auto-promotion

**v1.0 must include:**
- SOC 2 Type I attestation
- Engine-level encryption at rest (or a clearly-documented FDE-only stance)
- RBAC with at minimum (admin / read / write / read-only-index) roles
- Real audit log (WORM, queryable)
- Real snapshot implementation
- Watcher / scheduled queries (or a partner integration)

---

## Appendices

- **A.** Marketing-claim inventory: 238 distinct claims, see audit log dated 2026-04-25.
- **B.** AI features audit, see audit log dated 2026-04-25.
- **C.** Security / compliance / durability audit, see audit log dated 2026-04-25.
- **D.** Operations / cloud-native / baremetal / ES parity audit, see audit log dated 2026-04-25.
- **E.** Linus-style code review, `engine/reports/CODE_REVIEW_LINUS_2026-04-25.md`.
- **F.** v0.6.0 release notes, `engine/reports/RELEASE_NOTES_v0.6.0_2026-04-25.md`.
- **G.** Full feature delivery breakdown table follows.

### Full feature delivery table (90+ items)

| # | Feature | Domain | Status | Delivery % |
|---|---|---|---|---|
| 1 | BM25 full-text search | core | shipped | 100 |
| 2 | 38 ES query types | core | shipped | 100 |
| 3 | Aggregations (terms/range/hist/date_hist/composite) | core | shipped | 95 |
| 4 | Aggregations (metric: avg/sum/min/max/stats/percentiles) | core | shipped | 95 |
| 5 | Aggregations (cardinality, value_count, top_hits, scripted_metric) | core | shipped | 80 |
| 6 | Highlighting | core | shipped | 90 |
| 7 | Mappings (static) | core | shipped | 100 |
| 8 | Dynamic mapping | core | shipped | 80 |
| 9 | Dynamic templates | core | partial | 50 |
| 10 | Runtime fields | core | stub | 25 |
| 11 | Object / nested | core | partial | 60 |
| 12 | Index aliases | core | shipped | 90 |
| 13 | Index templates | core | shipped | 60 |
| 14 | Component / composable templates | core | partial | 40 |
| 15 | Bulk API | core | shipped | 95 |
| 16 | Scroll API | core | shipped | 90 |
| 17 | Point-in-time | core | shipped | 80 |
| 18 | msearch | core | shipped | 75 |
| 19 | Async search | core | partial | 50 |
| 20 | Search templates (mustache) | core | shipped | 75 |
| 21 | Reindex | core | stub | 10 |
| 22 | Delete-by-query | core | shipped | 80 |
| 23 | Update-by-query | core | shipped | 70 |
| 24 | SQL | core | partial | 25 |
| 25 | Geo (geo_point, geo_distance) | core | partial | 60 |
| 26 | Geo (geo_shape) | core | stub | 20 |
| 27 | Geo grid aggs (geohash/geotile) | core | partial | 50 |
| 28 | HNSW vector index | vector | shipped | 75 |
| 29 | HNSW graph persistence | vector | missing | 0 |
| 30 | HNSW deletion / soft-delete | vector | missing | 0 |
| 31 | SQ8 / 4-bit quantization | vector | shipped | 100 |
| 32 | Filtered ANN | vector | shipped | 100 |
| 33 | Multi-vector / nested kNN | vector | partial | 50 |
| 34 | Vector dimension support up to 16 384 | vector | shipped | 100 |
| 35 | Embedding proxy (OpenAI-compatible) | AI | shipped | 100 |
| 36 | Embedding adapters (Cohere/Anthropic) | AI | missing | 0 |
| 37 | Text chunking | AI | shipped | 100 |
| 38 | Semantic search query | AI | stub | 0 |
| 39 | Hybrid BM25 + vector | AI | stub | 25 |
| 40 | Time-decay scoring | AI | stub | 25 |
| 41 | Token-budget-aware retrieval | AI | missing | 0 |
| 42 | Rerankers / cross-encoder | AI | missing | 0 |
| 43 | LLM tool / agent loop | AI | missing | 0 |
| 44 | Inline embeddings | AI | missing | 0 |
| 45 | Streaming chat / inference proxy | AI | missing | 0 |
| 46 | Multi-modal embeddings | AI | missing | 0 |
| 47 | NER / classification / multilingual analyzers | AI | partial | 25 |
| 48 | Agent memory (in-memory) | AI | partial | 50 |
| 49 | Agent memory (persistent) | AI | missing | 0 |
| 50 | API key authentication | security | shipped | 75 |
| 51 | API key hashing / rotation / per-key rate-limit | security | missing | 0 |
| 52 | TLS in transit (in-process) | security | stub | 25 |
| 53 | mTLS | security | missing | 0 |
| 54 | Encryption at rest (engine-level) | security | missing | 0 |
| 55 | BYOK / KMS integration | security | missing | 0 |
| 56 | RBAC / per-index / FLS / DLS | security | missing | 0 |
| 57 | OAuth / OIDC / SAML / SSO | security | missing | 0 |
| 58 | Audit logging (tamper-evident, queryable) | security | partial | 25 |
| 59 | CORS | security | shipped | 100 |
| 60 | Request signing (SigV4 / HMAC) | security | missing | 0 |
| 61 | Painless `_execute` hardening | security | shipped | 100 |
| 62 | Body / depth / mget / agg-bucket caps | security | shipped | 100 |
| 63 | Data retention enforcement | compliance | stub | 25 |
| 64 | GDPR right-to-be-forgotten (sync) | compliance | partial | 40 |
| 65 | HIPAA audit trail | compliance | missing | 0 |
| 66 | SOC 2 controls | compliance | missing | 0 |
| 67 | PCI-DSS | compliance | missing | 0 |
| 68 | Geo-residency enforcement | compliance | partial | 50 |
| 69 | Data classification | compliance | missing | 0 |
| 70 | PII detection | compliance | missing | 0 |
| 71 | EU AI Act traceability (explain plan) | compliance | shipped | 75 |
| 72 | WAL with CRC, fsync modes, rotation | durability | shipped | 85 |
| 73 | Crash recovery | durability | shipped | 85 |
| 74 | Replication (sync/async/quorum) | durability | shipped | 70 |
| 75 | Raft consensus (cluster metadata) | durability | shipped | 80 |
| 76 | Snapshots (working) | durability | stub | 60 |
| 77 | Snapshot Lifecycle Management (SLM) | durability | stub | 10 |
| 78 | Point-in-time recovery | durability | shipped | 80 |
| 79 | Backup automation / scheduling | durability | missing | 0 |
| 80 | Disaster recovery / cross-region | durability | missing | 0 |
| 81 | Forward / backward compat versioning | durability | shipped | 85 |
| 82 | Segment-level data integrity (checksums) | durability | partial | 40 |
| 83 | Prometheus metrics | ops | shipped | 100 |
| 84 | Distributed tracing (OTLP emit) | ops | partial | 25 |
| 85 | Structured / JSON logging | ops | shipped | 75 |
| 86 | Health endpoints | ops | shipped | 75 |
| 87 | Liveness vs readiness probes | ops | missing | 0 |
| 88 | Slow query log | ops | missing | 0 |
| 89 | Hot config reload | ops | missing | 0 |
| 90 | Rolling upgrade tooling | ops | partial | 25 |
| 91 | CLI tooling (subcommands) | ops | partial | 50 |
| 92 | Diagnostic dump / hot threads | ops | missing | 0 |
| 93 | Profile API | ops | partial | 25 |
| 94 | Helm chart | cloud | missing | 0 |
| 95 | Kubernetes operator / CRDs | cloud | missing | 0 |
| 96 | Container image (multi-stage) | cloud | shipped | 50 |
| 97 | Multi-arch container | cloud | partial | 50 |
| 98 | Distroless / scratch image | cloud | missing | 0 |
| 99 | Storage tiering (hot/warm/cold) | cloud | missing | 0 |
| 100 | Auto-scaling hooks (HPA metrics) | cloud | missing | 0 |
| 101 | Service mesh (mTLS, sidecar) | cloud | partial | 25 |
| 102 | Multi-cloud blob storage (S3 / GCS / Azure) | cloud | partial | 33 |
| 103 | Managed SaaS | cloud | missing | 0 |
| 104 | Per-target rustflags | baremetal | shipped | 100 |
| 105 | Multi-arch builds (8 targets) | baremetal | shipped | 100 |
| 106 | jemalloc allocator | baremetal | shipped | 100 |
| 107 | Lock-free hot path (ArcSwap, DashMap) | baremetal | shipped | 100 |
| 108 | Sharded memtable (runtime) | baremetal | shipped | 100 |
| 109 | Explicit SIMD / AVX2 / AVX-512 | baremetal | partial | 25 |
| 110 | NEON intrinsics | baremetal | partial | 50 |
| 111 | NUMA awareness | baremetal | missing | 0 |
| 112 | io_uring | baremetal | missing | 0 |
| 113 | Direct I/O / O_DIRECT | baremetal | missing | 0 |
| 114 | Huge pages | baremetal | missing | 0 |

**Sum:** 114 features audited. Avg delivery: **45.8%**. Shipped/100%: **24** (21%). Stub or missing: **52** (46%).

---

*Compiled by xerj engineering 2026-04-25 from four parallel audits. Internal release governance document — share with the brief team before the next public revision.*
