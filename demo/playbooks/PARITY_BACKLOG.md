# ES parity backlog — autonomous close-out loop

Source: `es_api_audit.json`. Goal: drive every *closeable-on-single-node* gap to
REAL. Distributed/NA items (CCR, rollup/transform execution, ML) cannot be made
real on one binary and are the loop's terminal set.

## Batches (value-ordered)
- [x] **A — query/ingest features**: `_eql/search`, `_enrich/policy/:name/_execute`, `_validate/query` (GET+POST), `_xpack/usage` real counts
- [x] **B — index management ops**: `_close`/`_open`, `_cache/clear`, `_disk_usage`, `_shrink`/`_split`, `_freeze`/`_unfreeze`, `_recovery`
- [x] **C — cluster/node stats realness**: `_nodes`, `_nodes/stats`, `_nodes/:id/stats`, `_cluster/stats`, `_cluster/state`, `_cluster/pending_tasks`, `_internal/desired_balance`, `_index_template/_simulate_index`, `painless/_execute`, `_ilm/policy`
- [x] **D — cat honest-derived**: `_cat/segments`, `_cat/fielddata`, `_cat/pending_tasks`, `_cat/plugins`, `_cat/nodeattrs`, `:index/_alias`, `_tasks/:id/_cancel`, `_update_by_query`
- [x] **E — transform/rollup execution**: `_transform/:id/_start`+`_stop` (real pivot via composite agg → dest), `_rollup/job/:id/_start`+`_stop` (real date_histogram+terms+metrics → rollup_index), `_rollup/data/:index` (real caps). Live: 4008→47 pivot docs, 12 rollup docs.
- [x] **F — stragglers**: **search GET/POST** — found+fixed a real engine agg bug: under the sharded memtable the DV agg fast paths are disabled and the `run_aggs_with_all` fallback ran over only the 256-doc `materialisation_limit` window, under-counting every `size:0` aggregation on indices >256 docs. Now aggregates the FULL corpus (memtable + all segment stored sections), filtered by `doc_matches_query` for non-`match_all`. Verified count 256→4008 (and 100,200 at scale); this also retroactively fixes Batch E (transform/rollup were silently 256-capped). Plus `cluster_allocation_explain` (current_state derived from real index existence), `cluster_reroute` (real shard totals + dry_run/explain echo), `cat_thread_pool` (real node name; documented tokio work-stealing → truthful 0 counters).
- [x] **TERMINAL (cannot do single-node) — loop stops here**: 7 × `_ccr/*` (cross-cluster replication needs ≥2 clusters) + 3 × `_cat/ml/*` (needs an ML engine). Honest stubs, documented. **Final audit: REAL 152/162 = 93.8%, PARTIAL 1, STUB 9, 0 broken.**

## Loop protocol
Each iteration: pick next unchecked batch → implement (codegen workflow) → build →
boot+seed+verify live → re-audit affected groups → regenerate report → commit as
xerj-org → check off here → schedule next. Stop when only TERMINAL remains.
