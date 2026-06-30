# Multicore Scaling — Final Results
Date: 2026-04-17T23:20 UTC
Machine: 32 cores x86_64, 119GB RAM, tmpfs I/O

## Progression (20M nginx docs, best-of-3)

| Milestone | Ingest rate | Total rate | Cores | vs ES |
|---|---|---|---|---|
| Baseline (single WAL) | 790k/s | 720k/s | 9/32 | 28× |
| M2: 16 sharded WALs | 920k/s | 858k/s | 13/32 | 33× |
| M5+M7: Config shards + raw-byte segments | 915k/s | 825k/s | 13/32 | 32× |
| M8: Skip flush parse + lazy DV | 1,439k/s | 1,412k/s | 15/32 | 54× |
| M4+M6: Flush fix + memchr scanner | 1,426k/s | 1,384k/s | 15/32 | 53× |
| **Back-pressure tuning** | **1,579k/s** | **1,550k/s** | **~16/32** | **60×** |

## Key Changes

1. **Sharded WAL** (M2): 16 independent WAL files, route by xxh3(doc_id)
2. **Configurable shards** (M5): engine.ingest_shards drives WAL + memtable + FTS
3. **Raw-byte segments** (M7): source_bytes flow through to segment writer
4. **Skip flush parse** (M8): drain_shard_raw() skips serde_json parse entirely
5. **Lazy doc-values** (M8): DV sidecar build deferred for raw-bytes path
6. **Flush scheduler fix** (M4): `return` → `continue` in maybe_spawn_flush
7. **Back-pressure tuning**: 5ms × 10 (was 50ms × 5), raised thresholds

## Config

```toml
[engine]
ingest_shards = 16    # default: num_cpus / 2
flush_workers = 8     # default: num_cpus / 4
merge_workers = 2
search_workers = 8
```

## Remaining Opportunities

| Optimization | Expected gain | Effort |
|---|---|---|
| Zero-copy scan (M9) | 5-10% | 4h |
| Reduce format!() in doc_id gen | 3-5% | 1h |
| WAL write batching across shards | 5-10% | 3h |
| Increase flush_size_mb for fewer segments | 5% | config only |
| ARM NEON/SVE2 for hash + CRC | 10-15% on ARM | 8h |

## Summary

From **720k/s to 1,550k/s** = **2.15× improvement** in one session.
From ES 8.13's **26k/s** = **60× faster** ingest.
Storage: 942MB for 20M docs vs ES's ~3.4GB = **3.6× smaller**.
