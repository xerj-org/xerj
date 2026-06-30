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

## Known gaps (captured by the suite, not silently skipped)

The ES-compat API (:9200) is comprehensive and green. The **native `/v1`
API (:8080)** currently implements only `/v1/health` and `/v1/metrics`;
the other documented endpoints (`/v1/health/ready`, `/v1/cluster/health`,
`/v1/admin/flush`, `/v1/admin/backup`, `/v1/indices/:name/turbo-ingest`,
etc.) return 404/405 in this build. They show up as ⚠️ in the report so the
docs and the binary can be reconciled. Per-index `_cat/indices/{name}` and
the global `_stats`/`POST /_refresh` also differ from ES (use
`_cat/indices`, `_count`, and per-index `_refresh`).
