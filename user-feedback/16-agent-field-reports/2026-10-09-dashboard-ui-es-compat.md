# ES-compat check of XERJ as the backend for a Kibana-style dashboard UI (2026-10-09)

**Agent:** Claude Code (Claude Opus 5.5), on behalf of @Vinz2168  ·  **XERJ:** `xerj v1.0.0-rc.93`  ·  **Platform:** macOS 27, aarch64

**Pointed at:** an unpublished Dashboard / Discover / Dev Tools front end that talks to any ES-compatible endpoint via `_msearch`, `_search`, `_mapping`, `_cat/indices`; reduced to a 4-doc `repro` index.

**Used it for:** ES wire-protocol backend for a browser UI, cross-checked request by request against Elasticsearch 8.13.4.

**Verdict:** Good — the data side carries a real dashboard. `_msearch` with per-item errors, `date_histogram` + `extended_bounds`, percentiles, cardinality, terms ordered by single-value metrics, `match_phrase`, `track_total_hits` all behaved like ES. The gaps are small but each one fails *silently* (HTTP 200, wrong or missing output), which is the worst kind for a UI: wildcard highlight fields give no highlighting, a percentile order path or a typo'd one is dropped instead of erroring, and type errors become "0 hits", and `_msearch` has no top-level `took`. The empty `Warning: 299` header on every response turns into a deprecation warning per call in official clients. CORS works via `[cors]` in the TOML, but I only found it by grepping the binary.

**Numbers:** user's dashboard: 54 distinct queries matched a reference implementation on identical data, except the gaps below (reported, not re-run by me). Every divergence in the issues was re-run by me on a clean data dir against both rc.93 and ES 8.13.4.

**Also noticed, not filed:** `percentiles`/`cardinality` are exact where ES approximates (more accurate, may differ from ES on large data); `_cat/indices` lists 14 `.xerj_*` system indices next to user indices (no ES-side comparison possible on a fresh node).

**Filed alongside:** #1281 (wildcard highlight), #1282 (order on percentiles), #1283 (invalid order path), #1284 (type errors → 0 hits), #1285 (empty Warning header), #1286 (`--help`: CORS + default native port), #1288 (`_msearch` missing top-level `took`).
