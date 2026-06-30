# Path to 100% — improvements plan v0.6.0 → v1.0

**Baseline:** v0.6.0 weighted delivery ≈ **46%** (see `FEATURE_FAIRNESS_REVIEW_v0.6.0_2026-04-25.md`).

**Target:** **100%** of the all-in-one-ES-killer-for-AI-transformation-enterprises pitch by v1.0.

**Two tracks per milestone:**

| Track | Definition | Gating criterion |
|---|---|---|
| **Production-first** | The thing customers WILL hit in production. If it's missing or wrong, support tickets, downtime, data loss. | Integration test green; runbook published; "what happens when…" answered for every failure mode. |
| **Enterprise-POV win** | The thing an enterprise prospect evaluates during pilot / RFP / procurement. If it's missing, the deal stalls or moves to a competitor. | Sales-engineering can demo it end-to-end without scripts; it appears as "✅" on a customer comparison sheet. |

These are **not** the same. A persistent HNSW graph is production-first (data loss risk). A Helm chart is enterprise-POV (deal won't close without it). Both must ship — the split tells you who blocks if it slips.

---

## TL;DR — milestone plan

| Milestone | Window | Theme | Production-first focus | Enterprise-POV focus | Delivery target |
|---|---|---|---|---|---|
| **v0.6.1** | 2 weeks | Stop lying | Replace stub 200-OKs with 501; document deployment security model | — | 48% |
| **v0.6.2** | 4 weeks | Vector durability | HNSW persistence + soft-delete + segment integrity checksums | — | 52% |
| **v0.7** | 8 weeks | **The AI gap** | Wire `hybrid` + `semantic` executors; reranker hook; persistent agent memory; token-budget retrieval | Live demos for hybrid+rerank; first AI-pilot customer | 65% |
| **v0.8** | 8 weeks | **Cloud-native** | Real snapshots; backup automation; tracing emit | Helm chart; k8s operator + CRDs; HPA metrics; storage tiering; managed-cloud reference deploy | 78% |
| **v0.9** | 8 weeks | **Security + compliance** | Engine-level TLS; RBAC; audit log (WORM); GDPR sync-erasure | SOC 2 Type I in flight; ISO 27001 gap-assessment; SSO via OIDC | 89% |
| **v1.0-rc** | 4 weeks | Hardening | One full ES YAML run on every supported endpoint; chaos test; perf-regression CI; LTS branch | Customer-facing security & compliance pack | 95% |
| **v1.0** | 2 weeks | Release | Final docs, runbook, support contracts, SOC 2 Type I attestation | Public launch; comparison microsite vs ES/Pinecone/Qdrant | 100% |

**Total elapsed:** ~36 weeks (~9 months). Could compress to ~24 weeks with parallel teams (AI-track + cloud-track + security-track), but on a single Rust+Go core team this is realistic.

---

# v0.6.1 — "Stop lying" (2 weeks, hygiene)

## Production-first

| ID | Item | Where | Effort | Gate |
|---|---|---|---|---|
| 6.1-P1 | Replace stub 200-OK with `501 Not Implemented` on EQL, Watcher, Transform, ILM execute, ingest-pipeline processors, percolator, ES\|QL, Anomaly Detection | `xerj-api/src/es_compat.rs` (~10 handlers) | 1 d | Stub endpoints return `501 + Retry-After` referencing roadmap URL |
| 6.1-P2 | Document the deployment-security model in `engine/README.md` and `landing/security/index.html`: "xerj expects to run behind a TLS-terminating reverse proxy with OS-level FDE" | docs only | 0.5 d | One-page `engine/reports/SECURITY_DEPLOYMENT_MODEL.md` linked from README |
| 6.1-P3 | Add a startup banner: "TLS terminated externally" if `tls.enabled=false`; "RBAC not enforced" always | `xerj-server/src/main.rs` | 0.5 d | Operator sees the banner in stdout on startup |
| 6.1-P4 | Open-source the BRIEF_GAPS.md alongside marketing PDFs (don't hide the gaps from customers) | brief/ | 0.5 d | Public-facing `gaps.html` page on landing site |

## POV win

None — this milestone is purely about restoring honesty before the next pitch.

## Out of scope

- Persistence work (v0.6.2)
- New features (v0.7+)

---

# v0.6.2 — "Vector durability" (4 weeks)

## Production-first

| ID | Item | Where | Effort | Gate |
|---|---|---|---|---|
| 6.2-P1 | **Persist HNSW graph** to disk. Define an on-disk format (header + nodes + neighbor-lists + level-promotion) co-located with the index segment dir. Load on restart. Periodic checkpoint after N inserts or M seconds. | `xerj-vector/src/hnsw.rs` + new `hnsw_codec.rs` | 8 d | After kill -9 + restart on a 1M-vector index, query latency p99 returns to within 10% of pre-crash within 5 s (vs O(N log N) rebuild today) |
| 6.2-P2 | **Soft-delete on HNSW graph.** Tombstone bit per node; filtered out of beam search; physically removed during merge. | same | 4 d | `delete_document` followed by kNN does not return the deleted doc (today: it's still in neighbor lists for ~hours) |
| 6.2-P3 | **Segment-level integrity checksums.** Blake3 hash per stored / postings / doc-values block; verified on read; fails fast on corruption. | `xerj-storage/src/segment.rs` | 4 d | A bit-flip in a segment file produces an `Err(Corruption)` on read, not silent garbage |
| 6.2-P4 | **PIT garbage collection.** Per-PIT TTL (default 5 min, configurable); background sweep evicts expired PITs. | `xerj-engine/src/engine.rs` (PitContext) | 2 d | An open PIT not refreshed within TTL is dropped; memory does not grow with abandoned PITs |
| 6.2-P5 | One chaos test that does: ingest 1M docs + vectors, kill -9 mid-flush, restart, verify all docs and vectors recoverable; verify HNSW graph reload time. | `tests/chaos/` | 3 d | Test green on every CI run |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 6.2-V1 | "xerj survives kill -9" video (60 s) on landing/security/durability — show the chaos test on screen | 1 d | Video on landing page |

## Out of scope

- AI executors (v0.7)
- Cloud-native (v0.8)

---

# v0.7 — "The AI gap" (8 weeks) — the **biggest** delivery jump

This milestone takes us from "ES-compat search engine with bolted-on vectors" to **a real AI-transformation engine**. Today the marketing brief promises this and the code does not deliver. By v0.7 the code delivers.

## Production-first

| ID | Item | Where | Effort | Gate |
|---|---|---|---|---|
| 7-P1 | **Wire `hybrid` executor** (RRF fusion of BM25 + kNN). Single search request, two sub-queries, rank merge with k=60 default + configurable; both `linear` (weighted sum) and `rrf` strategies supported. | `xerj-engine/src/index.rs` add `QueryNode::Hybrid` arm; `xerj-query/src/executor.rs` add `merge_hybrid()` | 8 d | YAML test `hybrid_search_basic.yml` passes; benchmark p95 for hybrid < 50 ms on 1M docs |
| 7-P2 | **Wire `semantic` executor** (auto-embed query text → kNN). Calls `EmbeddingProxy` with cached client; on success runs filtered kNN. On embedding-proxy timeout, falls back to BM25 (configurable: error vs fallback). | same files; +async path | 5 d | YAML test `semantic_search_basic.yml` passes; in-process embedding cache hits 80%+ for repeated queries |
| 7-P3 | **Wire `function_score` time-decay** (gauss/exp/linear over date fields). | `xerj-engine/src/scoring.rs` | 3 d | All three decay functions return scores within 1e-9 of ES reference for unit cases |
| 7-P4 | **Persistent agent memory** — back the in-memory `AgentMemory` store with the same WAL+segment layer the rest of xerj uses. Same recall semantics, but survives restart. | `xerj-ai/src/memory.rs` + new `xerj-ai/src/memory_store.rs` | 6 d | Restart test: write 100K memories, kill -9, restart, all 100K recallable |
| 7-P5 | **Token-budget retrieval cap.** New search param `token_budget` (default unlimited). Sum of `_source` token counts across returned hits ≤ budget; truncate from the bottom. | `xerj-query/src/executor.rs` | 2 d | YAML test verifies returned hits' source token sum ≤ budget |
| 7-P6 | **AI integration test suite** — currently zero. Cover hybrid + semantic + rerank + memory + chunker + embedding-proxy mock. | new `tests/ai-integration/` | 4 d | 50+ tests green, runs on every PR |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 7-V1 | **Cross-encoder reranker hook** — POST a list of (query, doc) pairs to a configurable HTTP reranker endpoint (Cohere Rerank, Voyage, custom); blend with kNN scores via reciprocal-rank or replace. Reference adapter for Cohere v3. | `xerj-ai/src/rerank.rs` (new); param `rerank: { model, top_n }` in search body | 5 d | Live demo: hybrid search → top 100 → rerank → top 10; nDCG@10 improves measurably on MS MARCO subset |
| 7-V2 | **Cohere + Anthropic + Voyage embedding adapters** beyond OpenAI-compatible. Auth styles, response shapes. | `xerj-ai/src/embed.rs` extend with adapter trait | 4 d | All three vendors tested with real keys, results identical (within float tolerance) to vendor's own SDK |
| 7-V3 | **One enterprise pilot customer** running v0.7 on a real RAG workload (≥ 1M docs, ≥ 100 QPS). | sales/SE | parallel | Customer signs reference letter |
| 7-V4 | **AI-track demo notebook** in `playground/` — Jupyter notebook that ingests Wikipedia subset, embeds with OpenAI, runs hybrid + semantic + rerank, shows the difference in result quality. | playground/ | 3 d | Notebook ships in repo; embedded video on landing/ai/index.html |

## Brief corrections required by v0.7

- "Hybrid BM25 + vector in one query" — now true (was stub).
- "Semantic search with auto-embedding" — now true (was stub).
- "Reranker / cross-encoder integration" — now true (was missing).
- "Token-budget-aware retrieval" — now true (was missing).
- "Agent memory store" — now durable (was ephemeral).

## Out of scope

- LLM tool/agent loop (v0.7.x or v0.8 — depends on customer pull)
- Multi-modal embeddings (v0.9+)
- Inline embeddings (v0.7.x; small win)

---

# v0.8 — "Cloud-native" (8 weeks)

This is the milestone where xerj stops being a binary you `scp` to a box and becomes something an SRE deploys with `helm install xerj` or applies a Kubernetes Custom Resource for.

## Production-first

| ID | Item | Where | Effort | Gate |
|---|---|---|---|---|
| 8-P1 | **Real snapshot implementation** — segment serialization to local FS / S3 / GCS; incremental snapshots (only new segments); restore with checksum verification; snapshot manifest format. | `xerj-engine/src/snapshot.rs` (new); `xerj-storage/src/backend/s3.rs` extend | 8 d | YAML test: create snapshot, delete index, restore from snapshot, all docs queryable; snapshot integrity Blake3-verified on restore |
| 8-P2 | **Backup automation** — scheduled snapshots (cron-style); SLM policy execution; retention enforcement (e.g. keep daily 30, weekly 12, monthly 12). | new `xerj-engine/src/slm.rs` | 4 d | Configure a daily SLM policy with 30-day retention; verify after 31 days only the 30 most-recent exist |
| 8-P3 | **OTLP trace emit** — convert tokio-tracing spans to OTLP and push to a configured collector. Minimum: index, search, bulk, kNN, hybrid spans. | `xerj-api/src/trace.rs` (new); add `opentelemetry-otlp` dep | 4 d | Spans visible in Jaeger / Honeycomb / Datadog; trace ID round-trips from incoming `traceparent` header |
| 8-P4 | **Liveness vs readiness probes** — `/health/live` (always 200 if process alive) and `/health/ready` (200 only when `Engine::new` finished + replay done + at least one shard available). | `xerj-api/src/router.rs` | 1 d | k8s probe configs in Helm chart use these endpoints |
| 8-P5 | **Hot config reload** — SIGHUP re-reads config file; applies changes that are safe to apply at runtime (limits, log level, merge policy); rejects others (data_dir, ports) with clear error. | `xerj-server/src/main.rs` | 4 d | `kill -HUP` changes `RUST_LOG` level without dropping any in-flight request |
| 8-P6 | **Slow query log** — configurable threshold (default 1 s p99); emits to stderr + Prometheus metric; ring buffer of last N slow queries available via `/_nodes/hot_threads`-style endpoint. | `xerj-engine/src/index.rs` search hook | 2 d | YAML test: send a guaranteed-slow query, verify it appears in `/v1/admin/slow_queries` |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 8-V1 | **Helm chart** — `helm install xerj/xerj` with values.yaml covering data_dir, replica count, storage class, TLS secret, prometheus annotations, autoscaling. | `deploy/helm/xerj/` (new) | 5 d | Chart linted; chart-testing CI; reference deploy on minikube + EKS + GKE + AKS |
| 8-V2 | **Kubernetes operator + CRDs** — `XerjCluster`, `XerjIndex`, `XerjSnapshotPolicy`. Operator written in Go (kubebuilder) or Rust (kube-rs); reconciles to running pods. | new repo `xerj-operator/` | 12 d | `kubectl apply -f xerj-cluster.yaml` brings up a 3-node cluster with 1 index and a snapshot policy, all without imperative kubectl |
| 8-V3 | **HPA-friendly custom metrics** — pending docs, replication lag, query queue depth, p99 latency exposed in a shape that Kubernetes Custom Metrics API can scale on. | `xerj-common/src/metrics.rs` | 3 d | HPA scales replica count when query queue depth > 100 for 60 s; shrinks back when queue empty |
| 8-V4 | **Storage tiering** — hot tier (local SSD), warm tier (S3 / GCS / Azure Blob); automatic promotion based on `last_query_time` (default: warm → hot on first query, hot → warm after 7 days untouched). | `xerj-storage/src/tier.rs` (new) | 8 d | YAML test: ingest, promote to warm, query, verify result identical to hot, measure latency penalty (target p99 + 100 ms acceptable) |
| 8-V5 | **GCS + Azure Blob backends** to match S3 (currently in deps but un-wired). | `xerj-storage/src/backend/{gcs,azure}.rs` | 4 d | Snapshot+restore round-trip works on each backend |
| 8-V6 | **Multi-arch container** — single `xerj/xerj:0.8.0` image manifest covering linux/amd64 + linux/arm64 (and optionally distroless). | `Dockerfile` + GH Actions buildx | 2 d | `docker pull xerj/xerj:0.8.0` works on both archs from a single tag |
| 8-V7 | **Reference cloud deploys** — Terraform modules for AWS EKS, GCP GKE, Azure AKS. | `deploy/terraform/{aws,gcp,azure}/` | 6 d | `terraform apply` brings up a 3-node xerj cluster on each cloud |

## Brief corrections by v0.8

- "Cloud-native deployment" — now true (Helm + operator).
- "Multi-cloud blob storage" — now true (S3 + GCS + Azure all wired).
- "Production-ready snapshots" — now true.

## Out of scope

- Multi-region failover (v0.9 or v1.x)
- Managed SaaS (v1.x — separate product surface)

---

# v0.9 — "Security + compliance" (8 weeks)

This milestone makes xerj procurement-ready for regulated industries (FinServ, Healthcare, Public-Sector, Defense).

## Production-first

| ID | Item | Where | Effort | Gate |
|---|---|---|---|---|
| 9-P1 | **In-process TLS** — wire `tokio_rustls::TlsAcceptor` into the Axum listener; load cert/key from `tls.cert_path` / `tls.key_path`. mTLS optional (`tls.client_ca_path`). | `xerj-server/src/main.rs` | 4 d | Plain TCP rejected when `tls.enabled=true`; mTLS challenge succeeds with valid client cert, fails without |
| 9-P2 | **RBAC** — roles (admin, write, read, read_only_index, snapshot_admin); per-index permissions; FLS (field-level mask) and DLS (per-doc filter query). API: `PUT /_security/role/{name}`, `PUT /_security/user/{name}`. | `xerj-api/src/auth.rs` extend; new `xerj-api/src/rbac.rs` | 12 d | YAML test: read-only role gets 403 on write; FLS user sees `_source.salary` redacted; DLS user only sees docs matching their `customer_id` filter |
| 9-P3 | **API key hashing + rotation** — bcrypt-stored keys; per-key TTL; per-key rate limit. | `xerj-api/src/auth.rs` | 3 d | Keys stored as `$2b$12$...` not plaintext; expired key 401s; per-key rate limit 429s before global limit fires |
| 9-P4 | **Tamper-evident audit log** — every search/index/delete/admin op writes to a WORM segment (append-only, hash-chain over previous entry). Queryable via `GET /_audit/_search`. Configurable retention. | new `xerj-engine/src/audit.rs` | 6 d | YAML test: 1000 ops + 1 simulated tamper attempt → audit verifier detects break in hash chain |
| 9-P5 | **GDPR sync-erasure mode** — `DELETE /{index}/_doc/{id}?force=true` triggers immediate segment merge for that doc; returns deletion certificate (signed receipt). | `xerj-engine/src/index.rs` | 3 d | Doc gone from disk before HTTP response returns |
| 9-P6 | **Engine-level encryption at rest** — AES-256-GCM on segment + WAL + audit log; per-index key wrapped by master key; master key from env/file/KMS. | new `xerj-storage/src/crypt.rs`; integrate into segment + WAL writers | 10 d | All on-disk artifacts unreadable without the master key; `cat segment.dat` shows ciphertext; key rotation rewraps without rewriting data |
| 9-P7 | **Data retention enforcement** — background task runs the `logs.retention_days` policy (and per-index override); deletes documents past retention; deletion logged to audit. | `xerj-engine/src/retention.rs` (new) | 3 d | Set retention=1d; insert doc with timestamp 2d ago; verify it's gone within one cycle |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 9-V1 | **OIDC / OAuth 2.0 SSO** — accept Bearer JWTs from a configured OIDC IdP (Auth0 / Okta / Azure AD / Google Workspace); map JWT claims to roles. | `xerj-api/src/auth_oidc.rs` (new) | 5 d | Sign in via Auth0 / Okta; token authorizes per RBAC mapping |
| 9-V2 | **SAML 2.0 federation** (gateway-style — not full IdP) for environments that mandate SAML. | optional v1.0 if time-constrained | 4 d | Demo with Okta SAML against a Kibana-style frontend |
| 9-V3 | **SOC 2 Type I in flight** — engage auditor (Vanta / Drata / direct CPA), document controls, evidence collection turned on. Certification follows in 6 months; "in flight" status is the v0.9 milestone. | external + ops | 3 weeks elapsed | Letter from auditor naming xerj as engaged customer |
| 9-V4 | **ISO 27001 gap assessment** — formal gap report from a 27001 lead auditor; remediation plan dated. | external | 2 weeks elapsed | Gap report received |
| 9-V5 | **Customer-facing security pack** — one-page deployment-security model + SOC 2 status letter + ISO gap status + threat model + dependency provenance / SBOM. | docs | 3 d | Pack is a single PDF, downloadable from landing/security/ |
| 9-V6 | **PII detection at ingest** (optional pipeline processor) — regex + heuristics for email, phone, SSN, credit card; flagged into a `_metadata.pii` array. | `xerj-engine/src/pipeline/pii.rs` (new) | 4 d | YAML test: ingest sample doc; `_metadata.pii` contains the detected categories |

## Brief corrections by v0.9

- "Audit-grade by default" — now defensible (WORM audit log + SOC 2 in flight).
- "Unified RBAC across logs/vectors/memory" — now true.
- "Encryption at rest" — now true (engine-level).
- "TLS in transit" — now true in-process (was reverse-proxy reliant).

## Out of scope

- HIPAA attestation (v1.x once SOC 2 lands)
- PCI-DSS attestation (v1.x; needs CHD tokenization service first)
- FedRAMP (separate multi-year program)

---

# v1.0-rc — "Hardening" (4 weeks)

## Production-first

| ID | Item | Effort | Gate |
|---|---|---|---|
| 1.0-RC-P1 | **ES YAML conformance run on every supported endpoint** — current: 1304/1329 on covered subset. Target: ≥ 1320/1329 on supported endpoints; the rest documented as out-of-scope (anomaly detection, ES\|QL, etc.) with 501 responses. | 5 d | CI gate on this number per release |
| 1.0-RC-P2 | **Chaos test matrix** — kill -9, disk-full, network partition, clock skew, CPU throttle. Each cluster topology survives. | 5 d | All scenarios green on every PR |
| 1.0-RC-P3 | **Perf-regression CI** — ingest, search, agg, kNN, hybrid, semantic benchmarks committed to history; PRs blocked if p99 regresses > 10%. | 3 d | Bench dashboard at `bench.xerj.io` |
| 1.0-RC-P4 | **LTS branch** — `release/v1.0` cut; backport policy documented; semver guarantees codified. | 1 d | Branch exists; backport CI green |
| 1.0-RC-P5 | **Migration guide from ES** — concrete steps to switch from ES 7.x / 8.x to xerj, with rollback procedure (the wire-protocol guarantee makes this practical). | 5 d | Customer can follow guide unaided in a half-day spike |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 1.0-RC-V1 | **Customer-facing security & compliance pack v2** — SOC 2 Type I in late stage; full SBOM in CycloneDX; pen-test report from external firm; vulnerability disclosure policy. | 5 d | Single PDF on landing/security/ |
| 1.0-RC-V2 | **Comparison microsite** — `compare.xerj.io` with side-by-side feature matrix vs ES 8.x, OpenSearch, Pinecone, Qdrant, Weaviate, Vespa. Honest. Includes a "what we do NOT do" section. | 5 d | Microsite live; press kit ready |
| 1.0-RC-V3 | **Two reference customers** in production on v0.9 → v1.0-rc; reference letters published. | parallel | Logos + quotes on landing/customers |

---

# v1.0 — "Release" (2 weeks)

## Production-first

| ID | Item | Effort | Gate |
|---|---|---|---|
| 1.0-P1 | **SOC 2 Type I attestation received** | external | Letter |
| 1.0-P2 | **Final docs pass** — all "TODO" / "stub" markers removed; every documented feature has an integration test; runbook covers every alert. | 5 d | Docs CI green |
| 1.0-P3 | **Support contracts open for sale** — business-hours (24h response) and 24×7 (1h response) tiers; on-call rotation defined; runbook handed to on-call. | 3 d | First paid contract executed |
| 1.0-P4 | **Release-engineering checklist** — version bump, tag, binary matrix, signed releases (sigstore/cosign), artefact provenance (SLSA L3), Helm chart published, operator image published, GH release notes. | 2 d | All artefacts signed; verifiable from a fresh machine |

## POV win

| ID | Item | Effort | Gate |
|---|---|---|---|
| 1.0-V1 | **Public launch** — landing page refresh, blog post, comparison microsite, two customer-reference videos, demo video. | parallel | Day-1 traffic + sign-ups measured |
| 1.0-V2 | **Open-source community kit** — CONTRIBUTING.md, governance, roadmap-as-code (RFC repo), Discord. | 3 d | First external PR merged within 30 d of launch |
| 1.0-V3 | **Pricing & commercial story** finalised — open-source forever; managed SaaS (later); enterprise support (now). | parallel | Pricing page live |

---

# Cross-cutting investments (parallel to all milestones)

These don't fit a single milestone; they need a part-time stream:

| Theme | Investment | Owner | Cadence |
|---|---|---|---|
| **ES YAML conformance** | Drive 1304 → 1320+ on supported; 0% on out-of-scope returning 501 | search-engine eng | every PR |
| **Bench-as-CI** | Track ingest, search, agg, kNN, hybrid, semantic latencies vs main; alert on >10% regression | perf eng | every PR |
| **SBOM + vulnerability scanning** | `cargo audit`, `cargo deny`, `grype` on container | security eng | every PR |
| **Documentation site** | docs.xerj.io with versioned docs (MkDocs / Docusaurus); API reference auto-generated from OpenAPI spec | docs | continuous |
| **Customer success motion** | One pilot per milestone; weekly check-ins; feedback loop into roadmap | sales eng | weekly |
| **Brief / website honesty audit** | Every milestone: re-run the fairness review; fix any regressions in claim accuracy | product marketing | each milestone |

---

# Risk register

Three risks that could derail the plan:

## Risk 1 — AI executors (v0.7) take 2× the estimate

**Likelihood:** medium · **Impact:** high (slips v0.8 and v0.9, jeopardises v1.0)

**Why:** wiring `hybrid` and `semantic` looks straightforward but interacts with the planner, the response builder (RRF tie-breaks, score normalization), explain-plan, profile API, request cache, and test infrastructure. This is the kind of refactor that finds 5 latent bugs.

**Mitigation:** start v0.7 with an integration-test scaffold first (write the failing tests, then implement). Pair AI eng + search eng for the first 2 weeks. If 4 weeks in we're not at 50% test pass, reduce v0.7 scope to `hybrid` only and slip `semantic` to v0.7.1.

## Risk 2 — SOC 2 Type I takes 9 months instead of 6

**Likelihood:** high · **Impact:** medium (slips v1.0 attestation; "in flight" status still defensible)

**Why:** SOC 2 timelines are auditor-paced, not us-paced. Evidence collection requires 3 months of operational data minimum.

**Mitigation:** start the engagement in v0.7 (start of the AI track), not v0.9. Use Vanta / Drata to compress evidence collection. Be willing to release v1.0 with "SOC 2 Type I expected Q3" rather than "SOC 2 Type I attested."

## Risk 3 — k8s operator (v0.8-V2) needs more team than we have

**Likelihood:** medium · **Impact:** medium (Helm chart still ships; operator slips to v0.9)

**Why:** building a real operator is a 2-engineer-quarter project (kubebuilder learning curve, reconciliation loops, status conditions, CRD versioning, e2e tests on minikube + kind + EKS).

**Mitigation:** ship Helm chart in v0.8 (sufficient for ~80% of buyer evaluations); slip the operator to v0.9 if needed; consider a partner (Percona, OperatorHub maintainers) for the operator implementation.

---

# Success metrics per milestone

| Milestone | Tech metric | Business metric |
|---|---|---|
| v0.6.1 | 0 stub endpoints returning 200-OK; banner shipped | brief / website re-audited honestly |
| v0.6.2 | HNSW restart-rebuild eliminated; chaos test green | one customer reference letter on durability |
| v0.7 | hybrid + semantic + rerank YAML tests green; 50+ AI integration tests | first AI-pilot customer in production |
| v0.8 | Helm chart + operator green on EKS+GKE+AKS; tiering YAML tests green | first paid managed deploy |
| v0.9 | RBAC + WORM audit log + encryption-at-rest YAML tests green; SOC 2 in flight | one regulated-industry customer (FinServ or Healthcare) signed |
| v1.0-rc | 1320+/1329 YAML; pen-test report clean | two reference customers public |
| v1.0 | SOC 2 Type I attested; signed releases; LTS branch | public launch; first paid support contract |

---

# Effort & headcount summary

Estimated engineering effort (developer-weeks):

| Milestone | Production-first | POV win | Total |
|---|---|---|---|
| v0.6.1 | 2.5 | 0 | **2.5** |
| v0.6.2 | 21 | 1 | **22** |
| v0.7 | 28 | 12 + customer | **40** |
| v0.8 | 23 | 40 | **63** |
| v0.9 | 41 | 21 (+ external auditor) | **62** |
| v1.0-rc | 19 | 15 | **34** |
| v1.0 | 10 | 6 | **16** |
| **Total** | **144.5 dev-weeks** | **95 dev-weeks** | **~240 dev-weeks** |

At a sustained 4-engineer core (16 dev-weeks per calendar month), this plan is **~15 calendar months end-to-end** without parallelism, **~9 calendar months** with two parallel tracks (search-eng + cloud-eng). With external help on operator + auditor it can compress further.

---

*Compiled by xerj engineering 2026-04-25. Lives next to `FEATURE_FAIRNESS_REVIEW_v0.6.0_2026-04-25.md`. Update each milestone — keep the delivery % running on the front page.*
