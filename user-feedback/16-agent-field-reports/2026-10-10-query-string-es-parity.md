# Fixing query_string / lenient divergences against a live ES 8.13.4 (2026-10-10)

**Agent:** Claude Code (Claude Opus 5.5)  ·  **XERJ:** `xerj v1.0.0-rc.93` (release builds of `upstream/main` and of three fix branches)  ·  **Platform:** macOS 27.0.0, aarch64

**Pointed at:** small hand-built indices (2–8 docs: `text`, `keyword`, `long`, `float`), the same requests sent side by side to a docker ES 8.13.4. No autoindex.

**Used it for:** the ES-compat search surface (`_search`, `_count`, `?q=`, `query_string`, `match`, `multi_match`), to close #1284 and two `query_string` bugs found on the way.

**Verdict:** The `query_string` lowering was the weakest part I touched. A field group `title:(a OR b)` searched every field. `alpha -beta` returned documents containing `beta`. `+` was ignored, and AND/OR precedence was not Lucene's. All of it was silent: HTTP 200 with plausible hits, which is worse than an error for a dashboard user. Two things made the fixes slower than they needed to be:
- `_search` rewrites `match` on a numeric field into `term` at the JSON level in es_compat, and `_count` does not. A test that only hits `_count` misses a whole path.
- The YAML runner does not enforce `catch:`, so a 400 case must assert `status` on the body.

What worked well: `_validate/query?explain=true` shows the lowered AST, and each root cause was obvious within one call. The ES-YAML gate and scoped builds were reliable.

Friction: a second node on `--port 9311` died with `native REST: bind 127.0.0.1:9312 … Address already in use`, because the node on 9310 had taken that native port.

**Numbers:** each figure counts the cases whose status or hit ids differ from ES 8.13.4, before → after.
- `lenient` / type-check matrix (45 cases, `_count` and `_search`): 31 → 6. The 6 are out of scope and listed in #1293.
- `field:(…)` group matrix (27 cases): 23 → 2. The 2 are the empty `title:()` / `title:`.
- `+`/`-`/`NOT`/AND/OR matrix (40 cases): 26 → 0.

Full ES-YAML suite on each branch: 0 failed (1389 / 1388 / 1388 passed). Not measured: latency or scoring parity; only hit sets were compared.

**Filed alongside:** #1293 (#1284), issue #1298 → PR #1300, issue #1299 → PR #1301.
