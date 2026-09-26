# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-09-26 (against `v1.0.0-rc.77` and `main`). Statuses trace to issues, merged PRs, the CHANGELOG, and the conformance suite; items carried forward from the 2026-07-12 review without fresh live verification are marked as such. This review line is machine-checked: `docs_capability_lists` fails the build if a release is cut without re-reviewing this file (issue #298). This pass was a desk review of post-rc.77 `main` and the live tracker state, not a live re-verification: it rolled the *Next release* section and the open-defects shortlist forward — several entries from the 2026-09-21 review were stale within hours of that review (#950, #1015, #941, #874 and the stage-1 gating trio #937–#939 all closed the same evening; the fixes are recorded where the stale entries stood) — closed the CHANGELOG-gap GA item below (the rc.19–rc.70 backfill, PR [#1035](https://github.com/xerj-org/xerj/pull/1035)), marked the stage-2 object-storage item done ([#965](https://github.com/xerj-org/xerj/issues/965) wired in rc.77), and corrected the mail-ingest memory line to the post-[#1002](https://github.com/xerj-org/xerj/pull/1002) reality. The *Shipping today* claims were last live-verified against rc.76 (unchanged by this pass), and *The zero-token direction* below was verified separately on 2026-09-18, against `main` @ `4d8dadbf`.

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
- **The System One wire, answered locally** (since rc.76) — `POST /v1/systemone` (native REST) and `POST /_decide` (ES-compat): typed judgement questions (`noul` / `choice`) answered by a rank-weighted kNN vote over a labelled-history index — no model, no provider key, no egress ([benchmarks/decisions-as-retrieval](./benchmarks/decisions-as-retrieval): Banking77 0.933 accuracy / ECE 0.012, SMS spam 0.983). The acceptance gate is the unmodified pip `jev-reranker` ranked off a XERJ node ([benchmarks/systemone-gate](./benchmarks/systemone-gate)). It is **not** a judge model and claims no zero-shot judgement: where there is no history, use the `rerank` stage and its provider.
- **Columnar storage** — the ZBS2 columnar block with 9 domain-aware encodings, ZSTD/LZ4 codecs, and SQ8 vector quantization, wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, **executed** index-lifecycle policies (ISM-modeled, `_ilm/*` + `_plugins/_ism/*`, since rc.15), `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` — all live-verified.
- **A single native binary**, statically linked, no JVM, sub-second cold start.

The release-by-release record of how all of this landed is [CHANGELOG.md](./CHANGELOG.md) — this file no longer duplicates it. The record is now complete rc.1 → rc.77: the rc.19–rc.70 gap was backfilled on 2026-09-26 from `git log` (merge subjects and commit bodies) and the GitHub release records, with a provenance banner in the file — the reconstructed entries are drier than ship-time ones, and every issue/PR link they carry was checked to sit inside its release window (PR [#1035](https://github.com/xerj-org/xerj/pull/1035)).

## Next release — [v1.0.0-rc.78](https://github.com/xerj-org/xerj/milestones)

**rc.77 was cut on 2026-09-21** — its full contents are the
[CHANGELOG.md](./CHANGELOG.md) entry, not this file. It is **the stateless-index and
reader-fairness release**: `storage.backend = "s3"` works for real (#965 — one immutable
ZBM1 bundle per segment instead of 104 PUTs, `snapshot.json` as the publication point,
merges publish before retiring inputs, a fresh node adopts the bucket), and readers stop
starving under sustained ingest (#1013 — the seqlock publish bracket no longer spans the
flush build; the evenness check that catches a straddling capture is part of what
shipped). The same release carries the systemone vote-text fixes (#1000/#1001: the vote
text is exactly what the question points at, never the instruction prose — a measured
32-point accuracy swing from wording alone), the console-tour pill race (#1011), and CI
repairs (the reference-coding toolkit's tests now actually run).

**On `main` since rc.77, riding to rc.78:** the flush bracket landed — flush
drains freeze and the publication bracket wraps only the publish, closing
[#1015](https://github.com/xerj-org/xerj/issues/1015) (PR
[#1018](https://github.com/xerj-org/xerj/pull/1018)); the ingest-RSS
investigation closed with fixes, not just measurements — id-position maps come
from the `__id` projection with the stored reassembly streamed
([#950](https://github.com/xerj-org/xerj/issues/950), PR
[#1017](https://github.com/xerj-org/xerj/pull/1017), plus
`POST /{index}/_cache/clear` in PR
[#1009](https://github.com/xerj-org/xerj/pull/1009)); the by-query truncation
class is closed — `_delete_by_query` pages the whole match set ids-only
([#1019](https://github.com/xerj-org/xerj/issues/1019), PR
[#1021](https://github.com/xerj-org/xerj/pull/1021)) and `_update_by_query`
pages the match set with exact sig-text/enrich counts
([#1022](https://github.com/xerj-org/xerj/issues/1022), PR
[#1023](https://github.com/xerj-org/xerj/pull/1023)); the idle per-index budget
is met with ~3× margin — the O(N) metrics gauge loop is deleted for
scrape-time refresh behind an at-rest fixture and CI gate (PR
[#1020](https://github.com/xerj-org/xerj/pull/1020), closing
[#874](https://github.com/xerj-org/xerj/issues/874)), and the request-cache
seen-set stopped allocating on first tracked search, idle 206 → 64 kB per index
([#1024](https://github.com/xerj-org/xerj/issues/1024), PRs
[#1025](https://github.com/xerj-org/xerj/pull/1025) and
[#1034](https://github.com/xerj-org/xerj/pull/1034) — the `idle-budget` gate
keeps CPU < 0.5 % of one core and ≤ 0.2 MB RSS per idle index);
[discussion #1012](https://github.com/xerj-org/xerj/discussions/1012)'s
email-labelling question has its measurement (PR
[#1026](https://github.com/xerj-org/xerj/pull/1026): local `/_decide` on a
synthetic four-label corpus — 1.000 accuracy on the templated tier, but 0.625
at 0.902 mean confidence on a hand-authored boundary-crossing tier, the
wrong-and-confident shape the discussion itself flags for the hosted path; no
`autoindex` wiring exists yet); and `llms.txt`'s memory claims were corrected
to the post-#1002 reality (PR
[#1033](https://github.com/xerj-org/xerj/pull/1033), closing
[#1028](https://github.com/xerj-org/xerj/issues/1028)).

**In flight for rc.78:** nothing on the board right now — the previous two
in-flight items (#1015, #950) both closed on 2026-09-21, hours after the last
review of this file, and their fixes are the "on `main` since rc.77" list
above. The zero-token stage-1 defects found while measuring
(#937/#938/#939) also closed with fixes in the same window — see the update
under *The zero-token direction*.

**Open defects carried into rc.78.**
[#1031](https://github.com/xerj-org/xerj/issues/1031) (a source file that grows
during an autoindex run aborts the run, is misclassified as a bulk/backend failure,
and the retry advice points at the wrong fix), and
[#1032](https://github.com/xerj-org/xerj/issues/1032) (three per-segment caches —
`stored_value_cache`, `dv_cache`, `id_pos_cache` — stay unbounded until a merge
retires their segment; the residual heap-per-doc retention after #1002) are the open
defects with code consequences.
Tracker [#298](https://github.com/xerj-org/xerj/issues/298) (this file's own review
cadence) stays open by design. Two former trackers closed after the 2026-09-21
review of this file: [#941](https://github.com/xerj-org/xerj/issues/941) (zero-token
direction) closed 2026-09-21 when stage 1 was fully delivered, and
[#874](https://github.com/xerj-org/xerj/issues/874) closed 2026-09-22 with its
budget *met* — ~3× margin, via PRs
[#1025](https://github.com/xerj-org/xerj/pull/1025)/[#1034](https://github.com/xerj-org/xerj/pull/1034)
(see above). Everything else the rc.77 window closed is recorded in the
CHANGELOG, not here.

## The road to [v1.0.0 GA](https://github.com/xerj-org/xerj/milestone/2)

The 1.0 bar: **every public claim verified against the release binary, and every input either honoured or refused loudly.** The gate list, each item an issue:

- **Close the accepted-and-ignored class** (the [#204](https://github.com/xerj-org/xerj/issues/204) umbrella closed once its members carried their own tracking; PR [#258](https://github.com/xerj-org/xerj/pull/258) carried one pass of the sweep and is merged). Known members still open: `nested` `inner_hits` unparsed; `random_sampler`'s ignored `probability`; `weighted_avg` returning HTTP 200 with an error buried in the aggregations body instead of a 400 (the 400 is part of #258). **Retired from this list:** `nested` `score_mode`, which was parsed-then-ignored until [#862](https://github.com/xerj-org/xerj/pull/862) made a nested query roll its matching children's scores into the parent per `score_mode` (rc.71).
- **Security hardening backlog** — cargo-audit and fuzzing landed in CI with rc.16 ([#207](https://github.com/xerj-org/xerj/issues/207) closed); the deferred TLS/auth/symlink hardening items from the Phase-2 security backlog remain.
- **The mixed read-under-write p99 gap** — the 4 benchmark losses out of 85 measured comparisons, all the same root cause (reads landing on the live memtable under writer pressure). Written up in [`demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md); the candidate fix is a visibility/parity-mode design decision, not a micro-optimisation, and it stays on the GA gate until fixed or explicitly descoped with the benchmark loss kept public.
- **Ship-or-descope every entry in *Known partials* below.** GA does not ship with a "partial" section that reads like a feature list.
- **Close the CHANGELOG gap — closed 2026-09-26.** rc.19–rc.70 shipped without entries; the 52 sections are now backfilled from `git log` and the release records, with an in-file provenance banner and every link checked to sit inside its release window (PR [#1035](https://github.com/xerj-org/xerj/pull/1035), riding to rc.78). A project whose pitch is verified numbers cannot ask users to reconstruct releases from `git log` — and now it does not.

## The zero-token direction

**Zero-token** is the name for work XERJ does locally, inside the engine, so that a person or an agent spends no model tokens *finding*, *judging*, *watching for* or *handing over* an answer. Search already works that way — no model runs to answer a query. This section is the rest of that idea, as product work (long form: [docs/ZERO_TOKEN_DIRECTION.md](./docs/ZERO_TOKEN_DIRECTION.md)), with each item's status checked against `main` on 2026-09-18 (`4d8dadbf`, 32 commits after `v1.0.0-rc.74`) by reading the code named beside it. "In flight" means a pushed branch exists and nothing of it is on `main`; "planned" means no code exists. Tracking issue: [#941](https://github.com/xerj-org/xerj/issues/941).

**Stage 1 — all of it merged and released in v1.0.0-rc.75** (each landed as its own PR; the status lines below name the release that carries each item):

- **Judged search — a rerank stage.** Retrieve with BM25 or `hybrid`, then have a second stage re-judge the top *N* before the page is returned. *On `main` since rc.75:* the opt-in `rerank` block on `_search`, inert until an operator configures a key; a LOCAL cross-encoder provider was measured and did NOT beat the shipped hybrid (SciFact 0.7021 hybrid vs 0.7186 local-base, interval spanning zero; NFCorpus 0.3445 vs 0.3597 local-small), so it stays opt-in and unmerged. *Before rc.75:* the ES `rescore` block (query rescorer) and the `hybrid` query type with `rrf|linear`; there is no rerank stage. *History:* branch `feat/rerank-stage` — a `rerank` block on `_search` that hands the top hits to an external relevance judge, opt-in per request and inert until an operator configures a key. It is the only *search-time* feature that would send document text off the machine — proxy embeddings (`[embedding] default_endpoint`) send it at write time and the WAL tap replays writes to an external `_bulk` endpoint, both operator-configured and off by default ([every outbound connection](./docs/RERANK.md#every-way-data-leaves-a-xerj-node)) — which is exactly why a *local* judge is on this list. *The bar for a local model is a measured one.* On the BEIR test splits with the opt-in neural embedder (`--embed-mode neural`, all-MiniLM-L6-v2, CPU) the shipped `hybrid` RRF query is already the best arm we have — nDCG@10 **0.699–0.704 on SciFact and 0.345 on NFCorpus** over three runs, against 0.657 / 0.302 for BM25 alone, 0.676 / 0.329 for vectors alone, and 0.686 / 0.332 for "BM25 top-30 re-ordered by the same bi-encoder", which is *worse* than hybrid on both. So re-ordering with the embedder we already ship is not worth building, and **a local cross-encoder ships only if it beats 0.699 / 0.345 on the same harness, by more than the run-to-run spread** ([`benchmarks/neural-path-triage/`](./benchmarks/neural-path-triage/)). Those figures are with the neural embedder; the default embedder is lexical feature hashing and was not part of this run. The hybrid figure was the only arm that did not reproduce — 0.6993, 0.7023 and 0.7044 on SciFact across three runs of unchanged indices — because tied RRF scores were ordered by a per-process hash seed; [#940](https://github.com/xerj-org/xerj/issues/940) closed that on 2026-09-21 (tied fused scores no longer ordered by HashMap iteration), and the spread above is the pre-fix measurement. The 0.699 / 0.345 bar stands until re-measured on the deterministic path — a gain inside a spread that no longer exists is still not a gain until you show it.
- **Share links and a guest reading room.** Hand one person a link to one corpus or one document, read-only, without creating them an account. *On `main` since rc.75:* `xerj share` with scoped, expiring links, a guest reading room, and a `--tunnel` mode that names Cloudflare as a reader of the traffic. *Before rc.75:* nothing — there was no share or guest code in the tree, and reading a corpus needs an account session or an API key. *History:* branches `feat/share-links` (scoped, expiring links; a share can never name a system index) and `feat/console-corpus-reader` (the corpus browser and the reader the link opens) carried the work before it landed; pre-merge, their commits were marked unverified by their authors.
- **Mail ingest.** *Shipped in rc.75:* `xerj autoindex` extracts `.eml` / MIME messages — headers, body, and attachments indexed as their own records ([#921](https://github.com/xerj-org/xerj/pull/921)) — and, since [#949](https://github.com/xerj-org/xerj/pull/949), streams `mbox` mailboxes and extracted Google Takeout exports (archives are never opened) and writes `replies_to` / `attachment_of` edges from the mail headers. Measured on a synthetic mailbox only; ingest memory remains the practical limit — [#948](https://github.com/xerj-org/xerj/issues/948)'s unbounded memtable retention is fixed by [#1002](https://github.com/xerj-org/xerj/pull/1002) (rc.77: variant-C retention ~677 → ~108 MB; the 300M run now fits its 8 GiB cap at 7,963.6 MiB, breaker engagements 178 → 2), and what remains is bounded per-segment cache retention until merge ([#1032](https://github.com/xerj-org/xerj/issues/1032)).
- **`autoindex` resilience.** *Fixed in rc.75* ([#934](https://github.com/xerj-org/xerj/pull/934)): one refused dataset no longer aborts the run, an over-size catalog is split, a throttling node is waited out for 600 s, and `--fresh` adopts old state. Before it: one dataset the server refused aborted the whole run ([#929](https://github.com/xerj-org/xerj/issues/929)), the `--no-graph` progress line is indistinguishable from a hang ([#931](https://github.com/xerj-org/xerj/issues/931)), and `xc-index.sh --fresh` fails on a previously indexed corpus ([#930](https://github.com/xerj-org/xerj/issues/930)). *History:* PR [#934](https://github.com/xerj-org/xerj/pull/934) (`fix/autoindex-resilience`) carried the fix. It is on this list because every later item assumes a folder can be indexed unattended.

**Found while measuring stage 1, and gating it** — each was a filed issue with a literal repro; **all three closed with fixes that landed before the rc.77 tag** (2026-09-21):

- A declared analyzer stopped applying at flush: `analysis.analyzer.default` was honoured by the memtable only, a per-field `analyzer` was accepted and ignored, and an unknown analyzer name was accepted ([#937](https://github.com/xerj-org/xerj/issues/937)). Fixed by PR [#991](https://github.com/xerj-org/xerj/pull/991) — a declared default analyzer is honoured at flush, segment query and merge; stemming can be turned on, and the accepted-and-ignored membership for the GA gate above is retired.
- Neural ingest kept ~3.4 of 32 hardware threads busy: **6.1 documents/s** on ~1,470-character abstracts through one `_bulk` stream, **29.5 documents/s** from the same node when eight clients send concurrently, for the same total CPU ([#938](https://github.com/xerj-org/xerj/issues/938)). Fixed by PR [#995](https://github.com/xerj-org/xerj/pull/995) — bounded window concurrency for neural `_bulk` embedding.
- A `semantic` query on an index holding any multi-passage document was an exact scan that deep-copied every stored document per query: **~410 ms p50 on 5,183 documents, of which the model's forward pass is ~14 ms**; the same query on a 10,003-document single-passage index is ~14 ms ([#939](https://github.com/xerj-org/xerj/issues/939)). Fixed by PR [#979](https://github.com/xerj-org/xerj/pull/979) — the exact scan ranks addresses and hydrates only the winners; `vector_column.rs`'s own header records the old cost as something it "used to" pay.

**Stage 2 — planned; the status lines say what exists today, which in most cases is less than the API surface suggests:**

- **Semantic detections.** A standing question over incoming documents: a cheap `percolate` prefilter selects candidates, a typed judgment decides (a small closed set of outcomes, not free text), and an alert carries a **calibrated probability** — calibrated meaning measured on held-out labelled data and published with its reliability curve, or not shipped. *Today:* the `percolate` query is real — stored queries are matched against a supplied document. There is **no alerting**: `PUT /_watcher/watch/{id}` stores the body in memory, answers `"condition":{"met":true}`, and no scheduler ever evaluates a watch (`es_compat.rs`, `put_watch`); the console's `.xerj_alert_rules` and `.xerj_alert_fires` indices have schemas and are created at bootstrap, and no evaluator reads them (`xerj-console-api/src/indices.rs`). Before anything new is built, `_watcher` must either run watches or refuse them — it is an accepted-and-ignored surface today.
- **A real object-storage backend.** *Done in rc.77:* [#965](https://github.com/xerj-org/xerj/issues/965) wired the segment path to object storage — `storage.backend = "s3"` packs each segment family into one immutable ZBM1 bundle object with `snapshot.json` as the publication point, merges publish before retiring inputs, and a fresh node adopts the bucket (see *Shipping today* for the full statement). The client (`s3.rs`, real S3-compatible: Cloudflare R2, MinIO, AWS S3), the read-through segment cache, per-request cost accounting by billing class and the stop-don't-warn budget had landed in rc.75. Separately, `xerj autoindex s3://bucket/prefix` reads documents OUT of a bucket, which is a source, not a home.
- **A block index mode for logs.** An index mode for log-shaped data that stores rows in time-partitioned columnar blocks and skips whole blocks by their min/max metadata, instead of building a per-term inverted index over every field. *Today:* the `xerj-logs` crate implements that design (columnar encoding, log-template extraction, time-range queries with block skipping, retention), is compiled into `xerj-engine` and `xerj-server` as a dependency, and is **called from no non-test code**. Log-shaped analytics run through the general columnar segment format and the aggregation suite. Wire it behind an explicit index setting and measure it against the general path on the same data, or remove it — the *Log-analytics data path* theme below is this same item.
- **User-code ingest plugins.** *Today:* the ingest pipeline's transforms are built-in native Rust plugins — rename, drop, add, JSON parse, timestamp parse, PII redaction, grok, route — and they do run on `_bulk`. The crate is named `xerj-wasm`, but **the wasmtime backend is not in the tree**: there is no `wasmtime` dependency and no `wasm` feature, only a note that one could be added behind the same trait. Planned: a sandboxed runtime for user-supplied transforms with fuel and memory limits and no ambient filesystem or network access. Until then the refusal rule holds: a pipeline naming a processor this build does not implement is stored as unrunnable and every ingest through it is refused, never quietly run as a shorter pipeline.
- **A corpus hub of signed, pre-indexed packs.** Download a reference corpus — a standard library, a specification set — already indexed, and mount it, instead of every user spending the same CPU-hours indexing the same public text. *Today:* nothing; no pack code exists — though the substrate now has a tracker: [#1030](https://github.com/xerj-org/xerj/issues/1030) covers the portable one-file bundle (indexed data that moves between machines), which is the format this item would distribute. The pack format is the design work: a **format version** readers refuse when they do not know it (the rule segment headers already follow), **per-file checksums**, a **detached signature** over the manifest, and **licence and provenance on every record** so a hit can say where its text came from and under what terms. Three risks decide whether this ships at all: **redistribution licence** (an index is a derived copy of its source text — a pack may carry only what its licence lets us redistribute, which excludes much of what people most want indexed); **pack safety** (a pack is untrusted input to the segment readers, so it needs the fuzzing the on-disk formats get, and a signature proves who built a pack, not that it is safe to open); and **format stability** (a published pack outlives the release that built it, so the pack format needs a compatibility promise the internal segment format has never had to make).

## Beyond 1.0 — themes

- **AST language expansion** — 25 further tree-sitter grammars, tiered by demand, one PR per
  tier. [#295](https://github.com/xerj-org/xerj/issues/295) delivered the expansion to 34
  languages and is closed; the remaining tiers have no tracking issue yet, so this theme is
  a plan rather than a commitment until one exists. Tier 1 (Kotlin, Swift, Scala, Dart, Lua, Perl, R, Julia, Haskell, Elixir) may land earlier in an RC if the grammar/ABI checks prove out.
- **Distributed clustering maturity** — embedded Raft handles cluster metadata today, but the default run is **single-node**; multi-node sharding/replication hardening is a post-GA track, and XERJ does not claim multi-node production readiness until it is measured.
- **Neural embedder ergonomics** — one loaded model is already shared by every index with the same embedder configuration (each index holds its own `NeuralHandle`, and identical configurations resolve to one lazily loaded model — `shared_neural_cell` in `xerj-ai/src/embedder.rs`; corrected 2026-09-18, the previous wording said each index held its own model). Still open: optional pre-warm at startup (the first embed pays the model load) and a larger default model option. The two measured defects this theme used to carry — ingest throughput ([#938](https://github.com/xerj-org/xerj/issues/938)) and the multi-passage exact scan ([#939](https://github.com/xerj-org/xerj/issues/939)) — closed with fixes (PRs [#995](https://github.com/xerj-org/xerj/pull/995) and [#979](https://github.com/xerj-org/xerj/pull/979)) before the rc.77 tag.
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
