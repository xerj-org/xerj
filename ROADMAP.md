# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-08-16 (against `v1.0.0-rc.18` and `main`). Statuses trace to issues, merged PRs, the CHANGELOG, and the conformance suite; items carried forward from the 2026-07-12 review without fresh live verification are marked as such. This review line is machine-checked: `docs_capability_lists` fails the build if a release is cut without re-reviewing this file (issue #298).

## Follow the roadmap

- **This file** is authoritative. If any other surface disagrees with it, this file wins — and that disagreement is a bug worth an issue.
- **[Milestones](https://github.com/xerj-org/xerj/milestones)** — release-by-release, where they exist. They are **not** a live view: at the rc.18 cut 36 of 38 open issues carried no milestone and the only two that did sat on already-shipped rc.17. The [rc.19 milestone](https://github.com/xerj-org/xerj/milestone/4) was created at this cut — this file had referenced it while it did not exist — and the 31 issues below are now on it. It is a live view again.
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
- **Hybrid search** — BM25 + kNN combined in a single request via the `hybrid` **query type** with `rrf|linear|learned` fusion, verified live. (The ES-native top-level `{query, knn}` body does **not** fuse — see *Known partials*.)
- **Zero-config folder onboarding** — `xerj autoindex <folder>` sniffs files, infers datasets, and creates one index per dataset: tree-sitter AST extraction for 34 languages (symbols, defs, line numbers — the [#295](https://github.com/xerj-org/xerj/issues/295) expansion; still open: Clojure and source-SQL wait on usable grammar crates, Nim/Crystal have none, fixed-form Fortran is deliberately unclaimed), CSV/JSON/JSONL/XML/YAML/SQLite/PDF/DOCX/HTML/log formats, `.gitignore`/`.xerjignore` support, incremental re-runs, and a machine-parseable progress stream.
- **Agent-memory REST API** (`/_memory/*`), **second-brain knowledge graph** (`/_graph`), **anomaly detection** (`_ml` with continuous datafeeds), **auto-embed on ingest** (default embedder is deterministic **lexical** feature-hashing — never described as neural; `--embed-mode neural` runs the in-binary BERT encoder, `--embed-mode proxy` an external endpoint).
- **Columnar storage** — the ZBS2 columnar block with 9 domain-aware encodings, ZSTD/LZ4 codecs, and SQ8 vector quantization, wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, **executed** index-lifecycle policies (ISM-modeled, `_ilm/*` + `_plugins/_ism/*`, since rc.15), `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` — all live-verified.
- **A single native binary**, statically linked, no JVM, sub-second cold start.

The release-by-release record of how all of this landed (rc.1 through rc.18) is [CHANGELOG.md](./CHANGELOG.md) — this file no longer duplicates it.

## Next release — [v1.0.0-rc.19](https://github.com/xerj-org/xerj/milestone/4)

rc.18 was cut on 2026-08-16 — its contents are the [CHANGELOG.md](./CHANGELOG.md)
entry, not this file. Sixteen merges since rc.17. A correctness and
release-hygiene RC: nearly everything in it closes a defect rc.17 shipped, and
three came from outside contributors: #419 and #418 from @SebTardif and #429 from @buger. (#418 is a CLA signature rather than a code change, and it is squash-merged, so it carries no merge commit — which is how a count taken from `git log --merges` misses it.)

Items it retired from this roadmap:

- **`dense_vector` `scalar8` / `int8_hnsw` no longer scores from stale codes**
  ([#371](https://github.com/xerj-org/xerj/issues/371)) — the release-blocking
  correctness defect rc.17 shipped.
- **`_count` stopped answering a `term` on `text` from an oracle `_search` never
  uses** (via #417). Note what this is *not*: [#362](https://github.com/xerj-org/xerj/issues/362)
  was closed `not_planned`, not fixed — its `prefix`/`wildcard` half is unresolved
  and is now acceptance criteria on #423, which this file lists below as open and
  release-blocking. Only the `_count` oracle half shipped.
- **`sort` on an ES meta-field resolves to a real value**
  ([#401](https://github.com/xerj-org/xerj/issues/401), via #420) — `_seq_no`
  keyset pagination walks 30 of 30 where it walked 4.
- **A multi-index `scroll` reports the index each hit came from**
  ([#414](https://github.com/xerj-org/xerj/issues/414), via #424) — 600 hits had
  been collapsing to 300 distinct `(_index, _id)` pairs. Scoped: comma-list,
  wildcard and `_all` only; the alias case is [#433](https://github.com/xerj-org/xerj/issues/433).
- **autoindex lib tests no longer fail at full parallelism**
  ([#385](https://github.com/xerj-org/xerj/issues/385), via #429 from **@buger**)
  — verified ten consecutive runs clean where the issue reported five of ten
  failing.
- **Console deletes and upserts fail closed** (#419, from **@SebTardif**).
- **`main` was red on the CI toolchain and is not any more** (#422). Nothing pins
  the Rust toolchain, so a `stable` release introduced a clippy lint that failed
  every open pull request on code it had not touched.
- **autoindex no longer aborts a run on a file declaring 2+ SQL tables**
  ([#360](https://github.com/xerj-org/xerj/issues/360)) and the parallelism
  test-isolation defect ([#372](https://github.com/xerj-org/xerj/issues/372)) are
  closed.

**Open defects on the [rc.19 milestone](https://github.com/xerj-org/xerj/milestone/4)**, 31 issues, which this list mirrors. Each carries a measured repro:

- **Correctness, release-blocking.** Term-level matching has two implementations
  and only one has a schema: `doc_matches_query` evaluates buffered documents
  against raw `_source` with no mapping and no analyser, while flushed documents
  use the real term dictionary, so the same query answers differently before and
  after a flush ([#423](https://github.com/xerj-org/xerj/issues/423),
  consolidating eight symptom issues).
- **Data fidelity.** `_source` drops null-valued keys, and whether it drops them
  depends on corpus size — all four kept at 1,500 documents, mixed at 2,000, all
  lost at 4,000 ([#415](https://github.com/xerj-org/xerj/issues/415)).
- **Ranking.** A `bool` carrying any second clause collapses `_score` into a
  near-constant and reorders the hit set; two no-op filters both produce
  `ln(2)+1` for every hit ([#361](https://github.com/xerj-org/xerj/issues/361),
  [#399](https://github.com/xerj-org/xerj/issues/399)). Measured on this cut: a
  bare `match` and a single-clause `bool.must` are score-neutral AND stable
  across `size`; adding a `filter` **or** a `must_not` makes both untrue. So it
  is not "the filter path" — it is any `bool` carrying a second clause, which
  drops into the schema-less `_source` scorer described in #423.
- **Performance regression shipped in this RC.** The #401 fix disables the
  memtable pre-clone rejection for meta-sorts: 130 ms at the stock 128k flush
  threshold against 1.4 ms for an ordinary field sort, and 16.2 s for a
  `search_after` walk over a 128k-document buffered index — the very workload
  #401 was filed for. That is only one of the three items
  [#421](https://github.com/xerj-org/xerj/issues/421) records: `fix/issue-401`
  merged as #420 while its verification returned `sound=false`, and one of the
  unresolved items is that ES-YAML conformance was never run with a trustworthy
  result on a change that alters ES wire sort semantics for four meta-fields.
- **Silent wrong answers.** Switching `embedding.mode` from lexical to neural on
  an existing index is unguarded, so neural query vectors are scored against
  lexical document vectors in the same 384-dim space
  ([#434](https://github.com/xerj-org/xerj/issues/434)); `match` on a
  `semantic_text` field runs BM25 rather than kNN
  ([#363](https://github.com/xerj-org/xerj/issues/363)); a refused key suppresses
  field evolution for up to 100 documents
  ([#382](https://github.com/xerj-org/xerj/issues/382)).
- **Scroll, remaining.** Continuation pages never emit real `_seq_no`/`_version`
  ([#428](https://github.com/xerj-org/xerj/issues/428)); an alias-addressed
  multi-index scroll still reports the alias
  ([#433](https://github.com/xerj-org/xerj/issues/433)).
- **autoindex robustness.** Unqualified printable magic still junks text
  ([#379](https://github.com/xerj-org/xerj/issues/379)) — three fix attempts have
  now been refuted by independent verification, twice for breaking real images,
  and the next attempt starts from a multi-encoder corpus rather than a rule.
  `%PDF-` is the same class on a different code path, matched before the magic
  table ([#403](https://github.com/xerj-org/xerj/issues/403)).
  Fallback prose amplifies one ≤16 MiB magic-less payload into thousands of
  records at ~40x peak RSS ([#381](https://github.com/xerj-org/xerj/issues/381),
  reframed by **@buger**'s measurements). Reconciliation still aborts on the
  project's own reference corpora
  ([#367](https://github.com/xerj-org/xerj/issues/367)).
- **Performance.** The neural embedder runs ~15 docs/s on short strings
  ([#366](https://github.com/xerj-org/xerj/issues/366)); nested term aggregations
  materialise all sub-buckets ([#375](https://github.com/xerj-org/xerj/issues/375)).
- **CI can only see what it is configured to run.** The parallelism gate covers
  only `--lib`, so the class #385 belonged to stays invisible on the integration
  targets ([#384](https://github.com/xerj-org/xerj/issues/384)); an `esclient`
  test asserts a 260 ms wall-clock bound and flakes ~1 run in 5 on `main`, which
  will redden unrelated pull requests
  ([#436](https://github.com/xerj-org/xerj/issues/436)); and two green PRs once
  produced an unbuildable merge because the merged workspace is not resolved
  before merge ([#352](https://github.com/xerj-org/xerj/issues/352)).
- **Named nowhere until now, and material.** Multi-valued keyword fields flatten
  to one FTS token ([#332](https://github.com/xerj-org/xerj/issues/332));
  `match`/`multi_match` on a keyword field is mapping-aware only after flush
  ([#354](https://github.com/xerj-org/xerj/issues/354)) — both the same family as
  #423; a semantic/kNN clause nested in a `bool` is silently dropped, including
  the ES 8.x top-level `knn` block ([#395](https://github.com/xerj-org/xerj/issues/395));
  the scroll snapshot cap is applied per-index on one route and to the summed
  total on another ([#405](https://github.com/xerj-org/xerj/issues/405)); sorting
  on any unresolvable field strands `search_after` at page one
  ([#437](https://github.com/xerj-org/xerj/issues/437)); autoindex catalog IDs are
  global, so two corpora sharing a byte-identical file overwrite each other
  ([#416](https://github.com/xerj-org/xerj/issues/416)); and the audit log records
  neither writes nor who made them ([#329](https://github.com/xerj-org/xerj/issues/329)).
- **Carried forward.** The `fields` API omitting embedding companions
  ([#310](https://github.com/xerj-org/xerj/issues/310)), the dynamic-mapping
  field-budget overshoot ([#312](https://github.com/xerj-org/xerj/issues/312)),
  and the Painless call-depth limit holding only under optimised codegen
  ([#353](https://github.com/xerj-org/xerj/issues/353)).

In flight: [#425](https://github.com/xerj-org/xerj/pull/425) (**@SebTardif**),
[#430](https://github.com/xerj-org/xerj/pull/430) (**@buger**) and
[#431](https://github.com/xerj-org/xerj/pull/431) (**@Vinz2168**) — all three
from outside contributors. [#402](https://github.com/xerj-org/xerj/pull/402) was
closed: a request-level allowlist cannot know which meta-fields a given corpus
makes resolvable, so #401's remaining half is
[#437](https://github.com/xerj-org/xerj/issues/437). [#427](https://github.com/xerj-org/xerj/pull/427)
is green on paper and has been refuted three rounds running; it is not merging
until a corpus decides its rule.

## The road to [v1.0.0 GA](https://github.com/xerj-org/xerj/milestone/2)

The 1.0 bar: **every public claim verified against the release binary, and every input either honoured or refused loudly.** The gate list, each item an issue:

- **Close the accepted-and-ignored class** (the [#204](https://github.com/xerj-org/xerj/issues/204) umbrella closed once its members carried their own tracking; the sweep continues in PR [#258](https://github.com/xerj-org/xerj/pull/258)). Known members still open: `nested` `score_mode` parsed-then-ignored and `inner_hits` unparsed; `random_sampler`'s ignored `probability`; `weighted_avg` returning HTTP 200 with an error buried in the aggregations body instead of a 400. `dense_vector` `quantization` ([#275](https://github.com/xerj-org/xerj/issues/275)) is **closed** — `lookup_vector_quantization` is read on the serving path. Every issue on this GA gate is now closed, so the gate needs re-deriving from open work rather than reading as a live list.
- **Security hardening backlog** — cargo-audit and fuzzing landed in CI with rc.16 ([#207](https://github.com/xerj-org/xerj/issues/207) closed); the deferred TLS/auth/symlink hardening items from the Phase-2 security backlog remain.
- **The mixed read-under-write p99 gap** — the 4 benchmark losses out of 85 measured comparisons, all the same root cause (reads landing on the live memtable under writer pressure). Written up in [`demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md); the candidate fix is a visibility/parity-mode design decision, not a micro-optimisation, and it stays on the GA gate until fixed or explicitly descoped with the benchmark loss kept public.
- **Ship-or-descope every entry in *Known partials* below.** GA does not ship with a "partial" section that reads like a feature list.

## Beyond 1.0 — themes

- **AST language expansion** — [#295](https://github.com/xerj-org/xerj/issues/295) is **closed**: the expansion to 34 languages shipped, and "Shipping today" above describes the delivered state. What remains is not that issue — Clojure and source-SQL wait on usable grammar crates, Nim/Crystal have none, and fixed-form Fortran is deliberately unclaimed. Needs a fresh issue if it is to stay on this list.
- **Distributed clustering maturity** — embedded Raft handles cluster metadata today, but the default run is **single-node**; multi-node sharding/replication hardening is a post-GA track, and XERJ does not claim multi-node production readiness until it is measured.
- **Neural embedder ergonomics** — share one loaded model across indices (today each index lazily holds its own `NeuralHandle`), optional pre-warm at startup, a larger default model option.
- **Log-analytics data path** — the dedicated `xerj-logs` columnar module is still not invoked from non-test engine/server code; log-shaped analytics run through ZBS2 + the generic aggregation suite. Wire it or remove it.
- **Broader aggregation families** — geo/IP/nested/join coverage beyond the current surface; the conformance suite is the measure.

## Known partials

Honesty section: things that resolve without an error but do not implement full ES semantics. Each must be shipped or explicitly descoped before GA.

Re-verified against `main` 2026-08-11:

- **`weighted_avg`** — not in `SUPPORTED_AGG_TYPES`; still returns HTTP 200 with an embedded error instead of executing or returning 400 (the 400 is part of the #258 sweep).
- **`has_child` / `has_parent`** — recognised and rejected with a 400 (fail-loud by design until real parent-child join semantics exist; `REJECTED_QUERY_TYPES` in `parser.rs`).

Carried forward from the 2026-07-12 review, not re-verified live since:

- **`nested`** — matching is real and per-element (`test_nested_query`), but ES's separate nested-document indexing is missing: `score_mode` is parsed and ignored, `inner_hits` is not parsed (#204 members, above).
- **`span_term` / `span_or` / `span_not`** — return 0 hits **standalone**, while composite span queries (`span_near` / `span_first` / `span_containing`) using the same clauses return correct hits.
- **`type`** — mapped to `MatchAll`.
- **`combined_fields`** — mapped to `multi_match cross_fields`; scoring is not exact. `rank_feature` passes through on plain fields (no `rank_feature` field type).
- **ES-native top-level `{query, knn}`** — does not union the kNN hits; one-request BM25+kNN fusion works only through the explicit `hybrid` query type.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
