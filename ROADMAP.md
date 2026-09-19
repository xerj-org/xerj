# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-09-08 (against `v1.0.0-rc.74` and `main`). Statuses trace to issues, merged PRs, the CHANGELOG, and the conformance suite; items carried forward from the 2026-07-12 review without fresh live verification are marked as such. This review line is machine-checked: `docs_capability_lists` fails the build if a release is cut without re-reviewing this file (issue #298). *The zero-token direction* below was added and verified separately on 2026-09-18, against `main` @ `4d8dadbf`; the rest of the file was not re-reviewed on that date.

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
- **Zero-config folder onboarding** — `xerj autoindex <folder>` sniffs files, infers datasets, and creates one index per dataset: tree-sitter AST extraction for 34 languages (symbols, defs, line numbers — the [#295](https://github.com/xerj-org/xerj/issues/295) expansion; still open: Clojure and source-SQL wait on usable grammar crates, Nim/Crystal have none, fixed-form Fortran is deliberately unclaimed), CSV/JSON/JSONL/XML/YAML/SQLite/PDF/DOCX/HTML/log/`.eml`/mbox (Google Takeout) formats, `.gitignore`/`.xerjignore` support, incremental re-runs, and a machine-parseable progress stream.
- **Agent-memory REST API** (`/_memory/*`), **second-brain knowledge graph** (`/_graph`), **anomaly detection** (`_ml` with continuous datafeeds), **auto-embed on ingest** (default embedder is deterministic **lexical** feature-hashing — never described as neural; `--embed-mode neural` runs the in-binary BERT encoder, `--embed-mode proxy` an external endpoint).
- **Columnar storage** — the ZBS2 columnar block with 9 domain-aware encodings, ZSTD/LZ4 codecs, and SQ8 vector quantization, wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, **executed** index-lifecycle policies (ISM-modeled, `_ilm/*` + `_plugins/_ism/*`, since rc.15), `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` — all live-verified.
- **A single native binary**, statically linked, no JVM, sub-second cold start.

The release-by-release record of how all of this landed is [CHANGELOG.md](./CHANGELOG.md) — this file no longer duplicates it. Be aware of a real gap in that record: rc.1–rc.18 and rc.71 have entries, **rc.19 through rc.70 do not**. Those 52 releases are reconstructable only from `git log` and the release list, and closing that gap is itself a GA item below.

## Next release — [v1.0.0-rc.75](https://github.com/xerj-org/xerj/milestones)

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

## The zero-token direction

**Zero-token** is the name for work XERJ does locally, inside the engine, so that a person or an agent spends no model tokens *finding*, *judging*, *watching for* or *handing over* an answer. Search already works that way — no model runs to answer a query. This section is the rest of that idea, as product work (long form: [docs/ZERO_TOKEN_DIRECTION.md](./docs/ZERO_TOKEN_DIRECTION.md)), with each item's status checked against `main` on 2026-09-18 (`4d8dadbf`, 32 commits after `v1.0.0-rc.74`) by reading the code named beside it. "In flight" means a pushed branch exists and nothing of it is on `main`; "planned" means no code exists. Tracking issue: [#941](https://github.com/xerj-org/xerj/issues/941).

**Stage 1 — in flight now** (each lands as its own PR; none is merged at the time of writing):

- **Judged search — a rerank stage.** Retrieve with BM25 or `hybrid`, then have a second stage re-judge the top *N* before the page is returned. *Today on `main`:* the ES `rescore` block (query rescorer) and the `hybrid` query type with `rrf|linear`; there is no rerank stage. *In flight:* branch `feat/rerank-stage` — a `rerank` block on `_search` that hands the top hits to an external relevance judge, opt-in per request and inert until an operator configures a key. It is the one feature that would send document text off the machine, which is exactly why a *local* judge is on this list. *The bar for a local model is a measured one.* On the BEIR test splits with the opt-in neural embedder (`--embed-mode neural`, all-MiniLM-L6-v2, CPU) the shipped `hybrid` RRF query is already the best arm we have — nDCG@10 **0.699–0.704 on SciFact and 0.345 on NFCorpus** over three runs, against 0.657 / 0.302 for BM25 alone, 0.676 / 0.329 for vectors alone, and 0.686 / 0.332 for "BM25 top-30 re-ordered by the same bi-encoder", which is *worse* than hybrid on both. So re-ordering with the embedder we already ship is not worth building, and **a local cross-encoder ships only if it beats 0.699 / 0.345 on the same harness, by more than the run-to-run spread** ([`benchmarks/neural-path-triage/`](./benchmarks/neural-path-triage/)). Those figures are with the neural embedder; the default embedder is lexical feature hashing and was not part of this run. The hybrid figure is the only arm that does not reproduce — 0.6993, 0.7023 and 0.7044 on SciFact across three runs of unchanged indices — because tied RRF scores are ordered by a per-process hash seed ([#940](https://github.com/xerj-org/xerj/issues/940)); a gain inside that spread is not a gain.
- **Share links and a guest reading room.** Hand one person a link to one corpus or one document, read-only, without creating them an account. *Today on `main`:* nothing — there is no share or guest code in the tree, and reading a corpus needs an account session or an API key. *In flight:* branches `feat/share-links` (scoped, expiring links; a share can never name a system index) and `feat/console-corpus-reader` (the corpus browser and the reader the link opens). Both branches carry commits their authors marked unverified; they are not claims yet.
- **Mail ingest.** *Today on `main`, unreleased:* `xerj autoindex` extracts `.eml` / MIME messages — headers, body, and attachments indexed as their own records ([#921](https://github.com/xerj-org/xerj/pull/921)) — and, since [#949](https://github.com/xerj-org/xerj/pull/949), streams `mbox` mailboxes and extracted Google Takeout exports (archives are never opened) and writes `replies_to` / `attachment_of` edges from the mail headers. Measured on a synthetic mailbox only; the node's ingest memory is the limit ([#948](https://github.com/xerj-org/xerj/issues/948)).
- **`autoindex` resilience.** One dataset the server refuses currently aborts the whole run ([#929](https://github.com/xerj-org/xerj/issues/929)), the `--no-graph` progress line is indistinguishable from a hang ([#931](https://github.com/xerj-org/xerj/issues/931)), and `xc-index.sh --fresh` fails on a previously indexed corpus ([#930](https://github.com/xerj-org/xerj/issues/930)). *In flight:* PR [#934](https://github.com/xerj-org/xerj/pull/934) (`fix/autoindex-resilience`). It is on this list because every later item assumes a folder can be indexed unattended.

**Found while measuring stage 1, and gating it** — each is a filed issue with a literal repro, not a wish:

- A declared analyzer stops applying at flush: `analysis.analyzer.default` is honoured by the memtable only, a per-field `analyzer` is accepted and ignored, and an unknown analyzer name is accepted ([#937](https://github.com/xerj-org/xerj/issues/937)). This is why stemming cannot currently be turned on, and it is an accepted-and-ignored member for the GA gate above.
- Neural ingest keeps ~3.4 of 32 hardware threads busy: **6.1 documents/s** on ~1,470-character abstracts through one `_bulk` stream, **29.5 documents/s** from the same node when eight clients send concurrently, for the same total CPU ([#938](https://github.com/xerj-org/xerj/issues/938)).
- A `semantic` query on an index holding any multi-passage document is an exact scan that deep-copies every stored document per query: **~410 ms p50 on 5,183 documents, of which the model's forward pass is ~14 ms**; the same query on a 10,003-document single-passage index is ~14 ms ([#939](https://github.com/xerj-org/xerj/issues/939)).

**Stage 2 — planned; the status lines say what exists today, which in most cases is less than the API surface suggests:**

- **Semantic detections.** A standing question over incoming documents: a cheap `percolate` prefilter selects candidates, a typed judgment decides (a small closed set of outcomes, not free text), and an alert carries a **calibrated probability** — calibrated meaning measured on held-out labelled data and published with its reliability curve, or not shipped. *Today:* the `percolate` query is real — stored queries are matched against a supplied document. There is **no alerting**: `PUT /_watcher/watch/{id}` stores the body in memory, answers `"condition":{"met":true}`, and no scheduler ever evaluates a watch (`es_compat.rs`, `put_watch`); the console's `.xerj_alert_rules` and `.xerj_alert_fires` indices have schemas and are created at bootstrap, and no evaluator reads them (`xerj-console-api/src/indices.rs`). Before anything new is built, `_watcher` must either run watches or refuse them — it is an accepted-and-ignored surface today.
- **A real object-storage backend.** *Today:* `S3Backend` in `xerj-storage/src/backend.rs` is a **local-directory simulation** — it mirrors an S3 key layout onto a local path and contains no network client. The config selector knows this and **fails loud on purpose**: `storage.backend = "s3"` refuses to start with *"the S3 storage backend is not implemented in this build; only "local" is supported"* (`xerj-common/src/config.rs`). Planned: a real client behind the existing `StorageBackend` trait (range reads, atomic object PUT), the local segment cache in front of it, and crash-consistency tests against a real endpoint before the selector is allowed to accept the value.
- **A block index mode for logs.** An index mode for log-shaped data that stores rows in time-partitioned columnar blocks and skips whole blocks by their min/max metadata, instead of building a per-term inverted index over every field. *Today:* the `xerj-logs` crate implements that design (columnar encoding, log-template extraction, time-range queries with block skipping, retention), is compiled into `xerj-engine` and `xerj-server` as a dependency, and is **called from no non-test code**. Log-shaped analytics run through the general columnar segment format and the aggregation suite. Wire it behind an explicit index setting and measure it against the general path on the same data, or remove it — the *Log-analytics data path* theme below is this same item.
- **User-code ingest plugins.** *Today:* the ingest pipeline's transforms are built-in native Rust plugins — rename, drop, add, JSON parse, timestamp parse, PII redaction, grok, route — and they do run on `_bulk`. The crate is named `xerj-wasm`, but **the wasmtime backend is not in the tree**: there is no `wasmtime` dependency and no `wasm` feature, only a note that one could be added behind the same trait. Planned: a sandboxed runtime for user-supplied transforms with fuel and memory limits and no ambient filesystem or network access. Until then the refusal rule holds: a pipeline naming a processor this build does not implement is stored as unrunnable and every ingest through it is refused, never quietly run as a shorter pipeline.
- **A corpus hub of signed, pre-indexed packs.** Download a reference corpus — a standard library, a specification set — already indexed, and mount it, instead of every user spending the same CPU-hours indexing the same public text. *Today:* nothing; no pack code exists. The pack format is the design work: a **format version** readers refuse when they do not know it (the rule segment headers already follow), **per-file checksums**, a **detached signature** over the manifest, and **licence and provenance on every record** so a hit can say where its text came from and under what terms. Three risks decide whether this ships at all: **redistribution licence** (an index is a derived copy of its source text — a pack may carry only what its licence lets us redistribute, which excludes much of what people most want indexed); **pack safety** (a pack is untrusted input to the segment readers, so it needs the fuzzing the on-disk formats get, and a signature proves who built a pack, not that it is safe to open); and **format stability** (a published pack outlives the release that built it, so the pack format needs a compatibility promise the internal segment format has never had to make).

## Beyond 1.0 — themes

- **AST language expansion** — 25 further tree-sitter grammars, tiered by demand, one PR per
  tier. [#295](https://github.com/xerj-org/xerj/issues/295) delivered the expansion to 34
  languages and is closed; the remaining tiers have no tracking issue yet, so this theme is
  a plan rather than a commitment until one exists. Tier 1 (Kotlin, Swift, Scala, Dart, Lua, Perl, R, Julia, Haskell, Elixir) may land earlier in an RC if the grammar/ABI checks prove out.
- **Distributed clustering maturity** — embedded Raft handles cluster metadata today, but the default run is **single-node**; multi-node sharding/replication hardening is a post-GA track, and XERJ does not claim multi-node production readiness until it is measured.
- **Neural embedder ergonomics** — one loaded model is already shared by every index with the same embedder configuration (each index holds its own `NeuralHandle`, and identical configurations resolve to one lazily loaded model — `shared_neural_cell` in `xerj-ai/src/embedder.rs`; corrected 2026-09-18, the previous wording said each index held its own model). Still open: optional pre-warm at startup (the first embed pays the model load), a larger default model option, and the two measured defects listed under *The zero-token direction* — ingest throughput ([#938](https://github.com/xerj-org/xerj/issues/938)) and the multi-passage exact scan ([#939](https://github.com/xerj-org/xerj/issues/939)).
- **Log-analytics data path** — the dedicated `xerj-logs` columnar module is still not invoked from non-test engine/server code; log-shaped analytics run through ZBS2 + the generic aggregation suite. Wire it or remove it. The plan for wiring it is *a block index mode for logs* under *The zero-token direction* above.
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
