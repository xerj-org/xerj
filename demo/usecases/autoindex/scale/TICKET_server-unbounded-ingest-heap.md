# TICKET: xerj server anon-heap grows unboundedly under sustained bulk ingest (~7-10 KB per doc write, never released) — OOM at scale

**Severity:** blocker for any TB-scale (or even >5M-doc) ingest claim.
**Found by:** autoindex scale prover, 2026-07-09, binary built from feat/autoindex @ 353edb7 base.
**Related:** the 83 GB / 112 GB box-killing OOMs of 2026-07-09 (session cae8685a) — same signature, now reproduced under a 40G cgroup cap.

## Repro (deterministic, twice)
1. Boot server capped: `systemd-run --user --scope -p MemoryMax=40G -p MemorySwapMax=0 xerj --insecure --config <toml> --data-dir <dir>`
2. Bulk-ingest a 4.6 GB / 11.68M-record mixed corpus (JSONL/CSV/syslog; `xerj autoindex` client, 8 workers, 8 MB bulks, explicit idempotent `_id`s).
3. Server anon-RSS climbs ~400-450 MB/s at ~54k docs/s ingest and is cgroup-OOM-killed at ~41.8 GB.

- Run 1 (semantic_text on): killed at ~4.25M docs ingested (~2.7 GB source). `dmesg`: `Memory cgroup out of memory: Killed process 425872 (xerj) anon-rss:41810592kB`.
- Run 2 (`--no-semantic`): killed at ~5.10M docs. `anon-rss:41809836kB`. **Semantic embedding is NOT the driver** — same slope without it (~8.4 KB heap per doc).
- Control (923 MB / 2.6M-record subset): completes; server peak 24.5 GB, **idle RSS after ingest stops: 17.5 GB** vs **1.4 GB on disk** (~12x on-disk). It's retention, not transient working set.
- Overwrites count too: re-indexing the same `_id`s onto a warm server pushed HWM to 34.3 GB at ~4.95M cumulative doc-writes — dead versions' heap is not reclaimed until merge retires their segments.

## Evidence-backed observations
- anon-RSS (heap), not file-rss — mmap'd segment readers are not the growth.
- Memtables DO flush and merges DO run throughout (server log) while heap ratchets monotonically.
- Growth is linear in **doc writes** at ~7-10 KB/doc across corpora/configs.

## Suspects (from `engine/crates/xerj-engine/src/index.rs` field docs)
- `stored_value_cache: DashMap<String, Arc<Vec<Value>>>` — per-segment fully-parsed stored docs, comment says "unbounded parsed Values, ~3-6x raw bytes", "Left unbounded for now"; only evicted when a merge retires the segment id.
- `dv_cache` — same "left unbounded" caveat per its comment.
- `id_pos_cache: DashMap<String, Arc<HashMap<String, u32>>>` — per-segment map of EVERY doc id, populated by the explicit-`_id` ingest lookup path (autoindex always sends explicit ids).
- Note `stored_slices_cache` / `decoded_stored_cache` are already budgeted (`*_bytes` + budget) — the same treatment is missing for the two caches above.

## Suggested fix shape
Global byte budget + LRU (or wholesale clear at high-water) for `stored_value_cache` / `dv_cache` / `id_pos_cache`, mirroring the existing `STORED_SLICES_CACHE_BUDGET` pattern; and/or skip populating parse-heavy caches from the ingest id-lookup path.

## Impact
- 119 GB box dies at ~14M docs of continuous ingest (observed: 40 GB per ~5M docs).
- 1 TB of ~640 B log records = ~1.7B docs → ~12-17 TB of heap at the observed slope. TB-scale ingest is categorically blocked until fixed.

Raw series (RSS-vs-time CSVs, docs-vs-time, server/dmesg logs) captured by the scale-prover run; harness scripts in this directory reproduce the corpus (`gen_corpus.py`) and the run (`run_with_rss.sh`).
