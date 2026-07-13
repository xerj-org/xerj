# XERJ rc4 — User-Feedback Delivery Scorecard & rc5 Roadmap

**Date:** 2026-07-12
**Scope:** shipped rc4 XERJ scored against the real Elasticsearch user-feedback corpus (`user-feedback/`, 41 pain-point files across 15 categories, 98 scored pain points).
**Status:** INTERNAL strategy document. This is a capability self-assessment, not marketing. The gaps are the point — do not soften them, and do not paste verbatim competitor/G2/HN/Jepsen quotes into external material.
**Method:** each pain point was scored against current rc4 code (file:line) and the live binary (read-only probes), NOT the marketing copy.
DELIVERED = positive proof it works today. PARTIAL = works with named limitations. GAP = the pain is not answered today (rc5 candidate). NA = does not apply to a single-binary/embedded engine.

---

## 1. Aggregate delivery

**48 DELIVERED / 32 PARTIAL / 14 GAP / 4 NA** across 98 pain points.

Of the 94 applicable (non-NA) pain points: **51% fully delivered, 34% partial, 15% open gap.** The headline story is honest and defensible: XERJ *structurally erases* the JVM/GC, shard-management, cluster-coordination, licensing/paywall, and CVE-supply-chain classes of ES pain — those are code-proven, live-verified wins. It is weakest exactly where the campaign already knows it is weak: under concurrent load (mixed read-under-write p99), at multi-node scale (single-node only; cluster mode experimental/unsafe), on power-loss durability (batched WAL default below ES), on steady-state memory under churn (RSS-runaway ticket still open), and on out-of-box semantic quality (lexical default embedder). Several headline log-analytics and hybrid-search promises are *scaffolded but unwired* in the shipped binary.

### Category table

| # | Category cluster | Pain pts | Delivered | Partial | Gap | NA |
|---|------------------|:--------:|:---------:|:-------:|:---:|:--:|
| 01/10/13 | ops-simplicity (operational complexity, docs/UX, vendor/support) | 11 | 7 | 4 | 0 | 0 |
| 02/03 | resource-cost (JVM/memory, cost/pricing) | 16 | 8 | 7 | 1 | 0 |
| 04/05/09 | scaling-durability (clustering, HA, data-loss) | 12 | 4 | 5 | 3 | 0 |
| 06/08 | upgrades-migration-datamodel | 17 | 5 | 5 | 3 | 4 |
| 07 | query-performance | 10 | 7 | 2 | 1 | 0 |
| 15 | security-trust | 13 | 8 | 3 | 2 | 0 |
| 11/12/14 | ai-logs-ecosystem (vector/AI, log-analytics, alternatives) | 19 | 9 | 6 | 4 | 0 |
| | **TOTAL** | **98** | **48** | **32** | **14** | **4** |

**Ranking by strength (delivered share of applicable):** security-trust and query-performance and ops-simplicity are strongest; scaling-durability is weakest (single-node ceiling), with ai-logs-ecosystem carrying the most *unwired-scaffolding* gaps.

---

## 2. Per-category delivery

### 2.1 ops-simplicity (7 DELIVERED / 4 PARTIAL / 0 GAP)
XERJ's strongest category. Single-binary + ES-wire-compat + built-in Prometheus/health + native OTLP/syslog ingest genuinely erase the specialist-burden, self-monitoring-cluster, ELK-sprawl, and learning-curve pains — all confirmed on the live binary. The four partials are the two KNOWN-OPEN stability tickets (RSS-runaway heap; mixed read-under-write p99 — ES still wins under load) plus two overstated docs claims (OpenAPI is a static hand-maintained file, not code-generated/served; the "~30MB" Docker image is really >100MB). No total gaps.

### 2.2 resource-cost (8 DELIVERED / 7 PARTIAL / 1 GAP)
XERJ annihilates the JVM/GC/heap-ceiling class of pain and the licensing/feature-gating cost drivers — code-proven wins. Idle RSS 54.5MB vs the 33GB-empty/4.5GB-baseline complaint. GDPR right-to-be-forgotten is a genuine ES-can't-do-this win (synchronous forcemerge physically purges tombstones, unit-tested + live). But the headline RAM promise (<500MB / 1M docs) is an **overclaim** (596MB steady for 200k docs), the RSS-runaway ticket is the one hard GAP, and the "secure by default / 2-5× compression / 30MB image" numbers are each optimistic vs measured (TLS off by default; ~1.6× compression; ~75MB image).

### 2.3 scaling-durability (4 DELIVERED / 5 PARTIAL / 3 GAP)
The single-node story is genuinely strong and live-verified: shards, split-brain, JVM/GC, external deps eliminated by design (always-green, 1 shard/index, 0 unassigned, ~1s boot, embedded from-scratch Raft — no etcd/Pulsar/MinIO). But **every** multi-node / HA / extreme-scale / S3-native / per-tenant promise is unshipped or experimental-and-unsafe (plaintext unauth Raft on the cluster port; cosmetic `number_of_replicas`), and DEFAULT durability sits below ES (100ms power-loss window; `wal_sync=sync` is opt-in). XERJ wins the operational-simplicity war by *not competing on HA/scale-out at all* — which for the extreme-scale and clustering pain points is a gap, not a win.

### 2.4 upgrades-migration-datamodel (5 DELIVERED / 5 PARTIAL / 3 GAP / 4 NA)
The version-upgrade-safety story is strong and well-tested: the data-dir format marker refuses a newer-than-supported OR corrupt dir *before* any destructive GC (4 passing tests, live marker file) — arguably safer than ES's silent corruption-on-bad-upgrade. Single-node structurally erases the shard-allocation dance, cluster-state mapping broadcast, and ILM alias/rollover/shrink complexity. Real AI-native data modeling ships (first-class `chunk` type with parent propagation; live `/_memory` store/dedup/recall). But two headline promises are **unwired in rc4**: dynamic-mapping `strict`/field-limit is accepted-but-ignored on the ES ingest path (mapping explosion NOT prevented), and TTL `retention_days` has a policy type + config knob with ZERO runtime enforcement. Immutable field types are identical to ES with no promised "index versioning" escape hatch.

### 2.5 query-performance (7 DELIVERED / 2 PARTIAL / 1 GAP)
XERJ wins the STEADY-STATE query story ES users complain about: leading-wildcard/regex/prefix (3.8–4.9×), aggregations (7–30×), infinite-script exhaustion made *structurally impossible* (Painless subset has no loop statement), an engine-level timeout that actually preempts term-dictionary walks, and fail-safe rejection of the classic node-killers (huge `size`, deep window, bucket OOM → bounded 400/429, never crash). The one real GAP is the mirror image of the merge-storm complaint: **read-under-write p99** (4 LOSE cells, ES 2–4× faster at iso-load) — XERJ trades ES's refresh-lag for writer-lock contention. Two PARTIAL overclaims to correct: "no deep-pagination cliff" (the 10k cap is identical to ES; only `search_after` is offered) and "cost-based planner" (it is rule/structure-based, not statistics-driven).

### 2.6 security-trust (8 DELIVERED / 3 PARTIAL / 2 GAP)
Strong cluster. Apache-2.0 (the exact permissive license ES rug-pulled) is a code-provable trust win; no-JVM eliminates the entire Log4Shell/Groovy/Java-deserialization CVE class; secure-by-config is proven live (no-key → 401, constant-time key compare, 0600 key files, restrictive CORS, health exempt). The resource governor answers ES's crafted-query-OOM CVEs. The only outright GAPs are two **unshipped process promises** — no `cargo-audit`/`cargo-deny` advisory gate in CI, and no `cargo-fuzz` harnesses on the untrusted-input parsers. TLS-off-by-default and superuser-only RBAC (accepted-but-not-enforced `role_descriptors`) are honest PARTIALs.

### 2.7 ai-logs-ecosystem (9 DELIVERED / 6 PARTIAL / 4 GAP)
XERJ's AI-native primitives are genuinely shipped and differentiate vs ES today: agent-memory HTTP API (semantic dedup + recency recall — no ES/Weaviate/Qdrant/Pinecone equivalent), filter-pushdown HNSW (answers "kNN filters decrease perf / yield zero results"), inline `semantic_text`, 16,384 dims (4× ES), a bundled Console SPA, exact term-agg counts, and a ~64MB-RSS no-JVM binary. The category's **biggest hole is log-analytics**: the headline "dedicated columnar log engine" (`xerj-logs`) is a DEAD dependency — logs route through the inverted-index `index_document` path, so domain-aware compression, block-skip, and 100K ev/s are unproven and automatic retention doesn't exist. On the AI side the failing promises are RRF hybrid scoring (returns 400 — additive BM25-dominates persists), binary/RaBitQ quantization (unimplemented), and neural semantics (lexical default mis-ranks out of the box).

---

## 3. Confirmed strengths (code-proven / live-verified wins)

- **No JVM, by construction.** 39MB static Rust ELF + jemalloc. The entire stop-the-world / node-ejection / recovery-storm / heap-ceiling / compressed-oops / -Xms=-Xmx failure class *cannot occur*. Nothing to tune.
- **Cold start sub-second** (HTTP 200 on the first 0.2s poll) vs ES 30–60s; single-binary upgrade = swap file + restart, no rolling-restart shard dance.
- **Idle RSS ~54MB** vs the 33GB-empty-install / 4.5GB-baseline complaint — orders of magnitude better on the exact metric users quit ES over.
- **ES-wire compatibility is real and reversible.** Unchanged ES clients work zero-change on :9200; scroll+bulk works IN and OUT — the structural opposite of vendor lock-in. 1,360/1,363 ES-YAML conformance.
- **Zero shard management + split-brain structurally impossible** in the shipped default: one shard/index, always green, no master election, no quorum, no partition — the Jepsen dual-master acknowledged-write-loss class simply cannot happen single-node.
- **Parent circuit breaker keyed on ACTUAL RSS** (governor: memtable budget + RSS watermark + disk flood-stage + per-query 512MB guard) turns the classic node-killers into a survivable 429 — strictly better than ES's failure-prone pre-flight estimate.
- **Built-in Prometheus /v1/metrics (101 series) + auth-exempt health** — no separate monitoring cluster; native OTLP + Syslog + JSON + gRPC-streaming ingest replaces Logstash+Beats in one binary.
- **Data-dir format marker refuses newer/corrupt data BEFORE destructive GC** (4 tests, live marker) — a data-loss guard arguably safer than ES's silent corruption-on-bad-upgrade.
- **Query wins on the exact ES slow-query culprits:** leading-wildcard 4.01×, regexp 4.86×, prefix 3.85×, aggs 7–30×; infinite scripts impossible (no loop statement); engine timeout preempts term-dictionary walks.
- **Apache-2.0, fully self-hosted, no paywall / no ERU / no license metering** — collapses the rug-pull-distrust, feature-gating, pricing-opacity, and GC-specialist-talent cost complaints at once.
- **No-JVM CVE elimination** (Log4Shell/Groovy/deserialization gone by construction) + secure-by-config auth (constant-time compare, 0600 keys, restrictive CORS) proven live.
- **AI-native primitives ES lacks:** `/_memory` semantic dedup + recency recall; filter-predicate pushed INTO HNSW beam traversal; inline `semantic_text` auto-embed; 16,384 dims; bundled Console SPA; exact high-cardinality term-agg counts.
- **GDPR right-to-be-forgotten on demand:** synchronous forcemerge physically purges tombstoned docs from segments (unit-tested + live) — ES cannot force this.

---

## 4. Honest gaps — where XERJ does NOT yet beat ES

These are the strategic decisions for rc5, stated without spin.

1. **Mixed read-under-write p99 — ES wins under concurrent load.** 4 LOSE scorecard cells at iso-load (match_all 0.25×, terms 0.33×, range 0.36×, bool 0.50× vs ES). Root cause: live-memtable reads under the writer's per-shard lock (`MIXED_READ_UNDER_WRITE_FINDING`). This is the one benchmark axis where the ICP-scale story cracks, and it is the live analog of the "falls flat under load" horror story.
2. **Multi-node HA / clustering — single-node is a hard SPOF.** Node death = total outage. The only multi-node path is `cluster.enabled` = **experimental, unauthenticated, plaintext length-prefixed JSON-over-TCP Raft** with cosmetic (non-backing) `number_of_replicas`. There is no production HA, no failover, no zero-downtime upgrade, no compute-storage separation, no scale-out-without-migration in the shipped binary. XERJ cannot replace a multi-node ES log/search cluster today.
3. **Power-loss durability below ES default.** Default `wal_sync=batched` (`wal_batch_ms=100`) means acknowledged writes are only page-cache durable — a power-loss/panic loses up to 100ms of acks. ES defaults to per-request fsync (`translog.durability=request`). `wal_sync=sync` reaches ES parity but is opt-in, and nothing in XERJ is Jepsen-tested. The corpus's headline ES pain is acknowledged-then-lost writes — XERJ needs a *proven* guarantee, not an opt-in flag.
4. **RSS-runaway under tiny-batch churn — ticket still open.** 564MB anon RSS retained for just 200k tiny docs (~2.8KB/doc, ~5.7× on-disk). jemalloc decay *masks but does not eliminate* the heap amplification → OOMKill risk under sustained churn, which contradicts the "deterministic Rust memory / container limits work" K8s pitch and maps to ES's "randomly bloats up" complaint.
5. **Lexical default embedder — semantic quality poor out-of-box.** The default embedder is honest lexical feature-hashing; real neural is opt-in (`--embed-mode neural`, ~90MB first-use download). Live: "capital of France is Paris" out-ranks the correct doc for a UI-theme query. Semantic/RAG/memory relevance is limited unless the operator flips to neural.

**Secondary honest gaps (scaffolded-but-unwired or accepted-but-ignored):**
- RRF hybrid retriever returns 400 — hybrid falls back to additive `bool.should` where BM25 (0–∞) dominates vector (0–1): the exact normalization pain the response claimed to fix.
- `xerj-logs` columnar engine is a dead dependency (zero `use xerj_logs::` in engine/server/api) — logs are inverted-index documents.
- ILM/retention is store-only (DashMap round-trip); no background executor ever deletes expired data.
- Dynamic-mapping `strict` + field-limit accepted but not enforced on the ingest path — mapping explosion not prevented.
- `role_descriptors` accepted then ignored — every API key is effectively superuser.
- No `cargo-audit`/`cargo-deny` gate (528 unscanned dep crates) and no `cargo-fuzz` harnesses.
- Binary/RaBitQ (BBQ) quantization unimplemented; `bbq_*`/`int4_*` mappings silently keep full f32 (no memory saving) — HNSW RAM wall only 4× mitigated by SQ8, no DiskANN.

**Marketing numbers to reconcile with measured reality (honesty-posture debt):**
`<500MB RSS / 1M docs` (measured 596MB / 200k) · `~30MB Docker image` (~75–100MB) · `2–5× compression` (~1.6×) · `2 files/segment` (~4) · `TLS enabled out of the box` (off by default) · `OpenAPI generated from code` (static hand-maintained, 404 live) · `no deep-pagination cliff` (identical 10k cap) · `cost-based planner` (rule/structure-based) · error links use `xerj.io` vs canonical `xerj.org`.

---

## 5. rc5 roadmap — ranked candidates grouped into waves

Ranking = severity × reach (how many pain points / clusters it closes) × inverse effort. The first two waves are the "stop losing to ES" work; the rest expand the moat.

### Wave 1 — Stop losing to ES under load (the two open tickets)
*Highest priority: these gate the core "beats ES" claims and recur across ops-simplicity, resource-cost, scaling-durability, and query-performance.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Bound steady-state RSS under tiny-batch churn** (close the RSS-runaway ticket) | HIGH | L | resource-cost GAP + K8s/community-horror/scaling PARTIALs across C1/C2/C3/C7 — the "deterministic memory / limits work" pitch |
| **Fix mixed read-under-write p99** (snapshot/lock-free memtable read path) | HIGH | XL | query-performance GAP (4 LOSE cells) + ops/scaling PARTIALs — the only benchmark axis ES wins |
| **Reconcile / hit the `<500MB RSS / 1M docs` number** (shrink ingest-path heap or restate) | HIGH | M | resource-cost overclaim reviewers will catch; pairs with RSS-runaway fix |

### Wave 2 — Make the AI-native + log-analytics claims real (unwired scaffolding)
*Highest ROI feature work: the scaffolding largely exists; wiring it converts headline GAPs into differentiators.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Wire `xerj-logs` columnar engine into /logs + /otlp** (or drop the dead dep and the claim) | HIGH | L | ai-logs "dedicated columnar log engine" GAP; unlocks domain-aware compression + block-skip already written |
| **Implement RRF hybrid retriever** (`retriever.rrf` + `sub_searches/rank.rrf`) | HIGH | M | ai-logs hybrid-search GAP — the specific BM25-dominates normalization pain |
| **Background ILM/retention executor** (min_age → delete phase; honor `retention_days`) | HIGH | M | closes BOTH upgrades-datamodel AND ai-logs retention GAPs — nothing deletes old data today |
| **Batched high-throughput log ingest + prove 100K ev/s under an RSS bound** | HIGH | L | ai-logs 100K ev/s GAP (current path is per-doc `index_document`) |
| **Wire dynamic-mapping `strict` + field-limit into the ES ingest path** | HIGH | M | upgrades-datamodel mapping-explosion GAP ("no surprise mappings" is currently false) |

### Wave 3 — Multi-node HA & scale-out (the strategic XL bet)
*The single biggest asterisk on the cost, durability, and scale stories. Decide deliberately: this is where XERJ chooses whether to compete with ES on HA at all.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Authenticated + encrypted cluster transport (mTLS/shared-secret) + real replica shards** | HIGH | XL | scaling master-SPOF GAP + ai-logs 50-node scale-out — today's cluster path is unauth plaintext with cosmetic replicas |
| **Documented durability SLA + Jepsen-style test harness; reconsider default power-loss posture** | HIGH | L | scaling durability GAP — the corpus's headline ES pain (acknowledged-then-lost writes) |
| **Compute-storage separation as a real mode** (S3-backed segments + stateless read replicas) | MED | XL | scaling GAP #10 — the #1 2025–2026 user demand; `ObjectStore` scaffolding exists but default is Local |
| **Per-tenant resource quotas** (memory/CPU/disk/query-slot by namespace) on the governor | MED | L | scaling multi-tenancy PARTIAL — governor is process-wide only today |
| **Honor or reject `number_of_replicas`/`number_of_shards`** instead of silently echoing unbacked values | LOW | S | honesty fix — accepting `replicas:2` implies redundancy that doesn't exist |

### Wave 4 — Security & supply-chain hardening
*Small-effort, high-credibility: turns two unshipped promises and two PARTIALs into wins.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Add `cargo-audit` + `cargo-deny` advisory gate to CI** | MED | S | security dependency-scanning GAP (528 unscanned crates) |
| **Ship `cargo-fuzz` harnesses** for ES-DSL / query_string / bulk parsers | MED | M | security fuzz-testing GAP (the untrusted-input surfaces) |
| **TLS on-by-default via auto self-signed cert** (or first-run prompt) | MED | M | resolves the "secure/TLS by default" overclaim across resource-cost + security |
| **Enforce API-key `role_descriptors`** (real per-index/per-field RBAC) | MED | L | security RBAC PARTIAL — every key is superuser today; turns ES's paid-gate into an XERJ win |
| **Tamper-evident (WORM/append-only) audit log** | LOW | L | security audit PARTIAL (roadmap v0.9) — ES gates real audit behind Enterprise |

### Wave 5 — Semantic quality & vector scale
*Makes the AI-native default actually good and lifts the HNSW RAM wall.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Default (or auto-detect) the neural embedder** instead of lexical feature-hash | MED | M | resource-cost + ai-logs — semantic/RAG/memory quality poor out-of-box |
| **Binary/RaBitQ (BBQ 1-bit) quantization + make `bbq_*` mappings honest** | MED | L | ai-logs quantization PARTIAL (only 4× SQ8 today vs promised 32×) |
| **DiskANN / disk-backed vector index** | MED | L | ai-logs HNSW memory-wall GAP (graph is entirely in-RAM) |
| **Contextual retrieval: surface prev/next sibling chunks + parent metadata on recall** | MED | M | upgrades-datamodel + ai-logs chunking PARTIAL — RAG context quality |

### Wave 6 — DX & claims-honesty reconciliation
*Low-effort credibility work, consistent with the W4 benchmark-honesty campaign. Batch these.*

| Candidate | Sev | Effort | Closes |
|---|---|---|---|
| **Assisted schema-migration / index-versioning path** for field-type changes | MED | L | closes BOTH ops-simplicity migration-tooling AND datamodel immutable-mappings PARTIALs |
| **Generate OpenAPI from code and serve it live at /openapi.json** | MED | M | ops-simplicity api-and-client + docs "generated from code → accurate" claim (static 404 today) |
| **Fix/verify the ES→XERJ data-pull migration** (scroll/search_after → `_bulk`; remove broken `xerj-ingest` ref) | MED | S | datamodel migration PARTIAL — documented snippet references a non-existent binary |
| **Prove multi-version format upgrade** (bump format, add in-place migrator + optional downgrade) | MED | L | hardens no-skip-version / no-reindex / cannot-downgrade promises (only v1 exists today) |
| **Reframe "no deep-pagination cliff"** — lift or honestly restate the 10k cap | MED | S | query-performance overclaim (identical to ES cap) |
| **Reconcile inflated numbers** (30MB→~75MB image / 2–5×→1.6× compression / 2→~4 files-per-segment) | LOW | S | resource-cost honesty debt |
| **Fix `xerj.io`→`xerj.org` domain drift in error links; verify suggested-fix links resolve** | LOW | S | ops-simplicity live-verified doc/UX bug |
| **Reflect dynamically-added fields in GET /_mapping output** | LOW | S | datamodel correctness (dynamic fields queryable but invisible in `_mapping`) |
| **Drop or realize the "cost-based planner" claim** (rule/structure-based today) | LOW | L | query-performance claim-vs-reality (degenerate cases already win) |
| **Prove the no-post-refresh global-ordinals spike** (add first-query-after-refresh probe) | LOW | M | query-performance PARTIAL → DELIVERED with evidence |
| **Distroless/scratch container image** (or drop the ~30MB number) | LOW | M | ops-simplicity + resource-cost image-size overclaim |

---

## 6. Strategic read

- **Ship Waves 1–2 for rc5-core.** They are mostly L/M effort, close the loudest GAPs, and stop XERJ losing to ES on load and on its own log-analytics headline. Wave 1 is the honesty-critical work (the tickets are already public in `ROADMAP.md`).
- **Wave 3 is the fork in the road.** Multi-node HA is XL and changes XERJ's identity from "the simple single-node ES escape hatch" to "an HA search platform." Until it lands, the single-node SPOF, sub-ES power-loss durability, and no-scale-out ceiling are *positioning constraints, not bugs* — market to the ICP where they don't bite (dev/CI, single-node edge, cost-driven right-sizing) and stop implying HA that doesn't exist.
- **Waves 4–6 are cheap credibility.** The security process gates and the numbers-reconciliation are small and directly protect the honesty posture that is itself a differentiator vs ES's "Documentation ≠ API ≠ Reality."
- **The moat that already exists:** no-JVM CVE/GC elimination, Apache-2.0 trust, ES-wire reversibility, filter-pushdown HNSW, `/_memory`, and GDSR-forcemerge purge are real today and unmatched by ES. Defend and market those; fix the rest honestly.
