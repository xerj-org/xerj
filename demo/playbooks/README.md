# Xerj feature playbooks & smoke-tests

Real, runnable smoke-tests for every major Xerj use case — each one a
sequence of **actual commands against a live binary with their captured
output**, so the same material doubles as:

- **docs** — copy a step's `curl` + JSON straight into a docs page, and
- **screencast scripts** — drive an AISE scene from `playbooks.json`.

## Run

```bash
# 1. start xerj (ES-compat on :9200, native ops on :8080)
xerj --insecure --data-dir /tmp/xerj/data

# 2. run the suite (Node ≥18, uses curl)
node demo/playbooks/run.mjs
```

It rebuilds its own fixtures, executes every step, asserts pass/fail, and
writes the artifacts below. Re-running against a fresh `--data-dir` is
deterministic.

## Artifacts

| file | purpose |
|---|---|
| `SMOKE_REPORT.md` | coverage table + pass/fail + the doc-vs-binary gaps |
| `casts/<id>.md` | human-readable cast per use case (`$ curl …` → real output) — for docs & screencasts |
| `playbooks.json` | structured `{label, cmd, status, output}` per step — for generating AISE scenes |

## Use cases covered

boot/health · quickstart CRUD · bulk ingest · full-text query families ·
aggregations · vector/kNN · analyzers · mappings/templates/aliases ·
log-analytics · ES migration parity · native ops (health/metrics) ·
embedded Console UX.

## Status

**61/61 steps green.** The ES-compat API (:9200) and the native `/v1` ops
API (:8080) are both fully covered — including `/v1/health/ready`,
`/v1/cluster/health`, `/v1/admin/flush`, and `/v1/admin/backup` (a real
on-disk snapshot via `engine::create_snapshot`).

### ES wire-protocol divergences to remember
A few ES paths differ in shape (the suite uses the working forms): per-index
`_cat/indices/{name}` is not supported (use `_cat/indices`); global `_stats`
and global `POST /_refresh` differ (use `_count` and per-index `_refresh`).

### Native ingest content-types
`POST /v1/indices/:name/turbo-ingest` expects a **JSON array** with
`content-type: application/json` (not ndjson); native index creation is
`POST /v1/indices` (not `PUT`).
