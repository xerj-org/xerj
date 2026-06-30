# Battle Test: drain-outside-lock fix
Date: Thu Apr 16 05:25:04 PM UTC 2026

## Fix Applied
- drain_shard: release write lock BEFORE simd-json parse
- .cargo/config.toml restored (target-cpu=native)
- peek_shard_has_raw_bytes: skip FTS build for raw-bytes path

## 20M Doc Ingest (3 runs)

### Run 1
```
[2m2026-04-16T17:25:39.033961Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"6fae277c-0017-4ea1-bbbd-ccc40e73a298" [3mdoc_count[0m[2m=[0m108702
[2m2026-04-16T17:25:40.090163Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m75 [3mmax_seq_no[0m[2m=[0m19574599
[2m2026-04-16T17:25:40.090443Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"1e77ece5-f6a1-442f-8455-4ecffe894a91" [3mdoc_count[0m[2m=[0m192809 [3mmin_seq[0m[2m=[0m17772999 [3mmax_seq[0m[2m=[0m19574599
[2m2026-04-16T17:25:40.163090Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"1e77ece5-f6a1-442f-8455-4ecffe894a91" [3mdoc_count[0m[2m=[0m192809
[2m2026-04-16T17:25:40.330229Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m75 [3mmax_seq_no[0m[2m=[0m17652998
[2m2026-04-16T17:25:40.330506Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"1b4de533-b66f-4d8d-9a81-2aa4042b16e0" [3mdoc_count[0m[2m=[0m310000 [3mmin_seq[0m[2m=[0m13900001 [3mmax_seq[0m[2m=[0m17652998
[2m2026-04-16T17:25:40.467135Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"1b4de533-b66f-4d8d-9a81-2aa4042b16e0" [3mdoc_count[0m[2m=[0m310000
[2m2026-04-16T17:25:40.675843Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m75 [3mmax_seq_no[0m[2m=[0m19711566
[2m2026-04-16T17:25:40.676103Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"7e7754c3-2751-48b2-a04e-6ffc11a4c61e" [3mdoc_count[0m[2m=[0m159184 [3mmin_seq[0m[2m=[0m17762999 [3mmax_seq[0m[2m=[0m19711566
[2m2026-04-16T17:25:40.739879Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"7e7754c3-2751-48b2-a04e-6ffc11a4c61e" [3mdoc_count[0m[2m=[0m159184
```

### Run 2
```
[2m2026-04-16T17:26:19.472656Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:26:19.616712Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:26:19.699322Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m68 [3mmax_seq_no[0m[2m=[0m18732821
[2m2026-04-16T17:26:19.699604Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"eece4a47-7405-4513-915f-44dfc0d368b2" [3mdoc_count[0m[2m=[0m220000 [3mmin_seq[0m[2m=[0m15150001 [3mmax_seq[0m[2m=[0m18732821
[2m2026-04-16T17:26:19.762134Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:26:19.793091Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"eece4a47-7405-4513-915f-44dfc0d368b2" [3mdoc_count[0m[2m=[0m220000
[2m2026-04-16T17:26:19.914031Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:26:21.928715Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m68 [3mmax_seq_no[0m[2m=[0m19845053
[2m2026-04-16T17:26:21.929000Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"62b874c7-d19c-437f-a24f-3c0e118f5ecb" [3mdoc_count[0m[2m=[0m272941 [3mmin_seq[0m[2m=[0m15000001 [3mmax_seq[0m[2m=[0m19845053
[2m2026-04-16T17:26:22.046614Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"62b874c7-d19c-437f-a24f-3c0e118f5ecb" [3mdoc_count[0m[2m=[0m272941
```

### Run 3
```
[2m2026-04-16T17:26:59.885406Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"fee8e274-851d-4415-9059-e80f43f4701b" [3mdoc_count[0m[2m=[0m149956 [3mmin_seq[0m[2m=[0m17280001 [3mmax_seq[0m[2m=[0m19149905
[2m2026-04-16T17:26:59.966954Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"fee8e274-851d-4415-9059-e80f43f4701b" [3mdoc_count[0m[2m=[0m149956
[2m2026-04-16T17:27:00.042330Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m69 [3mmax_seq_no[0m[2m=[0m18886703
[2m2026-04-16T17:27:00.042600Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"f8cc258a-459c-4917-a4d3-605a53d29537" [3mdoc_count[0m[2m=[0m144135 [3mmin_seq[0m[2m=[0m17430001 [3mmax_seq[0m[2m=[0m18886703
[2m2026-04-16T17:27:00.054815Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:27:00.108782Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"f8cc258a-459c-4917-a4d3-605a53d29537" [3mdoc_count[0m[2m=[0m144135
[2m2026-04-16T17:27:00.219242Z[0m [33m WARN[0m [2mxerj_engine::index[0m[2m:[0m merge: failed to parse stored as RawValue: Serde("invalid type: newtype struct, expected any valid JSON value") at character 0
[2m2026-04-16T17:27:01.505988Z[0m [32m INFO[0m [2mxerj_storage::wal[0m[2m:[0m WAL checkpoint written [3mgeneration[0m[2m=[0m69 [3mmax_seq_no[0m[2m=[0m19389905
[2m2026-04-16T17:27:01.506267Z[0m [32m INFO[0m [2mxerj_storage::index_store[0m[2m:[0m segment flushed [3msegment_id[0m[2m=[0m"a75d9fd1-a2ec-4ccf-b979-d06d04362bed" [3mdoc_count[0m[2m=[0m242941 [3mmin_seq[0m[2m=[0m14920001 [3mmax_seq[0m[2m=[0m19389905
[2m2026-04-16T17:27:01.602750Z[0m [32m INFO[0m [2mxerj_engine::index[0m[2m:[0m memtable flushed to segment with FTS index [3msegment_id[0m[2m=[0m"a75d9fd1-a2ec-4ccf-b979-d06d04362bed" [3mdoc_count[0m[2m=[0m242941
```
