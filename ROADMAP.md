# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-08-31 (against `v1.0.0-rc.72` and `main`). Statuses trace to issues, merged PRs, the CHANGELOG, and the conformance suite; items carried forward from the 2026-07-12 review without fresh live verification are marked as such. This review line is machine-checked: `docs_capability_lists` fails the build if a release is cut without re-reviewing this file (issue #298).

## Follow the roadmap

- **This file** is authoritative. If any other surface disagrees with it, this file wins — and that disagreement is a bug worth an issue.
- **[Milestones](https://github.com/xerj-org/xerj/milestones)** — the release-by-release view. Every open issue is triaged onto a milestone; the next RC's milestone is the short-term roadmap.
- **[Project board](https://github.com/users/xerj-org/projects/1)** — live status of every open item.
- **[Pinned issue #298](https://github.com/xerj-org/xerj/issues/298)** — the standing pointer, including how releases are cut and how to influence priorities.

## Shipping today (for context)

These are implemented and exercised by real API requests / the test suite / benchmarks:

- Elasticsearch REST wire compatibility (1,366 / 1,369 ES-YAML conformance cases; the gate on every change is **0 failed**, and the case count grows as cases are added — read the current number off CI, not off this file).
- Full-text search (BM25) and **<!-- generated:query-type-count -->50<!-- /generated:query-type-count --> query types**. Neither the list nor the number is maintained by hand here: the list is generated from `xerj_query::parser::SUPPORTED_QUERY_TYPES`, printed in full in [engine/README.md](./engine/README.md#query-types-supported) and [llms-full.txt](https://xerj.org/llms-full.txt), and pinned to `parse_query`'s dispatch table by `parser::tests::dispatch_table_matches_capability_manifest`; the number above sits in a machine-checked region pinned to that constant's length by `docs_capability_lists::published_capability_counts_match_the_constants`. A further <!-- generated:rejected-query-type-count -->2<!-- /generated:rejected-query-type-count --> keys — `has_child` and `has_parent` — are recognised and **rejected with a 400**, and are listed as such in the same places (issue #211).
  **Honest caveat, unchanged:** that count is the *dispatch* surface — every name on it parses, plans and executes, which is not a claim that every one is semantically faithful to ES. The known divergences are enumerated under *Known partials* below, and the ES-YAML conformance suite is the measured answer.
- **Aggregations: <!-- generated:agg-type-count -->62<!-- /generated:agg-type-count --> types**, likewise generated from `xerj_engine::aggs::SUPPORTED_AGG_TYPES`, printed in [engine/README.md](./engine/README.md#aggregation-types-supported) and [llms-full.txt](https://xerj.org/llms-full.txt), and pinned by the same count test. This includes the full **pipeline family**. `weighted_avg` is **not** in `SUPPORTED_AGG_TYPES` — see *Known partials*.
  **Exactness, precisely.** No probabilistic sketch sits in the metric path: `cardinality` is a true distinct count rather than an HLL estimate, and `terms` `doc_count` is precise. Two deliberate exceptions, stated the same way in `engine/README.md` and `llms-full.txt`: (1) the **sampling family** is a sample by definition — `run_sampler` sorts the matched documents by `_score` and keeps the first `shard_size` (default **200**), so every sub-aggregation under `sampler`, `random_sampler` or `diversified_sampler` is computed over that slice rather than the whole match set, `diversified_sampler` additionally caps documents per `field` value, and `random_sampler` shares the `sampler` implementation and **ignores ES's `probability`** (an accepted-and-ignored input, #204); (2) `percentiles` with the `hdr` option returns HdrHistogram-quantized values, deliberately, so ES's own outputs reproduce — the default `tdigest` path sorts every value and interpolates instead.
- **Dense-vector kNN** (`knn` query and ES 8.x top-level `knn`): unfiltered kNN on a full-precision cosine field (≥1,024 docs) is served by a **persisted HNSW graph with exact rescoring** — measured recall@10 1.00 on the official bench query, 100-probe mean 0.976 (ES 8.13.4 same protocol: 0.937); filtered/nested kNN, non-cosine similarity, SQ8 fields, and small indexes run the exact brute-force scan (cosine mapped to `(1+cos)/2`).
- **Hybrid search** — BM25 + kNN combined in a single request via the `hybrid` **query type** with `rrf|linear` fusion, verified live. (`fusion: "learned"` is parsed and **rejected** with a 400 naming the supported values — it is not implemented.) (The ES-native top-level `{query, knn}` body also unions both halves since rc.72 — see *Known partials* for its performance caveat.)
- **Zero-config folder onboarding** — `xerj autoindex <folder>` sniffs files, infers datasets, and creates one index per dataset: tree-sitter AST extraction for 34 languages (symbols, defs, line numbers — the [#295](https://github.com/xerj-org/xerj/issues/295) expansion; still open: Clojure and source-SQL wait on usable grammar crates, Nim/Crystal have none, fixed-form Fortran is deliberately unclaimed), CSV/JSON/JSONL/XML/YAML/SQLite/PDF/DOCX/HTML/log formats, `.gitignore`/`.xerjignore` support, incremental re-runs, and a machine-parseable progress stream.
- **Agent-memory REST API** (`/_memory/*`), **second-brain knowledge graph** (`/_graph`), **anomaly detection** (`_ml` with continuous datafeeds), **auto-embed on ingest** (default embedder is deterministic **lexical** feature-hashing — never described as neural; `--embed-mode neural` runs the in-binary BERT encoder, `--embed-mode proxy` an external endpoint).
- **Columnar storage** — the ZBS2 columnar block with 9 domain-aware encodings, ZSTD/LZ4 codecs, and SQ8 vector quantization, wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, **executed** index-lifecycle policies (ISM-modeled, `_ilm/*` + `_plugins/_ism/*`, since rc.15), `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` — all live-verified.
- **A single native binary**, statically linked, no JVM, sub-second cold start.

The release-by-release record of how all of this landed is [CHANGELOG.md](./CHANGELOG.md) — this file no longer duplicates it. Be aware of a real gap in that record: rc.1–rc.18 and rc.71 have entries, **rc.19 through rc.70 do not**. Those 52 releases are reconstructable only from `git log` and the release list, and closing that gap is itself a GA item below.

## Next release — [v1.0.0-rc.73](https://github.com/xerj-org/xerj/milestones)

rc.72 was cut on 2026-08-31 — its full contents are the [CHANGELOG.md](./CHANGELOG.md)
entry, not this file. It is **the idle-cost release**: a measurement session found the
process was not quiet at rest — 464 small indices (the shape `xerj autoindex` produces)
cost 15-27% of a core and 115 wakeups/s with zero requests, and a 154 MB corpus spent
40+ minutes at ~110% CPU merging after ingest "finished". Three of the four mechanisms
are fixed ([#871](https://github.com/xerj-org/xerj/issues/871) event-driven merge
scheduling, [#872](https://github.com/xerj-org/xerj/issues/872) an incremental memtable
aggregate replacing 74,240 lock acquisitions per second,
[#873](https://github.com/xerj-org/xerj/issues/873) an idle-age flush so a small dataset
reaches a segment instead of living in RAM). It also carries six ES-compatibility and
autoindex correctness fixes, each with a test proven to fail on the unfixed code.

**In flight for rc.73:**

- **The fourth idle-cost mechanism** — merge re-analyzes every document instead of
  merging postings ([#876](https://github.com/xerj-org/xerj/issues/876)). The segment
  writer already accepts a `PostingsWriter` rather than text, so the fix reuses machinery
  that exists; the design is published on the issue. The
  [#874](https://github.com/xerj-org/xerj/issues/874) budget — idle CPU under 0.5% of one
  core *independent of index count* — is not met until this and the remaining
  [#873](https://github.com/xerj-org/xerj/issues/873) levers (adaptive WAL shards,
  cold-index state) land.
- **The regression rc.72 shipped knowingly** — `knn` beside `query` is now correct but is
  answered by a stored-document scan ([#892](https://github.com/xerj-org/xerj/issues/892)).
- **Retrieval quality** — the symbol index returns whole class and method bodies rather
  than declarations, measured at 32-48x more bytes than grep for the same answer
  ([#500](https://github.com/xerj-org/xerj/issues/500)). This one undercuts a claim the
  project leads with, so it is a correctness issue about our own marketing as much as a
  performance one.
- **CI reliability** — [#751](https://github.com/xerj-org/xerj/issues/751) still hangs the
  default-parallelism test step intermittently, and it was red on `main` repeatedly during
  the rc.72 cut. The cost is not the failed run, it is that a red gate stops distinguishing
  a real break from noise. [#891](https://github.com/xerj-org/xerj/issues/891) is a
  confirmed instance of the same class (a process-global counter two tests share).
- **Data-loss follow-up** — [#890](https://github.com/xerj-org/xerj/issues/890): the
  prefix-scoped exclusion sweep can over-delete across corpora on a legacy text-mapped
  catalog. Filed against code that shipped in rc.72.

## The road to [v1.0.0 GA](https://github.com/xerj-org/xerj/milestone/2)

The 1.0 bar: **every public claim verified against the release binary, and every input either honoured or refused loudly.** The gate list, each item an issue:

- **Close the accepted-and-ignored class** (the [#204](https://github.com/xerj-org/xerj/issues/204) umbrella closed once its members carried their own tracking; PR [#258](https://github.com/xerj-org/xerj/pull/258) carried one pass of the sweep and is merged). Known members still open: `nested` `inner_hits` unparsed; `random_sampler`'s ignored `probability`; `weighted_avg` returning HTTP 200 with an error buried in the aggregations body instead of a 400 (the 400 is part of #258). **Retired from this list:** `nested` `score_mode`, which was parsed-then-ignored until [#862](https://github.com/xerj-org/xerj/pull/862) made a nested query roll its matching children's scores into the parent per `score_mode` (rc.71).
- **Security hardening backlog** — cargo-audit and fuzzing landed in CI with rc.16 ([#207](https://github.com/xerj-org/xerj/issues/207) closed); the deferred TLS/auth/symlink hardening items from the Phase-2 security backlog remain.
- **The mixed read-under-write p99 gap** — the 4 benchmark losses out of 85 measured comparisons, all the same root cause (reads landing on the live memtable under writer pressure). Written up in [`demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md); the candidate fix is a visibility/parity-mode design decision, not a micro-optimisation, and it stays on the GA gate until fixed or explicitly descoped with the benchmark loss kept public.
- **Ship-or-descope every entry in *Known partials* below.** GA does not ship with a "partial" section that reads like a feature list.
- **Close the CHANGELOG gap.** rc.19–rc.70 shipped without entries. A project whose pitch is verified numbers cannot ask users to reconstruct 52 releases from `git log`; either backfill them or state plainly, in the file, which range is not documented and why.

## Beyond 1.0 — themes

- **AST language expansion** — 25 further tree-sitter grammars, tiered by demand, one PR per
  tier. [#295](https://github.com/xerj-org/xerj/issues/295) delivered the expansion to 34
  languages and is closed; the remaining tiers have no tracking issue yet, so this theme is
  a plan rather than a commitment until one exists. Tier 1 (Kotlin, Swift, Scala, Dart, Lua, Perl, R, Julia, Haskell, Elixir) may land earlier in an RC if the grammar/ABI checks prove out.
- **Distributed clustering maturity** — embedded Raft handles cluster metadata today, but the default run is **single-node**; multi-node sharding/replication hardening is a post-GA track, and XERJ does not claim multi-node production readiness until it is measured.
- **Neural embedder ergonomics** — share one loaded model across indices (today each index lazily holds its own `NeuralHandle`), optional pre-warm at startup, a larger default model option.
- **Log-analytics data path** — the dedicated `xerj-logs` columnar module is still not invoked from non-test engine/server code; log-shaped analytics run through ZBS2 + the generic aggregation suite. Wire it or remove it.
- **Broader aggregation families** — geo/IP/nested/join coverage beyond the current surface; the conformance suite is the measure.

## Known partials

Honesty section: things that resolve without an error but do not implement full ES semantics. Each must be shipped or explicitly descoped before GA.

Re-verified against `main` 2026-08-30:

- **`weighted_avg`** — not in `SUPPORTED_AGG_TYPES`; still returns HTTP 200 with an embedded error instead of executing or returning 400 (the 400 is part of the #258 sweep).
- **`has_child` / `has_parent`** — recognised and rejected with a 400 (fail-loud by design until real parent-child join semantics exist; `REJECTED_QUERY_TYPES` in `parser.rs`).

Carried forward from the 2026-07-12 review, not re-verified live since:

- **`nested`** — matching is real and per-element (`test_nested_query`) and `score_mode` now rolls matching children's scores into the parent (`avg`/`max`/`min`/`sum`/`none`, [#862](https://github.com/xerj-org/xerj/pull/862), rc.71). Still missing: ES's separate nested-document indexing, and `inner_hits` is not parsed (#204 member, above).
- **`span_term` / `span_or` / `span_not`** — return 0 hits **standalone**, while composite span queries (`span_near` / `span_first` / `span_containing`) using the same clauses return correct hits.
- **`type`** — mapped to `MatchAll`.
- **`combined_fields`** — mapped to `multi_match cross_fields`; scoring is not exact. `rank_feature` passes through on plain fields (no `rank_feature` field type).
- **ES-native top-level `{query, knn}` — RETIRED in rc.72.** It now unions both halves and scores a document reached by both as the sum, aggregations included ([#825](https://github.com/xerj-org/xerj/issues/825) via [#879](https://github.com/xerj-org/xerj/pull/879)). It is kept in this list for one release with its replacement caveat, which is a performance one rather than a correctness one: the pinned clauses cannot project to the full-text index, so this shape is answered by a stored-document scan and is substantially slower than the `hybrid` query type (~208 ms vs ~2.5 ms measured on 100k documents at k=10). [#892](https://github.com/xerj-org/xerj/issues/892) carries the indexed-route fix. Correct-and-slow was chosen deliberately over fast-and-wrong.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
