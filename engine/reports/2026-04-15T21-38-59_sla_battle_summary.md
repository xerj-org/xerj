# SLA Battle Test Results
**Date**: 2026-04-15 21:38:59 MDT
**Machine**: drai, 32 cores, 119Gi RAM
**xerj version**: v0.1.0
**Commit**: c7fa334 revert: M5.24 dv_cache LRU — didn't help either

## Summary

| Metric | Count |
|---|---:|
| **PASSED** | 44 |
| **FAILED** | 13 |
| **Pass rate** | 77.2% |

## Failures

| # | Test | Issue |
|---|---|---|
| 1 | LICENSE file | Missing engine/LICENSE file |
| 2 | bool+range query | Returned 0 instead of 100 — doc_values shortcut bug on combined bool+range with small segments |
| 3 | terms aggregation | Returned 0 buckets — agg on unflushed data path empty |
| 4-6 | Update by ID (3 docs) | Returns 201 (created) instead of 200 (updated) — _update API returns wrong status |
| 7 | Secure server startup | TLS cert auto-gen didn't start in 15s |
| 8-9 | Auth 401 checks | Secure server didn't start, so auth couldn't be tested |
| 10 | KNN search | Returned 0 results — vector search not indexed for these test docs |
| 11 | Time-range query | Returned 0 — @timestamp range query over dynamic mapping probably not indexed as date |
| 12 | SQL endpoint | Non-JSON response — SQL is a stub |
| 13 | _analyze endpoint | Returns -1 tokens — analyzer API not fully implemented |

## Analysis

**Core ES-compat (CRUD, Bulk, Search, Scroll, Aggs)**: 44/57 = 77 % pass.
**Failures are in edge features**: TLS/auth (4), vector/KNN (1), SQL (1), _analyze (1), update status code (3), and 2 query-correctness issues (bool+range, terms agg on unflushed data).

The 2 query-correctness failures (bool+range, terms agg) are the most concerning — they affect search quality and need investigation. The others are feature gaps (SQL, KNN, _analyze) or minor HTTP-status-code issues.
