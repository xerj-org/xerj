# xerj v0.6.0 — release notes

**Tag:** `v0.6.0`
**Date:** 2026-04-25
**Branch baseline:** `main` @ post-cherry-pick of `fix/painless-execute-security`

This release is a hardening + perf pass on top of v0.5.9. It closes
the remaining items from the 2026-04-25 Linus-style code review
(see `engine/reports/CODE_REVIEW_LINUS_2026-04-25.md`), brings in
parallel disk-parity work that landed via `perf/sync-engine`, and
hardens a security-flagged endpoint.

ES YAML test suite: **1303–1305 / 1329** (98.0–98.2%) — equal to
the v0.5.9 high-water mark, no regressions across multiple runs.

## What you'll feel

* **No more silent JSON-body drops.** Send a malformed `_search`
  body or one that exceeds the deserializer's nesting limit and you
  now get a `400 parse_exception` with the real reason instead of a
  `200 OK` with default `match_all` results. New extractor
  `OptionalJson<T>` migrated all 17 production handlers in
  `xerj-api/src/es_compat.rs`.

* **OOM/DOS hardening.** `from + size` capped at
  `limits.max_result_window` (default 10 000), `_mget` body capped
  at `limits.max_mget_docs` (default 10 000), aggregations capped at
  `limits.max_buckets` (default 65 536, mirrors ES `search.max_buckets`),
  query nesting capped at 64 levels via thread-local `DepthGuard`.
  `request_cache_seen` (membership tracker for hit/miss counts) is
  now a bounded FIFO instead of an unbounded `HashSet<u64>` that
  grew with unique-query cardinality.

* **Operator-tunable `Config`.** Three previously-hardcoded
  knobs are now in `LimitsConfig`/`MergeConfig`:
  `max_body_bytes`, `max_result_window`, `max_mget_docs`,
  `max_buckets`, `tier_floor_mb`, `min_merge_count`,
  `max_merge_count`. The `XERJ_MAX_MERGE_COUNT` env-var override
  was dropped — use `merge.max_merge_count` instead.

* **KNN segment cache.** First brute-force kNN against a segment
  pays the I/O + decompress + JSON parse; subsequent kNN over the
  same index hits an `Arc<Vec<Value>>` clone. For a 50-segment
  index with 50 MB stored each, repeated kNN goes from ~3 s cold to
  ~30 ms warm.

* **Faster terms aggregations.** `select_nth_unstable_by` partial
  sort replaces full-Vec sort + truncate in `terms`, `geotile_grid`,
  `geohash_grid`, `multi_terms`. For 10 M buckets / `size: 10` this
  is ~20× fewer comparisons.

* **Cross-build correctness.** The `MEMTABLE_SHARDS` const that
  silently routed to a 16-bucket mask while the actual shard array
  was sized from `Config.engine.ingest_shards` is gone — that
  panicked on any host with `ingest_shards != 16` (e.g., a 4-core
  box where the default is 2).

## Security

* `POST /_scripts/painless/_execute` is now hardened with input
  limits (`script.source` ≤ 4096 bytes, double-literal cap 256
  values) and an explicit SECURITY block on the handler doc-comment
  clarifying that the endpoint is a sandboxed stub, not a
  Painless interpreter. Closes T-004 (DREAD 7.6) from the
  internal security assessment.

## Bug fixes (carried in from `perf/sync-engine` since v0.5.9)

* **Merge silently drops every doc** in segments parsed with
  `simd_json + Box<RawValue>` — fixed by switching to
  `serde_json::from_slice` (handles RawValue's serde-private newtype
  tag that simd_json's adapter does not recognise). Same fix
  propagated to all stored-section parse sites in `index.rs`
  (kNN brute-force, `get_document` segment scan, raw-bytes flush
  reassembly).

* **SIGTERM hang** on shutdown — gRPC accept loop and per-Index
  merge task now exit cleanly on the shutdown signal.

* **Disk parity with Elasticsearch** via segment-level Zstd-19 and
  WAL force-rotate on `_flush`. Refer to the head-to-head reports
  under `engine/reports/`.

* **B-3 / B-2** kNN post-flush + raw-bytes flush FTS visibility,
  ES-compat `_flush` route, force-flush on shutdown.

* **Latent `ShardedFtsMemtable` panic** on small-core machines —
  the static `Self::shard_for(...)` helper hardcoded a 16-shard
  modulus while instance methods used `self.shard_mask`. Eight
  internal callers and two `Index` callers switched to
  `self.shard_for_dynamic(...)`; the static helper is gone.

* **`_source: false` in scrolls** — engine-level `apply_source_filter`
  semantics intentionally preserved; `_source` emission is suppressed
  in `es_compat.rs` so the response layer can still resolve
  `fields` / `_ignored` / `highlight`. (See
  `fix/painless-execute-security@71bf986` for the alternate
  engine-level nulling we deliberately did NOT take — would have
  broken response-layer field resolution.)

## Code-quality items closed from the Linus review

| Severity | Item | Where |
|---|---|---|
| P0 | `from + size` cap | `xerj-query/src/parser.rs` |
| P0 | Bool-query depth cap (64) | `xerj-query/src/parser.rs::DepthGuard` |
| P0 | `_mget` batch cap | `xerj-api/src/es_compat.rs::mget` |
| P0 | Aggregation `max_buckets` cap | `xerj-engine/src/aggs.rs::MAX_BUCKETS` |
| P1 | HTTP body limit → Config | `xerj-api/src/router.rs` |
| P1 | Memtable / WAL sizes already wired (audit corrigendum) | `xerj-engine/src/index.rs::store_config_from` |
| P1 | `MEMTABLE_SHARDS` runtime config + latent-panic fix | `index_store.rs`, `memtable.rs`, `index.rs` |
| P1 | Merge tier sizes → Config | `xerj-storage/src/merge.rs`, `xerj-common/src/config.rs::MergeConfig` |
| P2 | KNN segment cache | `xerj-engine/src/index.rs::stored_values_for` |
| P2 | Drop `to_vec()` on stored buffers (5 sites) | `xerj-engine/src/index.rs` |
| P2 | TopK partial-sort for terms-style aggs | `xerj-engine/src/aggs.rs` (3 sites) |
| P2 | Strict body extractor (no silent drop) | `xerj-api/src/extract.rs` (new), 17 callsites |
| P2 | Bound `request_cache_seen` (FIFO) | `xerj-engine/src/index.rs::RequestCacheSeen` |
| P2 | Drop `to_vec()` on bulk action/doc lines (3 sites) | `xerj-engine/src/bulk.rs` |
| P2 | Production-path panic audit (5 unwrap sites + 1 unreachable) | various |

## Known unfixed (still failing ES YAML)

22–24 tests across runs (run-to-run variance), all pre-existing
since the v0.5.9 high-water:

* HoltWinters smoothing constants
* `_tsid` Murmur128 hash sort
* HDR percentiles multi-shard precision
* `flattened` synthetic source + index sort
* Two-level field collapsing
* BM25 max_score Lucene-exact precision
* `date_nanos` cross-index resolution
* `_ignored` metadata-field terms agg ordering

These need targeted work in v0.6.x and are not in scope for this
release.

## Build & install

Binary releases for 8 targets are produced by the
`.github/workflows/release.yml` matrix on tag push:

* `x86_64-unknown-linux-gnu`
* `x86_64-unknown-linux-musl`
* `aarch64-unknown-linux-gnu`
* `aarch64-unknown-linux-musl` (built via `cargo-zigbuild`)
* `aarch64-apple-darwin`
* `x86_64-apple-darwin` (cross-built from `macos-14`)
* `x86_64-pc-windows-msvc`
* `aarch64-pc-windows-msvc`

Source: `cargo build --release -p xerj-server` from `engine/`.

## Upgrade notes

No on-disk format changes between v0.5.9 and v0.6.0 — segments,
WAL, doc-values, HNSW indices written by v0.5.9 are read by v0.6.0
unchanged.

Operators with custom `Config` should add new fields if not relying
on defaults:

```toml
[limits]
max_body_bytes     = 104857600  # 100 MiB
max_result_window  = 10000
max_mget_docs      = 10000
max_buckets        = 65536

[merge]
tier_floor_mb      = 4
min_merge_count    = 4
max_merge_count    = 16
# `max_segment_mb` default raised 5120 → 8192 to match historical hardcoded behaviour
```

If you previously set `XERJ_MAX_MERGE_COUNT`, move that value into
`merge.max_merge_count` — the env-var override is gone.
