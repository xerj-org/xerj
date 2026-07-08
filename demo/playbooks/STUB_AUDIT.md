# XERJ Stub / Implementation Audit

**Date:** 2026-07-07 · **Method:** multi-agent audit (105 agents) that
extracted every capability claim from the public site + README + docs
(290 claims), cross-referenced each against the actual source, and
verified candidate gaps against the code. **This file is the durable,
honest record.** Losing 100%-ES-compat claims to reality is the point:
we would rather ship an honest error than a silent wrong answer.

## Headline

XERJ is **not** 100% ES-compatible today. The audit confirmed **68 real
implementation gaps**; **34 of them intersect a public claim** (9 stubs,
17 partials, 8 missing). Most publicly-claimed gaps shelter under one
unqualified line — *"drop-in ES replacement"* — on the landing pages.

The four **silent-wrong-answer** exposures (HTTP 200 with fabricated or
wrong data on an advertised endpoint) were the most dangerous and are
**fixed this session** — they now compute the real answer or fail loudly:

| Was | Now | Commit |
|---|---|---|
| `script_fields` returned `null` for every scripted field (silent fake) | Real per-hit Painless evaluation; enforces `index.max_script_fields` | `65ce844` |
| EQL `sequence`/`sample` silently returned **all** docs (or a mangled single condition) | `501 verification_exception`; single-event EQL still works | `fbc817c` |
| `_disk_usage` fabricated the per-field/per-category breakdown (hardcoded 0s, `stored_fields` = whole index) | Real `store_size_in_bytes` total only; `fields` honestly empty | `fbc817c` |
| `phrase` suggester scanned `_all` and emitted garbage | `400 illegal_argument_exception`; `completion`/`term` suggesters work | `fbc817c` |

Each fix ships with a test and the full ES-compat YAML suite stays green
(1328 passed · 0 failed · 3 skipped); a new `search/340_script_fields.yml`
locks in script-field value correctness.

## What the categories mean

- **stub** — the endpoint/feature exists and returns 200, but never
  executes the advertised behaviour (store-only, canned response, no-op).
- **partial** — a common case works, but the *defining* sub-capability is
  missing or fabricated (e.g. single-event EQL works, sequences don't).
- **missing** — declared/scaffolded in code (types, config knobs, dead
  modules) with no execution path wiring it in.

## Recommended next actions (beyond the 4 fixed)

1. **Qualify the public claim.** Replace the bare *"drop-in ES
   replacement"* with a linked, scoped ES-compatibility matrix (the
   internal `ES_COMPATIBILITY.md` is accurate). This converts the
   remaining 30 publicly-claimed partials/stubs into *disclosed*
   limitations without waiting on engine work.
2. **Fail loudly on the remaining silent-wrong-answer partials** (e.g.
   `terms_set` min-should-match-field/script, `span_containing/within`,
   `rank_feature`, `combined_fields` fallbacks, `has_child/has_parent`) —
   return a clear 400/501 rather than an approximate/empty result.
3. **De-scaffold the config knobs that silently do nothing** (vector
   quantization `default_quantization`, `hnsw_offload_threshold`, TLS
   `cfg.tls.enabled`) — either implement, or reject at config load with a
   clear "not implemented" so operators aren't misled.
4. **Schedule the genuinely large builds** (Raft multi-node, distributed
   search/replication, S3 backend, columnar log storage, EQL sequences,
   quantization) — track in `PARITY_BACKLOG.md`; they are honest roadmap,
   not launch blockers, once the claims are qualified.

---

## Publicly-claimed gaps (34) — the honesty-critical set

These intersect a claim on the site/README/docs. Fixed rows first.

| Feature | Status | File | Note |
|---|---|---|---|
| Search: script_fields (POST /{index}/_search with "script_fields") | stub | `crates/xerj-api/src/es_compat.rs:6682` | ✅ FIXED (65ce844) — real per-hit Painless eval + max_script_fields |
| EQL search: POST /{index}/_eql/search (sequence/join queries) | partial | `crates/xerj-api/src/es_compat.rs` | ✅ FIXED (fbc817c) — sequence/sample now 501, single-event works |
| Index disk usage: POST /{index}/_disk_usage (per-field / per-category breakdown) | partial | `engine/crates/xerj-api/src/es_compat.rs:11862` | ✅ FIXED (fbc817c) — real total only, no fabricated breakdown |
| S3 / remote object storage backend with HTTP range reads (advertised pluggable backend) | stub | `crates/xerj-storage/src/backend.rs` | Confirmed real stub, not a false positive. Two layers of non-implementation: |
| SQL HAVING clause (native /_sql surface) | stub | `crates/xerj-engine/src/sql.rs:344` | REAL gap, and worse than "partial." HAVING tokens are consumed by the skip loop at sql.rs:344-371 and discarded — the SqlQuery struct (sql.rs:35-52) has no `hav… |
| TLS/HTTPS on the REST + ES-compat listeners (cfg.tls.enabled) | stub | `crates/xerj-server/src/main.rs` | REAL gap, not a false positive. Verified: serve() (main.rs:332-351) binds a plain tokio::net::TcpListener and runs `axum::serve(listener, router).tcp_nodelay(tr… |
| has_child / has_parent parent-child join queries | stub | `crates/xerj-query/src/planner.rs:633` | Confirmed real gap. The defining join semantics are entirely absent on the normal execution path — both execution paths run the inner query flat and discard the… |
| index.default_pipeline setting auto-applied on single-document writes (PUT/POST /{index}/_doc[/{id}]) | stub | `engine/crates/xerj-engine/src/index.rs:1049-1063` | REAL gap, but the candidate's stated evidence is partly inaccurate and needs correction. Verdict: stub (log-only no-op) for the specifically-named feature — the… |
| percolate query (POST _search {"percolate":{...}}) | stub | `crates/xerj-query/src/parser.rs` | CONFIRMED real stub. At parser.rs:309 the `percolate` query type is hard-wired to `Ok(QueryNode::MatchNone)` with the inline comment "percolate not supported". … |
| search script_fields (computed per-hit fields) | stub | `crates/xerj-api/src/es_compat.rs:6681-6686,8927-8935` | Confirmed real no-op stub on the normal path. The parser stores script_fields as opaque JSON (parser.rs:208) with no evaluation. In es_compat.rs:6682-6686 the h… |
| xerj-logs LogIngester flush-to-storage / durability (flush_threshold) | stub | `crates/xerj-logs/src/ingest.rs:276` | Confirmed a genuine no-op, not a false positive. At ingest.rs:276-280 the flush-threshold branch fires on the normal path but only emits debug!("...reached flus… |
| Agent memory (/_memory) — semantic dedup + recency-blended recall | partial | `engine/crates/xerj-ai/src/memory.rs` | Real gap, verified in both directions. The AgentMemory module in memory.rs is fully implemented and unit-tested (real cosine dedup, real e^(-age/7d) recency ble… |
| Cluster / Raft consensus mode (multi-node) — ClusterRunner::run() driving Raft over TcpTransport (cfg.cluster.enabled) | partial | `crates/xerj-cluster/src/runner.rs:92` | REAL gap, confirmed. The production-wired loop (spawned at main.rs:927) is ClusterRunner::run() (runner.rs:99-126), which only does: check-shutdown -> tokio::ti… |
| Hybrid search with FusionStrategy::Learned (learned/rerank fusion) | partial | `crates/xerj-engine/src/index.rs` | Real gap. The `Learned` fusion strategy is a genuine AST variant (ast.rs:66-67, documented as "weights stored in the index metadata") and the parser accepts "le… |
| Ingest pipelines on the _bulk API (per-item `pipeline` metadata execution) | partial | `crates/xerj-engine/src/bulk.rs:1030` | REAL gap, confirmed. The ingest-pipeline engine is genuinely implemented and works on the single-doc paths: Engine::process_through_pipeline (engine.rs:915) com… |
| Reindex: POST /_reindex (indices larger than 100k docs) | partial | `crates/xerj-api/src/es_compat.rs` | REAL partial gap, confirmed. reindex() (es_compat.rs:12931-13050, sole impl, routed router.rs:309) copies docs for small indices correctly but cannot faithfully… |
| asciifolding token filter (ES parity for accent/diacritic removal) | partial | `crates/xerj-fts/src/analyzer.rs:499` | Real, wired production code — not a stub or dead branch. AsciiFoldingFilter (analyzer.rs:485-497) is routed both as a builtin filter (resolve_builtin_filter, li… |
| combined_fields query (ES 7.13+ term-statistics pooling across fields) | partial | `crates/xerj-query/src/parser.rs:468` | Real partial, not a stub. parse_combined_fields (parser.rs:468) rewrites the query to multi_match type=cross_fields, forwarding all user params (query, fields, … |
| has_child / has_parent join queries | partial | `crates/xerj-engine/src/index.rs:13256` | REAL gap, correctly labeled partial. Traced the full path: parser (parser.rs:3021-3075) fully parses has_child/has_parent into QueryNode::HasChild{child_type,qu… |
| kNN num_candidates parameter (nested knn query) | partial | `crates/xerj-engine/src/index.rs:4640` | Genuine but narrow partial, not a stub and not fully ignored. Traced the whole path: the AST `QueryNode::Knn` carries only `{field, vector, k, filter, boost}` —… |
| query_string query (Lucene syntax) — fallback path for inputs that don't lower | partial | `crates/xerj-query/src/planner.rs:311` | The specific flagged line (planner.rs:311, the FtsSearch-on-"_all"-single-token branch) is DEAD CODE and its comment is stale, so as stated the candidate's evid… |
| rank_feature query (saturation/log/sigmoid/linear proximity scoring on rank features) | partial | `crates/xerj-query/src/parser.rs:3597` | Confirmed partial, not a stub and not fully working. parse_rank_feature (parser.rs:3597) reads only `field` and `boost` and rewrites the query into a function_s… |
| semantic_text field type / `semantic` query — default (no EmbeddingProxy) built-in embedder | partial | `crates/xerj-ai/src/local.rs:12` | Verified in code, not a stub. local_embed() (local.rs:63) is a genuine feature-hashing embedder — word unigrams (w=1.0) + padded char trigrams (w=0.35) hashed w… |
| span_containing / span_within queries | partial | `engine/crates/xerj-query/src/parser.rs:3006` | Confirmed real partial gap. At parser.rs:317 both `span_containing` and `span_within` route to `parse_span_containing_like` (lines 3007-3016), which reads only … |
| terms_set query with minimum_should_match_field / minimum_should_match_script | partial | `crates/xerj-query/src/parser.rs:3433` | Confirmed real partial gap. In parse_terms_set, the presence of `minimum_should_match_field` or `minimum_should_match_script` is detected but the actual per-doc… |
| top_hits aggregation with seq_no_primary_term: true | partial | `crates/xerj-engine/src/aggs.rs:7508` | Real but narrow gap. The top_hits aggregation is fully implemented (sort, source filtering, fields/docvalue_fields/stored_fields, version, _nested, matched_quer… |
| 4-bit quantization (Scalar4 / nibble-packed, advertised 8x compression + automatic hnsw_offload_threshold downgrade) | missing | `crates/xerj-vector/src/quantizer.rs:329` | REAL gap. Scalar4Quantizer (quantizer.rs:329-460) is a full, unit-tested Quantizer impl (encode/decode/quantize/distance + 4 passing tests at lines 527-590), so… |
| API tokens (/auth/api-tokens) — token issue/list/revoke (+ Bearer token auth) | missing | `crates/xerj-console-api/src/auth/store.rs:278` | Genuine dead scaffold with no execution path. store.rs:267-321 defines struct ApiToken plus put_api_token/get_api_token/list_api_tokens_for_user/revoke_api_toke… |
| Automatic document chunking before embedding on ingest (xerj-ai TextChunker) | missing | `crates/xerj-ai/src/chunker.rs:72` | REAL gap. TextChunker::chunk (chunker.rs:72) is itself a complete, tested implementation (sentence/word-boundary splitting, overlap, UTF-8-safe, 6 passing tests… |
| HNSW quantization offload threshold (config-driven auto-switch to Scalar4 to conserve memory) | missing | `crates/xerj-vector/src/quantizer.rs:321` | Genuine gap, no execution path exists. (1) `hnsw_offload_threshold` appears in ZERO Rust code except the doc comment at quantizer.rs:321. The VectorConfig struc… |
| Intelligent automatic field encoding engine (FieldAnalyzer/FieldEncoding) — per-field smart compression + the user-facing /v1/indices/:name/encodings endpoint and dashboard top_encodings stats | missing | `crates/xerj-compress/src/field_codec.rs (analyzer, fully implemented but orphaned); dead driver at /home/claude/ai/xerj/engine/crates/xerj-engine/src/memtable.rs:352 (collect_sample); empty-forever aggregator at memtable.rs:1288; consumer at /home/claude/ai/xerj/engine/crates/xerj-api/src/native.rs:1796` | REAL gap (dead scaffold), not a false positive — but with an important nuance. The FieldAnalyzer/FieldEncoding code in field_codec.rs is genuinely implemented a… |
| SSO / IdP configuration (OIDC / SAML federated identity) | missing | `crates/xerj-console-api/src/indices.rs:26` | Real gap, correctly identified as dead scaffold. The `.xerj_idp_config` index (IDP_CONFIG) is declared at indices.rs:26, added to the ALL set (line 43), given a… |
| Xerj Console cluster RAFT-state / topology endpoints (and the `.xerj_cluster_state` system index that backs them) | missing | `crates/xerj-console-api/src/indices.rs:27` | REAL gap, verified in both directions. (1) Dead scaffold confirmed: `CLUSTER_STATE = ".xerj_cluster_state"` (indices.rs:27) has a full per-node Raft schema (rol… |
| dense_vector quantization / memory-efficient vector storage (config default_quantization: none/scalar8/binary, advertised 4-32x memory reduction) | missing | `crates/xerj-vector/src/quantizer.rs` | REAL gap (dead scaffold with a silently-inert config knob). The quantizer.rs module itself is fully implemented and unit-tested in isolation (NoneQuantizer, Sca… |
---

## Internal gaps (34) — not on the public landing pages

Stubs/partials on surfaces XERJ does not advertise (Kibana/ES-client
handshake shims, internal console UI, cluster/infra scaffolds). Lower
public-risk, but several are disclosed only in internal docs.

| Feature | Status | File | Note |
|---|---|---|---|
| Suggesters: POST /_search "suggest" with a phrase suggester | partial | `crates/xerj-api/src/es_compat.rs:8760` | ✅ FIXED (fbc817c) — now 400 illegal_argument; completion/term work |
| .sidx per-segment skip index for fast random access within a segment | stub | `crates/xerj-storage/src/segment.rs` | Confirmed real gap (stub), not a false positive. SegmentWriter::finish() (segment.rs:388-405) writes a .sidx on every flush containing exactly one placeholder p… |
| CCR auto-follow: PUT /_ccr/auto_follow/{name} | stub | `crates/xerj-api/src/es_compat.rs:23825` | Verified stub / fake-success no-op. put_ccr_auto_follow (es_compat.rs:23825) inserts the body into state.engine.ccr_auto_follow (a DashMap declared at engine.rs… |
| Distributed document indexing — SearchCoordinator::route_index routes a write to the owning shard node | stub | `crates/xerj-cluster/src/coordinator.rs:283` | The evidence is technically accurate: route_index's local/default branch (lines 281-288) logs "Indexing document locally" and returns a fabricated IndexResponse… |
| Distributed search fan-out to remote shard nodes (SearchCoordinator::search over SearchTransport) | stub | `engine/crates/xerj-cluster/src/coordinator.rs:85` | REAL gap — dead scaffolding, not a false positive. Verified: (1) SearchCoordinator is never constructed outside #[cfg(test)]; grep for `SearchCoordinator::new`/… |
| Security: POST /_security/api_key (create API key) | stub | `crates/xerj-api/src/es_compat.rs:19457` | security_create_api_key (es_compat.rs:19457-19487) returns a well-formed, unique-per-call ES-shaped credential (v4 UUID id, base64 api_key, base64(id:api_key) e… |
| Smart per-field encoding analysis in the doc-values memtable (FieldAnalyzer / collect_sample / ANALYSIS_THRESHOLD → /v1/indices/:name/encodings) | stub | `crates/xerj-engine/src/memtable.rs:352` | Real gap, not a benign dead-scaffold. collect_sample (memtable.rs:352) is #[allow(dead_code)] with ZERO live call sites (verified by exhaustive grep: only its d… |
| Synchronous / Quorum WAL replication durability guarantee (ReplicationMode::Sync / Quorum) | stub | `engine/crates/xerj-cluster/src/replication.rs:217` | CONFIRMED stub, and the reality is worse than the candidate alleges. (1) Fake durability ACK: at replication.rs:217, replicate_and_wait counts an ACK as `transp… |
| TurboIngestPipeline parallel tokenisation (public new(batch_size, parallel) API + turbo_parallel config) | stub | `engine/crates/xerj-engine/src/turbo_ingest.rs:270` | VERIFIED real no-op. The `parallel: bool` field is stored by the constructor (turbo_ingest.rs:279-284) and never read anywhere — grep across the crate shows ref… |
| WAL replication wire protocol (replication messages over ClusterTransport) | stub | `engine/crates/xerj-cluster/src/replication.rs` | REAL gap — a stub, not a false positive. The wire protocol has only a send half and no decode/apply half. make_replication_raft_message (line 242) is an admitte… |
| Watcher: PUT /_watcher/watch/{id} and POST /_watcher/_start | stub | `crates/xerj-api/src/es_compat.rs` | Confirmed stub. put_watch (es_compat.rs:18502) genuinely persists the watch body into engine.watches (DashMap<String,Value>, engine.rs:172) and get/delete round… |
| gRPC API (advertised on :8081) — XerjSearch service (Search/Index/BulkIndex/Get/Delete/Health) | stub | `crates/xerj-server/src/main.rs:356` | CONFIRMED real gap, honestly disclosed. serve_grpc_placeholder (main.rs:356, spawned at main.rs:1036) binds the :8081 TCP port and then accepts every connection… |
| xerj-logs retention / automatic data expiry ("Automatic data expiry based on retention policy") | stub | `crates/xerj-logs/src/retention.rs` | REAL gap — I'd call it a stub of the feature (more severe than the candidate's "partial"). What genuinely works: RetentionPolicy::is_expired / expired_buckets c… |
| Aggregations inside a hybrid (RRF/Linear/Learned fusion) query | partial | `crates/xerj-engine/src/index.rs` | REAL partial gap, not a false positive. Any QueryNode::Hybrid request short-circuits at index.rs:4692 into run_hybrid() BEFORE the normal aggregation path (run_… |
| Batch vector insert (bulk kNN indexing) — claimed rayon-parallel distance computation | partial | `engine/crates/xerj-vector/src/hnsw.rs:392` | Verified real (narrow) gap. HnswIndex::insert_batch (hnsw.rs:392-397) is a plain serial loop `for (id, vec) in items { self.insert(id, vec)?; } Ok(())`. Its doc… |
| BitsetEnum encoding — documented "__other__" overflow sentinel for >16-cardinality status/method/level fields | partial | `crates/xerj-compress/src/field_codec.rs:439` | CONFIRMED as a real doc-vs-code gap, but the candidate's "data loss / unrecoverable documents" severity framing is overstated. TRUE facts: the doc (lines 406-40… |
| Data sources — GET /data-sources/connections/:id/indices for non-built-in (external) connections | partial | `crates/xerj-console-api/src/data_sources.rs:111` | Real but honestly-disclosed gap, correctly rated partial (not stub, not false positive). The endpoint fully works for the built-in connection: list_indices (dat… |
| Data sources — GET /data-sources/connections/:id/indices/:name/fields for non-built-in (external adapter) connections | partial | `crates/xerj-console-api/src/data_sources.rs:157` | The list_fields endpoint is fully implemented for the built-in connection: it resolves the real index, reads the live schema, and returns each field's name/type… |
| Field-encoding compression stats (compression_ratio / raw_bytes_per_value) surfaced via native _stats encodings endpoint and dashboard top_encodings[] | partial | `crates/xerj-compress/src/field_codec.rs:262` | Confirmed the candidate's evidence: compression_ratio_vs_raw() (field_codec.rs:262) computes raw_bpv/bpv where bpv (bytes_per_value, :156) is a REAL measurement… |
| Fields API / _version runtime field metadata (fields:[_version]) | partial | `crates/xerj-api/src/es_compat.rs:7510` | Confirmed real gap (not a false positive). At es_compat.rs:7510-7518 the Fields API path for `fields:[_version]` unconditionally inserts `json!(1)` — a hardcode… |
| Native dashboard GET /v1/dashboard/summary — per-index size_bytes | partial | `crates/xerj-api/src/native.rs:1864` | CONFIRMED real gap (partial). The dashboard_summary handler (native.rs:1841, routed at router.rs:128) is functional and returns real measured values for doc_cou… |
| Shard router refresh from authoritative cluster metadata (ShardRouter::update_from_metadata) | partial | `crates/xerj-cluster/src/router.rs` | CONFIRMED partial, real gap. update_from_metadata (router.rs:112-118) correctly clears+rebuilds routing_table from metadata.shard_assignments — that half works … |
| Thai analyzer / Thai word segmentation tokenizer | partial | `crates/xerj-fts/src/analyzer.rs` | Confirmed against analyzer.rs:745-804. ThaiTokenizer collects each contiguous run of Thai characters (is_thai, U+0E01-0E3A/U+0E40-0E5B) and emits the ENTIRE run… |
| icu_folding analyzer / Unicode NFKC normalization token filter | partial | `crates/xerj-fts/src/analyzer.rs` | Verified real partial implementation, not a false positive and not a bare stub. On the normal path IcuFoldingFilter::filter (analyzer.rs:814-828) runs for every… |
| top_hits aggregation with version: true | partial | `crates/xerj-engine/src/aggs.rs:7499` | Confirmed: aggs.rs:7497-7499 hardcodes _version:1 for every top_hits hit when `version:true`, and on that branch never consults any version source. The candidat… |
| xerj-fts skip-list accelerated posting traversal (PostingsReader seek/intersection) | partial | `crates/xerj-fts/src/postings.rs:673` | REAL (but internal, dead-code) partial gap — skip-list ACCELERATION is genuinely unimplemented, while plain posting traversal works correctly. |
| Alerting — alert rules / alert fires (xerj-console-api) | missing | `crates/xerj-console-api/src/indices.rs:32` | Real gap, confirmed by tracing. The ALERT_RULES/ALERT_FIRES constants (indices.rs:32-33) get typed schemas (lines 170-182) and are created on every boot by ensu… |
| Columnar on-disk log storage / type-specific encoding (xerj-logs columnar module: ColumnWriter/ColumnReader delta-of-delta, dictionary, bit-packing) | missing | `engine/crates/xerj-logs/src/columnar.rs:173` | CONFIRMED real gap (dead scaffold). The encode/decode code at columnar.rs:173 is real and correct (delta-of-delta timestamps, delta i64, dictionary/raw strings,… |
| Console data sources — external adapter surface (elasticsearch/opensearch/prometheus/postgres/xerj-remote) + connection write paths (POST/PATCH/DELETE) | missing | `crates/xerj-console-api/src/data_sources.rs:9` | Confirmed real gap, status "missing". The candidate's specific feature — external data-source adapters and connection create/update/delete — has no execution pa… |
| Dashboards/views live updates via SSE (/_stream) | missing | `crates/xerj-console-api/src/dashboards.rs:9` | Confirmed real, but honestly-disclosed, gap — not a deceptive stub. The SSE live-update capability for dashboards/views has zero execution path anywhere in the … |
| Magic-link issue (/auth/magic/issue) — invite/recovery link minting | missing | `crates/xerj-console-api/src/auth/store.rs:152` | CONFIRMED as a genuine (but honestly-scoped, non-public) gap. The candidate's evidence checks out on every point: |
| On-disk skip table for posting lists (postings.rs '.post' Skip table section) | missing | `crates/xerj-fts/src/postings.rs` | REAL gap — dead scaffolding, verified in both directions. encode_term (postings.rs:233-285) really does build a Vec<SkipEntry> every SKIP_INTERVAL(=8) blocks, b… |
| Recovery magic-link redemption (console account recovery) | missing | `crates/xerj-console-api/src/auth/magic.rs:119` | The `"recovery"` arm in `redeem()` (magic.rs:119-125) is real code commented "Reserved for v1.1 — same shape as invite," but it is unreachable end-to-end. I tra… |
| xerj-logs columnar log subsystem (LogIngester / LogQueryExecutor / RetentionManager / columnar storage + retention) | missing | `engine/crates/xerj-logs/src/lib.rs` | CONFIRMED dead scaffolding — candidate is accurate. xerj-logs is a 1,737-line, fully-implemented crate (columnar.rs 615, ingest.rs 423, query.rs 474, retention.… |
---

*Notes: gap rows are audit findings of varying confidence; the four fixed
items were hand-verified against the source before fixing. A few features
appear twice where the audit surfaced them from two angles (e.g.
has_child/has_parent as both query-parse and execution gaps). File paths
are where the gap lives, not necessarily where a fix would land.*
