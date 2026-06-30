# XERJ: Plan to Beat Elasticsearch on Ingest & Tail Latency

## 1. Executive Summary

**The gap.** On the single-client `_bulk` ingest benchmark at 1M docs, Elasticsearch sustains **68k–110k docs/s** (and *rises* with scale: 68k → 110k) while XERJ delivers **22k–31k docs/s** (and *falls* with scale: 31k → 22k). That is a **2.2x–4.9x ingest deficit** that widens as the corpus grows. XERJ already **wins reads by 1.5x–1.85x**, but those read wins are punctured by **~70ms p99 spikes** that coincide with flush/merge.

**The thesis.** XERJ is Rust, no JVM, no GC pauses, on a 32-core box. There is no structural reason it should lose ingest — it should *win* ingest too. The machinery to do so (sharded WAL, sharded FTS memtable, a turbo batch ingest path, rayon-parallel parse/tokenize, PFOR-128 postings at Lucene90 parity, size-tiered merge) **already exists in the tree**. The loss is not a missing capability — it is that the hot path **does not use the machinery it already has**.

**Headline root causes the measurement proved.** Two compounding facts explain almost the entire gap, and both are self-inflicted:

1. **A single bulk request runs effectively single-threaded.** The default `_bulk` index path never reaches the batch engine — it loops the batch and calls `idx.index_document(...).await` **one doc at a time** (`bulk.rs:736-801`). And even the turbo batch path, when reached, routes the **entire batch to ONE shard** keyed off the first doc's id hash (`index.rs:1384`, `index_store.rs:1792`). So the benchmark's single sequential client exercises **1 of 16 shards** while ES fans the same request across all 32 write threads via DocumentsWriterPerThread (DWPT). This is the dominant lever and the reason throughput is low.

2. **Per-doc/per-flush redundant work plus flush-on-the-active-shard.** The path re-parses each doc's JSON 2-3x, re-serializes the whole source just to count bytes, runs a full-body NDJSON rewrite, flushes through a heavy zstd columnar codec, **wipes all read caches on every flush**, rewrites the entire snapshot O(N) per flush, and **spawns the flush on the very shard the client is writing** — producing the 31k→22k scale decay and the 70ms p99 spikes.

**It is NOT fsync-bound.** The default WAL is Batched/`soft_flush` with fsync already off the hot path (`wal.rs:518-523`, `index_store.rs:1819-1828`); the brief's "WAL fsync" hypothesis is wrong for the default config. The loss is **single-threading + redundant CPU + flush-stall**, not durability.

> **★ Empirical confirmation (measured directly, `top -b` instantaneous, 32-core box):** during a single-client batched `_bulk` ingest (40×10k), the xerj process holds steady at **100–103% CPU = exactly 1.0 of 32 cores.** The other 31 cores sit idle. This is the proof of root cause #1/#2: one bulk request is single-threaded. ES fans the same request across its write-thread pool. **The 5× ingest gap is ~31 idle cores — pure headroom.**

---

## 2. Root-Cause Diagnosis (ranked by impact)

### Q: Is XERJ ingest single-threaded per bulk? **Yes — twice over.**
- The default `_bulk` path indexes serially, doc-by-doc, awaited in a loop: `for (item_idx, doc_id, doc_bytes) in batch { idx.index_document(id_opt, source).await }` (`bulk.rs:736-801`). The real batch path `index_batch_turbo_raw` (`index.rs:1184`) is documented as "called by `_bulk` when `X-Turbo:true`" — but **`grep 'X-Turbo'` in `es_compat.rs` returns nothing**. It is dead for the benchmark.
- Even when a batch path *is* reached, the **whole batch is pinned to one shard**: `let shard_idx = self.memtable.shard_for_dynamic(&processed[0].id)` (`index.rs:1048/1384`) and `let ws = self.wal_shard_for(&docs[0].0)` (`index_store.rs:1792`). The 16-way sharding parallelizes **across concurrent clients only**, never **within** a request.

### Q: Does it scale with concurrent clients? **Partially — but the benchmark is single-client.**
Sharding gives N concurrent clients ~N shards. A single sequential bulk client gets exactly one shard's worth of WAL-frame-build + memtable-insert + flush — i.e. ~1 core. ES dispatches one `_bulk` across its 32-thread write pool. This is the entire 2.2x–4.9x gap.

### Q: Is it fsync-bound, flush-stall-bound, or compression-bound? **Flush-stall + redundant-CPU + compression on flush; NOT fsync.**
- **Not fsync-bound:** Batched mode + `soft_flush` skips fsync on the hot path (`wal.rs:518-523`).
- **Flush-stall-bound:** `maybe_spawn_flush` runs per-doc and spawns a flush whose `drain_with_sources` takes `shards[s].write()` — **the same lock the ingest loop holds** (`index.rs:829`, `memtable.rs:461-468`). Flush is spawned on the active shard (`index.rs:1406-1408`). → the 70ms p99 spikes.
- **Compression/CPU-bound on flush:** the default flush stored codec `encode_stored_v2` **re-parses the JSON it just serialized** into `Vec<Value>`, pivots/clones into per-column vecs, runs an O(numeric × keyword × docs) CROSS_DEP determinism scan, and zstd-level-3 compresses every fallback column (`stored_codec.rs:144-257`, `STORED_ZSTD_LEVEL=3`). ES uses plain LZ4 BEST_SPEED with zero field analysis.

### Q: Why does throughput DROP at scale (31k → 22k)? **Three superlinear-with-corpus costs:**
1. **One hot shard accumulates the full live set.** With whole-batch single-shard routing, the single active shard's postings/term-hash/memtable grow O(N), so per-doc insert cost (hash probes, vec growth) rises (`index.rs:1390-1394`).
2. **`save_snapshot` rewrites the ENTIRE segment list per flush.** `serde_json::to_vec_pretty(&**snap)` + write + rename on every `finalize_flush` (`index_store.rs:1028, 1233-1235`) is O(total segment count) — every additional thousand segments taxes every subsequent flush.
3. **Unthrottled background merge on the shared pool.** `run_merge_once` (`index.rs:1846`) re-runs `encode_stored_v2` (zstd+cross-dep again, `:2156`) and rebuilds FTS on the **global rayon pool** (`:2184`) with **no IO throttle** — the `RateLimiter` (`merge.rs:140-178`) and `config.merge.io_rate_mb_per_sec=100` (`config.rs:429`) are wired only into the dead-code storage `MergeExecutor`. Merge workload grows with corpus and steals cores from ingest.

### Ranked root-cause table

| # | Root cause | Evidence | Severity |
|---|-----------|----------|----------|
| 1 | Default `_bulk` indexes serially per-doc; turbo batch path never invoked (no `X-Turbo`) | `bulk.rs:736-801`; `grep X-Turbo` → ∅ | **High** |
| 2 | Whole batch pinned to ONE shard (FTS + WAL) by first doc hash → single-client ≈ 1 core | `index.rs:1048/1384`, `index_store.rs:1792` | **High** |
| 3 | Per-doc redundant JSON re-parse on hot path | `bulk.rs:760` | **High** |
| 4 | Full-source re-serialize just to count bytes (`source.to_string().len()`) per doc | `index_store.rs:639, 691` | **High** |
| 5 | Single-threaded full-body NDJSON rewrite gated on coarse `!is_empty()` | `es_compat.rs:9298-9307, 19299-19364` | **High** |
| 6 | Flush stored codec = zstd-3 columnar + O(cols²×docs) cross-dep scan (vs LZ4) | `index_store.rs:862`, `stored_codec.rs:144-257` | **High** |
| 7 | `save_snapshot` O(N total segments) rewrite per flush | `index_store.rs:1028, 1233-1235` | **High** |
| 8 | Every flush wholesale-clears query/dv/stored caches → cold reads | `index.rs:1479-1480, 1824-1830` | **High** |
| 9 | Flush spawned on the active shard; drain contends ingest lock → 70ms p99 | `index.rs:829, 1406-1408`, `memtable.rs:461-468` | **High** |
| 10 | Flush+merge on the same untenanted runtime/rayon as ingest+search | `index.rs:1794, 493, 2184`, no RateLimiter in `run_merge_once` | **Med** |
| 11 | Per-doc locks/clones/vector-scan/flush-scan not hoisted to batch | `index.rs:699-778, 827, 829`; `index_store.rs:631-649` | **Med** |
| 12 | Flush granularity too fine: per-shard threshold = 500k/16 ≈ 31k → ~16 small segments/cycle | `index.rs:415, 1403-1404` | **Med** |
| 13 | `ingest_shards` default = (cpus/2).next_pow2 = 16 on 32 cores | `config.rs:763` | **Low** |
| 14 | Per-doc fsync amplification (.seg + .sidx + .ids each fsync) | `segment.rs:374-394`, `index_store.rs:953` | **Med** |

---

## 3. The Plan (prioritized, phased, ordered by gain-per-effort)

> Conformance constraint: **1326/1326 ES-compat tests must stay green** and read latency must not regress. Every change below notes its read/conformance risk.

### Phase 1 — Quick wins (route into existing machinery; kill redundant CPU)
*Low-risk, high-yield. These alone should roughly halve or close the gap.*

**P1.1 — Route the default `_bulk` index group through `index_batch_turbo_raw`.** *(Highest leverage; do FIRST.)*
- **What:** Replace the per-doc `for … { idx.index_document(…).await }` loop with a single `idx.index_batch_turbo_raw(batch_of_(id,bytes))` per target index; map the returned `Vec<IndexResponse>` back onto item slots by order. Feed the raw `Arc<[u8]>` bytes straight in (no re-parse). Files: `bulk.rs:705-801`; `index.rs:1184`.
- **Mechanism / Lucene equiv:** One FTS lock + one WAL lock + amortized fsync per batch — the batch-commit analogue of a DWPT flushing once, not per doc.
- **Expected gain:** Removes per-doc re-parse, per-doc WAL lock, per-doc flush scan. **~2-4x single-client throughput**; eliminates much of the with-scale degradation. (Note: this still pins to one shard — P2.1 lifts it the rest of the way.)
- **Effort:** M · **Risk:** medium · **Read/conformance risk:** response item ordering and per-item error semantics must match ES exactly — assert IDs/versions/`_seq_no` map back 1:1; run full conformance.

**P1.2 — Stop serializing the whole source just to measure bytes.**
- **What:** Delete `source.to_string().len()` in `IndexStore::index`/`index_batch`; use the known raw NDJSON byte length (the `Arc<[u8]>` len) or a cheap recursive size estimate. `memtable_bytes` only needs an approximation. Files: `index_store.rs:639, 691`.
- **Expected gain:** Removes the largest per-doc allocation after parse — several % throughput + big drop in allocator pressure.
- **Effort:** S · **Risk:** low · **Read risk:** none (only affects flush-threshold accounting).

**P1.3 — Default flush stored codec to LZ4; reserve zstd columnar for merge/forcemerge.**
- **What:** On flush, replace `encode_stored_v2` with `encode_stored_lz4` (compress the already-built JSON buffer directly; no re-parse, no column pivot, no cross-dep scan, no zstd). Keep `encode_stored_v2` only in `run_merge_once`/forcemerge. Add `storage.flush_stored_codec = lz4|columnar` (default lz4). Files: `index_store.rs:862`; `stored_codec.rs:67, 144`.
- **Mechanism / Lucene equiv:** Lucene90StoredFieldsFormat = LZ4 BEST_SPEED on flush; DEFLATE/best_compression is opt-in only.
- **Expected gain:** Removes the dominant flush-thread CPU cost, paid on every one of ~16 segments/flush. Large ingest uplift at scale.
- **Effort:** S · **Risk:** low · **Read risk:** decode must accept both codecs by segment header (it already does for merged segments) — verify mixed-codec reads in conformance.

**P1.4 — Gate the full-body NDJSON rewrite on actual field declarations.**
- **What:** Precompute a per-index bool "declares ignore_malformed/ignore_above/strict-date or is TSDB" at mapping-update time. Skip `rewrite_bulk_ignore_malformed` + `rewrite_bulk_time_series_ids` entirely for the common case (mapping present, no such fields) instead of the coarse `index_mappings.is_empty()` gate; when needed, fold the per-line transform into the existing rayon parallel parse. Files: `es_compat.rs:9298-9307, 19217, 19299-19364`.
- **Expected gain:** Removes a single-threaded O(body) parse+reserialize on the HTTP worker for the typical nginx-log bulk; frees a core, improves multi-client scaling.
- **Effort:** M · **Risk:** medium · **Conformance risk:** the precomputed predicate must exactly cover every case the rewrite handled — keep the rewrite as the fallback whenever the predicate is true; conformance for ignore_malformed/ignore_above/TSDB must stay green.

**P1.5 — Config width: `ingest_shards` → full core count; larger flush threshold.**
- **What:** Bump `ingest_shards` default from `(cpus/2).next_power_of_two()` (16) to full cpus (32) (`config.rs:763`). Make `flush_doc_threshold` config-driven (it is hardcoded 500_000 at `index.rs:415`) and raise it so flushes emit fewer, fuller segments.
- **Expected gain:** Better cross-request/multi-client scaling; fewer/larger segments → less merge pressure. Modest single-client effect, larger under concurrency.
- **Effort:** S · **Risk:** low · **Read risk:** larger memtables raise peak RSS and pre-flush query fan-out into the memtable — validate read p99 at the higher threshold.

### Phase 2 — Structural (the DWPT-equivalent + decouple flush)
*Where XERJ goes from "competitive" to "ahead." Medium risk; do after Phase 1 stabilizes.*

**P2.1 — Intra-request shard fan-out (the DWPT equivalent). — SINGLE HIGHEST-LEVERAGE STRUCTURAL CHANGE.**
- **What:** Instead of routing the whole batch to `shard_for(docs[0])`, partition the batch by `shard = hash(doc_id) & (n_shards-1)` into N sub-batches; process each concurrently (rayon scope / tokio `JoinSet`), each appending to its **own** WAL shard and inserting into its **own** FTS+storage memtable shard. Build one WAL frame buffer per shard. Reserve the contiguous seq range up front (already done) so global WAL order is preserved. Files: `index.rs:1025-1066, 1377-1408`; `index_store.rs:1560-1846` (accept pre-bucketed per-shard frame groups).
- **Mechanism / Lucene equiv:** DocumentsWriterPerThread — each thread builds its own in-RAM mini-segment with zero hot-path lock contention; one `_bulk` fans across all cores.
- **Expected gain:** **~min(Ncore, Nshard)x on the dominant insert+commit phase** — lifts 22-31k toward 100k+ — and **flattens the 31k→22k decay** because each shard now holds 1/N of the live set (per-doc insert cost stops rising). Parse/tokenize is already rayon-parallel, so this parallelizes the remaining serialized phase.
- **Effort:** M-L · **Risk:** medium · **Read/conformance risk:** seq_no/version ordering across shards must remain globally correct; refresh visibility must still expose all sub-batches atomically per request — conformance on `_seq_no`, optimistic concurrency, and refresh semantics.

**P2.2 — Carry parsed Value + pre-computed tokens into flush; stop re-parsing on the flush path.**
- **What:** Thread the already-parsed `Value` and ingest-time tokens (from `TurboIngestPipeline`) through the memtable into `do_flush_shard`, so FTS sidecar, doc-values, and stored encoder consume the in-memory representation instead of `serde_json::from_slice` + `extract_text_fields_from` per doc (`index.rs:6607-6624`) plus a third `from_slice` in `encode_stored_v2` (`stored_codec.rs:145`). Add `encode_stored_v2_from_values(&[Value])`. Files: `index.rs:6576-6700`; `memtable.rs` (drain payload carries tokens/Value); `stored_codec.rs:145`.
- **Mechanism / Lucene equiv:** DWPT tokenizes once into the in-RAM buffer; flush just serializes it.
- **Expected gain:** Cuts flush CPU per segment by ~2-3x of the parse+analyze cost → shorter flush wall-time (the back-pressure-critical bound) → more cores for ingest, smaller p99.
- **Effort:** L · **Risk:** medium · **Read risk:** none if output bytes are identical; add a parity assert (re-parse vs carried-Value produce byte-identical segment) behind a debug flag during rollout.

**P2.3 — Incremental / debounced snapshot persistence (kill the O(N)-per-flush tax).**
- **What:** Stop `serde_json::to_vec_pretty(&entire snapshot)` + write + rename on every flush. Either (a) coalesce `save_snapshot` behind a dirty-flag + time gate (the WAL already provides durability), or (b) write an append-only manifest delta (add/remove segment ids) compacted periodically. Use `to_vec` (not pretty) regardless. Files: `index_store.rs:1028, 1233-1257`.
- **Expected gain:** Removes the specific mechanism behind throughput decaying with corpus size — makes steady-state ingest **flat** instead of 31k→22k.
- **Effort:** M · **Risk:** medium · **Read/recovery risk:** crash-recovery must reconstruct the live segment set from WAL + last manifest — add a recovery test that kills mid-flush and replays.

### Phase 3 — Decisively beat ES + tighten p99 (background work fully off the hot path)

**P3.1 — Freeze-and-swap flush; never flush the shard being actively written.**
- **What:** Replace in-place Phase-1 drain (which locks the live shard) with a pointer swap: atomically replace the shard's FTS/storage memtable with a fresh empty one and hand the **frozen** buffer to the background flush task; ingest continues immediately against the new buffer. Trigger `maybe_spawn_flush` **once per batch**, not per doc. Files: `index.rs:829, 1406-1408, 1425-1482`; `memtable.rs:349-465` (add `swap_shard`/`freeze`).
- **Mechanism / Lucene equiv:** DWPT freeze-and-swap — the indexing thread never blocks behind its own flush.
- **Expected gain:** Eliminates the writer-vs-its-own-drain lock contention → **p99 from ~70ms toward single-digit ms**; recovers throughput lost to drain stalls.
- **Effort:** M · **Risk:** medium · **Read risk:** the frozen-but-not-yet-flushed buffer must remain query-visible (NRT) so reads don't lose just-ingested docs — conformance on read-after-write within refresh interval.

**P3.2 — Additive cache invalidation (stop wholesale cache-clear on every flush).**
- **What:** Segments are immutable, so a new tier-0 segment cannot invalidate dv/stored entries keyed by *existing* segments. Replace the blanket `query_cache.clear()/dv_cache.clear()/stored_value_cache.clear()` with: bump a `dataset_version` so query-cache results depending on the new segment miss naturally, leave per-segment DV/stored caches intact, and only evict a segment's entries when a merge actually drops it. Files: `index.rs:1479-1480, 1824-1830`, merge segment-drop site.
- **Expected gain:** Removes flush-coincident cold-cache read spikes — reads keep hitting warm immutable-segment caches across flushes, matching ES NRT readers. Directly tightens read p99.
- **Effort:** M · **Risk:** low · **Read/correctness risk:** must guarantee no stale cache entry survives a merge that drops/rewrites a segment — version-tag entries by segment id and assert on eviction.

**P3.3 — Move flush + merge onto a dedicated, throttled pool; wire the real IO rate limiter.**
- **What:** Run `do_flush_shard` Phase-2 (FST build, doc-values, compress, fsync) and `run_merge_once` on a dedicated rayon `ThreadPool` (sized from `config.merge_workers`) via `pool.install(...)`, **not** the global pool. Thread the existing `RateLimiter` (`merge.rs:140-178`) and `config.merge.io_rate_mb_per_sec` through the merge read/write loop. Respect `config.merge.max_concurrent` instead of `XERJ_MERGE_PARALLELISM`. Files: `index.rs:1794, 1846-2289, 2184`; `merge.rs:140-178`; `config.rs:429, 752`.
- **Mechanism / Lucene equiv:** ConcurrentMergeScheduler — dedicated merge threads with auto IO throttling; merges never starve indexing/search.
- **Expected gain:** Removes the background-vs-foreground CPU/IO collision behind both the 31k→22k decay and the flush/merge-coincident p99 spikes; throughput holds flat or rises like ES.
- **Effort:** L · **Risk:** medium · **Read risk:** throttling merges too aggressively raises segment count → read fan-out; tune rate to keep segment count bounded and re-check read p99.

**P3.4 — Reduce fsync amplification on the flush path.**
- **What:** Fold `.seg` + `.sidx` (+ `.ids`) into a single fsync group (or fsync the segment directory once after renames). Better: decouple visibility from durability — publish the segment to the snapshot after a buffered write (visible immediately; WAL handles crash recovery) and fsync lazily on a commit interval. Files: `segment.rs:374-394`; `index_store.rs:953, 799-1032`.
- **Mechanism / Lucene equiv:** refresh (visibility, no fsync) vs commit (durability, fsync).
- **Expected gain:** Cuts 2-3 fsync syscalls per shard-segment (×~16/cycle) to ~1 → removes flush-latency stalls, frees flush threads to drain faster.
- **Effort:** M · **Risk:** medium · **Recovery risk:** with lazy fsync, WAL must cover the unsynced window — recovery test required.

**P3.5 (optional polish) — Per-field codec: FST term dictionary + bit-packed/GCD doc-values.**
- **What:** Postings already match Lucene90 (PFOR-128). Add an FST term index and Lucene-style bit-packed/GCD/table doc-values. Files: `xerj-fts/src/index.rs` (FST), `doc_values.rs`.
- **Expected gain:** ~10-25% smaller segments + cheaper merges → indirectly steadier ingest and p99. Secondary; do last.
- **Effort:** L · **Risk:** low · **Read risk:** read decoders must support the new formats — broad read-path testing.

---

## 4. Tail-Latency (p99) Sub-Plan — beat ES on p99 too

The 70ms spikes have **two independent sources**; both must be killed:

1. **Flush-on-active-shard lock contention** → **P3.1 freeze-and-swap** + per-batch (not per-doc) flush trigger. The ingest thread never shares a lock with its own drain. *Target: removes the ingest-side stall entirely.*
2. **Cold-cache reads after every flush** → **P3.2 additive invalidation.** Reads stop falling back to mmap + LZ4/zstd decompress + column decode after each flush. *Target: removes the read-side spike entirely.*
3. **Merge IO/CPU bursts colliding with ingest** → **P3.3 throttled dedicated merge pool** (wire `RateLimiter` + `io_rate_mb_per_sec`). *Target: merges never steal foreground cores.*
4. **fsync stalls** → **P3.4** fold/defer fsyncs (visibility ≠ durability).

**Combined p99 target:** from **~70ms → single-digit ms**, below ES's tail. Guardrail: read p50/p99 must not regress at any phase (see §5).

---

## 5. Measurement / Validation

Use `bench-vs-es.mjs` as the single source of truth. Run each phase at **100k, 500k, 1M docs**, single-client (the benchmark's mode) **and** 8-client concurrent, capturing **ingest docs/s, read p50/p99, and the with-scale curve (does docs/s hold flat?)**.

**Per-phase protocol:**
1. `bench-vs-es.mjs --ingest --docs 100000,500000,1000000 --clients 1` → record docs/s at each scale and the slope (flat vs decaying).
2. `bench-vs-es.mjs --ingest --clients 8` → confirm multi-client scaling.
3. `bench-vs-es.mjs --read` → record read p50/p99; assert **no regression** vs the current 1.5x-1.85x read win.
4. **Conformance gate:** full ES-compat suite must stay **1326/1326**. Any drop blocks the phase.

**Per-phase target numbers (single-client, 1M docs):**

| After | Primary proof point | Ingest target | Scale curve | p99 target |
|-------|---------------------|---------------|-------------|------------|
| Baseline | — | 22-31k | **decays** 31k→22k | ~70ms spikes |
| P1 (route to turbo + LZ4 + kill redundant CPU) | re-parse/serialize gone; batch lock amortized | **45-70k** | decay reduced | spikes persist |
| P2 (fan-out + flush-no-reparse + incremental snapshot) | one bulk uses all cores; snapshot O(1) | **90-130k** | **flat** | spikes reduced |
| P3 (freeze-swap + additive cache + throttled merge + fsync) | no flush stall, warm reads | **120-150k+** | **flat/rising** | **single-digit ms** |

**Guardrails (must hold every phase):** read p99 ≤ baseline; read throughput within the existing 1.5-1.85x win; **1326/1326 conformance**; crash-recovery test green for any change touching snapshot/WAL/fsync (P2.3, P3.4).

---

## 6. "Beat ES" Scorecard

**ES baseline: 68k → 110k docs/s (rising with scale). XERJ baseline: 31k → 22k (falling).**

| Phase | Key changes | Projected XERJ ingest (1M, 1-client) | vs ES 110k |
|-------|-------------|--------------------------------------|------------|
| **Baseline** | — | 22k | **0.20x** (4.9x behind) |
| **Phase 1** | P1.1 route→turbo · P1.2 byte-count · P1.3 LZ4 flush · P1.4 rewrite gate · P1.5 width | **~45-70k** | **~0.5-0.6x** (closes half the gap) |
| **Phase 2** | P2.1 intra-request fan-out (DWPT) · P2.2 no-reparse flush · P2.3 incremental snapshot | **~90-130k** | **~1.0-1.2x — reaches/edges past ES** |
| **Phase 3** | P3.1 freeze-swap · P3.2 additive cache · P3.3 throttled merge · P3.4 fsync · P3.5 codec | **~120-150k+, flat/rising; p99 single-digit ms** | **~1.1-1.4x — decisively ahead on both throughput AND tail** |

> Projections are derived from the findings' stated per-change gains (P1.1 "2-4x", P2.1 "~min(Ncore,Nshard)x"), compounded conservatively and capped where levers overlap. They are estimates to be confirmed by §5, not measured results.

### The single highest-leverage change to do FIRST

**P1.1 — route the default `_bulk` index group through `index_batch_turbo_raw` instead of the per-doc `index_document().await` loop (`bulk.rs:736-801`).**

It is the cheapest change with the largest immediate payoff: it is a **routing change that unlocks machinery already in the tree** (parallel tokenize, one FTS lock, one WAL lock, amortized fsync, no re-parse), turning a serial per-doc loop into a batch commit for an expected **2-4x** with low effort. It also **clears the runway for P2.1** (intra-request shard fan-out), which is the structural change that actually lets one bulk request saturate all 32 cores and pushes XERJ **past** ES — the moment Rust + no-JVM + 32 cores finally wins ingest, not just reads.