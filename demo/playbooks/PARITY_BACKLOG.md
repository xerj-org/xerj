# ES parity backlog — autonomous close-out loop

Source: `es_api_audit.json`. Goal: drive every *closeable-on-single-node* gap to
REAL. Distributed/NA items (CCR, rollup/transform execution, ML) cannot be made
real on one binary and are the loop's terminal set.

## Batches (value-ordered)
- [x] **A — query/ingest features**: `_eql/search`, `_enrich/policy/:name/_execute`, `_validate/query` (GET+POST), `_xpack/usage` real counts
- [x] **B — index management ops**: `_close`/`_open`, `_cache/clear`, `_disk_usage`, `_shrink`/`_split`, `_freeze`/`_unfreeze`, `_recovery`
- [x] **C — cluster/node stats realness**: `_nodes`, `_nodes/stats`, `_nodes/:id/stats`, `_cluster/stats`, `_cluster/state`, `_cluster/pending_tasks`, `_internal/desired_balance`, `_index_template/_simulate_index`, `painless/_execute`, `_ilm/policy`
- [ ] **D — cat honest-derived**: `_cat/segments`, `_cat/fielddata`, `_cat/pending_tasks`, `_cat/plugins`, `_cat/nodeattrs`, `:index/_alias`, `_tasks/:id/_cancel`, `_update_by_query`
- [ ] **E — transform/rollup execution** (single-node feasible: scheduled aggregation jobs)
- [ ] **TERMINAL (cannot do single-node)**: all `_ccr/*`, `_cat/ml/*` — leave as honest stubs, document.

## Loop protocol
Each iteration: pick next unchecked batch → implement (codegen workflow) → build →
boot+seed+verify live → re-audit affected groups → regenerate report → commit as
xerj-org → check off here → schedule next. Stop when only TERMINAL remains.
