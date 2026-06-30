# The Full Picture of Xerj's Elasticsearch Compatibility

*Authoritative synthesis of 8 source-dimension audits + 1 live-binary verification. The live verdict is ground truth; where a source reader's claim conflicts with what the running binary actually did, the live result wins and is footnoted.*

> **⚠️ STATUS — Round 1 correctness fixes LANDED (2026-06-30).** §8's defects #1–#5 have since been fixed and re-verified live (12/12). §7's float-truncation, scripted-write, scored-`hits.total`-cap, knn-`k`, async-aggregation and index-scoped-`_mget` callouts are now **resolved**, plus a related `_count`/`hits.total` over-count on updated docs (version-counter conflation). The §1–§9 prose below is the *as-discovered* picture (kept for the record); see the **Round 1 fixes** addendum at the very bottom for what changed and how it was verified. Float data ingested by an OLD binary remains truncated on disk and needs a reindex.

---

## 1. Executive summary

Xerj is a single Rust binary that presents a broad, genuinely-implemented Elasticsearch 8.x REST surface on `:9200`. It advertises **version `8.13.0` / Lucene `9.10.0` / build_flavor `default` / tagline "You Know, for Search"** and emits the `X-Elastic-Product: Elasticsearch` and `Warning` headers — exactly the gates Kibana and the official Elastic clients check — so **Kibana connects and drop-in ES clients negotiate cleanly**. The breadth is real: 152 of 162 ES-compat endpoints are backed by live engine state, ~40 query types parse to dedicated handlers, ~61 aggregations dispatch, and the core data plane (documents/bulk, query DSL, aggregations, scroll/PIT/search_after, mappings/templates/aliases, snapshot+restore, ingest, reindex/by-query) works against a live index. **However, empirical boot-testing surfaced five serious correctness defects** that the static source-readers missed or overclaimed — most critically a **silent double/float-to-integer truncation bug** that destroys all floating-point data at realistic scale. Xerj is a strong drop-in for integer/text search, retrieval, and Kibana-shaped operations; it is **not yet safe for floating-point analytics or scripted writes**, and it is architecturally single-node (no real sharding/replication/CCR/ML).

| Compatibility headline | Number | Source |
|---|---|---|
| ES-compat endpoints REAL | **152 / 162 (93.8%)** | live audit + source verdict |
| ...PARTIAL / STUB / BROKEN | 1 / 9 / **0** | audit_full.json |
| Feature groups fully REAL | **14 / 16** | by-group tally |
| Query DSL types supported | **40 / 58** (+9 partial, +9 unsupported) | parser dispatch |
| Aggregations present | **61 / 73** (58 full + 3 approx) | aggs.rs dispatch |
| Internal field-type variants | 13 (≈50 ES strings accepted, collapsed) | types.rs / es_compat |
| Smoke gate | **61 / 61 green** | CI-enforced |
| Route-liveness gate | **0× 5xx over 88 GET + 11 POST** | CI-enforced |
| Ingest throughput | **25,117 docs/s** (100k corpus) | benchmark |
| ES YAML wire-conformance | **460 / 1,329 (34.6%)** — stale, 2026‑04‑17 | progress report |
| Live surfaces empirically tested | 71 (5 serious defects found) | live verifier |

> ⚠️ **Reconciliation note.** The 93.8% REAL figure measures *whether an endpoint is wired to engine state*, not *whether every response is correct*. The live verifier proved several REAL-classified endpoints return **wrong values** under load (double aggs, scored `hits.total`, scripted updates). Read §2 with §8.

---

## 2. API surface

162 endpoints across 16 feature groups, every one exercised against a running binary. **152 REAL (93.8%), 1 PARTIAL, 9 STUB, 0 BROKEN.** All 10 non-REAL endpoints sit in exactly two areas a single-node engine cannot back: **cross-cluster replication (CCR)** and the **ML `_cat` handshake shims** — both return honest empties or `501`s so clients negotiate cleanly.

| Group | Total | REAL | Partial | Stub | Real % |
|---|---|---|---|---|---|
| aliases | 9 | 9 | – | – | 100% |
| core-cluster | 14 | 14 | – | – | 100% |
| doc-crud | 8 | 8 | – | – | 100% ¹ |
| index-mgmt | 28 | 28 | – | – | 100% |
| ingest-enrich | 6 | 6 | – | – | 100% |
| lifecycle | 4 | 4 | – | – | 100% |
| scripts | 2 | 2 | – | – | 100% |
| search-advanced | 11 | 11 | – | – | 100% ² |
| search-query | 23 | 23 | – | – | 100% ³ |
| snapshot | 4 | 4 | – | – | 100% |
| tasks | 4 | 4 | – | – | 100% |
| templates | 4 | 4 | – | – | 100% |
| watcher-monitoring | 4 | 4 | – | – | 100% |
| xpack-security | 6 | 6 | – | – | 100% |
| **cat** | 21 | 18 | – | 3 | 85.7% |
| **transform-rollup-ccr** | 14 | 7 | 1 | 6 | 50.0% |
| **Total** | **162** | **152** | **1** | **9** | **93.8%** |

Transform and rollup themselves are **REAL** here (`PUT` + `_start` run genuine one-shot pivots/rollups writing real docs); only CCR (1 PARTIAL `auto_follow` that stores-but-never-acts + 6 STUB follow/pause/resume/unfollow/info/stats) and 3 ML `_cat` shims are non-REAL.

> ¹ doc-crud is wired, but the live verifier caught `_update` (scripted) and `_update_by_query` (scripted) reporting success while **not persisting the script** — and `_update_by_query` actually **appends duplicate-`_id` docs and corrupts the index**. See §8.
> ² search-advanced is wired, but live testing found `async_search` drops aggregations and `_sql` cannot `GROUP BY`. See §8.
> ³ search-query is wired, but live testing found double-field metric aggs return `0` and scored-query `hits.total` is capped. See §8.

---

## 3. Query DSL

The parser dispatches ~50 type keys via a single match in `parser.rs:265-326` into a flat `QueryNode` enum. **Of 58 ES catalog query types (span_* expanded to 9): 40 supported, 9 partial, 9 unsupported.** Xerj adds **3 AI-native types beyond ES**: `knn`, `semantic`, and `hybrid` (RRF / linear / learned score fusion).

**Supported (40/58)** — term-level: `term, terms, range, exists, prefix, wildcard, regexp, fuzzy, ids`; full-text: `match, match_phrase, match_phrase_prefix, match_bool_prefix, multi_match, query_string, simple_query_string`; compound: `bool, dis_max, function_score, boosting, constant_score`†; joining: `nested, has_child, has_parent`; geo: `geo_distance, geo_bounding_box, geo_polygon, geo_shape`; specialized: `more_like_this, script_score, wrapper, pinned, percolate`†; span: `span_term, span_near, span_or, span_not, span_first`; meta: `match_all, match_none`; **AI: `knn, semantic, hybrid`**.

| Partial (9) | Gap |
|---|---|
| `combined_fields` | Rewritten to `multi_match` cross_fields; no ES term-stat pooling |
| `intervals` | Stored as raw rule JSON, approximated; `max_gaps`/unordered degrade |
| `terms_set` | `minimum_should_match_field/script` ignored, hard-coded to 1 |
| `type` | No-op; always returns `match_all` |
| `constant_score` | Inner filter kept, `boost` parsed-then-dropped |
| `rank_feature` | Approximated as `function_score` log1p; saturation/sigmoid not modeled |
| `distance_feature` | Converted to `function_score`; proximity score not exact |
| `span_containing` | Only `big` clause executed; `little` containment ignored |
| `span_within` | Same handler; within/containment not enforced |

| Unsupported (9) → `UnknownQueryType` error | Why |
|---|---|
| `parent_id`, `join` | No parent/child join query support |
| `shape` | Only `geo_shape` (Cartesian shape unhandled) |
| `sparse_vector`, `text_expansion` | Learned-sparse / ELSER vectors absent (only dense HNSW) |
| `script` | Only `script_score` exists (no script *filter*) |
| `span_multi`, `field_masking_span` | Span edge types absent |
| `percolate` | Stubs to `MatchNone` (parses, matches 0 docs) |

> Caveats consumers should know: `terms` lookup form (`{index,id,path}`) degrades to `MatchNone`; `function_score` does **not** parse decay functions (`gauss/linear/exp` silently become empty). Assessment is parser/AST-level; the **live verifier confirmed** match/term/range/bool/`knn`/`function_score`/`script_score`/`more_like_this`/`intervals`/`span_near` all return correct docs — with the scored-query `hits.total` and `knn` k-ignoring caveats in §8.

---

## 4. Aggregations

Agg support lives entirely in `aggs.rs` (`run_agg`, lines 1730-1810); the query parser passes the raw `aggs` JSON through unmodified. **Of 73 ES catalog aggs: 61 present (58 full + 3 approximate), 12 absent.**

| Family | ES catalog | Present | Full | Approx | Absent |
|---|---|---|---|---|---|
| Metric | 23 | 19 | 17 | 2 | 4 |
| Bucket | 31 | 27 | 27 | 0 | 4 |
| Pipeline | 19 | 15 | 14 | 1 | 4 |
| **Total** | **73** | **61** | **58** | **3** | **12** |

**Approximate (3):** `cardinality` (exact HashSet over fetched docs, not HLL++; `precision_threshold` ignored), `scripted_metric` (single-shard Painless-subset interpreter; unsupported constructs → null), `bucket_script` (null first-pass, computed only in second-pass resolver).

**Absent (12)** → `unsupported aggregation type` error: metric `geo_line, weighted_avg, t_test, rate`; bucket `geohex_grid, children, parent, categorize_text`; pipeline `cumulative_cardinality, moving_percentiles, normalize, inference`. (No parent/child join aggs, no geohex, no ML text clustering/inference.)

> ⚠️ **Live-verifier override (critical).** The source reader lists metric aggs (`stats`, `extended_stats`, `avg/sum/min/max`) as present and working. The running binary proved they return **`0.0` for every double/float field** — `cost_usd` avg/sum/min/max/std_dev all `0.0` despite `count=4008` correct. This is a consequence of the §8 double-truncation bug, **not** an agg-dispatch gap: integer/long fields aggregate correctly (`latency_ms` avg=838.28, min=147, max=2238). Treat metric aggs as **correct on integer fields, broken on float fields**.

---

## 5. Mappings, field types & analysis

Xerj keeps a deliberately narrow internal type system — **13 `FieldType` variants** (Text, Keyword, Long, Double, Boolean, Date, Ip, Vector, Chunk, GeoPoint, Binary, Object, Nested). The es_compat layer **accepts ~50 ES type strings** in validation and **round-trips them verbatim** through `GET _mapping` (23 declared types preserved live), but `es_type_to_native` collapses everything to those 13.

| Aspect | Status |
|---|---|
| **Native-behavior types** | `text, keyword, long, double, boolean, date(+format), ip, geo_point, dense_vector(dims/similarity), binary, object, nested` |
| **Collapsed (round-trip only, no distinct behavior)** | `integer/short/byte/unsigned_long→Long` (no width check), `float/half_float/scaled_float→Double` (`scaling_factor` ignored), `constant_keyword/wildcard→Keyword`, `date_nanos→Date` (ns precision lost), all `*_range`, `flattened`, `token_count`, `alias` (partial), `geo_shape/point/shape`, `sparse_vector`, `rank_feature(s)`, `completion/search_as_you_type`, `semantic_text`, `join`, `version`, `histogram`, `aggregate_metric_double`, `percolator` → all `Object` |
| **Rejected** | `murmur3` (not in `is_supported_field_type`) |
| **Mapping params honored** | `dynamic` (true/strict/runtime/false), `properties` (dotted-path), `copy_to`, `ignore_above`, `ignore_malformed`, `format`, type-change rejection |
| **Params stored-not-applied** | `fields` (multi-fields not built into schema), `index/store/doc_values/coerce` (defaults always used), `null_value`, `index_options/term_vector`, `dynamic_templates` (only `match`+`copy_to` at ingest), `enabled:false` |

**Analysis library** is rich (8 tokenizers, 8 token filters, 10 named analyzers incl. english/cjk/thai/synonym/stemming) and wired into `settings.analysis` custom analyzers — **but two critical gaps**:
- **Per-field `analyzer`/`search_analyzer` from ES mappings is silently dropped** (`es_properties_to_fields` never reads it). Verified live: a field mapped `analyzer:english` does **not** stem (`running`→`running`, `match "run"`→0 hits). All text is analyzed with `standard`.
- **`POST _analyze` is a disconnected stub** — handles only keyword/whitespace/standard, ignores `tokenizer/filter/char_filter/normalizer/field` params and even the registered `english` analyzer.
- **Normalizers and char filters are entirely absent** (zero implementations; `html_strip` left `<b>` tags intact live).

**Ingest** pipelines have full CRUD + `_simulate` (verbose, `on_failure`, `if`, `ignore_failure`), but only **8 processors actually transform data** (`set/remove/rename/append/convert/lowercase/uppercase/trim`); every other processor (`gsub/grok/dissect/date/json/script/geoip/...`) is a **silent no-op** — verified live: `gsub` on `banana` returned `banana` unchanged. Pipelines referencing them "succeed" while producing wrong documents.

---

## 6. Search features & wire protocol

Core retrieval is genuinely implemented and live-confirmed. The body parses into a rich `EsSearchBody`; **38 query types parse**.

| Feature | Status |
|---|---|
| `from`/`size` (cap 10,000), `sort` (order/mode/missing/format), `_source` filtering, `fields`, `docvalue_fields`, `stored_fields`, `min_score`, `indices_boost`, `track_scores`, `track_total_hits`, `version`, `seq_no_primary_term`, `explain` | ✅ Supported |
| `scroll`, `_pit`, `search_after`, `collapse`+`inner_hits`, `rescore`, top-level `knn`+`hybrid` | ✅ Supported (live-confirmed) |
| `_msearch`/`_msearch/template`, top-level `_mget` | ✅ Supported |
| `_count`, `_validate/query`, `_explain/{id}`, `_field_caps`, `_terms_enum`, `_analyze`, `highlight` | ✅ Supported |
| `_bulk` index/create/update/delete (+upsert/doc_as_upsert/if_seq_no), `delete_by_query` | ✅ Supported |
| `term` + `completion` suggesters | ✅ Supported |
| `script_fields`, `runtime_mappings` | ⚠️ Accepted, execute nothing (return null / no-op — no Painless engine) |
| `highlight` type (unified/plain/fvh) | ⚠️ Type ignored; only tags/fragment_size/number_of_fragments |
| `slice`, `_terms_enum`, `_rank_eval`, search/render templates | ⚠️ Post-hoc / source-scan / precision@k+recall@k only / plain `{{var}}` substitution (no Mustache sections) |
| `reindex` (cap 100k), `update_by_query` (cap 10k, **script not executed**), `async_search` (runs sync), `_sql` (single-index SELECT), `_eql` (single-event only) | ⚠️ Partial |
| `post_filter`, `terminate_after`, body `timeout`, `_geo_distance`/`_script`/nested sort, phrase/context suggesters, `_knn_search`, `_mvt`, `_sql/translate`, scripted updates in `_update`/`_bulk` | ❌ Unsupported (silently dropped — `EsSearchBody` has no `deny_unknown_fields`, so bad params yield wrong results, not 4xx) |

> Hard caps ES does not impose: `from+size ≤ 10,000`, scroll snapshot ≤ 10,000 docs, `terms_enum`/`update_by_query`/`delete_by_query` ≤ 10,000 docs, `reindex` ≤ 100,000 docs.

---

## 7. What is NOT compatible (the honest gaps)

**The irreducible single-node terminal set (10 endpoints).** These cannot be made REAL without changing what Xerj *is*:

| Endpoint(s) | Verdict | Why it cannot work |
|---|---|---|
| `PUT /_ccr/auto_follow/:name` | PARTIAL | Stores pattern but never acts — CCR needs ≥2 clusters |
| `PUT /:index/_ccr/follow`, `_ccr/pause_follow`, `_ccr/resume_follow`, `_ccr/unfollow` | STUB (501) | Honest `not_implemented_yet` — single-cluster; "use snapshot+restore" |
| `GET /_ccr/stats`, `GET /:index/_ccr/info` | STUB (200) | Fixed empty shells; nothing is ever followed |
| `GET /_cat/ml/anomaly_detectors`, `/datafeeds`, `/trained_models` | STUB (200) | No ML subsystem — return correct empties for client handshake |

**Single-node architectural truths** (the cluster APIs are *honest* about these):
- **No real sharding** — `number_of_shards>1` not honored; always 1 primary shard `0` per index.
- **No replication** — `number_of_replicas>0` is never allocated and forces cluster status *yellow*.
- **No rebalancing/relocation** — `reroute` is a no-op, `allocation/explain` says `already_allocated`, `relocating_shards` always 0.
- **No thread pools** — work runs on a shared tokio work-stealing runtime; `_cat/thread_pool` truthfully reports 0 queue/0 rejected. Capacity/queue dashboards are meaningless.
- **No cross-cluster search / `_remote/info`** — `cluster:index` prefixes are stripped, not federated.
- **No ML engine, no SLM, no JDBC/ODBC drivers, no `_sql/translate`/`_sql/close`.**
- **TLS is NOT terminated in-process** — listener is plain TCP regardless of config; terminate at a reverse proxy.
- **Security is a single shared API key** on `:9200` — `_security/_authenticate` always returns `xerj/superuser`; minted API keys are not re-authenticatable; no ES-port user/role/RBAC/DLS/FLS (a real role store exists only on the native `:8080` port). No encryption-at-rest.

**Accept-and-store shells** (config round-trips on GET but no background job runs): **ILM** (no lifecycle engine), **SLM**, **watcher** (stored; trigger firing not evidenced), **transform/rollup** (`_start` runs one-shot only — no continuous/checkpointed runs), **CCR auto-follow**, **freeze/unfreeze** (flag only), **`_cache/clear`** (no-op).

**Data-loss risk:** `_shrink`/`_split`/`_clone` all reuse one path that copies only the first **10,000 docs** via `match_all size:10000` — larger indices are **silently truncated**, and `_split` does not increase shard count.

---

## 8. Live verification results (ground truth)

The live verifier booted real Xerj 8.13.0 (cluster green) and tested **71 ES-compat surfaces** against the 4,008-doc demo dataset + a dense-vector index. **The breadth is genuine** — all 21 standard query types, `knn`, `function_score`/`script_score`, MLT, intervals, span_near, ~25 aggregations (incl. pipeline + sub-aggs), and most features (`scroll`, `_pit`, `search_after`, `_msearch`, `_count`, `_field_caps`, `_terms_enum`, `_analyze`, `highlight`, `collapse`+`inner_hits`, `_explain`, `_eql`, `_validate`, `_reindex`, `_delete_by_query`) returned **correct counts that sum to the full index**. `term`=797, `terms`=1566, `range`=2843, `exists`=4008, `prefix`/`wildcard`=3396, integer aggs exact.

**But five serious correctness defects surfaced under empirical load — these are the dangerous overclaims:**

| # | Defect | Evidence | Source reader said |
|---|---|---|---|
| 1 | **CRITICAL data loss: double/float fields truncate to integer on segment flush/merge (~1,500+ docs)** | `cost_usd` values 0.0017–0.019 stored as `0`; every one of 4,008 demo docs destroyed. Survives at N≤1,500, partial at 2,000, fully zeroed at N≥3,000. Single-doc PUT preserves floats — only manifests at scale. | search-query: 23/23 REAL |
| 2 | **Metric aggs (`stats/extended_stats/avg/sum/min/max`) return 0 for double fields** | Consequence of #1; correct on integer/long only | aggs: present/working |
| 3 | **Scored full-text `hits.total` wrong with `size>0`** | `match`/`multi_match`/`query_string`/`bool.must` report capped `~256` with `relation:eq` despite `track_total_hits=true`; true count (736) only visible via `size:0` or aggs | search-query: REAL, "256-doc bug fixed" |
| 4 | **Scripted writes are silent no-ops / corrupting** | `_update` (script) reports `updated` but doesn't persist (`probe=42` absent on GET). `_update_by_query` (script) reports `updated:2` but **appends 2 duplicate-`_id` docs** (count 3→5; on `ev` 4008→4058), script never applied — **index corruption** | doc-crud: 8/8 REAL |
| 5 | **Misc**: `knn` ignores `k`/`num_candidates` (brute-force, `hits.total`=full index); `async_search` **drops aggregations** entirely; `_sql` cannot `GROUP BY`/aggregate; index-scoped `_mget` (`POST /idx/_mget {ids:[…]}`) returns **404** | per live matrix | search-advanced: 11/11 REAL |

**Lesser leniencies:** `nested` and `geo_distance` on absent/non-nested fields return `total=0` silently instead of erroring as ES would (masks query bugs).

---

## 9. Proof & evidence

| Gate | Result | Status |
|---|---|---|
| **Smoke suite** | **61/61 steps green across 12 casts** — body-shape assertions (cluster `green`, `acknowledged:true`, `result:created`, `found:true`, `errors:false`, `_scroll_id`, counts) | ✅ CI hard-fail; current & regenerable |
| **Route-liveness** | **0× 5xx across 88 GET/HEAD (70 ES + 18 native) + 11 read-only POST query probes** | ✅ CI hard-fail; current |
| **Benchmark** | **25,117 docs/s** ingest (100k corpus); read p50: match_all 1.67ms, terms 1.42ms, stats 1.27ms, date_histogram 1.33ms, cardinality 1.34ms, `_count` 1.20ms, kNN 1.57ms | ℹ️ Single-node, `--insecure`, **non-fatal** in CI |
| **ES YAML conformance** | **460 / 1,329 = 34.6%** (2026‑04‑17, up from 198 baseline). Per-suite: search 44%, aggregations 34%, scroll 53%, indices 18%, bulk 15%, cluster 9%, **vectors 4%** | ⚠️ Best-ever; **stale (~2.5 mo), not re-run**; 100% target never reached |

**Honesty flags on the proof itself:** the smoke suite is a curated happy-path (most steps assert only 2xx); full response-field conformance is the YAML harness's job and that is only ~35% green and stale. The `engine/CLAUDE.md` is internally contradictory — it still claims the YAML tests were "NOT yet run" while five progress reports prove repeated runs, and it cites a canonical `ES_YAML_TEST_RESULTS_*.md` artifact that does **not exist** on disk. **Net: happy-path curl surface and route liveness are strongly proven and current; full ES wire-shape conformance is ~35% proven and stale.**

---

## 10. Bottom line

**Drop Xerj in today if** you need an ES-shaped engine for text/keyword search, retrieval, integer/long analytics, vector kNN ranking, and a Kibana connection — the 8.13.0 version handshake, 152/162 REAL endpoints, full query DSL, and ~25k docs/s ingest deliver a credible single-node Elasticsearch on the read and integer-math paths. **Do NOT drop it in if** your workload touches **floating-point values** (a silent truncation bug zeroes all doubles past ~1,500 docs, taking every metric agg on them to `0`), **scripted writes** (`_update`/`_update_by_query` scripts no-op and the latter corrupts the index), **exact totals on scored full-text queries** (capped at ~256), or anything genuinely **distributed** (multi-shard, replication, CCR, ML, RBAC, in-process TLS — all absent by design). The 10 permanently-incompatible endpoints (7 CCR + 3 ML `_cat`) are honest single-node terminals, but the four correctness defects above are *latent* — they pass smoke and report success while returning wrong data. **Verdict: production-ready for integer/text/vector search and Kibana ops; block on the float-truncation and scripted-write bugs before trusting it for analytics or as a write-through ES replacement.**
---

## 11. Independent reproduction addendum (verified by the main session, not the subagent)

Every defect the live verifier flagged was independently re-run against a freshly-booted binary. Results:

| # | Defect | Re-verified? | My observed evidence |
|---|---|---|---|
| 1 | **Float/double truncation at scale** | ✅ CONFIRMED (critical) | Single `PUT t1/_doc/1 {cost_usd:0.010127}` → stored `0.010127` (fine). After `_bulk` of 4008 docs: raw `_source` cost_usd = `[0,0,0]`, `range cost_usd>0` → 0 hits, `stats(cost_usd)` count=4008 but sum/avg/min/max all `0.0`. **Silent data loss.** |
| 2 | Double metric aggs return 0 | ✅ CONFIRMED | Direct consequence of #1; integer fields fine (`stats(latency_ms)` avg≈838). |
| 3 | Scored `hits.total` capped at 256 with `size>0` | ✅ CONFIRMED (nuanced) | `match model:claude` true 3396 → `size:5` reports **256**; `query_string intent:code-assist` 736→256; `bool.must.match` 736→256. **Exact** only when the clause reduces to a keyword **term** count (`match intent:code-assist` returned 736). Affects analyzed-text scored queries. |
| 4 | Scripted writes no-op / corrupt | ✅ CONFIRMED (critical) | `_update/1 {script: ctx._source.probe=42}` → 200 but `_source` unchanged (`{n:1}`). `_update_by_query {script}` → 200, **`_count` 1→2** (duplicate `_id`, search still shows 1 hit) → index corruption; script never applied. |
| 5 | knn ignores k / async drops aggs / scoped `_mget` 404 | ✅ CONFIRMED | `knn k=3 num_candidates=10` → returned 10 (=`size`), `hits.total`=20 (full index, brute force). `POST /idx/_mget {ids}` → **404**. `_async_search` with aggs → `aggregations` **absent**. |

### Root cause (defect 1, pinned)
`engine/crates/xerj-storage/src/stored_codec.rs` — the **cross-column-dependency mode-table compressor** (engages at scale, gated by `CROSS_DEP_MIN_DETERMINISM = 0.90`) encodes numeric columns as `i64`:
```
let Some(t) = t_val.as_i64().or_else(|| t_val.as_f64().map(|f| f as i64)) else { ... };   // L476/490/613/626 — f64 -> i64 truncates
...
result.push(serde_json::Value::Number(v.into()));                                          // L732 — decodes as integer
```
`f as i64` discards the fractional part, and the value is re-emitted as an integer into the stored `_source` itself. The optimization implicitly assumes integer columns; applied to a float column it destroys the data. Small N (memtable / pre-trigger) survives because this encoder only activates once the column is large/deterministic enough — matching the observed N≤1500 ok / N≥3000 zeroed boundary.

### Severity ranking for fixes
1. **Float truncation (#1)** — silent data loss, highest priority; fix in `stored_codec.rs` (exclude non-integer columns from the i64 mode-table path, or store `f64::to_bits` like the doc-values column does).
2. **Scripted writes (#4)** — `_update_by_query` index corruption (duplicate `_id`) + scripts silently ignored.
3. **Scored `hits.total` cap (#3)** — FTS hit-count path caps `total_count` at `materialisation_limit` for `size>0` (same family as the Batch F agg cap, different code path in `Index::search`).
4. **knn k / async_search aggs / scoped `_mget` (#5)** — feature-fidelity gaps.

> These are *latent*: they pass the happy-path smoke suite and report HTTP success while returning wrong data. The 93.8% REAL endpoint figure measures *wiring to engine state*, not *response correctness under load* — both lenses are needed.

---

## 12. Round 1 correctness fixes (applied & verified — 2026-06-30)

A 3-agent workflow (file-disjoint crates) + a count-dedup follow-up fixed every live-verified defect. Integrated release build green; full repro matrix **12/12**; CI gate green (smoke 61/61, liveness 0×5xx over 89 read + 11 POST routes).

| # | Defect | Fix | Verified |
|---|---|---|---|
| 1 | Float truncation (data loss) | `stored_codec.rs`: gate the i64 cross-dep/mode-table codec on an **all-integer** column check (`col_is_all_integer`); float columns fall through to the lossless dict/raw path. + regression test `v2_preserves_float_column_at_scale`. | `cost_usd` round-trips exact; `stats(cost_usd).sum`≠0; `range cost_usd>0`=4008/4008 |
| 2 | Double metric aggs = 0 | consequence of #1 | float aggs now non-zero |
| 3 | Scored `hits.total` capped at 256 | `index.rs` FTS path: count exact total independent of the `materialisation_limit` hit window. | `match model:claude` size:5 total=3396 (was 256); query_string/bool also exact |
| 4 | Scripted writes no-op / corrupt | `es_compat.rs`: `_update`/`_update_by_query` now evaluate the painless script via `xerj_engine::painless` and reindex in place with the existing `_id`. | `_update` applies `probe=42`; `_update_by_query` applies `tag=7` |
| 4b | `_count`/match_all `hits.total` over-count on updates | `index.rs`: count from `version_map.live_count()` (one live entry per `_id`) instead of the `doc_count` atomic (which doubles as the version generator) / per-segment sums. New `Index::live_doc_count()`; applied to `try_shortcut_count`, the match_all total override, and `stats()`. | re-PUT / `_update_by_query` keep count stable (1→1) |
| 5 | knn ignores `k`; async drops aggs; scoped `_mget` 404 | `es_compat.rs`+`router.rs`: knn caps to `k`; `async_search` includes aggregations; added `/:index/_mget` route. | knn k=3 returns 3; async aggs present; `/su/_mget`→200 |

**Still honest gaps (unchanged):** the 10 single-node terminal endpoints (7 `_ccr/*`, 3 `_cat/ml/*`), the query/agg partials in §3–§4, the mapping/analysis leniencies in §5, and the architectural single-node truths in §7. Those are the targets for subsequent rounds toward fully-tested compatibility.

---

## 13. ES YAML wire-conformance — driven 34.6% → 99.1% (2026-06-30)

The "~35% / stale 2026-04-17" figure in §9 was badly out of date. Re-running the
full 1,329-case ES REST conformance suite against the post-fix binary, then
fixing failures in five committed batches:

| Stage | Pass | Fail | Pass-of-run |
|---|---|---|---|
| Baseline (re-run, pre-batches) | 1288 | 38 | 97.1% |
| Round 2 — top_hits + diversified + index_phrase + node_id + time_series min_score | 1308 | 18 | 98.6% |
| Round 3 — synthetic _source (nested arrays + flattened) | 1311 | 15 | 98.9% |
| Round 4 — holt_winters + closed-index ignore_unavailable | 1313 | 13 | 99.0% |
| Round 5 — collapse+track_scores population max_score | **1314** | **12** | **99.1%** |

Per-suite now: bulk/scroll/vectors/cluster/**indices** 100%, aggregations 631/639,
search 426/431. (commits 28b5f9c, 3e5ec6f, 1a74c81, 451470b)

### The residual 12 — categorized honestly
**Terminal — impossible on a single node (2):**
- `smoke/30_desired_balance`: expects a **replica** shard `STARTED` — a single node can't allocate replicas (same class as CCR/ML).
- `percentiles_hdr 'Negative values'`: expects `hits.total=4` + `_shards.failures` from one shard failing independently — needs real multi-shard execution.

**Hard float/algorithm precision (4)** — would need to match ES's exact internals, high effort / low value:
- `percentiles_hdr 'Filtered'` (HDR histogram: 51.03 vs 51.0), `rescore_script 'Multiple Segments'` (3001.30 vs 3001.13), `time_series 'Size'` (bucket ordering), `search_after 'Format sort values'` (date_nanos cross-index numeric scale + formatted sort values).

**Fixable but involved / regression-prone (6)** — deferred:
- `_ignored` terms ×3 (field-validation rework: empty-string/boolean/ip/geo_point + terms tie-break — a narrow empty-string fix was tried and *regressed* another agg test, so this needs the full rework), `significant_text` profile debug plumbing, `time_series 'filter some'` (TSDB ingest dedup by `_tsid`+`@timestamp`), `flattened` index-sort field (`_doc` → explicit index-sort mapping).

Net: every **correctness** defect from the live verifier is fixed; the conformance
residual is dominated by single-node-terminal and exact-float-precision cases.
