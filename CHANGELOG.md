# Changelog

All notable changes to XERJ are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Performance

- **Merge copies posting lists instead of re-analysing every document it
  merges** ([#876](https://github.com/xerj-org/xerj/issues/876)). A merged
  segment's postings ARE its inputs' postings with the document ids remapped —
  nothing about them depends on the source text — but the merge rebuilt each
  side-car by walking back to every surviving document's stored `_source`,
  re-extracting its field values and re-running the whole analyzer chain.
  Under size-tiered levelling that re-analyses every document once per level,
  which is why a post-index merge tail could rival the index itself. The merge
  now streams each input's term dictionary, replays the decoded posting lists
  with the doc ids remapped and tombstoned documents skipped, and carries the
  norms and field statistics across.

  Measured on a 100 MiB text corpus force-merged to one segment (2 000
  documents, 32-core box, median of four runs per arm): the merged segment's
  FTS side-car build drops from **3.15 s to 0.85 s (~3.7x)**, and peak RSS
  over the whole force-merge from ~2.8 GiB to ~2.4 GiB. End-to-end force-merge
  time moves less, ~41 s to ~32 s, and is noisy — because after this change the
  merge is dominated by the doc-values column build (7-12 s per batch against
  0.85 s for FTS), which this change does not touch.

  Everything a query can read out of a merged segment is identical to what
  the old path wrote — every posting list, `doc_freq`, norm byte, `_score`
  and `field_length` — and the `.fst` and `.post` side-cars are byte-for-byte
  identical. Two on-disk representations differ without any query being able
  to tell:

  - The `.norms` array may be SHORTER. Byte 0 spells both "field absent" and
    "field length <= 1", and the reader drops byte-0 entries, so a trailing
    run of such documents cannot be replayed and the dense array stops at the
    last non-zero document instead of at the last document carrying the
    field. A single-token `keyword` field is entirely byte 0, so its `.norms`
    shrinks to its header. Both files load to the same norms table.
  - `.meta`'s `total_term_frequency` for a docs-only (`keyword`) field: a
    document that repeats the same value merges with
    `total_term_frequency = doc_frequency`, because that format never stored
    a per-document frequency and its reader synthesises 1. Nothing outside
    `xerj-fts` reads `total_term_frequency`; `.meta` is otherwise identical,
    `total_field_length` included, so `avgdl` still counts the repeat.

  Two further edges exist only when a merge also DROPS documents: a dropped
  document whose value analysed to zero tokens leaves no trace to reclaim its
  `total_docs` seat, and a dropped docs-only document's length is recovered
  from the quantised norm byte, exact to length 7.

  This also changes one behaviour worth stating plainly: because postings are
  now preserved as written, a merge no longer re-analyses old segments under a
  changed mapping. Editing an analyzer and running `_forcemerge` used to
  rewrite the old documents' terms as a side effect; it no longer does — that
  needs a reindex. A change to a field's POSITION setting is still detected
  and still falls back to the rebuild; an analyzer-only change is not
  detectable and is not re-applied. This matches Lucene's semantics.

  A merge falls back to the old rebuild — same output, old cost — when the
  replay cannot be proved equivalent: an input whose side-cars will not open
  or enumerate, an input carrying documents but no FTS data, or a field whose
  stored position setting disagrees with what the merge would write.
  `Index::merge_reanalysed_document_count()` reports how many documents took
  that fallback, `XERJ_PROF=1` prints a per-batch `merge-batch` phase
  breakdown, and `XERJ_MERGE_FTS_REANALYZE=1` forces every merge back onto the
  old path.

- **`--embed-mode neural`: a long passage no longer pads a whole batch up to
  its own length** ([#366](https://github.com/xerj-org/xerj/issues/366)). The
  built-in Candle encoder tokenized every call with `PaddingStrategy::
  BatchLongest` and ran one rectangular forward pass over it, so a window that
  held one 512-character chunk beside sixty short lines cost `61 × long`
  instead of `long + 60 × short` — the model did the work, the attention
  tensors were allocated, and the padding contributed nothing. The encoder now
  sorts a call's passages by token length and cuts batches at 64 rows or a 4096
  `rows × padded_sequence_length` budget (`xerj-ai/src/microbatch.rs`, shared
  with the experimental ONNX backend), pads each batch to its own longest row,
  and restores input order. This affects the opt-in neural backend only; the
  default embedder is unchanged lexical feature-hashing.

  Measured on this repo's 32-core x86_64 box with MiniLM-L6 (median of three
  interleaved runs each, box shared with other builds — the spread is wide, so
  the medians are quoted, not the best runs):

  | shape | before | after |
  |---|---|---|
  | one 210 KB source file (504 chunks, one call) | 46.2–57.8 s, 8.97 GB peak RSS | 19.9 s, 0.42 GB peak RSS |
  | 45 files of `apache/lucene`, chunked as ingest chunks them (1214 passages) | 8.9 passages/s | 12.1 passages/s |
  | synthetic window of 4 long chunks + 60 short lines | 40.2 passages/s | 145.7 passages/s |
  | uniform short documents, window of 64 | 171.0 passages/s | 175.6 passages/s |
  | one passage per call | 68.9 passages/s | 72.8 passages/s |

  Padding waste (`rows × padded_sequence_length` ÷ real tokens) on the Lucene
  corpus fell from 1.50 to 1.13, and on the synthetic mixed window from 2.66 to
  1.00.

  The first row is the other half of #366 — "each of those two files held the
  run for 30+ seconds" on 200 KB C++ headers. `semantic_embedding_window_end`
  always admits a whole document even past the scheduling window, so every
  chunk of a large field reached the model as one forward pass whose attention
  tensors scaled with the document: 8.97 GB of resident memory for a single
  210 KB file, which is an OOM on a laptop rather than a slowdown. The row cap
  bounds it.

  **What this does not fix, stated plainly:** #366 reported ~15 documents/s on
  short uniform strings and read it as per-document inference. That reading
  does not hold — HTTP `_bulk` already batches (`bulk.rs` collects the whole
  request, `index.rs` windows 64 passages across documents), and the last two
  rows above show that shape is unchanged by this work. On uniform short
  documents ~15 docs/s is the cost of CPU transformer inference, and moving it
  needs quantization, a GPU provider or a smaller model. What was genuinely
  broken was mixed-length work, which is what pointing `autoindex` at a real
  folder produces. Binary-protocol `_bulk` embedded one document per inference
  call when this was written (`xerj-api/src/binary_protocol.rs`); that half is
  the entry below.

- **Binary-protocol `_bulk` shares one embedding pass across the batch instead
  of one inference call per document**
  ([#903](https://github.com/xerj-org/xerj/issues/903), split out of #366).
  `handle_bulk` looped `index_document` per document, and `index_document`
  embeds its own `semantic_text` fields — so a 64-document bulk cost 64
  inference calls of one passage each. It now collects the batch and hands it
  to `Index::index_documents_batched`, which routes it through the same
  `apply_semantic_embeddings_batch` entry the HTTP `_bulk` path uses: passages
  are windowed at `embedding.onnx_scheduling_window` (default 64) across
  documents, publication stays per document, and one document's failure still
  fails only that document. There is no second batching rule.

  Measured end to end over a real TCP connection through the handler with the
  harness added here (`cargo run --release -p xerj-api --features neural
  --example binary_bulk_throughput -- <documents> <per-bulk>`), on this repo's
  32-core x86_64 box with `--embed-mode neural` (built-in Candle backend,
  `sentence-transformers/all-MiniLM-L6-v2`). Corpus: 1 000 lines of
  `apache/lucene`'s `lucene/CHANGES.txt`, 40–400 bytes each so every document
  is exactly one passage. Medians of interleaved runs; the box was shared with
  other builds, so the spread is wide and the medians are quoted, not the best
  runs:

  | documents per bulk | before | after |
  |---|---|---|
  | 16 (n=3) | 60.7 docs/s | 82.6 docs/s |
  | 64 (n=5) | 64.0 docs/s | 102.7 docs/s |
  | 256 (n=3) | 84.3 docs/s | 124.9 docs/s |

  **Bounds, stated plainly.** The win is a short-document win. A document long
  enough to fill a scheduling window on its own already got its own inference
  call: `semantic_embedding_window_end` admits a whole document even past the
  window (`index.rs`), so before and after are the same call for it. On the
  default lexical feature-hashing embedder there is no inference to batch and
  no change was measured — 872 → 865 docs/s (medians of n=7 interleaved,
  ranges fully overlapping). And the module this fixes is a library entry
  point: `serve_binary_protocol` is not bound by any listener in `xerj-server`
  (`xerj-engine/src/index_guard.rs` says so, and nothing in the tree calls
  it), so no shipped-server ingest path changes speed with this release.

### Fixed

- **`knn` beside a `query` is answered from the inverted index again — and its
  page no longer drops the documents matched by both halves**
  ([#892](https://github.com/xerj-org/xerj/issues/892)). This closes the
  regression rc.72 shipped knowingly: the
  [#825](https://github.com/xerj-org/xerj/issues/825) fix pre-executes the
  vector leg and pins its top-`k` into the query tree as `constant_score{ids}`
  clauses, `_id` is not in the FTS term dictionary, so the projection declined
  the whole tree and every segment fell to a stored-document scan.

  That route was not only slow. The scan admits documents in stored-layout
  order and stops materialising once it reaches the page cap — no score
  comparison — so on any index with more lexical matches than that cap, the
  documents reached by BOTH halves, the ones ES scores `query_score +
  knn_score` and ranks first, never entered the page. `hits.total` was right;
  the page was not. Live-verified on 100 000 documents: for a `match` + `knn`
  k=10 request whose vector top-10 held exactly two lexical matches, rc.72's
  top 20 was twenty lexical-only documents tied at 1.693 and neither
  both-halves document appeared at all. They now rank 1 and 2, at 3.153 and
  3.144.

  The projection now lifts the pinned disjunct to an FTS leaf carrying
  `(doc_id, score)` pairs, resolved per segment through the same `_id` →
  stored-position index that `ids` queries and `_mget` already use. The
  lexical half keeps the inverted index and exact BM25, the vector half keeps
  its own scores, and the `bool` sums them exactly as before.

  Measured on that corpus (`text` + 8-dim `dense_vector`, ~10 % lexical
  selectivity, bulk-loaded once and never updated — which is the only state in
  which this route engages, see the scope note below — closed-loop, fresh query
  vector per request so the query cache cannot answer twice, medians of 2
  rounds × 7 requests, on a shared build machine — the two control rows bound
  the noise):

  | request | rc.72 | with this fix |
  |---|---|---|
  | `query` alone (control) | 0.45 ms | 0.52 ms |
  | `knn` alone, k=10 (control) | 1.99 ms | 2.12 ms |
  | `query` + `knn` k=10 | 265.9 ms | **6.4 ms** |
  | `query` + `knn` k=100 | 326.9 ms | **8.4 ms** |
  | `query` + `knn` k=1000 | 505.0 ms | **27.5 ms** |
  | `query` + `knn` + `aggs` k=10 | 784.1 ms | **534.4 ms** |

  `hits.total`, the aggregation buckets and the field-sorted page came back
  identical across the two arms — one request shape per column, compared as
  JSON — and so did `query`-alone and `knn`-alone. The agg row keeps most of
  its cost because a `terms` agg over this shape still materialises the whole
  corpus — a different path, untouched here.

  Two things to know:

  - **`_score` values change for this shape.** The lexical half is now scored
    by exact BM25 — the same number the identical query returns *without* a
    `knn` beside it — instead of the stored scan's heuristic. Ordering follows
    the #825 contract as before; absolute values move, so a `min_score` tuned
    against rc.72's hybrid needs revisiting.
  - **The indexed route applies only to an index that has never taken an
    overwrite or a delete** — that is the scope of every number above, and it
    is a one-way door, not a transient window. The gate is the one the `ids`
    pre-filter already applies: the per-segment `_id` index keeps one position
    per id, so a segment physically holding a document twice (an overwrite
    flushed alongside its predecessor) could hand back the superseded copy. It
    reads `VersionMap::ghost_events`, which is **monotonic by design** — it
    counts overwrite and delete events for the life of the open index and is
    never decremented, not even by the merge that purges the superseded copies.
    One `PUT` over an existing `_id`, or one `DELETE`, therefore turns this
    route off for that index until it is re-opened, and it stays off across
    that re-open while superseded copies remain on disk. An index that takes
    updates or deletes keeps rc.72's stored scan for `knn` beside `query` —
    still correct, and no slower than it was, but with none of the speedups
    above. Append-only indexes — bulk load, reindex-and-swap, log and event
    corpora — get them. Widening the route to ghost-bearing indexes is
    separate work: it has to prove the #825 union/sum contract with tombstones
    present, and is not attempted here.

- **`xerj autoindex --no-graph` no longer aborts with `Access denied
  (os error 5)` on Windows**
  ([#482](https://github.com/xerj-org/xerj/issues/482)). The durable-snapshot
  path fsynced the directories it had just published with
  `File::open(dir)?.sync_all()?`. That is a Unix-only idiom: on Windows
  `File::open` on a *directory* returns `ERROR_ACCESS_DENIED` (os error 5) on
  every call, because `std` cannot pass `FILE_FLAG_BACKUP_SEMANTICS`. So the
  first such call — sealing the source snapshot, immediately after the journal
  was written and before a single document was indexed — killed the run with a
  context-free, locale-dependent io error (`Acceso denegado. (os error 5)` in
  the report). A second, unconditional open of the same kind in snapshot GC
  then failed *every later run* over that state directory, whether or not
  `--no-graph` was passed again. `xerj-autoindex` now routes directory
  durability through `xerj_common::fsio::fsync_dir`, which has carried the
  documented `#[cfg(windows)]` shim since the same mistake stopped the server
  creating any index at boot; the surrounding filesystem calls also carry
  context now, so a future failure names the operation instead of only its
  errno. This costs Windows no durability it had: the code it replaces flushed
  nothing there — it returned `Err` and ended the run. Windows keeps the
  engine-wide posture that directory-namespace changes carry no durability
  claim; file contents inside a snapshot are still fsynced. The
  windows-latest `autoindex-fd-smoke` CI job now runs the reported
  `--no-graph` flow twice over one state directory, which is what would have
  caught this: the job existed, but only ever ran the default graph path,
  which never touches `sync-snapshots/`.

- **A code symbol's retrievable unit is the whole declaration, not the single
  physical line the name sits on**
  ([#500](https://github.com/xerj-org/xerj/issues/500)). Promoting each
  declaration to its own document
  ([#579](https://github.com/xerj-org/xerj/pull/579)) closed the
  constant-and-field half of that issue. The stored `code` was still literally
  `lines[name_row]`, so anything that did not fit on that one line came back
  wrong: a wrapped signature returned `public void configure(`; a declaration
  whose modifiers sit above the name (`static unsigned long\nhash_bytes(…)`,
  `#[inline]\npub fn scan(…)`) returned the wrong line entirely; and a
  declaration sharing its line with the body (`void m(){ int local = 1; }`)
  returned implementation nobody asked for. `code` is now sliced from the
  declaration's first token — leading attributes included — to where its body
  begins, so the unit is a complete signature rather than a fragment of one.

  Measured with the `decl_bench` harness added here (`XERJ_DECL_BENCH=<repo>
  cargo test --release -p xerj-autoindex --lib decl_bench -- --ignored
  --nocapture`), which computes the old unit and the new one side by side in one
  pass over the same parse and the same symbol set:

  | corpus | slice ending mid-signature, was → now | median bytes, was → now | the symbol's whole `line..end_line` span |
  |---|---|---|---|
  | Lucene — 5 627 Java files, 79 102 declarations | 4 137 (5.2 %) → 18 (0.02 %) | 49 → 52 (mean 50.5 → 60.7) | median 229, mean 1 046 |
  | valkey + memcached — 981 C files, 28 347 | 1 444 (5.1 %) → 54 (0.2 %) | 48 → 49 (mean 51.6 → 58.8) | median 123, mean 454 |
  | tantivy — 434 Rust files, 8 970 | 851 (9.5 %) → 27 (0.3 %) | 41 → 50 (mean 44.2 → 60.9) | median 218, mean 721 |
  | cilium + dpdk + vpp-agent + xdp-tools — 26 357 Go/C files, 914 561 | 87 267 (9.5 %) → 1 678 (0.2 %) | 49 → 53 (mean 54.6 → 67.4) | median 102, mean 387 |

  So the unit costs 7–17 bytes more on average and is a complete declaration
  instead of a fragment — still 1.9–4.4× smaller than that symbol's own span at
  the median and 5.7–17× smaller at the mean. 39.6 % of Lucene declarations wrap
  past one line (the single-line slice could only ever return a fragment of
  those) and 51.5 % have a name line that does not stop where the declaration
  does.

  Extraction is not free: medians of five interleaved runs over the same
  6 106-file Lucene tree, one box, `extract_bench` — 1 600 files/s before this
  change, 1 455 after. About 9 %, for an ancestor climb bounded at 12 levels and
  a pruned body search bounded at 4 096 nodes.

  Two honest limits. The cap is unchanged at 400 characters and a declaration
  that overruns it still falls back to the start line, so the WORST case per
  symbol is what it always was — but an individual document can grow, since a
  45-character name line may now store a 400-character signature. And "stops
  where the body begins" needs the grammar to say where that is: for a Haskell
  equation nothing marks it, so the slice is the whole equation. Corpus-wide
  that is 1.2–5.9 % of symbols whose slice covers their entire span, which
  `decl_bench` now reports as its own column rather than leaving to assertion.

  The file document's `symbols` sidecar gains `end_line`, so a caller that
  wants the implementation can address exactly `line`..`end_line` instead of
  guessing "up to where the next symbol starts" — the guess that returns a
  whole class for a class and the rest of the file for the last symbol.

  Upgrading does not rewrite what is already indexed: `xerj sync` reconciles by
  content digest, so an unchanged file produces no operation and its symbol
  documents keep the old single-line `code` and carry no `end_line`. An index
  upgraded to this release is a mixture until the files change or it is rebuilt
  from scratch.

- **The exclusion sweep can no longer delete a sibling corpus's catalog
  documents on a catalog an older build left `text`-mapped**
  ([#890](https://github.com/xerj-org/xerj/issues/890)). `autoindex-catalog` is
  shared by every corpus on the node, so when a widened ignore rule excludes a
  file, the sweep that removes its already-published documents is scoped to the
  corpus's `prefix`. On any catalog a v1.0.0-rc.15..rc.67 build wrote to before
  the mapping declared that field — which includes catalogs first created by an
  earlier release — it is dynamically inferred **`text`**, and a `term` query
  against an analyzed field matches the field's *tokens*: `prefix: "ax"` also
  reached documents whose prefix is `ax-2` (tokens `[ax, 2]`). Conjoined with
  the `path`/`file_key` a
  byte-identical file (a LICENSE, a lockfile) shares between corpora, the sweep
  deleted the **sibling corpus's live catalog documents** — the cross-corpus
  over-delete the scope was added in
  [#737](https://github.com/xerj-org/xerj/issues/737) to prevent, and a delete
  is not recoverable. Present since v1.0.0-rc.68.

  Both scoped catalog deletes now go through an exact scoped delete: the same
  query still selects candidates server-side, but every hit's raw scope values
  are re-checked against `_source` before it is deleted by `_id`. That is
  independent of the catalog's mapping, so it holds on a keyword catalog and a
  legacy text one alike, and it covers the `path`/`file_key` conjunct as well as
  the corpus scope. Coverage of the excluding corpus's own documents is
  unchanged, including alias documents the frozen plan does not name, which only
  this pass can reach. The candidate walk is bounded at ten pages of 1,000, and
  it ends on an exact `hits.total` as well as on a short page, so the boundary
  case of exactly 10,000 candidates completes rather than refusing. Past that
  bound it fails *closed* — it refuses with a `_reindex` remedy rather than
  reporting a removal it only partly made.

  Not fixed here: on a legacy catalog an analyzed field can also match *fewer*
  documents than it should, so a scoped sweep can still miss a document an older
  build wrote. That is the under-match
  [#755](https://github.com/xerj-org/xerj/issues/755) answers with the keyword
  `corpus_scope` field, and the run already warns with the `_reindex` that
  retires the legacy mapping.

- **CI's default-parallelism test step no longer races itself for a port**
  ([#751](https://github.com/xerj-org/xerj/issues/751), the second half). With
  the deadlock above fixed, the same step then failed on a real port race that
  the hang had been masking: `xerj-server`'s cleartext-bind tests allocated a
  port by binding `127.0.0.1:0`, reading the number, releasing it, and handing
  it to a child process that binds it later. The kernel is free to hand that
  same ephemeral port to the next `bind(":0")`, and several tests in that file
  keep a real server alive for seconds, so at default parallelism they
  overlapped — `gRPC: bind [::1]:46843 ... Address already in use`. Ports now
  come from a band below every platform's ephemeral range, strided per process
  and probe-bound on both `127.0.0.1` and `::1` (a port free on one family can
  be taken on the other, which is exactly how it failed). Test-only change.

- **A search that aggregates over a cold segment could deadlock forever**
  ([#751](https://github.com/xerj-org/xerj/issues/751)). On a multi-thread
  runtime `Index::search` runs its whole body inside
  `block_in_place(|| Handle::current().block_on(..))`: that hands the worker's
  core to a tokio *blocking-pool* thread — which then runs the scheduler loop
  for the life of the runtime — and parks the calling thread, itself a
  blocking-pool thread, for the entire search. An aggregation with no columnar
  fast path assembles the full corpus from inside that park, and the cold
  segment hydration it needs (`stored_values_for_async`) queued its decode with
  `spawn_blocking` — onto the very pool the waiter was holding a thread of.
  tokio grows that pool only when it observes zero idle threads at push time,
  so the core hand-off and the decode submission could both see the same idle
  thread, both merely notify, and the one thread that woke took the core and
  never returned. The decode was then queued behind threads that were all
  permanently running worker cores: the search waited for a task that could
  never be scheduled. The decode now runs on the engine's own rayon maintenance
  pool, whose threads are never consumed by tokio, so the wait cannot be
  circular. This is what made CI's default-parallelism workspace-test step hang
  on hosted runners and leave `main` with no verdict for hours. Measured on a
  4-cpu affinity mask with the reported test binary: 12 hangs in 60 runs before,
  0 in 200 after (and 0 in 100 on a 2-cpu mask). One consequence worth knowing:
  a cold hydration a search is waiting on now runs at the maintenance pool's
  `nice(10)` instead of a `nice(0)` blocking thread, which is only observable
  when every core is already saturated.

- **CI: every workflow job is bounded, so one hang can no longer cost `main` an
  afternoon of CI** ([#770](https://github.com/xerj-org/xerj/issues/770)). 22 of
  this repo's 23 GitHub Actions jobs declared no `timeout-minutes` and so ran
  against GitHub's 360-minute default. Three workflows serialise on a
  concurrency group that never auto-cancels (`ci-CI-refs/heads/main`,
  `pages-deploy`, `release-metrics`), so a single hung job holds that slot for
  six hours while every later run queues behind it. That is measured, not
  hypothetical. On 2026-08-25, before #767 capped `build-test`, the #751 hang
  ran that job into the 360-minute default and was killed by it (run
  32796557309, 01:21:28 → 07:21:44 — every other job in the same run finished
  within 18 minutes). A second hang the same morning held the slot from 11:28
  to 17:03, and the seven main pushes that landed behind it were all discarded
  with **zero jobs run**; a normal ~37-minute run would have let at least four
  of them through, since the gaps between them were 48, 62, 38 and 150 minutes.
  Every job now carries a cap sized in tiers from the measured duration of every
  job execution across the last ~245 CI runs (3 873 executions) and 27 release
  runs: 15 min for jobs that install no toolchain (measured max 2.3 min), 30 min
  for jobs that install a toolchain but compile no workspace member (max
  3.8 min), 60 min for jobs that compile workspace crates (max 48.3 min,
  `Build + Test`), and 75 min for the two jobs with the heaviest tails —
  `autoindex-fd-smoke`, whose `windows-latest` leg runs 18.8 min at the median
  but 49.6 at the maximum, and `release.yml`'s cross-compile matrix (max
  38.3 min, `aarch64-pc-windows-msvc`). Worst-case slot starvation drops from
  360 minutes to 75 (CI on `main`), 20 (`pages-deploy`) and 75 (a release tag).
  A new `.github/scripts/workflow-timeout-guard.py` keeps it true for jobs added
  later. This bounds the blast radius of the next hang; it is not itself a fix
  for any hang, and #899 remains what fixed #751's.

## [1.0.0-rc.72] - 2026-08-31

The idle-cost release. A 2026-08-29 measurement session found XERJ was not
quiet at rest: on a node holding 464 small indices — the shape `xerj autoindex`
produces from a reference corpus — the process burned 15–27% of a core and 115
wakeups per second with zero requests, and a 154 MB corpus spent 40+ minutes at
~110% CPU merging after ingest "finished". Customers reported it as fans
spinning on an idle laptop. Three of the four mechanisms are fixed here; the
fourth ([#876](https://github.com/xerj-org/xerj/issues/876)) has a published
design and is not yet implemented. The budget every fix is held to, and the
remaining gap, are tracked in the open meta-issue
[#874](https://github.com/xerj-org/xerj/issues/874).

Alongside it, six Elasticsearch-compatibility and autoindex correctness fixes,
each shipped with a test proven to fail on the unfixed code.

### Performance

- **An idle index no longer costs a timer, a thread or a wakeup**
  ([#871](https://github.com/xerj-org/xerj/issues/871)). Every index ran its own
  `tokio` task that woke every 5 seconds forever and evaluated merge policy
  whether or not anything had been written — 464 indices meant 93 wakeups per
  second and two runtime workers each burning ~10% of a core, on a node serving
  nothing. Merge scheduling is now event-driven, the same shape the WAL fsync
  scheduler adopted in rc.16 for the identical defect
  ([#334](https://github.com/xerj-org/xerj/issues/334)): a segments-changed hook
  fires on every snapshot swap that alters the segment set (flush publication,
  merge application, tombstone-only segment, orphan recovery) and a debounced
  request arms at most one check. `XERJ_MERGE_INTERVAL_SECS` keeps its meaning
  as the debounce delay. Measured at the unit level with an evaluation counter:
  768 evaluations across a simulated idle hour become 48.

- **The resource sampler stopped walking every index × every shard lock**
  ([#872](https://github.com/xerj-org/xerj/issues/872)). The memory breaker's
  sampler summed memtable bytes every 100 ms by taking a read lock on all 16
  shards of every index — 74,240 lock acquisitions per second at 464 indices, in
  one thread, to compute a number that is almost always zero. The aggregate is
  now maintained incrementally by the write paths that already compute the byte
  delta, and the sampler is a single relaxed load. The breaker's
  starvation-immunity property is unchanged: it still runs on a dedicated OS
  thread, it just no longer does O(indices × shards) work to do it.

- **A multi-index search runs its per-index searches concurrently**
  ([#875](https://github.com/xerj-org/xerj/issues/875)). The fan-out awaited each
  index before starting the next, so an `ax-*` query cost the SUM of its
  per-index latencies instead of the maximum — the shape every reference-coding
  retrieval uses. Hit ordering, aggregation merge order and the global search
  permit that bounds concurrency are all unchanged.

  Two behaviour changes worth knowing: `queries_by_index` now increments for
  every resolvable index even when a later index errors the request (those
  searches genuinely run now), and per-index deadlines all start at request time
  rather than sequentially, so a search queued behind the permit pool can return
  `timed_out: true` with partial results where the serial loop returned complete
  results slowly.

### Fixed

- **A field holding both numbers and strings in one segment no longer loses the
  numeric documents from `term` / `terms`.** The doc-values builder collects a
  segment's values into a numeric map (numbers and booleans) and a keyword map
  (strings), then writes both into one column set — so a *type-mixed* field had
  its numeric column silently overwritten by the keyword one, leaving every
  number/boolean document filed as null in a column the query prefilter treats
  as exact. Those documents were then dropped from the result with no error.
  Such a field now ships **no** doc-values column at all, exactly as a
  multi-valued field already does, and every consumer falls back to the
  stored-source scan: correct, at scan cost rather than column speed. Reindex
  onto a single type to get the column back. Whether it bit you depended on
  core count — a many-core host scatters a flush across shards and each segment
  stays single-typed, so this reproduced only where documents of both kinds
  landed in one segment.

- **A field mapped `integer`, `long`, `float` or `boolean` now enforces that
  type on write** ([#781](https://github.com/xerj-org/xerj/issues/781)). The
  declared type was enforced for nothing: `1.9` into an `integer` field was
  stored as `1.9`, `9999999999` was kept exact, `"abc"` was stored as a
  string, `"yes"` went into a `boolean`, and — per the issue's follow-up — an
  entire nested object `{"bad":"x"}` was indexed into an `integer`. Every one
  of those answered `201 created`.

  The consequence was wrong hits, not just leniency. With `1.9` sitting in an
  `integer` field, `range {"i":{"gte":1.5}}` **matched** in XERJ and would not
  in ES (which indexes the truncated `1`), while `term {"i":1}` matched in ES
  and returned nothing here — the same query, over the same document, under
  the same mapping, giving different answers.

  Ingest now applies ES 8.x's own rules (`coerce` defaults to `true`): `1.9`
  → `1`, `"5"` → `5`, `"1.9"` → `1`, `"true"` → `true`; out-of-range for the
  declared width, an unparseable string, an object, an array element that is
  none of those, and anything but `true`/`false` in a `boolean` are refused
  with a 400 `document_parsing_exception` — a per-item 400 under `_bulk`. An
  explicit `"coerce": false` additionally refuses a decimal part and a string,
  and `"ignore_malformed": true` still wins: that field's bad values are
  dropped into `_ignored` rather than failing the document.

  **What this changes about stored data, and what it means on upgrade.** A
  coerced value is rewritten in `_source`: ES keeps `_source` verbatim and
  coerces only the indexed value, but XERJ indexes from the stored source, so
  matching ES's *hits* costs source fidelity here. A document written
  `{"i": 1.9, "b": "false"}` now reads back `{"i": 1, "b": false}`. An index
  that spans this upgrade therefore holds **both** spellings of the same
  logical value — documents written before hold `"false"` / `"5"`, documents
  written after hold `false` / `5` — and no reindex is performed for you.

  That mixed index still answers correctly, and closing that gap is part of
  this change. Because the canonicalisation moves a declared `boolean` from
  the string spelling to a real JSON boolean in `_source`, the **query** side
  now runs `term` / `terms` values on a `boolean` field through the same
  predicate — ES parses a query value with the same `Booleans` its field
  mapper uses, so `"true"` and `true` name one term. Without it,
  `terms {"b":["true"]}` found nothing on a doc-values-only boolean field
  while the equivalent `term` still matched. The `_source` scan comparator
  relates the two spellings in both directions (as it already did for a number
  and its string spelling), for single- and multi-valued fields alike, and a
  `terms` aggregation buckets them together — so one query sees the whole
  index rather than half of it. **Reindex** if you want a uniform `_source`;
  nothing breaks if you do not.

  Also narrowed deliberately, all documented in `xerj_common::field_coercion`:
  the index-level `index.mapping.coerce` setting is not read (only the
  field-level parameter), `scaled_float` is range-checked but not quantised by
  its `scaling_factor`, an empty string is left alone rather than dropped, and
  `_update` is not re-validated — it merges into a document already checked on
  write.

- **A top-level `knn` beside a `query` now scores as a sum over the union of
  both halves, aggregations included**
  ([#825](https://github.com/xerj-org/xerj/issues/825)). ES 8.x defines this
  shape as a disjunction: the hit set is the union of the lexical matches and
  the global top-`k` neighbours, each document scores `query_score +
  knn_score`, and aggregations are computed over that union. XERJ folded the
  request to `bool.should[query, knn]` but the generic matcher/scorer has no
  `knn` arm, so a vector-only document vanished and a lexical match lost its
  vector score — silently, with `_shards.failed: 0`. The
  [#458](https://github.com/xerj-org/xerj/issues/458) carve-out covered one
  narrow shape with RRF rank fusion (itself a scoring divergence from ES) and
  left every request carrying `aggs`, `sort`, `collapse` or `rescore` on the
  silent drop. The engine now pre-executes the `knn` leg and pins its top-`k`
  into the tree as per-document constant scores, so one path serves the union
  with summed scores and every request feature applies to it.

  Two behaviour changes worth knowing before you upgrade:

  - `{query, knn:{field: <not a vector field>}, aggs}` now answers **400**
    (`the field is not answerable by knn`), matching ES and the existing
    [#498](https://github.com/xerj-org/xerj/issues/498) rule. It previously
    answered 200 with lexical-only hits — the wrong answer, quietly.
  - **This shape is slower.** A pinned tree contains `ids` clauses, which the
    FTS projection cannot lift, so the request is answered by a stored-document
    scan instead of the inverted index. Measured on 100 000 documents (`text` +
    8-dim `dense_vector`, ~10 % lexical selectivity, closed-loop, fresh query
    vector per request): `query` + `knn` k=10 goes from 2.5 ms (the old RRF
    route, which returned ES-divergent scores) to ~208 ms; k=1000 to ~385 ms;
    k=10000 to ~5.1 s. The shapes that previously carried the silent drop —
    anything with `aggs`/`sort`/`collapse`/`rescore` — already took the scan
    and cost about a quarter more (476 ms → 605 ms with a terms agg),
    but they now return the right documents. Restoring an indexed route is
    tracked as [#892](https://github.com/xerj-org/xerj/issues/892);
    correctness landed first.

- **`match_phrase` `slop` now admits transposed terms at Lucene's cost of 2**
  ([#830](https://github.com/xerj-org/xerj/issues/830)). The sloppy-phrase
  walk was in-order only — each next term matched strictly after the
  previous one — so a reordered pair never matched at ANY slop:
  `{"match_phrase":{"t":{"query":"quick brown","slop":2}}}` returned zero
  hits on a document reading `brown quick`, where Lucene/ES match it at
  distance 2 (`SloppyPhraseMatcher`'s own javadoc example). The evaluator is
  now the Lucene move-distance semantics — pick one document position per
  phrase term; the distance is the span of the positions after subtracting
  each term's query offset — implemented once
  (`xerj_fts::search::phrase_positions_match`) and called by both the segment
  positional clause and the engine's memtable/stored-scan walk
  (`phrase_walk`), so slop is evaluated identically on either side of a
  flush. `match_phrase_prefix` and `multi_match` phrase/phrase_prefix go
  through the same evaluator and gain the same behavior. Documents that
  matched before still match — an in-order pick has strictly increasing
  positions, so its span telescopes to exactly the old summed-gaps value —
  and that is checked rather than assumed, by an exhaustive test over every
  document of length <= 5 and every phrase of length <= 3 from a 3-symbol
  alphabet at slop 0..3, comparing against both the old walk and a
  brute-force reference. A repeated phrase term still needs as many DISTINCT
  document positions as it has slots (`"a a"` does not match a doc holding
  one `a`).


- **A small index reaches a segment instead of living in memory forever**
  ([#873](https://github.com/xerj-org/xerj/issues/873)). `needs_flush` was
  threshold-only, so a dataset below the size thresholds never flushed: its
  documents stayed memtable-resident for the life of the process, pinned their
  WAL generations on disk, and replayed on every start. Measured on a real
  corpus: a 100,001-document dataset with zero segments 30+ minutes after
  ingest. New `storage.flush_idle_secs` (default 300 s, `0` disables) flushes a
  non-empty memtable whose contents have not changed for that long, detected by
  a probe on the existing periodic flusher so the write path pays nothing. This
  matches Elasticsearch's `indices.memory.shard_inactive_time` default.

- **`match_phrase` `slop` admits transposed terms at Lucene's cost of 2**
  ([#830](https://github.com/xerj-org/xerj/issues/830)). The sloppy-phrase walk
  was in-order only — each next term had to match strictly after the previous —
  so `"quick brown"~2` never matched `brown quick`, diverging from Lucene's
  `SloppyPhraseMatcher`. It is now a minimal-window sweep over adjusted
  positions, shared by the memtable and segment arms so the answer does not
  change at flush, and covering `match_phrase`, `match_phrase_prefix` and
  `multi_match` phrase modes.

- **A date field's epoch scale comes from its mapping, not from each value**
  ([#790](https://github.com/xerj-org/xerj/issues/790)). The scale was guessed
  per value — four or more fractional-second digits meant nanoseconds, coarser
  meant milliseconds — so one column could hold both scales six orders of
  magnitude apart. A sub-millisecond document matched a `range` or `term` before
  flush and vanished after it, because the millisecond bound bisected to nothing
  against a nanosecond sort key. Precision is now a property of the field
  (`date` is milliseconds, a declared `date_nanos` is nanoseconds), threaded
  through every sort and shadow-range site. No on-disk format change.

- **Numeric and boolean fields enforce their declared type**
  ([#781](https://github.com/xerj-org/xerj/issues/781)). A field mapped
  `integer` accepted `1.9` and kept it, accepted values outside the type's
  range, and accepted non-numeric strings. Writes now follow ES's `coerce`
  semantics: `1.9` into an integer type stores `1`, `"5"` stores `5`, and
  out-of-range or unparseable values are refused with a 400 instead of being
  stored. A field-level `"coerce": false` is honoured, and `ignore_malformed`
  keeps precedence.

  **Upgrade note:** coercion rewrites the value in `_source`, so an index that
  spans this upgrade can hold both spellings of the same logical value. `term`
  and `terms` on a boolean field now normalise their operand the way `term`
  already did, and the two spellings compare equal, so a query matches
  documents written on either side of the upgrade.

- **`knn` beside `query` scores as a sum over the union**
  ([#825](https://github.com/xerj-org/xerj/issues/825)). The ES-native hybrid
  body silently dropped the vector half whenever aggregations, sort, collapse,
  rescore or highlight were present: the request answered 200 with a purely
  lexical result set and no warning. The vector leg is now pre-executed and its
  top-k pinned into the query tree, so the hit set is the union, a document
  reached by both halves scores the sum, and every request feature applies.

  **Known regression, measured and not hidden:** on the hybrid shape that
  previously took the RRF route, this is substantially slower — the pinned
  clauses cannot project to the full-text index, so the query falls back to a
  stored-document scan (~208 ms vs ~2.5 ms on a 100k-document index at k=10).
  Correct-and-slow was chosen over fast-and-wrong;
  [#892](https://github.com/xerj-org/xerj/issues/892) carries the indexed-route
  design that removes it.

- **`autoindex` never aborts a run over a legacy global-catalog mapping**
  ([#755](https://github.com/xerj-org/xerj/issues/755)). Upgrading onto a
  catalog whose `prefix` field pre-existed as `text` aborted the entire
  indexing run. An upgrade must never do that.

- **Removing a file no longer leaves graph edges pointing at it**
  ([#736](https://github.com/xerj-org/xerj/issues/736)). Deletion removed a
  file's documents and its outbound edges but left inbound edges dangling, so
  graph queries returned edges to nodes that no longer existed.

### Recorded late

These shipped in rc.70 or earlier and were never written down, because
rc.19–rc.70 have no CHANGELOG sections at all. They are recorded here rather
than left under an "Unreleased" heading that misdescribes them; closing the
rest of that gap is a GA item in [ROADMAP.md](./ROADMAP.md).

- **Wrapping a query in a one-clause `bool` no longer changes its `_score` or
  its ranking** ([#399](https://github.com/xerj-org/xerj/issues/399)).
  `{"bool":{"must":[X]}}` and bare `X` are the same query — Lucene's
  `BooleanQuery.rewrite` erases a one-clause boolean before any `Weight` is
  built (`BooleanQuery.java:279-298`) — but XERJ steers on the *shape* of the
  query tree, and `is_doc_scan_query` matches every `bool`. On an index whose
  documents were still in the memtable, the wrapped form was therefore scored
  per-document by the IDF-less brute scorer (`1 + ln(1 + tf)`) while the
  identical bare `match` took the memtable BM25 arm. Measured on a
  120-document fixture with one `text` field, same clause throughout: the
  wrapped query scored `d004` at `2.7917595` where the bare clause scored it
  `0.0068451734`, and `size:1` returned a different document for each. The
  flushed (segment FTS) population already agreed and still does.

  A one-clause `bool` is now erased before any path decision reads the tree —
  a lone `must`, or a lone `should` with `minimum_should_match` absent, `0`
  or `1`, exactly the cases Lucene rewrites to the clause itself. A lone
  `filter` (`BoostQuery(ConstantScoreQuery(q), 0)`, score 0) and a lone
  `must_not` (pure-negative) are *not* their child and are left alone. The
  two page-local score-rewrite gates further down now read the same
  normalised tree the executor was steered by, rather than the raw request,
  so the divergence cannot reappear one gate later.

  Known and unchanged: on an unflushed index a `filter`-only `bool` still
  scores `1.0` instead of `0.0`. That is the separately-tracked filter-context
  defect in the memtable's brute scorer, not this one, and this change neither
  fixes nor worsens it.

- **A file excluded by a widened ignore/hidden rule was reported as a user
  deletion, so the graph resume path refused the rerun and its recovery
  contract told operators to "restore the removed file(s)" — a file still on
  disk** ([#439](https://github.com/xerj-org/xerj/issues/439), first fix). The
  resume-plan reconciler (`UnsupportedInventoryDelta::between`) now splits a plan
  file that vanished from the walk by whether it is still on disk, probed through
  the reversible raw-bytes `path_id` so a hidden *non-UTF-8* name (the case #439
  was filed for) is matched instead of read as a phantom removal through its
  lossy `rel`. A genuine deletion keeps the "restore the removed file(s)" route;
  a still-on-disk exclusion now says the file is still on disk and names the real
  routes (narrow the rule to re-admit it, or rebuild isolated to drop it). The
  refusal itself is unchanged — the documents still stay live — because sweeping
  an excluded group from the destination (so a widened exclusion actually removes
  the data) is a larger change tracked in
  [#589](https://github.com/xerj-org/xerj/issues/589).

- **`sort` on a field XERJ cannot resolve returned `null` on every hit
  instead of an error** ([#437](https://github.com/xerj-org/xerj/issues/437)).
  `compute_sort_values` special-cased `_score`, `_doc`, `_id` and the four
  meta-fields [#420](https://github.com/xerj-org/xerj/pull/420) made
  resolvable (`_seq_no`, `_version`, `_primary_term`, `_index`); every other
  ES meta-field name (`_source`, `_size`, `_doc_count`, `_field_names`,
  `_meta`, `_tier`, `_nested`, `_nested_path`, `_feature`, `_parent`,
  `_matched_queries`) and any unmapped or misspelled field fell through to a
  plain `_source` lookup that returned `Null` when absent. The whole result
  set tied on `[null]`, HTTP 200, and `search_after` paging on that field
  was stranded at page one with no error anywhere — the same
  silently-truncated-export failure mode
  [#198](https://github.com/xerj-org/xerj/issues/198) closes for scroll,
  reached through `sort` instead.

  A prior attempt, [#402](https://github.com/xerj-org/xerj/pull/402), tried
  a request-level denylist of "known-unresolvable" field names and was
  closed after independent verification refuted it: XERJ deliberately
  accepts a metadata-named key inside a document body (`{"_seq_no": 1}`,
  where real Elasticsearch rejects it — see #420's entry above), so whether
  a name resolves is a property of the *corpus* being searched, not of the
  field name, and a static list is guaranteed wrong on some corpus. #402
  was also wired into `_search` only, leaving `_msearch`,
  `_search/template` and `_async_search` answering the identical
  "unresolvable" field with 200.

  This fix checks the index's **schema** instead of the corpus — the same
  thing real Elasticsearch validates — with a single gate inside the
  engine's `search_inner`, which every search entry point funnels through
  regardless of endpoint. A sort field that is not one of the seven
  already-resolvable special cases and is not declared in the schema (via
  the same recursive dotted-path resolution already used for `.keyword`
  multi-fields and nested/object properties) is now rejected up front with
  Elasticsearch's real error: a 400 `search_phase_execution_exception`
  wrapping `query_shard_exception`, reason `"No mapping found for [x] in
  order to sort on"`.

  **Known gap, not closed here:** `collapse.inner_hits.sort` and
  aggregation `top_hits.sort` implement their own sorting independently of
  `compute_sort_values` and are not covered by this gate — an unresolvable
  field named in either still sorts silently wrong rather than erroring.
  Tracked as a follow-up.

## [1.0.0-rc.71] - 2026-08-29

An Elasticsearch-compatibility correctness release: query, aggregation, API and
autoindex semantics defects, a ~100x incremental re-index, and two metrics
counters that had been registered but never incremented.

Note on scope: releases rc.19 through rc.70 shipped without CHANGELOG sections.
This entry covers only rc.70..rc.71 and does not attempt to reconstruct that
gap. The three entries still under `[Unreleased]` above describe work that
shipped in rc.70 or earlier and are deliberately left there rather than
silently reattributed.

### Fixed — query semantics

- **`match` honors `minimum_should_match`** ([#832](https://github.com/xerj-org/xerj/issues/832)).
  It was ignored outright, so every `match` behaved as a pure OR.
- **`minimum_should_match` honors negative and combination specs**
  ([#846](https://github.com/xerj-org/xerj/issues/846)). `-2`, `-25%` and
  `"3<90%"` now resolve against the should-clause count instead of degrading to
  OR. The `MinShouldMatch` `Serialize` impl is now hand-written: an untagged
  derive emitted `Negative(2)`, `Percentage(2)` and `Fixed(2)` all as the bare
  number `2`, so the query cache — which hashes the serialized request —
  conflated three semantically different queries into one entry
  ([#860](https://github.com/xerj-org/xerj/issues/860)).
- **`dis_max` over memtable documents scores through the real per-field BM25
  path** ([#834](https://github.com/xerj-org/xerj/issues/834)). Buffered matches
  took a flat-1.0 catch-all, so a document matching more disjuncts tied with
  single-disjunct documents and the ranking changed at flush.
- **A `nested` query scores parents by the child `score_mode`**
  ([#836](https://github.com/xerj-org/xerj/issues/836)) — `avg` (the ES default),
  `max`, `min`, `sum`, `none` — instead of contributing a flat 1.0.
- **`fuzzy` and `match` honor `prefix_length`**
  ([#848](https://github.com/xerj-org/xerj/issues/848)), on the doc-scan and
  segment paths and through nested-path rewriting.
- **A `must_not`-only `bool` no longer projects to a pure-negative FTS bool**
  ([#797](https://github.com/xerj-org/xerj/issues/797)).
- **A `term` on a date field matches by instant across every path**
  ([#788](https://github.com/xerj-org/xerj/issues/788)), and `terms` on date/ip
  fields normalizes values the same way a single `term` does
  ([#799](https://github.com/xerj-org/xerj/issues/799),
  [#801](https://github.com/xerj-org/xerj/issues/801)).
- **`exists` requires a non-null value, not merely key presence**
  ([#792](https://github.com/xerj-org/xerj/issues/792)).
- **A keyword `prefix`/`wildcard` inside `bool.must_not` stays case-sensitive
  after flush** ([#794](https://github.com/xerj-org/xerj/issues/794)).
- **A CIDR `term` survives flush** — rewritten to a range on an `ip` field
  ([#782](https://github.com/xerj-org/xerj/issues/782)) and routed to the source
  scan when schemaless ([#786](https://github.com/xerj-org/xerj/issues/786)).
- **`function_score` per-function filters are schema-aware**
  ([#802](https://github.com/xerj-org/xerj/issues/802)) — the last schemaless
  `doc_matches_query` caller in production code.
- **`sort` on a multi-valued field reduces to a min/max representative**
  ([#826](https://github.com/xerj-org/xerj/issues/826)).

### Fixed — aggregations

- **Metric aggregations fold ALL values of a multi-valued field**
  ([#842](https://github.com/xerj-org/xerj/issues/842)), instead of the first.
- **`terms` `include`/`exclude` regexes are fully anchored**
  ([#837](https://github.com/xerj-org/xerj/issues/837)) — they matched as
  substrings.
- **`terms` bucket keys are typed by the field's source shape, not by content**
  ([#864](https://github.com/xerj-org/xerj/issues/864)). A keyword value that
  merely looks numeric (`"007"`) stayed a string instead of being coerced to 7.
- **`terms` `_key` ordering is numeric on numeric fields**
  ([#839](https://github.com/xerj-org/xerj/issues/839)).
- **`missing` counts empty and only-null arrays as missing**
  ([#844](https://github.com/xerj-org/xerj/issues/844)) and resolves presence
  via the `exists` resolver ([#847](https://github.com/xerj-org/xerj/issues/847)).
- **`filters` supports `other_bucket` / `other_bucket_key`**
  ([#856](https://github.com/xerj-org/xerj/issues/856)).

### Fixed — API

- **`mget` honors a per-item `_source`**
  ([#855](https://github.com/xerj-org/xerj/issues/855)) — `false`, an includes
  list, or `{"includes":[..],"excludes":[..]}`.
- **`GET /_doc` `_source_includes`/`_source_excludes` honor nested paths**
  ([#850](https://github.com/xerj-org/xerj/issues/850)).
- **`_field_caps` `fields` filtering honors full globs**
  ([#853](https://github.com/xerj-org/xerj/issues/853)).
- **A partial `_update` deep-merges nested objects**
  ([#821](https://github.com/xerj-org/xerj/issues/821)) — it was shallow and
  dropped sibling keys.

### Fixed — autoindex

- **C++ symbols defined inside a function body are no longer indexed**
  ([#852](https://github.com/xerj-org/xerj/issues/852)) — a function-local
  `struct`/`class` and its enumerators leaked as file-scope API symbols.
- **Top-level constants are indexed as symbols across 7 languages**
  ([#500](https://github.com/xerj-org/xerj/issues/500)).

### Fixed — observability

- **`xerj_bytes_written_total` is wired to the flush and merge segment writes**
  ([#804](https://github.com/xerj-org/xerj/issues/804)) and
  **`xerj_bytes_read_total` to validated segment opens**
  ([#819](https://github.com/xerj-org/xerj/issues/819)). Both counters were
  registered but never incremented, so each read a flat zero regardless of load.
- **The disk flood-stage log reports free space, and the watermark can be
  retuned at runtime** ([#823](https://github.com/xerj-org/xerj/issues/823))
  via `PUT _cluster/settings`, matching ES's recovery flow without a restart. An
  override cannot enable the watermark when it is disabled in config.

### Performance

- **Incremental re-index skips the Phase-A parse for byte-identical files**
  ([#868](https://github.com/xerj-org/xerj/issues/868)). The tree-sitter parse
  is ~72% of extraction cost (~57 ns/byte, measured), and an unchanged re-index
  re-parsed the whole corpus to rebuild sketches. Gating on the committed
  content digest turns that from O(corpus) into O(changed) — roughly 100x on the
  edit-and-rerun and CI paths. The digest check in `reconcile_plan` remains the
  correctness authority: a changed file whose scan was wrongly skipped
  mismatches there and fails closed rather than carrying stale documents.

  Known limitation: the fast path is keyed on file content only. It carries no
  extractor version, so upgrading to a release that extracts *more* symbols does
  not re-extract unchanged files until their content changes.

### Changed

- **Release PRs are gated on their notes matching the tree**
  ([#474](https://github.com/xerj-org/xerj/issues/474)) — issue states against
  the live tracker, merge coverage since the previous tag, and freshness against
  the base.
- **The Build+Test job timeout is 60 minutes** (was 50)
  ([#770](https://github.com/xerj-org/xerj/issues/770)). This is defense in
  depth, not a fix: [#751](https://github.com/xerj-org/xerj/issues/751), the
  intermittent hang in the default-parallelism workspace test step, is still
  open and still bounded by that step's own 12-minute cap.
- **`llms.txt` no longer hardcodes the current release version**
  ([#796](https://github.com/xerj-org/xerj/pull/796)) — it drifted behind every
  release and handed agents the wrong version.

### Documentation

- The reference-coding case study's charts are rebuilt as vertical comparison
  bars, and its colours now resolve in both themes (the page referenced CSS
  variables that do not exist, so every chart fell back to night-only values).
- `match_phrase` `slop` is documented as in-order only, not transpositions
  ([#830](https://github.com/xerj-org/xerj/issues/830)).

## [1.0.0-rc.18] - 2026-08-18

### Security

- **Patched `h2` for RUSTSEC-2026-0258, "unbounded empty DATA frames"** (0.4.13 ->
  0.4.16). `h2` is the HTTP/2 implementation beneath `hyper`, so it sits under
  every listener this server runs — ES-compat and native REST through axum, gRPC
  through tonic. An unbounded stream of empty DATA frames was a remote
  resource-exhaustion vector against any reachable node; the patched version
  answers with `GOAWAY ENHANCE_YOUR_CALM` instead.

  The fix adds a per-connection budget, and it is a **token bucket, not a
  quota** — though, as below, that is not enough to keep it off real traffic.
  `h2-0.4.16/src/proto/streams/counts.rs:94-101`: a DATA frame *under* 256 bytes
  consumes `256 - payload_len`; a frame of 256 bytes or more **replenishes**
  `payload_len - 256`. The refill stops at a 25,600-byte ceiling, which lives
  elsewhere — `Budget::new(DEFAULT_DATA_FRAME_BUDGET)` at `counts.rs:89` and the
  `.min(self.max)` at `counts.rs:26`, over `DEFAULT_DATA_FRAME_BUDGET = 256 * 100`
  at `proto/mod.rs:39`. Any ordinary payload frame
  therefore refills what small frames drain — but only a frame that reaches 256
  bytes does. A body chunked below that is made entirely of frames that drain,
  which is what the paragraphs below are about.

  Two further details decide whether you can ever meet it. A small frame *with*
  a payload is refunded when the application reads it (`streams.rs:1515`) — but
  only once the read happens, which is later than you would think and is the
  subject of the next paragraph. A frame with an **empty payload**
  is not: h2 discards it before the application sees it (`recv.rs:757` returns
  early for an empty frame without END_STREAM), so its 256 is never refunded —
  only offset by a large frame elsewhere on the connection. "Empty" here means
  zero *payload*, not zero bytes on the wire: a padding-only frame counts, and
  so does a small frame on a stream that is reset before the application reads
  it. An empty frame carrying END_STREAM is delivered, and is refunded. Exhaustion is a connection-level
  `GOAWAY ENHANCE_YOUR_CALM`, and it takes the **101st** empty frame with
  nothing ≥256 bytes in between (`checked_sub` fails once `available` reaches 0).

  Reading the body is **not** by itself protection, and this is the part to take
  operationally. The charge lands on arrival and the refund only on the read, so
  once a flow-control window's worth of frames has arrived before the handler is
  scheduled, the connection is already over budget. Measured on this build, a
  handler consuming every frame in a tight loop is cut off at exactly the same
  frame as one that never reads: 101 at 1 byte, 134 at 64, 201 at 128, 356 at
  184. XERJ advertises `SETTINGS_INITIAL_WINDOW_SIZE = 1048576`, far above h2's
  64 KiB default, so more frames buffer here than elsewhere before a read lands.

  The safe line is frame size, not frame count, and it is sharp: **a frame of 256
  bytes or more never charges at all**. The trip frame for a smaller size is
  `floor(25600 / (256 - size)) + 1` — the first frame whose charge underflows the
  remaining budget. Below roughly 200 bytes a legitimate, authenticated,
  fully-read request can take the `GOAWAY`: a valid 27 KB `_bulk` chunked into
  128-byte DATA frames did so repeatedly across runs, and a 307 KB body at 184
  bytes likewise. How often depends on load and on how the client frames and
  drains — measurements on two machines differed in both directions, so treat
  the hazard as real and the frequency as unpredictable rather than as a rate.
  Tracked as [#485](https://github.com/xerj-org/xerj/issues/485).
  If you control the client, chunk at 256 bytes or above.

  XERJ has such a path: `auth_middleware` is an axum layer, so a request that
  fails authentication is answered `401` without the handler ever extracting its
  body (`xerj-api/src/auth.rs`). An unauthenticated client that streams many
  small frames at a node with auth enabled therefore accumulates charges nothing
  refunds, and the connection is eventually closed with `GOAWAY`. Authentication is not special
  here: any rejection that answers without reading the body does the same, so an
  *authenticated* client that mistypes a route and streams a small body into the
  404 sees the identical `GOAWAY`. Worth recognising for what it is rather than
  reading it as a client bug.

  Two `unsound` advisories were cleared in the same pass: `anyhow` 1.0.102 ->
  1.0.104 (RUSTSEC-2026-0190, `Error::downcast_mut()`) and `memmap2` 0.9.10 ->
  0.9.11 (RUSTSEC-2026-0186, unchecked pointer offset), plus the yanked
  `spin` 0.9.8 -> 0.9.9 (#483).

### Fixed

- **A node whose ES-compat port was taken printed a success banner and stayed
  up, and `xerj autoindex` then wrote the user's documents into whichever other
  node owned that port.** The three listeners were bound inside tasks spawned
  *after* the banner, where a bind error was logged and explicitly non-fatal, so
  losing one port was survivable. The process that does own the port answers
  `GET /_cluster/health` with `green` — the readiness probe the docs prescribe —
  so nothing downstream could tell the difference. Found by three independent
  agents in a field study; one of them wrote 905 files, including contracts,
  invoices and a bank export, into a stranger's data directory and got
  `ok=true exit=0` for it. Every listener is now bound before the banner, a
  refused bind ends startup with the port and the reason on stderr, and the
  banner is printed from the addresses the kernel actually returned (#465,
  #466).

- **A `keyword` field holding a JSON array was flattened into one token, and the
  answer changed at `_flush`.** Two independent causes, which is why the symptom
  looks inconsistent. In a segment the array was joined into a single string
  before the FTS layer saw it, and the keyword analyzer emits its whole input as
  one token — so every clause that projects to a whole-value `FtsQuery::Term`
  missed the document, the first element included; measured on
  `{"tags":["red","blue"]}`, after a flush both `red` and `blue` returned 0 hits
  and only the joined artefact `"red blue"` matched. The type name is
  load-bearing: `terms` reads like such a clause but does not compile to one —
  it routes to the doc-values path, which already bailed on array fields, and it
  returned the document correctly on both sides of the flush. Before the flush a
  different cause applied: the memtable's single-valued columns keep only element
  0, and the fused columnar walk did not bail out on such a field the way its
  sibling paths did. That walk serves `term` and `range`, and `bool` with `must`
  or `filter`, through `constant_score` and boosted wrappers — so a `bool`/
  `filter` query was affected too, not only a bare `term`. There `red` matched
  and `blue` did not. The
  first-element-survives shape people remember is the pre-flush one and does not
  describe a flushed index. Array elements are now indexed as N independent
  values separated by a position gap, so a phrase query cannot span two elements,
  and the columnar path refuses an array-valued field rather than silently
  reading only its first element (#332, #470).

- **The audit log recorded no writes and could not name the actor, and
  `/_audit` was readable by any credential.** Create, index, update, bulk and
  delete appended nothing; the one audited data-path op recorded the literal
  `"anonymous"` for an authenticated admin call (#329, #471).

- **`bool` scoring: a scoring-irrelevant clause collapsed `_score` and reordered
  the page — partially fixed.** A `filter`, a second `must`, or a `must_not`
  changed relevance scores and hit order, with no error and no warning. Three
  defects of the same shape, all of them a clause or a pass affecting scoring
  when it must not: `bool.filter` was projected onto the FTS `must` slot so its
  BM25 landed in `_score`; the IDF rescore counted `filter`/`must_not` toward
  its trigger; and that rescore derived IDF from `final_hits.len()`, which made
  `_score` a function of `size`.

  **Read the scope before you re-baseline. This fix is partial and #361 remains
  open.** What is fixed: a `filter` or `must_not` carrying a `term`-shaped
  child, on a page served entirely by the segment FTS path. What is *not*:
  `filter: [{match_all: {}}]` and `filter: [{exists: ...}]` still return a flat
  score over unrelated documents, and any page that also carries memtable or
  stored-scan hits keeps rc.17 behaviour — that fallback is IDF-less
  (`1 + ln(1 + tf)`) and was deliberately untouched. On those shapes `_score`
  still varies with `size`; measured on rc.18, one query returned four different
  top-1 documents at `size` 2 / 5 / 10 / 50.

  **Where it does apply, this changes `_score` values and hit ordering versus
  rc.17.** If you have pinned expected scores, recorded relevance baselines, or
  tests asserting an exact order, re-baseline them on rc.18. The new behaviour
  is the correct one — Lucene draws the same line at
  `BooleanClause.isScoring()` — but a silent ranking change is exactly the kind
  of thing that should not arrive unannounced (#361, #387).

- **`xc.py` could not tell a corpus that was never loaded from a query that
  genuinely matched nothing** (#476).

- **An alias 404'd on `_refresh`, `_forcemerge`, `_cache/clear` and
  `_terms_enum`.** `resolve_indices_for_op` had no alias branch, so a valid
  alias was simply "not an index". It now expands to every member rather than
  the first — a `_refresh` reaching one of three members makes the next search
  non-deterministic with nothing in the response saying so (#459). The remaining
  read-path half — an alias answered as an alias over one member — is still open
  (#449).

- **The air-gapped deployment recipe failed open: a bad digest extracted and
  installed anyway.** The verify step did not stop extraction, and because
  `set -eu` sat at the top of the block rather than inside it, a failure killed
  the operator's shell — so the natural recovery, which the page itself named,
  was to re-paste without it, and that made the digest check non-fatal. Every
  block that decides something now runs inside its own subshell, the digest is
  computed locally and asserted against the `.sha256` with `grep -qxF`, and the
  block that verifies is the block that installs (#441).

- **The install page's checksum step never hashed the archive.** It ran
  `sha256sum -c`, which reports success for whatever filenames the `.sha256`
  happens to list, skips `#` comment lines silently, and never checks that any
  line names the archive being extracted. A `.sha256` carrying a comment naming
  the archive plus a valid digest for an unrelated file verified clean at exit 0
  while the archive was never hashed. Both the install page and `llms.txt` now
  compute the digest, assert it is 64 hex characters, demand that exact line,
  and chain extraction and install to the result (#444, #452).

- **A text file whose first two characters were `BM`, or whose first four were
  `GIF8`, was junked as an image and never indexed.** They were the only two
  entirely-printable signatures *in the magic table* accepted without a
  qualifier — the scoping matters, and the rest of this entry says why — so `sniff`
  returned `Family::Binary` and `scan_file` skipped the file citing "binary
  content (gif)" / "(bmp)" — a reason naming a format the file did not have. The
  reported case is a CSV whose first column header is `BMW`; a health export
  headed `BMI` fails the same way, as does prose beginning "BM" or "GIF8".
  rc.17 shipped this knowingly and said so twice, pinning the wrong answers in
  `gif8_and_bm_are_still_taken_on_faith` so the deferral stayed visible. That
  test is replaced by `text_that_opens_with_gif8_or_bm_is_text`, and
  `every_printable_signature_carries_a_qualifier` now walks every row of the
  signature table so the defect cannot return *through that table*. It is not
  the whole class: `%PDF-` is matched earlier, is equally printable, and prose
  opening with it is still handed to the PDF extractor — deliberately pinned and
  still open as #403.

  **A corpus indexed before this release will not necessarily pick the file up
  on a re-run.** Two things decide it and they are easy to confuse: the indexing
  path decides whether the file is re-sniffed at all, and the state dir decides
  whether anything can then accept it. The cases differ enough that the only
  guidance worth giving is: check the exit code, and read the error if there is
  one.

  On the default (graph) path the frozen plan is reused instead of re-sniffed, so
  a file recorded in `plan.junk_files` is never reconsidered: the re-run leaves
  the index unchanged and exits 3 (`completed-with-junk`), the same code the
  original run gave — there is no signal that anything is now recoverable.

  With `--no-graph` the file is re-sniffed and then has to be re-admitted, which
  is decided by four gates in `classify_new` (`reconcile_plan.rs:279-323`) — the
  family/group filter, `ensure_compatible`, a field-overlap threshold, and a
  refusal when two frozen datasets match equally well — which between them select
  one of three outcomes. All three are
  reachable — the file is recovered in place, or the run aborts naming it, or it
  stays junk — and which one you get depends on what the other files in the
  corpus froze into the plan. This entry deliberately does not predict it: six
  attempts to state the rule concisely were each refuted by measurement, and the
  honest instruction is to run it and read the exit code. A `csv` file rejoining
  a compatible `csv` dataset is recovered incrementally in seconds; prose never
  is, because prose datasets record `family = docs` while the file sniffs
  `txt-prose`, so the first gate can never match.

  A new `--state-dir` with a new `--prefix` recovers the file in every
  configuration measured. `--fresh` recovers it in some and is refused in
  others — the refusal tracks what the state dir contains, not which flag you
  pass. Both re-extract the whole corpus and re-embed it on a semantic index.
  The new-prefix route also leaves the old prefix's indices live and serving
  until you delete them by hand, so budget for two copies of the corpus in the
  interim.
  The full matrix, including the cross-path cases where a corpus indexed one way
  is re-run the other, is #490.

  A residual class stays `Family::Binary` regardless: the GIF arm junks a file
  whose prefix fills the sniff buffer **or** whose bytes cannot be written as a
  canvas dimension, and the second disjunct has no size floor — the release's
  own tests pin a 3,000-byte prose file junked as an image under a 1 MiB budget.
  Describing the cost as "large files only" would name one half of it
  (#379, #380, #427).

- **A GIF whose comment chain outruns the sniff budget is decided by the chain,
  not by a canvas heuristic** (#427, #442).

### Added

- **`xerj feedback`** — one command drafts the field report the project asks every
  agent to file, auto-filling version, OS and what was indexed;
  `xerj feedback --open-pr` stages that report under
  `user-feedback/16-agent-field-reports/` and opens the PR — it stages the report
  but does not restrict the commit to it, so check `git status` first if you have
  other work staged. Field-report PRs are CLA-exempt, so they merge without a
  signature (#473).

- **A build gate against release notes that promise the next release.** A file
  shipping inside the release tag may no longer describe a capability as arriving
  later — that sentence cannot be checked at review time and is wrong by
  construction afterwards, whichever way it resolves. Both halves of that shipped
  during this cycle. The gate is a list of known phrasings rather than a rule
  about meaning, and says so; #474 tracks what would close the class (#474, #479).

- **`word_delimiter` / `word_delimiter_graph` token filters and a `code`
  analyzer**, so an identifier can be matched by its sub-words
  (`getHTTPResponse` -> `get` / `HTTP` / `Response`) while the whole identifier
  is preserved for exact matches. Capability only — no performance claim (#468).

- A reproducible verification harness for the neural embedder's missing
  batching, measuring lexical 1.3 s against neural 677.5 s on an identical
  101-file slice. This is the measurement, not the fix — **#366 stays open**
  (#467).

- Operational-recipe verification protocol in `docs/CONTRIBUTION_REVIEW.md`,
  including the two clauses that would have caught the round of #441 that went
  green while still broken: run each negative control with the following block
  appended, and with the failing block's exit status discarded (#447, thanks
  @buger).

- `docs/XERJ_VS_LUCENE.md` — a side-by-side comparison with Lucene 10.3.1,
  honest in both directions (#373, #448, thanks @buger).

### Changed

- **XERJ no longer sizes its memory budgets to the whole machine. New default:
  8 GiB.** Every budget derived from `effective_memory_limit_bytes()`, which is
  min(cgroup limit, total system RAM) — so with no cgroup, the normal laptop and
  macOS case, a bigger machine bought a hungrier XERJ. Measured on a 121 GiB
  host, the derivation base was 124,609 MB, giving a 31,152 MB memtable budget
  and a 24,921 MB hydration cache before anything else allocated. Users reported
  ~20 GiB resident for two indexed projects.

  `limits.max_process_memory_mb` now defaults to `8192` and caps the derivation
  base itself, so every dependent budget shrinks coherently. It only ever
  lowers: a 4 GiB laptop stays 4 GiB and a smaller cgroup limit still wins. Set
  `0` to restore the previous machine-proportional behaviour on a dedicated box
  (#461).

  **Read this before relying on it as a ceiling.** It caps the budgets derived
  from machine size. It is NOT an RSS limit, and an audit of this release
  measured a node configured with exactly these budgets reaching 7,924 MB
  resident while the capped caches held 873 MB — the governed budgets cover
  under half of peak RSS, and the rest is merge-path allocation, aggregation
  workspace and allocator retention that consult no budget at all. On a large
  corpus the cap can therefore surface as an HTTP 429 on ingest rather than as
  flat memory. If you index a large project and see 429
  `circuit_breaking_exception`, raise `limits.max_process_memory_mb`. Reducing
  the memory that is outside these budgets is follow-up work, not something this
  release completes.

- **The README's lead was rewritten three times in this release; the third is
  what ships.** #457 moved the lead from reference coding to `autoindex`, on the
  argument that reference coding is one use case standing in for the whole
  product, and changed `llms.txt` the same way. #472 moved it back to reference
  coding with its measured 2.7x-fewer-output-tokens result at an equal solve
  rate, and added a community CTA. #478 then replaced the lead again with a
  paste-to-agent prompt — *"One prompt, and your AI agent installs XERJ, indexes
  your code, and reads the exact implementation"* — and removed the top-level
  `## Install` section, demoting installation to a `## Install by hand` section
  further down.

  So the net change versus rc.17 is the **#478** shape, not #472's, and rc.18
  does not lead install-first. The badge and body corrections from #457 survive
  all three (#457, #472, #478).

- **Capability badges corrected.** The default embedder is lexical and offline;
  neural is opt-in and downloads ~90 MB on first use. The previous pair
  (`semantic` beside `built-in, offline`) described a configuration that does
  not exist, and the README body said so correctly eighty lines below. The
  release badge no longer carries `include_prereleases&sort=semver`. Recording a
  correction to this entry rather than restating it: neither parameter is
  actually broken for this repository's tags — all four combinations, the
  pre-#455 URL included, render `v1.0.0-rc.17` against the live endpoint. The
  parameters were dropped as unnecessary, not as a fix (#455).

- **`autoindex --follow-symlinks` no longer follows a link out of the indexed
  folder unless you ask it to.** Pointing autoindex at a folder is not consent
  to index whatever that folder links to, and the hidden-file rule — the thing
  keeping `.env`, `.ssh` and `.aws` out of a queryable brain — was defeated
  entirely by a visible link name: `notes.txt -> .secretdir/k.txt` indexed the
  secret, and `shared -> /etc` indexed `/etc/shadow` under `shared/shadow`,
  while the run reported that the hidden directories had been pruned (#438).

  A followed link is now judged by what it **resolves to**, not by the name it
  wears. Targets outside the folder are refused and reported under a new
  `symlink:outside-root` rule; targets inside a hidden directory are skipped
  like any dotfile; and one that does both — left the folder AND resolved
  through a dotted component — is reported as `symlink:outside-root+hidden`,
  because it is refused even with the opt-in below and neither of the other two
  labels tells the operator anything they can act on.

  **This will index less than the previous release for some setups**, and the
  first run after upgrading drops what it previously indexed through such a
  link. If following links outward is why you turned the flag on — a vendored
  sibling checkout, a monorepo package link, a mounted volume — pass
  `--follow-symlinks-outside-root` to restore it (it requires
  `--follow-symlinks`, and is refused on its own rather than accepted as a
  silent no-op). That waives the root boundary
  and nothing else: the hidden-file rule still applies to the resolved path,
  judged from the point where the target diverges from the folder you pointed
  at — so `notes -> ../sibling/pkg` is followed and `keys -> ../sibling/.ssh/id_rsa`
  is not, while a dotted directory the two paths SHARE (a `/tmp/.tmpXXXX`, a
  checkout under `~/.local`) does not refuse anything, because your own folder
  is already inside it.

  Known limit, stated rather than implied: this is a rule about names and about
  where a link resolves. A **hard** link has neither, so a visible name hard-
  linked to a file inside a hidden directory is still indexed, on this release
  and on every earlier one.

### Fixed

- **A multi-index `scroll` reported the wrong `_index` on every continuation
  page, so `(_index, _id)` stopped being distinct**
  ([#414](https://github.com/xerj-org/xerj/issues/414)). The scroll context
  stored bare hits plus one context-level `index`, and every page after the first
  stamped that single name onto every hit. Page one was correct, because it maps
  the real per-hit index, so the divergence only appeared once paging crossed
  into the second index.

  The two context-creation sites stored *different* wrong values, which is why
  the symptom has two faces: `search_impl` (`/{a,b}/_search?scroll=`) stored
  `index_names.first()`, so hits were labelled with the first concrete index
  (`mi_a`), while `search_with_scroll` (`/{spec}/_search_scroll`) stored the raw
  path spec, so hits were labelled with the un-split string — the
  `_index: "sa,sb"` that #414 reported, 11,900 of 12,000 hits.

  Ids routinely collide across indices (each numbers its own documents), so a
  consumer keyed on `(_index, _id)` silently kept a fraction of the corpus.
  Measured on two 300-document indices with identical id ranges: **600 hits
  collapsed to 300 distinct `(_index, _id)` pairs**, all labelled with the first
  index. `search_after` over the same two indices was unaffected and returned
  600 of 600 — the two paging APIs disagreed about which index a document came
  from. The blast radius is reindex, migration, backup and CDC consumers, which
  are exactly the readers that use scroll and least survive losing half a
  corpus.

  The comment at the context-creation site asserted the opposite of the
  behaviour — that keeping the raw index spec left per-hit `_index`
  "authoritative when paging" — which is why the defect outlived review.

  `ScrollContext::hits` is now `Vec<(String, Hit)>`, so a hit carries the index
  it came from rather than borrowing one from the context. Both context-creation
  sites previously discarded it with the same `.map(|(_, h)| h.clone())`.

  **Scope, established by an independent adversarial verification of the fix and
  stated here rather than discovered later.** This covers scrolls addressed by a
  comma list (`/a,b/_search?scroll=`), a wildcard, or `_all`. It does **not**
  cover a scroll addressed through an **alias** spanning several indices: alias
  names are not expanded by the index resolver, so every hit still reports the
  alias and `(_index, _id)` remains non-distinct there. That matters because
  reindex and CDC tooling routinely scrolls an alias, which is exactly the
  audience named above — tracked separately rather than folded into this entry ([#433](https://github.com/xerj-org/xerj/issues/433)).
  The type change is also a breaking one for any out-of-tree reader of the
  public `ScrollContext::hits` field.

### Documentation

- **A three-model reference-coding benchmark, and a number that does not match the
  headline.** `demo/playbooks/REFCODING_BENCHMARK_3MODEL_2026-08-18.md` measures
  XERJ at **~1.4x fewer output tokens than grep**, consistent across all three
  models, plus a turnkey SWE-bench harness under `tools/xerj-code/swebench/`.
  The README, `llms.txt` and the badge headline **2.7x**, from the earlier
  single-model case study. Both are real measurements of different task sets;
  neither supersedes the other, and the 1.4x figure is the one measured across
  models. If you are choosing a number to quote, quote that one and say what it
  was measured on (#480).

- **Withdrew security and supply-chain claims the build does not back.** The
  security page's air-gap row moved from `GA` to `DOCS` and its
  `XERJ_AIRGAP=1 disables all telemetry` line was removed — that variable exists
  nowhere in the tree. The `SBOM · SUPPLY CHAIN` section, which promised a
  CycloneDX SBOM with every release, SLSA provenance, signed git tags and release
  archives, cosign-compatible signatures and reproducible builds, was replaced by
  a `RELEASE INTEGRITY` section stating what actually ships: a `.sha256`, which is
  a checksum and not a signature or an attestation. The public-sector page lost
  its `SBOM · SLSA` compliance row and one at-rest/BYOK line. Others on that page
  survive — two of them carrying no status marker at all — so the withdrawal is
  partial, not complete (#491).
  A verified air-gapped deployment recipe was added in their place (#430).


- **The 10,000-document scroll snapshot cap is now published, and pinned to the
  constant that enforces it**
  ([#370](https://github.com/xerj-org/xerj/issues/370)). XERJ's `_search?scroll=`
  is a bounded up-front snapshot, not a segment-walking cursor, so a query whose
  exact result set exceeds `SCROLL_SNAPSHOT_MAX_HITS` is refused with a 400
  rather than paged and silently truncated (#198). The refusal is the right
  behaviour and its message already names `search_after` as the way past it —
  but the ceiling appeared in no user-facing document, and scroll is precisely
  what the ES ecosystem reaches for to read a *whole* index
  (`elasticsearch-py`'s `helpers.scan()`, reindex internals, export and backup
  tooling), where the result set is normally larger than the cap. "Scroll is
  supported, with a 10,000-document snapshot cap; use `search_after` beyond
  that" is a different compatibility claim from "scroll is supported", and only
  the first one is true.

  The cap, the 400 it produces and the `search_after` alternative are now stated
  on `landing/docs/api-es-compat.html` (which also gains the three scroll
  endpoints it never listed), in the "what needs rewriting" list on
  `landing/docs/migration-from-es.html`, and in the honesty/capability-boundary
  section of `landing/llms-full.txt`. Every published figure sits in a
  `<!-- generated:scroll-snapshot-cap -->` region checked against
  `xerj_api::es_compat::SCROLL_SNAPSHOT_MAX_HITS` by
  `xerj-api/tests/scroll_snapshot_cap_is_documented.rs`, so changing the
  constant now fails the build until the pages follow — the mechanism already
  used for the capability counts in `docs_capability_lists.rs`. The same test
  drives the real ES-compat router over a corpus one document past the cap and
  reads the enforced number back out of the 400 the server produced, so the
  published number is checked against the server's behaviour rather than
  against a second hand-written copy of it.

  **The published escape hatch is now executed, not just spell-checked.** A
  pinned number is worth nothing if the recipe past it is wrong, and the first
  draft of this page's `search_after` transcript was: it paged with
  `search_after: ["999"]` against 11,450 numeric-looking `_id`s. `_id` is a
  keyword and sorts lexicographically, so `"1000" < "999"` and page 1 of 1,000
  actually ends at `_id` `"10897"`. Followed exactly as printed, including the
  page's own stop rule ("until a page comes back empty"), that walk collected
  1,010 of 11,450 documents — 10,440 missing, every response a 200 — which is
  the silently-truncated export that #198 and this very page exist to rule out.
  The transcript now publishes the cursor the response actually returns, shows
  the hit it comes from, and states the lexicographic ordering that produced the
  mistake. `published_search_after_recipe_walks_the_whole_corpus` parses the
  transcript out of the page, seeds the corpus the transcript itself names,
  replays the printed *paging* request bodies (the two carrying `sort`) against the real router, and
  asserts the walk collects every seeded `_id` over the page count the
  transcript claims — so a wrong cursor, an early stop, a repeated page or a
  drifted claim all fail the build.

  Three further scroll claims elsewhere in the tree were corrected to match:
  `landing/agent-search/index.html` said scroll *was* a deep-pagination cursor;
  `landing/llms-full.txt` §6 and `README.md` carried the bare "scroll is
  supported" form; and the ES→XERJ migration recipe in
  `docs/recipes/production-deployment.md` told readers they could point a
  whole-index scroll copy at a XERJ source with no mention of the ceiling.

  One boundary is documented rather than fixed here: the cap is applied to the
  request's summed total on `POST /{index}/_search?scroll=` (`es_compat.rs:14229`)
  but *per index* on the `POST /{index}/_search_scroll` alias
  (`es_compat.rs:19808`), so a multi-index scroll on that route can snapshot the
  ceiling from each index. That direction is permissive, not lossy. The pages now
  say so and point at
  [#405](https://github.com/xerj-org/xerj/issues/405), where the engine
  inconsistency is tracked.

  Verified by `cargo test -p xerj-api --test
  scroll_snapshot_cap_is_documented` — 3 passed in the `ci-test` profile
  (8.67s, overflow-checks off) and 3 passed in `dev` (12.40s, overflow-checks
  on). It drives `build_es_compat_router` over tempdir engines: a
  10,001-document index (`SCROLL_SNAPSHOT_MAX_HITS + 1`)
  refuses the scroll open with `400 illegal_argument_exception` naming `[10001]`
  and `[10000]`; a 128-document control scroll returns a `_scroll_id`; the
  printed first page returns 1,000 hits ending at `_id` `"10897"` with
  `sort: ["10897"]`; and the published walk from that cursor returns 11,450
  distinct `_id`s over 12 pages with none missing and none repeated. Reverting
  only the transcript hunk to the `["999"]` form fails the same test with
  `left: Array [String("999")] / right: Array [String("10897")]`, and with the
  cursor assertion inverted so the walk still runs, with "collected 1010 of
  11450 documents over 2 pages, silently missing 10440". The `_search_scroll`
  divergence above was measured on two 6,000-document indices in one engine:
  `POST /sa,sb/_search?scroll=1m` → 400 naming `[12000]`/`[10000]`;
  `POST /sa,sb/_search_scroll?scroll=1m` → 200 with a `_scroll_id` and
  `hits.total {value: 12000, relation: "eq"}`.

### Fixed

- **`xerj autoindex` now skips hidden names that are not valid UTF-8.**
  The walker already refused `.env`, `.ssh`, and `.git`, but the check
  used `file_name().to_str().starts_with('.')`. On Unix a name whose
  first byte is `.` and that is not valid UTF-8 (`to_str()` is `None`)
  was walked and indexed. The skip now compares the first encoded byte
  to `.`. Typical UTF-8 secrets were already skipped. `--dry-run`'s
  `ignored_files_in_pruned_dirs` count uses the same predicate, so a
  hidden non-UTF-8 file (or the contents of a hidden non-UTF-8
  directory) is no longer reported as a non-hidden file inside a
  pruned directory.

  **Upgrade.** A corpus indexed by a pre-fix binary that contains a
  hidden non-UTF-8 name looks like a file removal to the graph-enabled
  path (the default, and what `xerj brain` uses). As of
  [#439](https://github.com/xerj-org/xerj/issues/439)'s first fix the
  rerun's refusal names the real cause — the file is still on disk,
  excluded, not removed — instead of the misleading "restore the removed
  file(s)". It still exits 1 with `unsupported_content_group_removal`,
  writes nothing, and the already-indexed documents still stay live;
  sweeping them (so a widened exclusion removes the data) is tracked in
  [#589](https://github.com/xerj-org/xerj/issues/589). Recovery that
  removes the data today: delete the published indices and the state
  directory, then re-index the folder. An isolated rebuild with a new
  `--state-dir` / `--prefix` also exits 0, but leaves the old target's
  documents live until those indices are deleted by hand.

- **Autoindex snapshot and replay one-shot failpoints can no longer be stolen by unrelated parallel tests.** ([#385](https://github.com/xerj-org/xerj/issues/385)).
  The two test-only failpoints are now owned by the thread that arms them, while
  retaining their existing one-shot behavior and serialization locks.

- **Console delete/upsert no longer reports success after a refused write.**
  `DELETE /_xerj-console/api/v1/views/{id}` swallowed `delete_document`
  errors and returned `204` while the view stayed on disk (the same class
  as the passkey revoke fix). Auth and prefs writes used delete-then-create,
  so a replacement the engine rejected (write block, unparseable date)
  dropped the only live user, session, token, or prefs row. Deletes now
  propagate the engine error; upserts use a single `index_document`.

- **`sort` on an ES meta-field other than `_score` / `_doc` / `_id` resolved to
  `null` on every hit** ([#401](https://github.com/xerj-org/xerj/issues/401)).
  `compute_sort_values` special-cased exactly those three keys and sent
  everything else to `get_field_value(source, field)` — a lookup *inside*
  `_source`. `_seq_no`, `_version`, `_primary_term` and `_index` are engine
  metadata and are never literal `_source` keys, so the lookup missed on every
  document and the whole result set tied on `[null]`. The damage lands on
  keyset pagination: `search_after: [null]` is never strictly greater than the
  next hit's `[null]`, so `sort: [{"_seq_no":"asc"}]` + `search_after` — the
  consistent full-scan pattern used for migrations — either re-reads page one
  forever or stops after it, with no error either way. Measured on a 30-document
  index before the fix: 4 ids collected out of 30, page two empty.

  These fields now resolve through the same version-map accessors that populate
  the response meta-fields (`lookup_seq_no` / `lookup_version`), so a hit's
  `sort` value and its `_seq_no` / `_version` agree. `_primary_term` is the
  constant `1` the hit meta-field already emits and `_index` is the index name;
  both are constant within one index, so they tie like any other constant sort
  key and a client paginating on them still needs a tie-breaker. Sorting on
  `_seq_no` under `index.disable_sequence_numbers` is unchanged — es_compat
  rejects it at the API edge before the request reaches the engine.

  Real sort values are only half of it: the memtable's bounded sort-candidate
  path narrows the heap's input using a doc-values column keyed by the sort
  field, which for a meta-field is derived from `_source` rather than from the
  version map the heap now ranks on. With no such key present it classified
  every buffered document as "missing the field" and returned an arbitrary
  `materialisation_limit`-sized prefix. A correctly ranked page over a wrong
  candidate set is still a silently wrong page, so that path — and the
  segment-side sort shadow — now decline meta-fields and take the full walk.
  The same gate fixes an independent instance of the identical defect on `_id`,
  which already resolved to a real sort value: over a 2 000-document memtable
  `sort: [{"_id":"desc"}]` returned
  `["d1995","d1989","d1981","d1973","d1961"]` instead of
  `["d1999","d1998","d1997","d1996","d1995"]`.

  There is a third such prefilter, and it is the one the two gates above route
  meta-sorts *onto*: the memtable scan's pre-clone rejection
  (`memtable_primary_key_rejects`) skips a document whose primary sort key,
  read from `_source`, already loses to the full heap. Once the heap ranks on
  version-map metadata, a document carrying a `_source` key of the same name
  is compared against the wrong frontier and dropped before the heap ever sees
  it. Measured over ES-compat HTTP on 2 000 documents where only `d0000`
  carried `"_seq_no": 10000000`: `{"size":5,"sort":[{"_seq_no":"asc"}]}`
  returned `[d0001, d0002, d0003, d0004, d0005]`, omitting the true first
  document, while `"size": 3000` returned it — a silent, size-dependent
  missing document. That path now declines meta-fields too.

  **Behaviour change:** `sort` on `_seq_no` / `_version` / `_primary_term` /
  `_index` now ignores a `_source` key of the same name and always ranks on
  engine metadata. Previously it ranked on the `_source` value where one
  existed and on `null` everywhere else. This is observable because xerj —
  unlike Elasticsearch, which rejects a metadata-named key inside a document
  with a `document_parsing_exception` — accepts `{"_seq_no": 1}` in a document
  body and echoes it back in `_source`. Such a key is now inert for sorting;
  it is still stored, still returned in `_source`, and still usable for
  ordinary queries. Rejecting metadata-named `_source` keys at the API edge is
  deliberately **not** part of this change — xerj writes `_routing` into
  `_source` itself, so the edge rule needs its own design.

- **`xerj autoindex` aborted a whole run when one file declared two or more SQL
  tables** ([#360](https://github.com/xerj-org/xerj/issues/360)). One file can
  feed several datasets — a SQL dump is one file and N tables — but the sealed
  record count was a single whole-file total. Finalisation charged that total to
  *each* of the file's datasets in turn and compared it against that one
  dataset's read-back, so any multi-table file disagreed with itself by
  construction and exited `1` with `dataset t1 exact read-back count 2 disagrees
  with sealed count 4`. The rows were always indexed correctly; only the
  reconciliation arithmetic was wrong, so `exit=1` discarded a complete corpus.
  Reported against a real `unum-cloud/usearch` file — `sqlite/README.md`,
  ordinary prose with three ` ```sql ` blocks in it, which the sniffer routes to
  the `sqldump` family.

  The sealed ledger is now keyed by the dataset identity each record was written
  under (`records_by_dataset` on the prepared artifact,
  `expected_records_by_dataset` on the manifest group), the read-back is scoped
  by `ax_dataset` as well as `ax_file`, and `validate_groups` rejects a ledger
  that names a dataset the group does not feed or that does not sum to the group
  total. Measured against a local node with the fixed binary: the two-table and
  three-table repros from the issue exit `0`, every table's rows are present
  (`_count` 2 per table), and each dataset's catalog document reports
  `record_count: 2` instead of the whole file's total.

  Existing state directories survive the upgrade: both new fields are omitted
  from serialization when empty, so a snapshot or manifest sealed by an older
  version re-serializes to the same bytes and keeps its digest. A pre-upgrade
  group that fanned out has no recoverable split, so it withdraws from that one
  equality rather than failing it — the state directory left behind by the
  aborted repro run resumes and commits on the fixed binary.

- **`xerj autoindex` aborted a run because *another* corpus on the same node had
  already indexed a byte-identical file** — the second of the three aborts
  [#360](https://github.com/xerj-org/xerj/issues/360) reports, and the one that
  killed the reporter's second corpus. A file's identity is derived from its
  CONTENT alone (`content::full_digest`) and the catalog is one global index
  (`autoindex-catalog`) that no `--prefix` namespaces, so two unrelated
  checkouts sharing one file — an Apache-2.0 `LICENSE` is the everyday case —
  share that file's catalog document IDs. A corpus holding the content five
  times publishes a canonical document plus four alias documents; a corpus
  holding it once republishes only the canonical, and the other corpus's four
  alias documents survive under their own `run_id`. Be clear about what that
  does *not* mean: the shared file's canonical catalog document
  (`file:<content_id>`) belongs to whichever run published it last, so after the
  second corpus runs, the first corpus's alias documents point at a
  `duplicate_of` whose canonical entry is now the other corpus's. That is
  pre-existing — the binary before this change overwrites it too, then aborts —
  and this change removes the false abort without repairing it. The catalog
  identity collision itself is tracked in
  [#416](https://github.com/xerj-org/xerj/issues/416). The generation-wide barrier
  counted every catalog document carrying the group's `file_key` — across every
  run on the node — against `1 + aliases.len()` for the group in front of it, so
  the second run exited `1` with `catalog canonical/alias count disagrees with
  desired group axg1-…` on a corpus whose data was complete. The count is now
  scoped to this generation's `run_id`, the same way the run-summary read-back
  already is, so the barrier asks what it exists to ask: did *this* generation
  publish one canonical document and its aliases.

  Measured on the reporter's own corpora, one node, release binary,
  `aarch64-unknown-linux-gnu`. `apache/lucene` and `unum-cloud/usearch` share
  exactly one indexable content identity — the 11,357-byte Apache-2.0 `LICENSE`,
  present 5× in lucene and 1× in usearch. Indexing lucene and then usearch on
  one node, each with its own `--prefix` and `--state-dir`: **before**, usearch
  exited `1` after 41.5s with `catalog canonical/alias count disagrees with
  desired group axg1-f5c714bcb8c945dd3687e83218352c66`. **After**, the same
  usearch run prints `generation 1 committed — 4 datasets, 308 records live` and
  exits `3` (junk recorded, never fatal — `cli.rs` EXIT CODES) in 30.2s, and the
  43 alias documents lucene published are still in the catalog: the barrier
  stopped counting them, it does not delete them.

  This is not what closed #360 — that closed `completed` on 2026-08-15 via #407. The third abort the issue reports — `catalog
  read-back for ds:… disagrees with the sealed generation projection`, which
  ended the full `apache/lucene` run after 1,073s before this change and 663s
  after it — reproduces on this branch and is not an autoindex defect:
  `POST /_bulk` drops explicit `null` fields from `_source` once the request is
  large enough (measured directly with `curl` against this binary: 1,500
  documents / 129,786 B keeps them, 4,000 documents / 349,786 B loses them,
  2,000 documents is where it starts to be mixed), and autoindex's dataset
  documents carry `"time_field": null`, `"time_min": null`, `"time_max": null`
  and `"semantic_field": null`. The exact read-back is the tripwire that caught
  the engine rewriting a document, so it is deliberately left armed rather than
  taught to ignore the difference. Tracked as
  [#415](https://github.com/xerj-org/xerj/issues/415).

- **A lexical query on a `semantic_text` field now says so** (#363). `match`,
  `match_phrase`, `multi_match` and `query_string` against a `semantic_text`
  field score with BM25 over the analysed text and never consult the embedding
  that field was indexed to produce — the response was byte-identical to the
  same query against a plain `text` field, same ids and same scores, so a
  caller had no way to tell which question had been answered. `_search`
  responses now carry an additive `_xerj.hints` entry (`code:
  "lexical_on_semantic_text"`) that names the field, says the embedding was
  not consulted, and gives a ready-to-paste `semantic` query body. Scoring,
  hits and every ES-compat response field are unchanged, and the hint stays
  quiet when the same request already reaches the vector through `semantic`
  or `knn` (including the ES 8.x top-level `knn` block).
- **A `quantization: "scalar8"` (`int8_hnsw`) vector field scored updated
  documents from stale quantized data, silently, forever**
  ([#371](https://github.com/xerj-org/xerj/issues/371)).
  There were **two** write-once caches on this path, and either one alone
  reproduces the reported symptom.

  1. The per-field SQ8 **code store** was populated write-once: the first kNN
     that observed a document computed its codes and nothing ever recomputed
     them — not an update, not a delete-and-reindex, not a merge.
  2. The per-field **codebook** (`Sq8Params`, the per-dimension min/scale pair)
     was fitted from the first <=1000 candidate vectors the field was ever
     scanned with and then kept for the life of the process. A vector written
     afterwards that falls outside that fitted range is *clamped* into it, and
     when the fitted range is narrow the clamped decode is indistinguishable
     from the vector the document used to hold.

  Both were reproduced at the HTTP boundary before the fix, each against a
  byte-identical full-precision control index that performs the identical
  manipulation:

  * **codes** — 8-dimension cosine field, doc `0` starts as the query vector
    and is rewritten with its exact negation. The control drops it as it must;
    the `scalar8` index returned it at `_score` **0.99999774**, the same value
    to the last bit as before the rewrite, with the entire 20-document score
    list unchanged, while `GET /{index}/_doc/0` confirmed `_source` had taken
    the negation. `PUT /{index}/_doc/{id}` and `_bulk` were both affected;
    `_bulk` was untested in the original report.
  * **codebook** — same shape, but over a corpus whose dimension 0 never leaves
    `+1.0`, so it fits the degenerate range `[1,1]`. Doc `0` negated to `-1.0`
    decodes straight back to `+1.0`: still first at `_score` **1.000000**, the
    whole score list frozen. On the workload the issue names as what makes it a
    blocker — re-embedding a corpus after a model change — a codebook fitted to
    the old embedding space clamped 50 re-embedded documents into a 0.002-wide
    score band, destroying ranking outright.

  The fix removes both caches rather than adding invalidation to them. The
  brute-force scan already holds each candidate's live f32 vector — it just
  read it out of `_source` to apply the filter — so it now fits the codebook
  over exactly the candidates it is about to score and quantizes/dequantizes
  each one on the spot, through a new `Sq8Params::encode_into` and
  `Sq8Params::fit_borrowed`, into two buffers reused across the scan (no
  per-document allocation). **No SQ8 state of any kind now outlives a query**,
  so there is nothing left to go stale. Fitting over exactly the set being
  encoded additionally makes clamping impossible on this path rather than
  merely unlikely: every value passed to `encode_into` is inside `[min,max]` by
  construction. `scalar8` still scores from 1-byte-per-dimension codes, so its
  recall profile is unchanged — measured recall@10 **0.998** against the exact
  float32 index on the `vector-quantization` recipe corpus, byte-identical
  scores to before this change on that (unfiltered, <1000-doc, never-updated)
  shape.

  Three defects in the removed objects went with them: unbounded growth (the
  code map had no eviction and no upper bound — one entry per document ever
  queried, for the life of the process), an exclusive write lock taken on the
  query path on *every* kNN over the field, and a codebook that could be fitted
  from an empty candidate set — a single query with a wrong-length vector
  installed an all-zero-scale codec that every later score on that field
  collapsed onto. That last one is now structurally absent rather than guarded:
  a codebook that does not outlive its query cannot be observed by another.

  **Not closed by this change**, tracked in
  [#392](https://github.com/xerj-org/xerj/issues/392):
  * `scalar8` still reads the full-precision vector from `_source` on every
    query, so it **does not reduce resident memory** — it buys the precision
    profile of int8 and nothing else. Making it a memory win means the
    ordinal-addressed, ingest-time code array described in #371, written next
    to the data it describes as Lucene does it.

    The "~4x smaller vector working set" wording has been corrected in every
    place this change could find it, listed exhaustively so the claim is
    checkable rather than taken on trust. **Docs and shipped artifacts:**
    `docs/recipes/vector-quantization.md`, `docs/recipes/README.md`,
    `docs/examples/vector-quantization/quant_demo.py`,
    `recipes/vector_quantization.py`, `engine/xerj.default.toml`,
    `xerj-common/src/config.rs`, `xerj-common/src/types.rs`,
    `xerj-vector/src/quantizer.rs`, `demo/playbooks/ES_COMPATIBILITY.md`,
    `demo/playbooks/STUB_AUDIT.md`,
    `demo/playbooks/USER_FEEDBACK_SCORECARD_RC4.md`. **Published site**
    (`landing/docs/*.html` is the hand-maintained mirror of `docs/*.md`):
    `landing/docs/recipes/vector-quantization.html`,
    `landing/docs/recipes/index.html`, `landing/docs/index.html`,
    `landing/docs/vectors.html`, `landing/docs/config.html` (both the
    reference table and the example TOML), `landing/docs/operations.html` (the
    RAM capacity-planning line), `landing/docs/playbooks/vector-search.html`,
    plus the two `docs-index` search snippets that are duplicated into all 44
    docs pages. **Marketing copy:** `landing/solutions/index.html`,
    `landing/pricing/index.html`, `landing/resources/index.html`,
    `landing/industries/finserv.html`, `landing/agent-search/index.html`,
    `landing/use-cases/ai-search-retrieval.html`, `landing/product.html`,
    `landing/llms-full.txt`.

    **Deliberately NOT corrected here**, because replacing them needs numbers
    this change cannot supply: the interactive capacity model on
    `landing/industries/retail.html` (lines 82/93/177) and the matching row on
    `landing/industries/healthcare.html:188` size a deployment from an SQ8
    vector footprint (e.g. "18 GB (SQ8)" vs "92 GB (float32)"), as does the
    "10 M SKUs ON ONE NODE · SQ8" card at `landing/industries/index.html:123`.
    Those are product claims with a whole page built on them and they are
    wrong until #392 lands; they need replacement figures and sign-off, not a
    silent edit. Also left as-is: the dated review record
    `engine/reports/FEATURE_FAIRNESS_REVIEW_v0.6.0_2026-04-25.md` (lines
    81/272), which is an archive of what was believed on 2026-04-25, and the
    synthetic demo corpus article "Scalar quantization: 4x memory savings for
    free" (`demo/data/generate_ai_kb.py:56`, echoed as sample search output at
    `demo/DEMO_RUNBOOK.md:528`), which is generic industry text in a sample
    knowledge base rather than a claim about XERJ.
  * because the codebook is now fitted per query, a `scalar8` `_score` depends
    on the candidate set — **and that changes the returned order, not merely
    the score.** Each individual score moves by at most SQ8's own quantization
    step (1/255 of the fitted per-dimension range), inside the approximation
    error `scalar8` already carries, but documents whose true scores are close
    swap places, so a caller sees a different ranking. Measured at the HTTP
    boundary against this branch, on a 60-document 4-dimension cosine field:

    - adding `filter: {"term": {"grp": "a"}}`, which removes only the 30
      unrelated `grp:b` documents, returned the same 30 survivors with a
      maximum `_score` difference of **1.976e-05** but a **different order at
      19 of the 30 positions**;
    - the trigger is the candidate set, not the `filter` keyword — indexing
      **one** more unrelated document moved the same 30 by up to **7.100e-06**
      and reordered their top 10;
    - on an **unfiltered corpus over 1000 documents** the scores also differ
      from v1.0.0-rc.17, which fitted the codebook from the first ≤1000
      candidates and cached it for the life of the process. On 1500 documents
      all 40 of the top-40 `_score` values changed (maximum **4.880e-05**) and
      two adjacent ranks swapped. This is a wire-visible change on any
      `scalar8` field with more than 1000 candidates. Below 1000 documents,
      unfiltered and never updated, scores are byte-identical to rc.17 — that
      is the shape the recipe corpus has, and the only shape the
      "byte-identical scores" note above covers.

    None of this could happen on rc.17 once the first query pinned the
    codebook, so it is behaviour this change introduces; it is the price of
    the codebook not outliving the query. It is also **not** the dependency
    Lucene has: Lucene fits per segment at index time, so a Lucene score is a
    function of the index state alone, while here it is a function of the
    query's candidate set as well.
  * `scalar8` continues to disqualify a field from HNSW-served kNN, so opting
    in still costs ANN.

  Cost: measured, but **not resolved**. `ci-test` profile, 4000 docs x 384
  dims, warm, min/p10/p50 of 60 requests, three builds per side. On the
  scan-bound shape (`k=4000`) the two sides overlap and the ordering flips with
  the statistic — best min-of-60 was 22.66 ms on main vs 24.88 ms here, but
  best p10 was 29.26 ms on main vs 25.09 ms here — on a machine that produced
  p50 outliers up to 159 ms from background load. No direction can be claimed
  for that shape from these numbers. The small-`k` shape (`k=10`,
  `num_candidates=100`) was consistently marginally faster here, 0.15-0.16 ms
  against 0.17-0.18 ms on main, which is the per-query `RwLock` read on the
  codec store going away. In principle this change adds one min/max pass over
  the candidate vectors and removes a lock acquisition; both are small against
  the encode/decode/similarity work the scan already does.

- **`_count` no longer reports documents `_search` cannot return**
  ([#362](https://github.com/xerj-org/xerj/issues/362)). `POST /{index}/_count`
  is `_search` at `size: 0`, so the two are supposed to answer the same
  question. On a `text` field they did not. A `text` mapping carries no
  doc-values, so the term-count shortcut had no column to read and answered
  from the segment's FTS term dictionary instead — analysed tokens, tokenised
  and lowercased — while `_search` resolves a `term` on such a field against
  the whole `_source` value. Two oracles, two questions, wrong in **both**
  directions on a flushed segment:

  | `term` query on a `text` field | old `_count` | `_search` |
  |---|---|---|
  | `{"term":{"title":"testsegmentreader.java"}}` | `1` | 0 hits |
  | `{"term":{"title":"quick"}}`, value `"the quick brown fox"` | `1` | 0 hits |
  | `{"term":{"title":"Doc7.java the quick brown fox"}}` — the byte-exact `_source` value | `0` | **1 hit** |

  The shortcut now abandons when a segment has no doc-values column for the
  field, and the ordinary delete-aware scan — the code that produces the hits —
  answers. That shortcut is not `_count`-only: its result also authorises the
  bounded stored scan at `size > 0`, so `hits.total` was affected too. On a
  200,000-document index, with a `term` on a `text` field matching 10,000
  documents, `_search` at `size: 10` returned its 10 hits under
  `"total": {"value": 130}`, and `_count` for the same query answered `0`. Both
  now answer `10000`.

  **This costs real time, and the bill is not only "the wrong spellings".**
  Scan cost does not depend on how the term is spelled, so every `term` count on
  a `text` field with no doc-values now pays a full scan — the byte-exact
  spelling included. Its cost changes exactly as much as a misspelling's does,
  and per the table above its *value* changes too, from an undercount to the
  truth. Measured on this branch in the `ci-test` profile, one segment, 20
  distinct query values per shape to defeat the request cache (median of 20,
  same box, same corpus, only `index.rs` swapped):

  | `_count` shape, 200,000 docs | before | after |
  |---|---|---|
  | `term` on `text`, no doc-values | 4.2 ms | 356 ms |
  | same, byte-exact `_source` value | 4.1 ms (and answered `0`) | 356 ms (answers `1`) |
  | `term` on `keyword`, `doc_values: false` | 21 µs | 45 µs |
  | `term` on `keyword` with doc-values (control) | 347 µs | 216 µs |
  | `term` on `long`, `doc_values: false` | 383 ms | 353 ms |
  | `_search` at `size: 10`, `term` on `text` matching 10,000 docs | 9.4 ms (total `130`) | 225 ms (total `10000`) |

  At 50,000 documents the first row is 2.3 ms → 81 ms. That is roughly 35× at
  50 k and 85× at 200 k, but the box carried load average 70–85 from unrelated
  work throughout, so treat the multiple as order-of-magnitude; the number to
  plan around is the absolute one, about a third of a second per `_count` at
  200,000 documents.

  The middle rows are there to bound the blast radius, and they are the
  reassuring ones. A `keyword` field with `doc_values: false` reaches the same
  abandoned branch, but a `term` on a declared `keyword` field still projects
  into the FTS tree, so the segment scan's own `term_doc_freq` shortcut answers
  it: its **route** changed, its cost class did not, and it stays sub-linear
  rather than becoming a scan. A `long` with `doc_values: false` is unchanged
  because it was already scanning — a field with no FTS dictionary never reached
  the removed fallback in the first place. And every count already served from a
  doc-values column is untouched, which is what the control row shows.

  **One shape's `_search` page changes, not just its total.** With the shortcut
  gone the stored scan no longer stops after materialising `from + size`; it
  walks every match and then picks the page. For an *unsorted* `term` on a
  `text` field with many equally-scoring matches, that changes which documents
  come back — measured over 2,000 identically-scoring documents at `size: 10`,
  the ten ids differ before and after (and the total goes from `80` to `2000`).
  Every score involved is identical, so neither page is more correct than the
  other, but the change is observable. Queries with an explicit `sort` return
  byte-identical pages and identical totals before and after.

  **ES compatibility, stated rather than buried.** On the repaired shape this
  moves `_count` *away* from the Elasticsearch answer while making it
  self-consistent: ES answers `{"term":{"body":"quick"}}` with `1` against
  `"the quick brown fox"`, XERJ used to answer `1` as well — but with zero
  retrievable hits — and now answers `0`, which is what its own `_search`
  returns. A count no page of hits can reproduce is a coincidence, not
  compatibility. What `_search` *should* answer for a `term` on an analysed
  field is a separate, hit-set-moving decision. It was raised as
  [#397](https://github.com/xerj-org/xerj/issues/397), which was closed
  `not_planned` on 2026-08-15 — not abandoned, but consolidated into
  [#423](https://github.com/xerj-org/xerj/issues/423), which is open and carries
  #397's repro as acceptance criteria. #423 is the tracker to watch: it treats
  the cause rather than this symptom — `doc_matches_query` evaluates buffered
  documents against raw `_source` with no mapping and no analyser, while flushed
  documents go through the term dictionary. Until it closes, this shape has no
  sub-linear count: budget for the measured cost above, or avoid `_count` with a
  `term` on an analysed field in a serving path.

  Two things this change does **not** do. It does not touch the other half of
  #423, which consolidated #362 (closed `not_planned` for that reason) — `case_insensitive` accepted and silently dropped on
  `prefix`/`wildcard`, the term/prefix case-folding split #362 reported:
  `{"term":{"title":"TestSegmentReader.java"}}` still answers `1` while
  `{"prefix":{"title":"TestSeg"}}` answers `0`. And it is not the narrower fix
  the issue thread proposed (dropping the lowercase retry the shortcut did on a
  dictionary miss); that was tried and measured, and it leaves the first two
  rows of the first table wrong while additionally making the byte-exact
  `{"term":{"title":"TestSegmentReader.java"}}` count `0` where `_search`
  returns `1`.

## [1.0.0-rc.17] - 2026-08-15

### Added

- **`xerj autoindex` understands Unity projects.** Text-serialized scenes,
  prefabs, `.asset`/`.mat`/`.anim` files become **one record per
  GameObject/Component** (`unity_class`, `unity_class_id`, `file_id`,
  `ref_guids`, `script_guid`, plus the flattened body); `.meta` sidecars become
  a guid↔asset-path table; MonoBehaviour records carry a denormalized
  `script_class`/`script_path` so "which scenes use this script?" is one query.
  Detection is by the `%YAML` + `%TAG !u! tag:unity3d.com` header and never by
  extension, so binary-serialized assets stay junk (enable Force Text
  serialization). Unity's generated directories (`Library/`, `Temp/`, `obj/`,
  `Logs/`, `UserSettings/`) are pruned and recorded only when a sibling
  `ProjectSettings/ProjectVersion.txt` proves the tree really is a Unity
  project.

  Reland of community PR #274 by **@gonchar**, brought up to current `main` and
  corrected — see Fixed below for what changed on the way in.

- **BVH motion capture** — one metadata record per clip (`joints`,
  `joint_count`, `frames`, `frame_time_s`, `duration_s`). The numeric MOTION
  block, which is most of the file, is never read: extraction stops at the
  `Frame Time:` line.

- **`--stub <glob>`** designates files that should be *referenceable but not
  parsed*: each match is indexed as one name-card record and its contents are
  never opened.

- **Push a filtered subset of indices to an external ES-compatible target —
  the single-node WAL tap** ([#320](https://github.com/xerj-org/xerj/issues/320)).
  Nothing pushed data out of the engine before this: `_ccr/*` answered `501`,
  reindex-from-remote was refused up front, and snapshot/restore — scheduled and
  whole-index — was the only export path. A XERJ node used as a lightweight edge
  collector had no way to stream a curated slice of its data up to a central
  cluster.
  - New `[wal_tap]` config section (10 settings, off by default) and a runtime
    REST surface: `GET`/`PUT`/`DELETE /_xerj/wal_tap` and
    `GET /_xerj/wal_tap/_stats`. The allowlist is glob-based and adjustable
    without a restart. That brings the documented config surface to
    **115 settings** (counted by `journey_zero_config`, not by hand).
  - **Every `/_xerj/*` route is superuser-only.** `PUT /_xerj/wal_tap` is a
    whole-node export primitive — `target_url` names any host, `indices: ["*"]`
    names every index, and the tap attaches an operator-supplied
    `Authorization` header — so it is data exfiltration and authenticated SSRF
    in one call. Same reasoning `authz.rs` already applies to snapshot and
    restore: nothing here is index-scoped, so there is no per-index decision to
    make. The reads are covered too — `GET /_xerj/wal_tap` echoes `target_url`
    and `_stats` enumerates the node's whole index inventory.
  - **`target_url` may not carry credentials.** `https://user:pass@host` is an
    ordinary URL that `reqwest` turns into a `Basic Authorization` header — i.e.
    `target_auth` by another spelling, in the one field the API echoes back and
    the boot log prints. It is refused at startup and by `PUT`, and redacted
    everywhere it is rendered.
  - The wire format is `_bulk` and nothing else, so the target can be
    Elasticsearch, OpenSearch, or another XERJ node with nothing installed.
  - **This is not CCR.** It is one-directional, single-node, and independent of
    the post-GA sharding/replication track. `_ccr/*` still answers `501`.
  - The tap adds nothing to the write path: it tails WAL files from disk on a
    timer and takes no lock the writer wants. A poll costs the bytes appended
    since the last one, not the size of the file — the tail reader is a sliding
    window over a `File`, shaped after Lucene's `BufferedIndexInput.refill`
    (`lucene/core/src/java/org/apache/lucene/store/BufferedIndexInput.java:289-317`,
    Apache-2.0). A caught-up poll reads the 16-byte WAL header and stops —
    measured at exactly 16 bytes over a 4,176,200-byte generation, asserted
    through `tail_shard` itself by
    `wal::tests::a_caught_up_poll_does_not_read_the_whole_generation`. (The
    window's 64 KiB read-ahead is deliberately not applied to that header read:
    it was, and it made the figure 65,536 bytes per shard per poll rather
    than 16.)
  - Delivery is at-least-once, with `version_type: external` carrying the WAL
    `seq_no` on every action so a redelivery converges on the same document the
    source holds. Covered against a stub that implements the actual ES external
    versioning rule — absent `_id` is created at any version, present `_id` is
    overwritten only by a strictly greater one, everything else comes back
    `409 version_conflict_engine_exception` — and asserted on both halves: four
    documents accepted on the first delivery, four conflicts and zero extra
    `docs_shipped` on the redelivery, watermark unchanged. Note that XERJ's own
    `_bulk` ignores per-action `version` / `version_type` (only the single-doc
    path honours them), so a XERJ target degrades to last-write-wins by
    arrival; against Elasticsearch or OpenSearch the mechanism is live.
  - `_stats` counts what the **target accepted**, per item, not what was
    rendered into the request. `lag_seq` is derived from the highest accepted
    `seq_no`, and each index carries a `healthy` boolean that is false both when
    sends are failing and when the target answers `200` while applying none of
    the actions inside it — for **any** reason, not only a version conflict.
    A target rejecting every document with `mapper_parsing_exception` is a `200`
    with `errors: true`, so no send fails and the cursor advances past every
    dropped document; `last_item_rejection` in `_stats` carries the target's own
    word for it, because "fix the mapping" and "look for a second writer" are
    opposite responses.
  - Cursor state is flushed once per poll cycle rather than once per index per
    poll; a deferred flush can only cause a redelivery, which the external
    versioning above absorbs.
  - Deleting an index drops its cursor at the moment of deletion, so a
    `DELETE` + `PUT` of the same name inside one poll interval — never observed
    as an absence at a 500 ms default — cannot leave a byte offset pointing into
    a WAL stream that no longer exists. For a delete that happened while the
    node was down, the reader treats a cursor offset past the end of its
    generation as the discontinuity it is (WAL files are append-only) and
    restarts the stream with a reported gap instead of clamping to EOF.
  - WAL retention is deliberately *not* coupled to the tap — a slow remote must
    not be able to fill the local disk. A tap that falls far enough behind loses
    entries and says so: `gaps` in `_stats`, plus a warning per occurrence. New
    `wal_tap.min_retained_generations` (default `0`, unchanged behaviour) buys
    **bounded** slack: it is a floor, not an Elasticsearch-style retention
    lease, so it holds at most `n × storage.wal_max_size_mb` per WAL shard
    however far behind any consumer is. The floor is enforced inside
    `IndexStore::wal_maintain_all_verified` — the prune pass the engine actually
    runs — using the same arithmetic and the same "inside the deletion pass, not
    in its callers" placement as Lucene's
    `KeepLastNCommitsDeletionPolicy.onCommit`
    (`lucene/core/src/java/org/apache/lucene/index/KeepLastNCommitsDeletionPolicy.java:51-58`,
    Apache-2.0). At the default `0` the loop is unchanged: no extra syscall, no
    behaviour change.
  - **The retention floor is live, and the API no longer promises otherwise.**
    `min_retained_generations` is the one `[wal_tap]` setting that does not live
    in the tap — it lives in every open index's `WalWriter`, seeded at open from
    `Engine.config`, an `Arc<Config>` written once at boot and never mutated.
    `PUT /_xerj/wal_tap` therefore pushes it straight onto the live writers
    (`Engine::apply_wal_retention_floor`, after Lucene's `LiveIndexWriterConfig`
    and `IndexFileDeleter.revisitPolicy`,
    `lucene/core/src/java/org/apache/lucene/index/LiveIndexWriterConfig.java:39-126`
    and `IndexFileDeleter.java:516-543`) and answers with
    `retention_floor_applied_to_indices` — the number of writers it reached —
    instead of a warning. `DELETE /_xerj/wal_tap` releases it the same way, and
    `Engine::new` folds the tap's effective configuration into the boot `Config`
    so a store opened after a restart is seeded with the persisted floor rather
    than the file's. Verified over HTTP end to end
    (`xerj-api/tests/wal_tap_retention_floor_is_applied.rs`): after
    `PUT {"min_retained_generations": 3}` an index opened before the call reads
    `3` on all 16 WAL shards, an index created after it opens with `3`, and both
    still read `3` after a restart whose config file still says `0`.
  - A configuration set through `PUT /_xerj/wal_tap` is persisted next to the
    cursors and re-applied over the config file on the next boot, so "runtime
    config, no restart" does not quietly mean "and gone after one".
    `DELETE /_xerj/wal_tap` drops the overlay and reverts to the file — to the
    value the tap kept verbatim at construction, not to a re-read of
    `Engine.config`. The state file holds `target_auth`, so it is written `0600`
    through the same writer the API-key store uses.
  - The unapplied-batch health signal counts **polls**, not `_bulk` requests.
    How many chunks a poll splits into is `max_batch_bytes` arithmetic: at
    `max_batch_bytes = 1` one legitimate at-least-once redelivery of four
    documents produced four all-conflict `_bulk` responses, tripped the
    three-in-a-row threshold on its own, and emitted `healthy: false` with
    `last_error: "… nothing is being replicated"` about a poll in which every
    document was already at the target. The field is now
    `consecutive_unapplied_polls` and rises by at most one per poll; three
    unapplied polls in a row still reports the stall.
  - Every multiply in the retry-backoff computation saturates.
    `max_retry_backoff_secs.max(1) * 1000` overflowed above ~1.8e16: a panic
    under `cargo test`'s overflow-checks that killed the spawned tap task
    silently, and under a release profile a *wrapped* cap that can be
    arbitrarily small — the value `18_446_744_073_709_552` wrapped to a 384 ms
    ceiling, i.e. a retry storm aimed at an already-failing target.
  - **Every `[wal_tap]` numeric knob is range-checked at boot as well as by
    `PUT`,** through one shared `WalTapConfig::check_limits`. The config file
    was the way around the API's validation: `PUT
    {"min_retained_generations": 100}` is a `400` because the knob costs
    `n × storage.wal_max_size_mb` per WAL shard per index, while `xerj.toml`
    took the same `100` in silence and the node then held 100 rotated
    generations per shard forever. Bounds are `poll_interval_ms 50..=60000`,
    `max_retry_backoff_secs 1..=86400`, `min_retained_generations <= 64`, and
    `max_batch_docs` / `max_batch_bytes` / `request_timeout_secs` at least 1.
    Same precedent as `compression.block_size_docs` (#318): an out-of-range
    value is a typo the operator should hear about at boot, not as a disk-full
    page later.
  - XERJ's own `.xerj*` system indices are never shipped, whatever the allowlist
    says, and a wildcard never expands to a hidden index.
  - There is no backfill. A new index is shipped whole; an established one ships
    from the moment it is allowlisted. Seed the target with snapshot/restore
    first if it needs the existing documents.

### Changed

- **BREAKING — `PUT /{index}` now refuses an `analysis` block XERJ cannot
  honour, instead of accepting it and analysing differently**
  ([#204](https://github.com/xerj-org/xerj/issues/204)).
  `AnalyzerRegistry::apply_settings` is total by design (it also runs when an
  existing index is opened, where a hard failure would brick it), so an
  unresolvable tokenizer silently became `standard` and an unresolvable token
  filter was dropped — the index was created, `GET /_settings` echoed the block
  back, and the field was analysed in a way nobody asked for. Index creation
  now validates the block and answers `400` with the exact constructs it cannot
  build. `PUT /{index}/_settings` with an `analysis` block is likewise refused
  (it only ever changed what `GET /_settings` echoed; the registry is built
  once, at create/open, and there is no rebuild path — Elasticsearch also
  refuses analysis updates on an open index). That refusal covers every
  spelling this handler *accepts*, not just the nested one: the flat dotted
  `{"index.analysis.analyzer.x.type": "custom"}` and half-dotted
  `{"index": {"analysis.analyzer.x.type": "custom"}}` forms that ES clients
  routinely send used to walk straight past it into the display copy, and
  `GET /_settings` then echoed back an analyzer that analysed nothing.

  **This rejects settings blocks that XERJ used to accept with a `200`, and the
  ES-YAML conformance suite does not cover them — CI being green is not
  evidence that your index still creates.** What is honoured:

  | | honoured (still `200`) |
  |---|---|
  | `analysis.filter.*.type` | `synonym`, `length`, `shingle`, `asciifolding` |
  | `analysis.tokenizer.*.type` | `ngram`, `edge_ngram`, `pattern` |
  | `analysis.analyzer.*.type` (non-custom) | any name the built-in registry resolves — measured: `standard`, `english`, `whitespace`, `keyword` |
  | `analysis.char_filter` | nothing — the char-filter slot is hard-coded empty |
  | `analysis.normalizer` | nothing — never built |

  Anything else in those positions is now a `400`. There is one important
  exception to read first: XERJ resolves a declared filter or tokenizer by
  **name**, not by `type`, so a declaration whose *name* is a built-in is still
  honoured whatever its type says. Measured, all still `200`:
  `{"english_stop": {"type": "stop", "stopwords": "_english_"}}`,
  `{"lowercase": {"type": "lowercase"}}`,
  `{"stemmer"|"english_stemmer": {"type": "stemmer", "language": "english"}}`
  — the canonical Elasticsearch-docs `rebuilt_english` block among them, which
  has a regression test pinning it. Naming a built-in analyzer on a field
  (`"analyzer": "english"`) needs no `analysis` block at all and is unaffected.

  What now fails is a declaration under a name that does NOT resolve — which is
  the usual ES habit of naming filters after what they do in *your* index.
  Measured blocks that returned `200` in rc.14 and return `400` from this
  release:

  - a `keyword` field's `normalizer` (any);
  - a `char_filter` (any, including `html_strip` and `mapping`), declared
    either under `analysis.char_filter` or as an analyzer's `char_filter`;
  - a custom-named token filter whose `type` is `lowercase`, `stop`, `stemmer`,
    `snowball`, `kstem`, `porter_stem`, `ngram`, `edge_ngram`,
    `word_delimiter_graph`, `trim`, `unique`, `elision`, `decimal_digit`,
    `apostrophe`, `limit`, `truncate`, `pattern_replace`, `keyword_marker`,
    `uppercase` or `reverse` — e.g. `{"my_lowercase": {"type": "lowercase"}}`,
    which never lowercased anything, because the analyzer referencing
    `my_lowercase` found no such filter and dropped it;
  - a custom-named tokenizer whose `type` is `standard`, `whitespace`,
    `keyword`, `classic`, `uax_url_email`, `path_hierarchy`, `char_group`,
    `letter`, `lowercase` or `simple_pattern`;
  - an analyzer whose non-custom `type` is `simple`, `stop`, `pattern`,
    `fingerprint`, or a language other than `english`;
  - a `synonym` filter whose `synonyms` is a bare string rather than an array
    (it built a filter with zero rules).

  **The gate covers every spelling, at create as well as at update.** An
  `analysis` declaration written with flat dotted keys
  (`{"index.analysis.filter.my_lower.type": "lowercase"}`) or half-dotted ones
  (`{"index": {"analysis.analyzer.a.type": "custom"}}`) is the same request as
  the nested form to an Elasticsearch client, and this handler already parses
  dotted keys for other settings (`index.sort.field`). The analyzer registry
  resolves the nested spellings only, so a dotted declaration was accepted,
  echoed back by `GET /{index}/_settings`, and applied to nothing. It is now a
  `400` naming the offending key, on both `PUT /{index}` and
  `PUT /{index}/_settings`. Measured on this release, the same
  `my_lower`/`lowercase` declaration in four spellings — nested, `index.`-
  namespaced, fully dotted, half-dotted — answers `400`, `400`, `400`, `400`;
  a settings body with no analysis in it (`index.number_of_shards`,
  `index.sort.field`) still answers `200`. Nothing already on disk changes
  meaning: the dotted form is refused, not newly honoured, so no existing
  index's analysis is re-derived on reopen.

  **Upgrading:** there is no opt-out flag. Either remove the analysis block and
  name a built-in analyzer on the field (`"analyzer": "english"`), or keep only
  the constructs in the table. The `400` names every offending construct in one
  response, so one round-trip is enough to see the whole list. **Two paths are
  not covered and still accept these blocks silently: an index template's
  `template.settings.analysis` is dropped wholesale before the gate ever sees
  it, and option-level divergence inside a supported type (`synonyms_path`,
  `length.min`, `asciifolding.preserve_original`) is not checked at all.**

- **BREAKING — `PUT /_ingest/pipeline/{id}` refuses processor configuration
  XERJ cannot honour, so a pipeline that used to be acknowledged can now be a
  `400`, and a stored one can start refusing writes**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). This is the same
  change as the "`200 acknowledged` … means we will run this" entry under
  *Fixed* below, stated from the upgrader's side, because it converts `201`
  into `400` for any pipeline using an ES processor or option this build does
  not implement. Read that entry for the full list.

  **What is NOT broken, deliberately.** A definition persisted by a build with
  **no gate** was accepted by whichever build wrote it, and there is no caller
  left to answer `400` at boot. Boot replay therefore compiles *those*
  definitions in a compatibility mode that reproduces the previously shipped
  behaviour for every check this sweep added (unknown `grok` pattern name,
  unknown `convert` target, a bare string where an array is required,
  `set.copy_from`, a processor-level `if`/`on_failure`/`ignore_failure`), so
  **an upgrade alone never turns a running cluster's writes from `201` into
  `400`**. Each one is logged at ERROR naming the pipeline and the offending
  config, and recorded in `Engine::degraded_pipelines`; a re-`PUT` of the same
  definition answers `400` with the same reason, which is the repair path. A
  definition that did not compile before the upgrade either (unknown stage
  type, missing required field) still fails at boot and is recorded unrunnable,
  as before.

  **Which definitions those are is decided by provenance, not by the calendar.**
  A definition THIS build refused at `PUT` time is recorded in
  `<data_dir>/ingest-pipeline-refusals.json` and replayed at *definition*
  strictness, so the refusal survives a restart. Without that marker the replay
  could not tell "written by an older, laxer build" from "written by this build
  and answered `400`", and a plain restart overturned the answer the operator
  had already been given: measured, `PUT` a `{"remove": {"field": "secret"}}`
  pipeline, add an `if` guard to it (refused — every write `400`s), restart the
  node and nothing else, and the identical write answered `201` with `secret`
  dropped from a document the guard EXCLUDES. It covered every check the sweep
  added; only an unimplemented *stage* survived a restart, because the raw ES
  body it stores does not deserialise. This is the same pattern as an index's
  `<index>/analysis-binding.json`: the marker's ABSENCE is the load-bearing
  signal.

  The marker is provenance, not a tombstone. A marked definition that compiles
  cleanly on the new build — because that build implements what the refusing
  one could not — clears its own marker at boot and runs, with an INFO line; a
  successful re-`PUT` or a `DELETE` clears it too. If the file exists but
  cannot be read or parsed, no pipeline's provenance is known, so **every**
  persisted pipeline is replayed at definition strictness and any that fails is
  recorded unrunnable with the file named in the reason — fail closed and
  loudly, rather than silently reopening the hole. **In that state a refusal is
  also not acknowledged:** the file is written whole from the in-memory map,
  which a failed load leaves empty, so the next refused `PUT` would have
  replaced it with a one-entry map and erased every refusal it still held —
  after which it parses cleanly, every earlier refusal reads as "no provenance",
  and the fail-closed behaviour converts itself into the hole it exists to
  close after exactly one process lifetime. `PUT /_ingest/pipeline/{id}` now
  answers `500` naming the file instead, and the file is left byte-for-byte as
  found; move it aside and restart to clear the state. An older binary reading a
  data dir written by this one simply ignores the file; it is a sidecar
  precisely because `cluster_state.json`'s format-1 envelope is
  `deny_unknown_fields` and widening it would break downgrades.

  **The marker survives concurrent refusals, not just sequential ones.** The
  sidecar is rewritten whole, so two refused `PUT`s in flight at once were a
  lost update rather than a merge: each snapshotted the map, and whichever
  wrote second with the older snapshot erased the other's marker — while both
  callers were told `{"acknowledged": true}`. Measured at a live node against
  the first cut of this work: 40 concurrent refused `PUT`s, 40 acknowledged, 37
  markers on disk, and after a plain restart the three that were lost answered
  `201` and dropped `secret` from a document their `if` guard excludes. The
  snapshot and the write now happen under the same `cluster_state_write` mutex
  that `flush_cluster_state` has always taken. Measured on this release: 40
  concurrent refused `PUT`s, 40 acknowledged, 40 markers on disk, plain restart,
  40 of 40 still `400`.

  The same rule applies one level up. Teaching the analysis resolver the
  canonical `settings.index.analysis` nesting (see the analysis entry above) is
  a fix at index *create* and a data bug at index *open*: `settings.json` keeps
  the caller's nesting verbatim, so an index created before this release with
  the canonical shape has postings that were written with `standard`, and
  honouring the declaration on reopen would tokenise every new write
  differently from everything already on disk. An index now records the
  analysis binding it was created with (`<index>/analysis-binding.json`) and
  keeps it for life. An index with no marker and analyzers declared *only*
  under `index.analysis` keeps the legacy binding if it holds documents — with
  an ERROR naming the index and saying a reindex is the repair — and adopts the
  canonical binding if it is empty, recording that so a later restart cannot
  disagree.

### Performance

- **A field mapped `dense_vector` — and the undeclared `_chunks` companion the
  passage pipeline writes beside it — no longer gets a full-text term
  dictionary** ([#328](https://github.com/xerj-org/xerj/issues/328)). Both
  halves are named in that headline because the companion is where most of the
  bytes are: of the 39,796,380 B removed below, **26,518,005 B (67%) is
  `<seg>.emb_chunks.fst`**, and in that corpus `emb_chunks` is not in the
  mapping at all — it is matched by NAME, not by anyone declaring it
  `dense_vector`. The field was fed
  through the lexical indexing path and given an FST + postings that no query
  path can read: kNN is served by `hnsw/graph.bin`, `exists` from
  `_source`/doc values, and `_field_caps` and highlighting never open the
  field's term dictionary. `extract_field_text`'s array arm joined the
  components with spaces, so a 128-dim vector became one enormous
  decimal-string term per document.

  Measured on a 5,000-doc × 128-dim corpus (text + keyword + long +
  `dense_vector` + its `_chunks` companion), after `force_merge(1)`, summing
  every file in the data directory except the write-ahead log:
  **54,068,549 B → 14,251,975 B (−73.6%, 3.79×)**. Per file, `<seg>.emb.fst`
  13,256,659 B → 0, `<seg>.emb.post` 10,834 B → 0, `<seg>.emb.norms` 48 B → 0,
  `<seg>.emb_chunks.fst` 26,518,005 B → 0, `<seg>.emb_chunks.post` 10,834 B → 0,
  while the three lexical `.fst` files (`body` 278 B, `cat` 53 B, `n` 209 B) are
  byte-identical on both sides.

  `.wal` is excluded from that total because its tail is reclaimed
  asynchronously and it is not reproducible: on the 300-doc fixture the
  committed regression test uses, runs put the *whole directory* anywhere
  between 858,147 B and 1,105,001 B while the durable bytes moved by ~500 B. The
  test prints both and asserts on the durable one. It reports
  **3,249,323 B → 857,265–857,769 B** for that fixture (before-side measured by
  reverting `index.rs` and `memtable.rs` to the merge base `ca4d75a` on the same
  fixture), which is the same −73.6% at 1/17th the corpus. Eight runs of the
  committed test on this revision printed 857,265 / 857,529 / 857,533 /
  857,545 / 857,549 / 857,657 / 857,657 / 857,769 B, a 504 B spread; the ceiling
  it asserts is 1,500,000 B, three orders of magnitude clear of that jitter and
  still 1.7 MB below the before-side.

  A `dense_vector` nested under an object mapping is covered too — its
  components were landing in the *parent's* term dictionary, because the segment
  builder flattens the whole object into one text field. On the nested fixture
  `<seg>.passages.fst` goes 398,558 B → 45 B, for both mapping shapes that reach
  `FieldType::Vector` (a dotted top-level `passages.vec`, and a `vec`
  sub-mapping under a `passages` object).

  The exclusion covers the whole family a vector field generates, not just the
  base name: `<field>_chunks` (the per-document multi-vector) and
  `__xerj_passage_meta__<field>` are excluded on the same walk that decides
  which fields get an HNSW graph. The companion is the bigger half —
  26,518,005 B against `emb.fst`'s 13,256,659 B, 49% of the whole pre-change
  index — so a base-name-only exclusion would leave more behind than it removes.

  **Two rules, and they do not have the same strength.** The base field is
  excluded BY DECLARED TYPE (`dense_vector`, unconditionally). The two
  companions are excluded BY NAME, because they arrive in `_source` with no
  mapping entry behind them, so there is no declared type to read — and a
  name-based rule YIELDS to any declaration it finds. **If you have mapped your
  own field called `<vector>_chunks` or `__xerj_passage_meta__<vector>` as
  anything at all other than `dense_vector` — `text`, `keyword`, `long`,
  `integer`, `short`, `byte`, `unsigned_long`, `double`, `float`, `half_float`,
  `scaled_float`, `date`, `date_nanos`, `boolean`, `ip`, `geo_point`, `binary`,
  `object`, `nested`, or a type string XERJ does not recognise — nothing about
  it changes**: it keeps its term dictionary, every query naming it keeps
  answering exactly what it answered before, and only the `dense_vector` beside
  it loses its postings. The same holds for a field of that name XERJ mapped for
  you dynamically, whatever type it inferred, as long as the value is not itself
  a multi-vector. Measured on
  `{text body, keyword emb_chunks, dense_vector emb}`, 50 docs, half `tenant-a`
  (before → after): `term {emb_chunks:"tenant-a"}` 25 → 25,
  `terms` 25 → 25, `bool{filter:[term emb_chunks]}` 25 → 25,
  `bool{must:[match body],filter:[term …]}` 25 → 25,
  `constant_score{filter:term}` 25 → 25,
  `bool{must_not:[term emb_chunks]}` 25 → 25, `<seg>.emb_chunks.fst`
  54 B → 54 B, `<seg>.emb.fst` 16,713 B → 0. Same with a `text` mapping:
  `match` / `match_phrase` / `multi_match` / `simple_query_string` / `prefix`
  all 25 → 25 and `<seg>.emb_chunks.fst` 71 B → 71 B. A `long` mapping keeps a
  44 B `.fst` and a `double` mapping a 48 B one, both 25 → 25 on `term`,
  `terms`, `range`, `bool{filter}` and `bool{must_not}`. A field of that name
  XERJ mapped for you *dynamically* is exempt on the same rule — measured
  25 → 25 for a `text` one (54 B `.fst`) and 25 → 25 for a `long` one
  (44 B `.fst`).

  **What makes "yields to any declaration" affordable** is that the companion
  is stopped from ever acquiring one. `<vector>_chunks` is user-supplied
  `_source` — an array of float arrays — so dynamic mapping used to register it
  like any other unmapped key and infer `double` from it, producing a mapping
  entry indistinguishable from one you wrote. Dynamic mapping now declines to
  register a `<vector>_chunks` key when a `dense_vector` named `<vector>` is
  already mapped *and* the value is a rectangular, non-empty array of non-empty
  all-numeric arrays — the exact multi-vector shape, and nothing else. A scalar,
  a string, a flat array or a ragged one under that name is your field and is
  mapped as one. Measured on `{text body, dense_vector emb}` with `emb_chunks`
  left out of the mapping, 50 docs: an `[[…],[…]]` value gets no mapping entry,
  `<seg>.emb_chunks.fst` = 0 B and a lexical clause on it answers 0; a `"tenant-a"`
  value is mapped `text`, keeps a 54 B `.fst` and answers 25; a `7` value is
  mapped `long`, keeps a 44 B `.fst` and answers 25.

  Both halves were measured on their own, by disabling each in the tree and
  re-running the committed tests:

  | | `emb_chunks.fst` (300-doc) | durable index | declared `long` sibling |
  |---|---:|---:|---|
  | this release | **0 B** | 857,265–857,769 B | 44 B `.fst`, answers 25 |
  | yield restored to `long`/`double`-excluded | 0 B | 857,445 B | **0 B, answers 0, `must_not` returns 50** |
  | dynamic-mapping refusal disabled | **1,592,118 B** | **2,451,336 B** | 44 B, answers 25 |

  The middle row is the behaviour rc.16 would have shipped; the bottom row is
  the 67% of the saving that an unconditional "any declaration wins" gives up.
  Neither cost is paid here.

  A lexical clause that NAMES a `dense_vector` (or one of its companions) is now
  lowered to `match_none` at plan time rather than falling through to the
  stored-doc scan. That is the correctness half of the change, not only the
  speed half: with the postings removed and no lowering,
  `{"multi_match":{"fields":["emb"],"query":"0"}}` returns **every document**
  instead of none, because the scan renders the float array to text and every
  component contains a `0`. Measured on the corpus above, first call for each
  shape (`main` → postings-gone-without-lowering → this release):

  | query | `main` | no lowering | this release |
  |---|---|---|---|
  | `multi_match {fields:["emb"], query:"0"}` | 0 hits, 0.409 ms | **5,000 hits**, 123.5 ms | 0 hits, 0.012 ms |
  | `multi_match {fields:["emb_chunks"], query:"0"}` | 0 hits, 2.24 ms | **5,000 hits**, 138.5 ms | 0 hits, 0.014 ms |
  | `term {emb: "<component>"}` | 0 hits, 89.2 ms | 0 hits, 186.8 ms | 0 hits, 0.061 ms |
  | `range {emb: {gte:-2, lte:2}}` | 0 hits, 119.8 ms | 0 hits, 134.5 ms | 0 hits, 0.009 ms |
  | `simple_query_string {fields:["emb"]}` | 0 hits, 0.033 ms | 0 hits, 114.5 ms | 0 hits, 0.015 ms |
  | `constant_score {filter: term emb}` | 0 hits, 83.0 ms | 0 hits, 110.3 ms | 0 hits, 0.012 ms |
  | `exists {field: "emb"}` | 5,000 hits, 83.0 ms | 5,000 hits, 108.0 ms | 5,000 hits, 88.5 ms |
  | `match {body: "liquidity"}` | 5,000 hits, 5.18 ms | 5,000 hits, 5.96 ms | 5,000 hits, 4.95 ms |
  | `term {cat: "even"}` | 2,500 hits, 2.32 ms | 2,500 hits, 2.87 ms | 2,500 hits, 2.27 ms |

  `exists`, `knn` and `semantic` on the field are untouched, a mixed `fields`
  list keeps its lexical members, and a `nested` query is deliberately not
  rewritten (its field names resolve against the element, not the root).

  **Four behaviour changes to know about:**

  - **A `dense_vector` (and its undeclared `_chunks` companion) now has no
    lexical surface at all: every lexical leaf naming one *outright* answers 0
    hits.** "Outright" excludes one shape, measured rather than assumed: a
    `fields` entry that is a PATTERN (`["emb.*"]`) is NOT lowered, because
    resolving a pattern needs the expansion universe — dynamic fields included —
    and lowering one that would have expanded onto a real lexical field is the
    silent-zero failure this release goes out of its way to avoid. So
    `{"multi_match":{"query":"0","fields":["emb.*"]}}` still answers 300 of 300
    on the 300-doc fixture, exactly as on `main`: the unprojectable clause falls
    to the stored-doc scan, which resolves the pattern against the source's
    field names and matches the rendered decimals. Unchanged in both columns, so
    not a regression — but not closed either. `["emb"]`, `["emb*"]` and `["*"]`
    all answer 0. Before this release
    such a clause fell through to the stored-doc scan, which renders the float
    array back to decimal text and matches against *that*, so whether a shape
    answered 0 or answered most of the corpus depended entirely on whether the
    rendered decimals happened to contain the probe. This is a CLASS of changed
    answers, not one row. Measured on the 300-doc × 128-dim regression fixture,
    `main` → this release:

    | query | before | after |
    |---|---:|---:|
    | `wildcard {emb: "0*"}` | **300** (every document) | 0 |
    | `fuzzy {emb: {value:"0", fuzziness:2}}` | **300** (every document) | 0 |
    | `wildcard {emb_chunks: "0*"}` | **300** (every document) | 0 |
    | `fuzzy {emb_chunks: {value:"0", fuzziness:2}}` | **300** | 0 |
    | `prefix {emb: "0"}` | 146 | 0 |
    | `prefix {emb_chunks: "0"}` | 146 | 0 |
    | `match_phrase_prefix {emb: "0"}` | 50 | 0 |
    | `term {emb: <exact component>}` numeric | 1 | 0 |
    | `terms {emb: [<exact component>]}` numeric | 1 | 0 |

    Shapes whose probe text never appeared in a rendered float (`term` with a
    string, `match`, `match_phrase`, `regexp`, `range`, `simple_query_string`,
    `query_string`) answered 0 on both sides. ES rejects these queries on a
    `dense_vector` outright and Lucene gives a field with no terms an empty
    scorer, so 0 is the answer this release adopts for all of them. The
    regression test pins the whole class rather than sampling it.
  - **A `bool` that mentions a `dense_vector` can change `_score` and
    `max_score` without changing its hit count.** Dropping the dead clause makes
    the surviving bool projectable onto the inverted index, so BM25 scores it
    where the stored-doc scan used to. Measured on the same fixture:

    | query | hits | `_score` | `max_score` |
    |---|---|---|---|
    | `bool{should:[term emb, term cat:"even"]}` | 150 → 150 | 0.008402659 → 0.6931471 | 1.6931472 → 0.6931471 |
    | `bool{must:[match body], must_not:[term emb]}` | 300 → 300 | 0.008402659 (unchanged) | 1.6931472 → 0.008402659 |

    Both moves are toward consistency — on `main` these queries reported a
    `max_score` that **no returned hit carried**, and the first now scores
    exactly like `{"term":{"cat":"even"}}` on its own, which is what it reduces
    to. Absolute scores are not part of XERJ's compatibility surface, but
    anything comparing scores across a version boundary or asserting on
    `max_score` for a bool that touches a vector field will see this. Plain
    lexical queries are unaffected; the blast radius is bools with a
    `dense_vector` clause in them.
  - **`GET /<index>/_mapping` no longer lists a `<vector>_chunks` field that
    XERJ mapped for you.** The multi-vector companion beside a declared
    `dense_vector` used to be registered by dynamic mapping as `double` (an
    array of float arrays walks to its first scalar). It is not registered at
    all now, so it disappears from `_mapping` and `_field_caps` — the same
    treatment `__xerj_passage_meta__<vector>` already had. Nothing that reads
    `_source` changes; the values are stored and returned exactly as before, and
    `knn` and `semantic` on the vector itself are untouched. A field of that
    name that you mapped yourself, or that XERJ mapped from a non-multi-vector
    value, is unaffected and still listed.

    Aggregating and sorting on that companion still answer `200` — measured on
    the wire against a live node with `emb` mapped `dense_vector` and
    `emb_chunks` unregistered: `terms` returns real buckets, `stats` returns
    `count: 0` byte-identical to the previous release, and `sort` returns sort
    values. Nothing that worked before returns an error now.
  - **Existing indices keep their bloat until a merge rewrites them, and an
    index created by rc.16 or earlier only ever reclaims the base-vector half.**
    This is a write-side rule. Segments written by an earlier release keep their
    `.emb.*` / `.emb_chunks.*` files, and the per-segment `fts_has_field` gate
    reads whichever shape it finds.

    `POST /<index>/_forcemerge?max_num_segments=1` reclaims `.emb.*`. **It does
    not reclaim the companion.** Measured by building an index on an rc.16
    server (`emb` mapped `dense_vector` before ingest, 500 docs × 32 dim with
    `emb_chunks` multi-vectors), then reopening the same data directory with
    this release and force-merging: `.emb.fst` 262,904 B → 0, but
    `.emb_chunks.fst` 525,200 B → 525,355 B, and
    `wildcard {emb_chunks: "0*"}` still answers 501 of 501. The reason is not a
    bug in the merge: rc.16 persisted `emb_chunks: {"type": "double"}` into the
    mapping, and the by-name rule correctly yields to a field you (or an earlier
    release) declared. Since a mapping entry cannot be dropped in place, the
    remediation for an existing index is to **reindex into a fresh index**. On
    the corpus this entry headlines that companion is 67% of the saving, so an
    in-place upgrade recovers roughly a third of the published number.

    The companion rule has an ordering limit of the same kind, and it fails in
    the safe direction: XERJ can only decline to map a companion it can
    recognise, and recognising one needs the `dense_vector` to be mapped
    *before* the documents arrive. Index first and add the mapping afterwards
    and `<vector>_chunks` already carries a dynamic `double` entry, which the
    by-name rule then yields to — so that index keeps the companion's term
    dictionary (measured 9,467 B on a 50-doc × 16-dim fixture) instead of
    reclaiming it. It costs bytes, not answers.

  *Known limitation, filed as [#382](https://github.com/xerj-org/xerj/issues/382):*
  refusing a `FieldConfig` for the companion leaves the pre-existing schema-hash
  throttle asserting the key set is unchanged, so if a later document puts a
  differently-typed value under `<vector>_chunks` it stays unregistered for up
  to 100 documents and queries on it answer zero until the throttle expires.
  Doc-order-dependent and self-healing. The throttle is pre-existing and the fix
  belongs to it.

  *Not changed:* ES rejects `term`/`match`/`range` on a `dense_vector` outright,
  where XERJ answers `200`. This release removes bytes and adopts Lucene's
  zero-hit answer; it does not add the rejection.

### Fixed

- **`200 acknowledged` on an ingest pipeline now means "we will run this", and
  every write path enforces it**
  ([#204](https://github.com/xerj-org/xerj/issues/204)).
  `PUT /_ingest/pipeline/{id}` used to store the raw body first and only
  warn-log a compile failure, so a pipeline that could never run was
  acknowledged and echoed back by `GET`. It now compiles first, with three
  outcomes: compiled → stored and acknowledged; a processor config we refuse →
  `400`; a processor or option this build cannot honour → accepted (as
  Elasticsearch does) but recorded as **unrunnable**, so the write refuses
  rather than storing an untransformed document.

  Defects this surfaced, each of which answered `200`/`201` and did something
  smaller than asked: **every** ES `rename` processor failed to compile (the
  handler emitted `{from, to}`, the plugin requires `{mappings: {…}}`) and
  renamed nothing; ES `append` was executed as `set`, replacing the field
  instead of extending it, and disagreeing with `_simulate`, which implemented
  a real append; an ES `set` processor did not override an existing value where
  Elasticsearch does (the ES→XERJ translation now writes `"override": true`
  explicitly when the processor omits it — the **native** `type: "set"` stage
  keeps its long-standing `override: false` default, because native definitions
  are recompiled from `cluster_state.json` at every boot and flipping the stage
  default would have changed what an already-running cluster's pipelines do to
  documents on a binary upgrade alone); an unknown `pii_redaction` type built an
  empty pattern set and redacted nothing; an unknown `grok` pattern name
  silently became the
  catch-all; an unknown `convert` target left the field unconverted per
  document; array-typed keys given as a bare string were dropped and the stage
  behaved as if unconfigured; and `DELETE /_ingest/pipeline/{id}` removed the
  definition while leaving the compiled pipeline live under the same name.
  ES `convert` `long`/`double` now map to XERJ's equivalent `integer`/`float`
  rather than being refused.

  Two more of the same, at the definition's own edges: a **pipeline-level**
  `on_failure` block (the top-level body key, not the processor-level one) was
  accepted, echoed by `GET`, and never run — the pipeline is now recorded
  unrunnable, exactly as its processor-level twin already was; and `set` with
  Elasticsearch's `copy_from` is reported as an unsupported *option* (accepted
  at `PUT`, refused at the write) rather than as `mapper_parsing_exception:
  missing value`, which told the caller their valid ES pipeline was malformed.

  **The refusal reads the value, not the key.** A processor-level option is
  refused only when what the caller *wrote* asks for behaviour this build does
  not have; writing the value XERJ already implements is the same as omitting
  it. `ignore_failure: false` is Elasticsearch's own default and ES clients emit
  it verbatim — a presence check made that pipeline register and then `400`
  every write through it, while the byte-identical processor without the key
  indexed fine. Accepted: `ignore_failure: false`, an empty `on_failure: []`,
  `ignore_missing: true` (which is exactly what `remove`/`rename`/`convert`/
  `date` already do — they pass the document through unchanged when the field
  is absent), `ignore_empty_value: false`, `allow_duplicates: true`. Refused:
  the opposite value in each case, and any `if` at all. (Modelled on Lucene's
  consume-then-complain: `AbstractAnalysisFactory.getBoolean(args, name,
  default)` at `lucene/core/src/java/org/apache/lucene/analysis/AbstractAnalysisFactory.java:213-217`
  removes the key and returns the default only when it was absent, and
  `ArabicStemFilterFactory` at
  `lucene/analysis/common/src/java/org/apache/lucene/analysis/ar/ArabicStemFilterFactory.java:44-51`
  then throws on whatever is left. Lucene is Apache-2.0, the same licence as
  XERJ; no code is reproduced.)

  **`remove` and `rename` no longer walk around the gate.** Their ES→XERJ
  translation built a fresh config object holding only the translated keys, so
  a processor-level `if` / `on_failure` / `ignore_failure` / `ignore_missing` on
  them was dropped *before* the gate ran: `{"remove": {"field": "secret", "if":
  "ctx.tenant == 'a'"}}` compiled, was acknowledged, and dropped the field on
  every document — while the identical `if` on `set` was refused. Every branch
  of the translation now edits the caller's config instead of rebuilding it.

  **`_simulate` speaks Elasticsearch's vocabulary again.** It walks the
  *stored* pipeline, and once ES `rename` started compiling the stored form
  became XERJ's internal `stages` shape — whose stage names leaked straight into
  the response, so `processor_type` read `field_rename` where ES reports
  `rename`. The stages→processors conversion now inverts both halves of the
  translation, name and config.

- **`GET /_ingest/pipeline[/{id}]` answers in Elasticsearch's `processors`
  vocabulary instead of XERJ's internal `stages` shape**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). Two shapes reach the
  store: the translated `stages` body (everything that compiles) and the raw ES
  body (kept verbatim for a definition XERJ stores but cannot run). Only the
  second was ever ES-shaped, so `GET` spoke XERJ's vocabulary for every pipeline
  that *worked* — measured before this release, an ES `rename` pipeline came
  back as
  `{"p":{"description":"d","stages":[{"type":"field_rename","config":{"mappings":{"a":"b"}}}]}}`.
  This is a wire-visible change on both `GET /_ingest/pipeline/{id}` and
  `GET /_ingest/pipeline`. It also mattered beyond cosmetics: `GET`'s output is
  what an operator edits and `PUT`s back, and a `stages`-shaped body takes the
  handler's pass-through branch — which is exactly how the restart hole
  described under *Changed* was reached in ordinary use.

  `set` is rendered with an explicit `"override"`, always, because it is the one
  key whose default differs between the two vocabularies (Elasticsearch `true`,
  XERJ's native `set` stage `false`). Emitting a bare ES `set` for a native
  stage would claim it overwrites when it preserves. A pipeline created through
  `PUT /_ingest/pipeline` therefore shows `"override": true` even when the
  caller omitted it — that is the value XERJ applies, and it round-trips: PUTting
  `GET`'s own output back cannot silently flip the stage's behaviour. Relatedly,
  `_simulate`'s `set` interpreter honoured no `override` at all and always
  overwrote, so it reported a document the ingest path would never produce; it
  now applies Elasticsearch's rule (default `true`; `false` leaves an existing
  non-null value alone).

  **This is also the only read endpoint for a pipeline created through
  `PUT /v1/pipelines/{name}`** — the native surface registers no `GET` — so the
  round trip has to be lossless in xerj's own vocabulary too, and it was not.
  `PUT /_ingest/pipeline/{id}` rebuilt the stored definition as
  `{description, stages}` and dropped every other top-level key, so PUTting
  `GET`'s own output back silently discarded a natively-defined pipeline's
  `on_error` and `timeout_ms`: the error policy reverted from `pass` (keep the
  document) to the `Drop` default (discard it) under a
  `200 {"acknowledged": true}`. The translation now edits the body instead of
  rebuilding it — the same rule already applied to `remove`/`rename` one level
  down. Measured on this release, `{"description":"n3","on_error":"pass",
  "timeout_ms":250,"stages":[…]}` reads back with both keys intact, and PUTting
  that body back leaves them intact, before and after a restart. Elasticsearch's
  own pipeline metadata (`version`, `_meta`, `deprecated`) rides along the same
  way and is now echoed back rather than dropped. `PUT /v1/pipelines/{name}`
  given that ES-shaped body used to answer `500` with `internal error: missing
  field stages`; it now answers `400` naming `/_ingest/pipeline/{name}` as the
  endpoint that accepts it.

- **Four wire-visible refusals that were fixed in this sweep but not written
  down** ([#204](https://github.com/xerj-org/xerj/issues/204)). All four
  replace a silent fallback with an error, which is the point of the issue, but
  each changes a status code:
  `POST /v1/admin/backup` answers `400` on a request body that is present but
  unparseable, where it previously ran a *default* backup under a success
  response; `GET /{index}/_stats` and `GET /_all/_stats` carry an added
  non-Elasticsearch key `primaries.mappings.schema_persist_failures` inside an
  otherwise ES-shaped response; the authorization layer's response pruning
  answers `500` when it cannot parse the body it was asked to filter, instead
  of falling back to the unfiltered bytes; and the bundled console's
  `DELETE /_xerj-console/api/v1/auth/passkeys/{id}` propagates a failed delete
  instead of discarding it — it answered `204 No Content` unconditionally, so a
  revoke refused by the engine (index write block, disk flood stage, storage
  error) reported success while the passkey stayed on disk and kept
  authenticating. An already-absent credential is still a `204`; only a genuine
  failure now surfaces.

  **The refusal now covers every ingest path, not only `PUT /{index}/_doc`.**
  `POST /_bulk?pipeline=`, `index.default_pipeline` under `_bulk`,
  `_reindex` `dest.pipeline` and `POST /{index}/_update_by_query?pipeline=`
  previously read the instruction and dropped it — a redaction pipeline that
  the single-document endpoint refused was ignored in bulk, and the document
  was stored verbatim under `errors: false`. All four now run the pipeline, and
  refuse when it cannot run. `_reindex` and `_update_by_query` validate the
  name before touching a single document, so a bad pipeline fails the request
  instead of half of it.

  Two related honesty fixes on the same paths: `_clone` / `_shrink` / `_split`
  copied at most 10,000 documents and swallowed every per-document write error
  before answering `{"acknowledged": true}` (now full keyset paging, and any
  failed write — or a failed source flush — fails the request); and a failed
  `_bulk` item no longer reports `"result": "deleted"` alongside its own
  `status: 400`, which claimed a deletion that never happened.

  **`ignore_missing: false` is refused on purpose, and it is not the same call
  as `ignore_failure: false`.** Both are Elasticsearch defaults that ES clients
  emit verbatim, so the difference is worth stating plainly: the rule is *accept
  the value this build actually implements*, and the two land on opposite sides
  of it. `ignore_failure: false` means "a failing processor is not ignored",
  which is exactly what XERJ does, so it is accepted. `ignore_missing: false`
  means "FAIL the document when the field is absent", which XERJ cannot do at
  all — `ProcessAction` has no error variant, and every field-reading stage
  passes the document through untouched. Accepting it would be precisely the
  silent lie this issue exists to remove, so it is refused: a `remove` /
  `rename` / `convert` / `date` processor that spells out `ignore_missing:
  false` registers and then `400`s every write through it. **Omitting the key
  is accepted** and behaves as `true`. That asymmetry — omission accepted,
  the ES default written out refused — is deliberate but is a hard `400` for
  ES-generated pipelines; strip the key, or set it to `true`, to state what
  XERJ will actually do. `ignore_empty_value` is the mirror image: omitting it
  or writing `false` is honoured exactly, `true` is refused.

  **Known gaps, stated rather than closed:** `_simulate` interprets the stored
  ES body directly and covers processors the compiled path does not, so a
  pipeline recorded as unrunnable can still show a transformation under
  `_simulate` that ingest refuses to perform; `GET /_ingest/pipeline/{id}`
  carries no runnability signal — it renders the stored definition in ES
  vocabulary whether or not this build can run it, so an operator only learns
  at write time; a write addressed to an **alias** does not pick up the
  backing index's `default_pipeline`; `_update_by_query` reports a `drop`
  processor as a per-document failure, because it updates in place and has no
  delete path; `Engine::degraded_pipelines` is in-memory only and has no
  HTTP surface yet, so a boot-replay degradation is visible in the startup log
  and nowhere else — unlike a definition-time refusal, which is now durable
  (see *Changed*); and a refusal marker is only read back at boot, so a data
  dir opened by two processes at once is outside what the sidecar can promise
  (the node lock already prevents that).

- **Unity assets were sampled through a 4 MiB cap, silently junking whole
  object classes.** Unity YAML is a grouped family — each `unity_class` is its
  own dataset and a class's first document can sit anywhere in a scene that
  `extract/unity.rs` itself says can exceed 200 MB. A class first appearing
  past the cap was never sampled, got no entry in the plan, and phase B then
  had nowhere to route its records: they became `file_junk` with nothing said.
  The cap is now 512 MiB for this family, **and** any record whose group was
  never sampled is now reported by name and count instead of silently counted
  as junk.

- **A `script_guid` that failed to resolve produced no counter, no warning and
  no report line.** `build_unity_guid_map` discarded `extract_meta`'s `Result`,
  so an unreadable `.meta` was indistinguishable from a script nothing
  references — on the feature's own headline query. Unreadable sidecars,
  sidecars carrying no usable guid, and unresolvable `script_guid`s are now
  each reported.

- **`script_path`/`script_class` were mapped only when the phase-A sample
  happened to contain a `script_guid`**, while phase B stamps them whenever a
  guid resolves. A cluster whose sampled window held no `m_Script` therefore
  got them dynamic-mapped at index time, feeding the field-budget overshoot in
  #312. They are now registered for every Unity cluster, and the enrichment
  runs *before* `coerce_record` rather than after it, so the fields it stamps
  are validated like every other field instead of bypassing coercion.

- **`build_unity_guid_map` ran serially, unmetered, on the critical path of
  every run** — including a resumed no-op incremental, which otherwise has no
  work to do. A real Unity project has 10k-500k `.meta` sidecars. It now runs
  on `crate::pool` under the progress meter (the unattributed-stretch pattern
  of #241).

- **A `--stub` file was named from the content path**, which under durable
  preparation is a content-addressed blob — so every stub's one and only field
  would have been titled after a blob ordinal (the #294 failure class).
  It now uses `Sniffed::logical_name`.

- **`order::band_from_family_str` disagreed with `order::band`.** The string
  form's catch-all ranked `binary` as `Bulk` where the enum form ranks it
  `Vendored`, so a resumed run could order work differently from the run that
  planned it. Both now agree for every family, pinned by a test that iterates
  the whole enum.

- **The autoindex use-case README documented three flags and behaviours that
  do not exist.** `--no-default-excludes` and `--no-gitignore` are spelled
  `--no-default-ignores` and `--no-ignore`, and `cli::parse` hard-errors on an
  unknown argument — so a reader who copied the documented invocation got an
  error instead of an index. The marker-gated pruning of `node_modules/` and
  `target/` was described but had been dropped from the branch. A test now
  parses every `--flag` named in that README and fails if the CLI would reject
  it.

- **`stub_matcher_tests::an_invalid_pattern_fails_loudly_at_startup` did not
  test its own name** — it asserted only that a *valid* pattern compiles.
  `glob_to_regex` escapes every metacharacter, so no glob can produce a
  syntactically invalid regex; the one reachable failure is the compiled-size
  limit, which `?` and `**` reach at ~10^5 characters. Over-long patterns are
  now rejected explicitly with a message naming the flag and the limit, and the
  test exercises that path.

### Changed

- **Two proposed byte-statistics binary heuristics were dropped before they
  shipped; raw TGA is detected by its header instead.** Both were attempts to
  recognise a raw texture that decodes into printable characters, and both
  turned out to be tests for "not written in Latin script".

  The first, from PR #274, classified any text over 4 KiB with under 5%
  whitespace as binary. `nonblank` is built from `text.lines()`, so newlines
  are already stripped and only intra-line whitespace counts — which means
  Chinese, Japanese, Korean, Thai, Lao, Khmer and Burmese prose, base64 blobs,
  FASTA sequences and minified single-line files all score 0%. This repo's own
  `failure_resume_http_tests::legacy_key_collision_fails_before_visibility_with_scoped_guidance`
  builds a 65,537-byte fixture of one repeated ASCII letter; with that guard
  reinstated the test fails with `left: 3, right: 0` — the run exits 3 instead
  of 0.

  The second was introduced by the first attempt to reland #274 and is
  **removed here**: "decoded via lossy windows-1252 AND over 30% non-ASCII is
  pixel soup". windows-1252 is the fallback every legacy 8-bit codepage decodes
  through, so it is not a test for image data. Measured through `sniff()` on
  `ca4d75a` versus that branch, with identical fixtures:

  | fixture | `ca4d75a` | with the guard |
  |---|---|---|
  | windows-1251 Russian, 13,000 B | `txt-prose` | `binary` |
  | KOI8-R Russian, 13,000 B | `txt-prose` | `binary` |
  | windows-1253 Greek, 12,200 B | `txt-prose` | `binary` |
  | windows-1255 Hebrew, 11,000 B | `txt-prose` | `binary` |
  | windows-1256 Arabic, 10,800 B | `txt-prose` | `binary` |
  | Shift-JIS Japanese, 22,401 B (byte pad 1; likewise pad 3) | `txt-prose` | `binary` |
  | Shift-JIS Japanese + ASCII code, 13,360 B | `txt-lines` | `binary` |
  | Shift-JIS Japanese + ASCII code, 66,800 B | `txt-lines` | `binary` |

  `scan_file` turns `Family::Binary` into `junk: binary content (unknown)`, so
  each of those files stopped being indexed. A `looks_like_legacy_cjk` escape
  hatch shipped with it and could not carry the weight: single-byte codepages
  never form valid Shift-JIS/GBK/Big5/EUC-KR double-byte pairs, so Cyrillic,
  Greek, Hebrew and Arabic were never rescued at all; it required a *lossless*
  trial decode while `sniff()` sees only the first 8192 bytes, so the same
  Japanese document was text at byte-offset pads 0 and 2 and binary at pads 1
  and 3; and a realistic Japanese technical document (prose around ASCII code
  fences) sits below its 30% ideograph floor while sitting above the guard's
  30% non-ASCII ceiling. Every fixture in the table above now classifies
  exactly as it does on `ca4d75a`.

  What replaces it is `looks_like_tga_header`: TGA has no magic number, but its
  18-byte header is constrained enough that bytes 1 and 2 of the file must both
  be control characters. **Headerless raw payloads (`.raw`, `.bytes`,
  uncompressed PCM) still classify as text**, exactly as they do on `ca4d75a`.
  Bounding what a magic-less binary costs is the fix for that and is not
  attempted here — see [#381](https://github.com/xerj-org/xerj/issues/381);
  `for_each_section` already streams, so the unbounded quantity is the record
  count, not resident memory.

- **The new magic-byte signatures now require structural confirmation.** Nine
  signatures (PSD, both TIFF byte orders, RIFF, OGG, FLAC, MP3, FBX and EXR)
  were added by the first reland attempt as bare `starts_with` tests. **Six** of
  them are entirely printable ASCII — `8BPS`, `RIFF`, `OggS`, `fLaC`, `ID3` and
  the 18 letters `Kaydara FBX Binary` — so they matched ordinary text. Measured
  through the real `sniff()` on `ca4d75a` vs this branch with identical
  fixtures:

  | fixture | `ca4d75a` | first reland |
  | --- | --- | --- |
  | CSV whose first column header is `ID3` | `csv` | `binary`/`mp3` |
  | prose opening `RIFF is a container format used by WAV files. …` | `txt-prose` | `binary`/`riff` |
  | prose opening `OggS pages carry the packets of an Ogg stream …` | `txt-prose` | `binary`/`ogg` |
  | prose opening `fLaC is the four byte magic of a FLAC audio …` | `txt-prose` | `binary`/`flac` |
  | prose opening `8BPS is the magic of an Adobe Photoshop …` | `txt-prose` | `binary`/`psd` |
  | `.md` note opening `Kaydara FBX Binary is the 20-byte magic …` | `txt-prose` | `binary`/`fbx` |

  Each of the six is now qualified by what must follow it: PSD version, RIFF
  FORM type, Ogg stream-structure version, FLAC block type, ID3v2 major version
  and synchsafe size, and for FBX the full 23-byte header (`Kaydara FBX
  Binary`, two spaces, NUL, `0x1A`, `0x00`). The FBX case was missed by the
  first pass at this fix and caught in review — at 18 printable characters it is
  the likeliest of the six to open a real sentence, and likeliest precisely
  inside the Unity/3D-asset corpus this feature targets. Every fixture in the
  table above now classifies exactly as it does on `ca4d75a`. The true positives
  are unchanged and still covered; the FBX true-positive fixture was corrected
  to the real 23-byte header, which is what any Autodesk tool emits.

  This is what Lucene's `CodecUtil.checkHeader` does with `CODEC_MAGIC`
  (`lucene/core/src/java/org/apache/lucene/codecs/CodecUtil.java:183`, which
  hands straight to `checkHeaderNoMagic` at `:202` and refuses the file unless
  the codec name *and* a version in range follow — the magic alone is never
  taken as proof). Apache-2.0, same licence as XERJ; adapted, not copied.

  **Not fixed here, deliberately:** `GIF8` and `BM` are printable-ASCII
  signatures with exactly the same defect, but they are **pre-existing** — they
  are unchanged from `ca4d75a`, and prose opening `GIF8`/`GIF89a`, a CSV whose
  first column header is `GIF8`, and prose opening `BM` were each measured to
  classify as `binary`/`gif` and `binary`/`bmp` on `ca4d75a` **and** on this
  branch, identically. Fixing them changes behaviour unrelated to Unity, so it
  is filed as **#380** rather than smuggled in here, and today's wrong answers
  are pinned by `gif8_and_bm_are_still_taken_on_faith` so #380 has to flip that
  assertion deliberately. `GIF8` is listed alongside `BM` so the next reader
  does not conclude `BM` is the only one.

- **A junk entry could overwrite a successfully indexed file's catalog row.**
  When phase B met a record group phase A never sampled, the worker pushed a
  whole-file junk entry while leaving `send_err` unset — so the file also
  reached `journal.file_done`, and both passes wrote `catalog::file_doc` under
  the same `file:{file_key}` id into the same bulk. The junk document landed
  second and won: a file that indexed records was reported in the catalog as
  status `junk` with `records: 0`, `files_junk` counted it, and
  `CodeCoverage::observe` ran for it twice. Reachable with no Unity involved,
  via a >64 MiB SQL dump whose first row for some table starts past
  `SQLDUMP_SAMPLE_LIMIT`. The unsampled group is now reported on the progress
  meter instead, the dropped records stay on that file's own completion where
  they already were, and the disjointness the code claimed in a comment is now
  enforced at the one place that holds both sets (`shadowed_junk_entries`).

### Known issues

- `ref_guids` (Unity) and `joints` (BVH) are multi-valued keyword arrays, and
  #332 means array elements are joined into one FTS token, so a post-flush
  `term`/`match` on a single element does not hit. `script_guid`,
  `script_path` and `script_class` are single-valued and unaffected, so the
  documented "which scenes use this script?" query is not impacted. Fixing
  #332 is an engine-side change to the FTS writer input type.

- **`GIF8` and `BM` are still unqualified printable-ASCII magic signatures**, so
  a text file whose first characters are `GIF8`, `GIF89a` or `BM` is classified
  `binary` and junked. This is **pre-existing, not introduced here** — measured
  identical on `ca4d75a` and on this branch (`binary`/`gif` and `binary`/`bmp`
  in both) — and is left unchanged because fixing it is a behaviour change with
  no Unity content. Both names are recorded here on purpose: `BM` is the
  obvious one and `GIF8` is the one a reader would otherwise miss. Tracked as
  **#380**; `gif8_and_bm_are_still_taken_on_faith` pins the current behaviour.

- **A magic-less binary is still sectioned and indexed in full.** The two
  byte-statistics guards this branch removed (they junked non-Latin text) also
  caught NUL-free binary payloads that decode printable under the
  windows-1252 fallback. Nothing replaced them except `looks_like_tga_header`,
  which covers TGA only. Measured on this branch: a 4,194,495-byte printable
  NUL-free blob named `texture.bytes` sniffs `txt-prose` and expands into 2048
  indexed records. This is **not** the peak-RSS bug from #239 —
  `for_each_section` is still streaming and per-file memory is still bounded;
  the unbounded quantity is the record COUNT. Removing the guards was still
  right: they deleted CJK, Cyrillic, Greek, Hebrew and Arabic documents
  worldwide. Tracked as **#381**.

- **`autoindex` aborted permanently on a corpus containing a byte-identical
  duplicate** (#345). A duplicate path is the only thing that makes a run issue
  the catalog's duplicate-alias `_delete_by_query` at all, and that call was
  fatal. Because it runs *after* every document is durably indexed and after the
  per-file journal has committed, a 5xx there turned a fully successful run into
  `xerj-done ok=false exit=1 reason=aborted` — and it stayed that way: the
  journal was complete, so every rerun indexed zero files and aborted on the
  same line. The sweep is metadata-only bookkeeping and is now reported instead
  of fatal: the run finishes `ok=true exit=3
  reason=catalog-alias-sweep-failed`, names the paths whose alias document was
  not swept, and records `catalog_alias_sweep_failed_paths` /
  `catalog_alias_sweep_error` on the run document. Exit `3` already means
  "completed, and something is recorded rather than fatal", so no new exit code
  was introduced — read `reason` to tell it apart from `completed-with-junk`.
  The catalog *bulk* that follows the sweep is still fatal, so a catalog index
  that cannot be written still fails the run.

- **Any 429/5xx from the server was reported as a bare status line.** The retry
  wrapper kept `HTTP 500 Internal Server Error` and discarded the response body,
  which is the same sentence for a poisoned index, a full disk and a panicking
  handler — the reason #345 was filed as "not investigated". A bounded prefix of
  the server's own `error.type: error.reason` (or the raw body, for a proxy's
  error page) now travels with the status on every `with_retry` call site.

- **The duplicate-alias sweep issued one HTTP request per duplicate path.** It
  now names up to 1024 paths per request with a `terms` filter, so a corpus with
  K duplicates costs `ceil(K/1024)` round trips instead of K — and has
  proportionally less surface on which to fail.

## [1.0.0-rc.16] - 2026-08-13

Both rc.15 known issues (the progress-stream forgery and the outer-`.gitignore`
reach into nested checkouts) are fixed in this release — see Fixed below.

### Performance

- **An idle node with many indices burned ~4 CPU cores and made the host
  machine unusable.** `IndexStore` spawned one OS thread per index that woke
  every `wal_batch_ms` (default 100 ms) and locked *every* WAL shard of that
  store to ask whether it was dirty. With the default 16 ingest shards that is
  ~1.5M lock operations per second on a 9,382-index node, for an answer that is
  always "no" on an idle index. WAL fsync is now scheduled by one process-wide,
  event-driven pool (`(cores/2).clamp(2,8)` threads, spawned lazily): a shard
  arms itself when a write arrives, and **an index with no pending writes costs
  nothing — no thread, no wakeup**.

  Measured on a real 9,382-index corpus, idle, with zero clients (10 samples ×
  15s): threads **9,709 → 339**; CPU **718-760% → 59.7-68.3%**; context
  switches **~197,000/s → ~4,000/s**; interrupts **~142,000/s → ~7,600/s**.
  Thread count no longer scales with index count — 20 indices versus 200 is 335
  threads versus 335, where it was 365 versus 541.

  Durability is unchanged and was verified with `strace -f -y -tt` so WAL
  fsyncs are distinguishable from snapshot fsyncs: `sync` fsyncs before ack,
  `batched` issues exactly one fsync inside the `wal_batch_ms` window, `async`
  issues none, and `kill -9` plus restart recovered every acked document in all
  three modes. RSS is a smaller win and reported as such: marginal 0.699 → 0.682
  MB per index (~12 KB/index of thread stack), since per-index RSS is dominated
  by index state rather than the thread. (#334)

  Not fixed here: ~0.65 core remains on that node from node-global sampling and
  O(indices)-per-tick work on shared timers — a different mechanism, tracked
  separately.

### Security

- **`cargo-audit` and `cargo-fuzz` now actually run in CI — they were only ever
  documented** ([#207](https://github.com/xerj-org/xerj/issues/207)).
  `user-feedback/09-security/cves-and-vulnerabilities.md` listed "`cargo-audit`
  for dependency vulnerability scanning" and "fuzz testing (cargo-fuzz) on all
  input parsing paths" as part of XERJ's answer to Elasticsearch's CVE history.
  Neither existed: no audit job, no fuzz job, no fuzz target anywhere in the
  tree. A reader comparing the two engines priced in a fuzzed parser surface
  that was not there.
  - New CI job `security-audit` runs `cargo audit` on every push and pull
    request; a RUSTSEC vulnerability advisory fails the build.
  - New CI job `fuzz` builds and runs seven libFuzzer harnesses
    (`engine/fuzz/`) over checked-in seeds: the Elasticsearch query DSL, the
    Lucene `query_string` grammar, date math and date-format patterns,
    index-name date math, SQL, Painless scripts, and the aggregation engine's
    two separate script tokenisers. `.github/scripts/fuzz-smoke.sh` is the same
    entry point locally and in CI.
  - `xerj-engine/tests/security_tooling_claims.rs` fails if a documented tool
    stops running, if a fuzz target ships without a seed corpus, or if the
    "all input parsing paths" overclaim reappears in any prose file.
  - The claim itself is now specific about what is and is not covered.
  - **Known gap, stated because the point of this entry is not to overclaim
    again:** seven harnesses are seven parsers, not the engine's whole input
    surface, and one parser outside them is already known to abort.
    `aggs::parse_time_zone_offset` slices a `time_zone` string at byte 2 after
    a *byte*-length check, so a `date_histogram` with `"time_zone": "+中a"` in
    an unauthenticated `_search` body dies on a character boundary. It is
    pre-existing, is not touched by this change, and is tracked in
    [#272](https://github.com/xerj-org/xerj/issues/272) with a fix and a
    proposed harness for the aggregation date parsers.

- **Fixed two high-severity advisories in the document-ingest XML parser.**
  Turning the audit gate on found `quick-xml` 0.36.2 in `xerj-autoindex`, which
  parses `.docx`, `.xlsx` and `.xml` files supplied by whoever owns the folder
  being indexed: RUSTSEC-2026-0195 (unbounded namespace-declaration allocation →
  memory-exhaustion DoS, CVSS 7.5) and RUSTSEC-2026-0194 (quadratic run time on
  duplicate attribute names, CVSS 7.5). Upgraded to 0.41, which also
  de-duplicates the crate — 0.41 was already in the tree via `pdf_oxide`.
  `crossbeam-epoch` moved 0.9.18 → 0.9.20 for RUSTSEC-2026-0204.

### Fixed


- **Following the documented setup and running `xerj autoindex` failed with a
  401.** `AuthConfig::default()` is `enabled: true`, so copying the shipped
  config as the docs instruct produced an auth-enabled server, while the pages
  that show `autoindex` all pass `--insecure` and the pages that tell you to
  write a config never mention a key. `ping()` also discarded the HTTP status,
  so the 401 on the first round trip was swallowed and the run printed ~15 lines
  of healthy-looking progress before dying at the embedding-identity probe with
  a message about the server's *capabilities* — leading the reporter to conclude
  their server lacked a feature rather than a credential. `ping()` and
  `embedding_execution_identity()` now fail fast and actionably on 401/403,
  `autoindex` gained the `admin.key` fallback `brain` already had (loopback
  only), the server sends `WWW-Authenticate: ApiKey realm="xerj"`, and the 401
  body names the real data dir plus `XERJ_API_KEY`, `--api-key` and
  `--insecure`. Reported by a user on macOS. (#333)

- **`xerj-done` reported an identical success line whether or not any source
  code was indexed.** A corpus in which every code file was junked printed the
  same terminal record as a healthy one, which is how a silently degraded
  index went unnoticed. The terminal line and run document now carry
  `code_files`, `code_files_indexed` and `code_files_junked` on both the
  generated and legacy paths, and a warning is emitted before the terminal line
  when code files were detected and none produced an indexed record. Named
  `code_files_indexed` rather than `ast` deliberately: a code file with no
  captured symbols still indexes correctly, so `ast` would overstate. (#294,
  #316)


- **The licence detector was wrong on 2 of the 13 repositories it had
  classified.** `sonic` was recorded as `GPL` when it is **MPL-2.0** — MPL-2.0
  defines "Secondary License" in terms of the GNU GPL inside its own text, and
  restrictive-first *body* matching hit that first. `quickwit` was recorded as
  `Apache-2.0/UNKNOWN` when it is plain **Apache-2.0** — `LICENSE-3rdparty.csv`
  is a dependency inventory, not the project's licence. The classifier now
  reads the title line before the body and skips third-party inventories.
  Elasticsearch's triple licence still resolves to `AGPL` from its title, so
  the safe direction is preserved where it matters most; that is a test.

- **A corpus manifest could write outside the corpus directory and destroy an
  unrelated checkout.** `--from` is documented as "rebuild a corpus someone
  else defined", so a manifest is untrusted input — but only the *corpus* name
  was screened, while the per-repo `repo` field, from the same file, was used
  directly as a path. A manifest with `"repo": "../../../work/repo"` moved that
  checkout to the manifest's commit, discarded its uncommitted changes and
  untracked files (`checkout --force`, `clean -fd`), and rewrote its `origin`
  remote. `repo` must now be a plain directory name, enforced in `xc-corpus.sh`
  and in `validate_manifest.py`, with the destructive path double-guarded.

- **`xc-index.sh` silently ignored an unrecognised argument.**
  `xc-index.sh <corpus> --frsh` set no flag, ran an *incremental* index and
  exited 0. Because `autoindex` keeps incremental state in `~/.xerj/autoindex/`,
  a run that should have been fresh skips every file as "already indexed" and
  leaves a corpus with 0 documents that retrieves nothing, reporting no error
  anywhere. Unknown arguments are now rejected by name.

- **`xc.py` warned about only `GPL` and `LGPL`, by exact match**, so it was
  silent on every restricted repository the hub deliberately ships:
  elasticsearch (AGPL/SSPL/Elastic), meilisearch's BUSL Enterprise Edition
  parts, and — once the detector above was corrected from `GPL` to `MPL-2.0` —
  sonic, which had been warning before. The warning at retrieval time is the
  one that counts, because that is when an agent is looking at the code and
  deciding whether to lift it; it now covers the same set as `xc-corpus.sh`.

- New CI job `reference-coding`: shell and Python syntax, the offline
  round-trip suite (`tools/xerj-code/tests/test_xc_corpus.sh`, 28 checks, local
  git repositories over `file://`, never touching the real `~/.xerj-code`), and
  `validate_manifest.py --hub` over every shipped manifest.

- **A binary that does not understand `cluster_state.json` now fails closed
  before activating storage.** The previous loader accepted future and
  partially-understood shapes, dropped fields it did not know, and could later
  rewrite the reduced document over the original. Boot now accepts only the
  complete shipped format-1 envelope, including duplicate-key and nested-shape
  checks. A rejected document leaves liveness at 200 and readiness at 503, but
  opens no user index, Console system index, WAL, segment, or durable audit
  sink; Console bootstrap and storage background jobs remain disabled until a
  supported restart. After the existing request-size, authentication, and
  authorization gates, HTTP storage access returns the stable
  `cluster_state_unavailable` 503; authenticated gRPC calls return
  `UNAVAILABLE`. Existing 401/403/body-limit precedence is unchanged. Client
  responses give the category and recovery action without a local path or
  persisted object names; the server log carries the detailed classification
  and path for the operator. The original bytes and any legacy staging file
  stay in place, and a blocked boot creates no salvage copy. Format-1 rewrites
  retain the existing
  same-directory temp-file fsync and atomic rename; parent-directory sync is a
  best-effort attempt whose errors are ignored, so this change does not claim
  strict namespace durability across power loss. Data-directory creation,
  `node.lock` acquisition/PID diagnostics, and the server's earlier
  credential/TLS preparation retain their existing behavior; the no-open/no-
  replay claim begins at the cluster-state classification and covers storage
  stores, not those process-bootstrap files.

- **`PUT /{index}/_settings {"index.lifecycle.name": null}` actually detaches
  the index, and the detach survives a restart** (#282, ported from #262;
  regression-tested in [#290](https://github.com/xerj-org/xerj/pull/290)). The
  null previously fell through a string-only settings reader, so the operator
  got `200 acknowledged` while the index stayed attached — to a policy whose
  delete phase then destroyed it. A detach now removes the execution cursor,
  writes a persisted tombstone (`ism_managed_indices.json` grew a
  `managed`/`detached` envelope; the old bare-map file is still read), and
  scrubs the stale `index.lifecycle.name` from the stored settings.
- **The lifecycle delete action gained #262's safety rails.** It now refuses —
  visibly, in `explain`, never silently — to delete a dot-prefixed internal
  index (`.ds-*` backing indices exempt), a data stream's current write index,
  or an index whose age cannot be established from its execution cursor.
- **An ILM policy naming an action the engine cannot execute is refused at PUT
  time with the action named** (400), instead of being stored with the action
  silently dropped — the accepted-and-ignored class (#204). Executable ILM
  actions are `rollover`, `delete`, `readonly`.
- **`autoindex` no longer rejects a resumed generation after journal replay
  when inferred floating-point schema statistics need exact decimal
  round-tripping.** Unchanged reruns and junk-bearing corpus shrink now replay
  correctly; existing affected journals resume without a rebuild, while the
  manifest format and integrity checks remain unchanged.

- **A `query_string` containing one non-ASCII character aborted the process.**
  `{"query_string":{"query":"_\u0660"}}` — or the same text in `?q=` — took the
  server down. The Lucene tokeniser walks bytes but slices `&q[start..i]` as a
  `&str`, and it broke on `is_whitespace()`, which is true for Latin-1 NBSP
  (0xA0). In valid UTF-8 that byte is only ever a *continuation* byte, so the
  scan stopped between the two bytes of U+0660 and the slice panicked on a
  character boundary. Every delimiter test in that scanner is now `is_ascii_*`,
  which makes "the scan can only stop on a character boundary" true by
  construction. Nothing is lost: the only non-ASCII bytes the old test accepted
  were 0x85 and 0xA0, neither of which is a standalone character in UTF-8.

- **An index name of `<{}{}>` aborted the process.** Index-name date math finds
  the first `{` and the last `}` inside the braces; when the `}` comes first,
  `date_part[2..0]` is a panic, not an empty slice. Reachable from any request
  that names an index. The closing brace is now required to come after the
  opening one.

- **A Painless script containing one non-ASCII character aborted the process.**
  The second thing the new fuzz harnesses found, inside a second. The tokeniser
  walks *bytes* (`bytes[i] as char` reinterprets each byte as Latin-1) but
  slices `&src[start..i]` as a `&str`. Every continuation byte of a multi-byte
  character reads as an alphabetic Latin-1 char, so an identifier scan could
  start or stop inside one and the slice panicked on a char boundary —
  `end byte index 24 is not a char boundary; it is inside 'ʋ'`. Scripts arrive
  in ordinary search and update bodies and the release profile sets
  `panic = "abort"`, so this was not a 400, it was the whole server. The
  identifier scan is now ASCII-only, which keeps both ends on a character
  boundary by construction; a non-ASCII byte reaches the tokeniser's
  `unexpected char` error instead. String literals still carry any Unicode.

- **A search body could abort the whole process: `{"range":{"ts":{"gte":"now+33333333333333H"}}}`.**
  The first thing the new `date_math` fuzz target found, 20 seconds into its
  first run. `chrono`'s `Duration::hours` and its siblings **panic** when the
  count does not fit a `TimeDelta`, and they run before the
  `checked_add_signed` that was supposed to make
  `xerj_query::dates::add_unit` total — so the careful checking around them
  bought nothing. A range bound goes straight from the request body into this
  code (`parser.rs:931`), the release profile sets `panic = "abort"`, and the
  result is an unauthenticated remote denial of service on a released binary.
  Fixed with the fallible `try_*` constructors. Grepping for the same pattern
  found **three** more copies in the index-name date-math resolvers —
  `xerj-engine/src/index.rs`, and *both* branches of
  `xerj-api/src/es_compat.rs`: the `now` form (`<logs-{now+9999999999999d}>`)
  and the anchored form (`<logs-{2026-01-01||+9999999999999d}>`), which are
  handled by two different functions sixty lines apart. All three are fixed,
  their `n * 30` / `n * 365` multiplications are `checked_mul`, the unit is read
  as one character rather than one byte, and the addition goes through
  `checked_add_signed`. `es_compat`'s resolver is the one that runs on the wire
  — the index path parameter of every create/index call and the `_search` index
  list — so `crates/xerj-api/tests/index_name_date_math_is_total.rs` now drives
  it at the crate boundary; the same-file fix that missed the anchored branch
  passed because nothing exercised the public entry point.

- **A `_search` aggregation script containing one non-ASCII character aborted
  the process.** `painless::tokenize` was not the only Painless-subset scanner
  in the tree: `aggs.rs` holds two more with the identical byte-walk /
  `&str`-slice shape — `lex_script` (`scripted_metric`) and `tokenize_script`
  (`bucket_script` / `bucket_selector`) — and both took `&src[i..i + 2]` to test
  for a two-character operator at a byte index that can sit inside a multi-byte
  character, because every arm above it is ASCII-only.
  `{"m":{"scripted_metric":{"map_script":"中"}}}` was `end byte index 2 is not a
  char boundary`. Both also skipped whitespace with `is_whitespace()` on
  `bytes[i] as char`, which is true for Latin-1 NEL and NBSP — bytes that in
  valid UTF-8 are only ever *continuation* bytes. The two-character probe now
  requires both bytes to be ASCII (every such operator is ASCII, so nothing is
  lost) and whitespace is `is_ascii_whitespace`. A new `agg_script` fuzz target
  covers all three entry points, which is how the next one gets found instead of
  reported.

- **An XML entity nobody declared no longer discards the text around it.**
  Fallout from the `quick-xml` upgrade above, which is a breaking change to how
  character data arrives: 0.38 stopped folding `&amp;` into the surrounding
  `Event::Text` and began emitting each reference as its own
  `Event::GeneralRef`. Entity handling is preserved across that change (a
  reader that ignored the new event would have silently dropped every `&`, `<`,
  `>` and `&#233;` while the words around them still arrived), and one case
  gets better: 0.36's `BytesText::unescape()` failed the whole text node on an
  entity no DTD declared for us, and `unwrap_or_default()` then discarded it —
  now only the reference itself is dropped. Character data is also reassembled
  from its fragments before being emitted, so an entity-bearing field stays one
  value instead of becoming an array of pieces.

- **`constant_score` on a mixed flushed/unflushed corpus returned the wrong
  page, and any tied hit set could be re-sorted into `_id` order**
  ([#270](https://github.com/xerj-org/xerj/issues/270),
  [#300](https://github.com/xerj-org/xerj/pull/300)). Two defects. A
  top-level `constant_score` (or `boost`) wrapper defeated the match-all
  shortcut, so the stored scan ran in exact-counting mode and a bounded page
  came back all-memtable — `size:1` on 40 flushed + 300 unflushed identical
  docs returned the memtable doc where the oldest flushed doc was correct;
  the wrapper is now peeled for the count/bounds decisions only, with scoring
  unchanged. Separately, five post-sort re-sorts — the bool-text IDF rescore,
  the near-zero-BM25 TF-IDF fallback, and the three `rescore` sorts — tied by
  `_id` alone, so any of them firing on tied scores reordered the page; the
  main sort and all five now route through the one page-order key
  (`score DESC, seq_no ASC, _id ASC`). Known residual, documented rather
  than asserted around: bounded pages for multi-clause bools on a mixed
  corpus are still admitted under first-pass scores (#188's remit).

- **A shrinking file set on a junk-bearing `autoindex` generation aborted the
  cutover with `desired manifest digest does not match sync_begin payload`**
  ([#283](https://github.com/xerj-org/xerj/issues/283),
  [#296](https://github.com/xerj-org/xerj/pull/296)). A fresh plan kept
  duplicate-file aliases whose content was junk or skipped while the
  incremental projection dropped them, so the two disagreed on byte-identical
  junk duplicates — two empty files suffice. The fresh plan now projects
  exactly like the incremental path, so the shrink reconciles cleanly. Where
  a journal is *genuinely* inconsistent, the replay and validation paths now
  say what to do — no remote data changed, re-running will not repair it,
  rebuild with a new `--state-dir` and `--prefix` — with the invariant kept
  as the error cause.

- **An outer repository's `.gitignore` no longer judges files inside a nested
  checkout** (the second rc.15 known issue below;
  [#287](https://github.com/xerj-org/xerj/pull/287), repairing
  [#279](https://github.com/xerj-org/xerj/pull/279)). The rule walk had no
  repository-boundary stop, so a root `*.md` hid the README of every
  vendored dependency beneath it. The git-sourced rule kinds now stop at a
  `.git` boundary — the set kept under a nested checkout is byte-for-byte
  what `git status --untracked-files=all` reports there — while
  `.xerjignore` and the built-in defaults deliberately still govern the
  whole folder you named. Three more defects from the same merge: hidden
  directories pruned by the dotfile rule were deep-counted into
  `ignored_files_in_pruned_dirs` (97,731 phantom files on this repository's
  own walk); that number reached the `xerj-done` line and `--progress json`
  as a bare total when it is budget-capped — a new
  `ignored_files_in_pruned_dirs_exact` flag now says whether it is a total
  or a floor; and `--no-ignore`/`--no-default-ignores` were accepted and
  ignored (#204) on `autoindex map` and `status`, which never walk a
  filesystem — both are now refused there with the reason. The shipped docs
  that contradicted the code are corrected.

- **A finished `autoindex` run could leave its state-directory lock held, so
  the immediately following run on the same state dir was refused as
  already in use** ([#305](https://github.com/xerj-org/xerj/pull/305)).
  `flock` ownership follows the open file description, so a helper
  subprocess forked while the journal was live (the PDF extraction helper,
  git invocations) could inherit the descriptor and keep the lock held past
  the owner's close. The lock guard now records the acquiring PID and
  unlocks on drop only in that process — a fork-inherited copy closes its
  descriptor without releasing the live owner's lock. No retry, wait or
  takeover behaviour was added; crash recovery is unchanged. Covered by a
  same-OFD unit oracle and a real-fork integration regression on
  Linux/macOS.

- **`autoindex` indexed a `.mts` or `.cts` file as prose**
  ([#284](https://github.com/xerj-org/xerj/pull/284)). The extension
  registry claimed `.mjs`/`.cjs` but not TypeScript's own ESM/CJS
  counterparts, and an unclaimed extension does not degrade to "code without
  symbols" — it fell through to the prose extractor, so the file carried no
  `language`, no `defs`, no `symbols`, was split into several body-only
  records, and was invisible to symbol search.

- **An exported module `const` reached `defs` in neither TypeScript nor
  JavaScript** ([#285](https://github.com/xerj-org/xerj/pull/285),
  [#304](https://github.com/xerj-org/xerj/pull/304), closes
  [#293](https://github.com/xerj-org/xerj/issues/293)). Capture only fired
  when the value was a function, so a module whose whole public surface is
  built by factory calls — `export const users = pgTable(...)`, a router, a
  config object — extracted zero symbols and could not be found by symbol
  search at all. The new pattern is exported-only, anchored to module scope,
  and matches the `const` token, so function-local bindings and mutable
  exported `let` stay out; module-private `const` was measured and excluded
  because it added weight without retrieval gain.

- **A PHP 8.1 enum extracted zero symbols, and class constants were never
  captured** ([#286](https://github.com/xerj-org/xerj/pull/286)). An enum
  declares no class, interface or trait, so a file holding one could not be
  found by its own name through symbol search. Enums, enum cases and `const`
  declarations — class-level and file-scope — now land in `defs`, with
  cases treated as named constants the way Go's `const_spec` members are;
  `define()` calls stay uncaptured.

### Added


- **`xerj mcp` — the MCP server now ships in the installed binary.** XERJ had a
  complete, tested 10-tool MCP server that no user could obtain: releases built
  only `xerj-server`, the installer shipped only `xerj`, and MCP appeared in no
  agent-facing doc. It is now a subcommand, so anyone who ran
  `curl -fsSL https://xerj.org/get | sh` gets it on their next upgrade, with no
  installer or release-matrix change and no new third-party dependencies. Tools:
  `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`,
  `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall`,
  `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink`,
  `xerj_brain_overview`. A XERJ node must already be running; `xerj mcp` does
  not start one. The published tool schema at
  `/docs/agents/schemas/mcp-tools.json` had drifted to six tools against ten
  served — it is now generated from a real `tools/list` and gated in CI against
  drifting again.

- **`[profile.quick]` and `[profile.ci-test]`** for the build and test halves of
  the verify loop. `[profile.release]` is `lto = "fat"` + `codegen-units = 1`,
  deliberately the slowest build Rust can produce because it yields the fastest
  binary — correct for a shipped artifact, wrong for "does this compile?". CI's
  test job spent 76m05s compiling for 1m52s of tests. Neither profile is ever
  valid for a released artifact or a published performance number.
  `docs/FAST_BUILDS.md` documents the remaining levers.


- **The reference-coding toolkit ships in the repo, and a corpus definition can
  now rebuild a corpus** ([#319](https://github.com/xerj-org/xerj/issues/319)).
  `xc-corpus.sh`, `xc-index.sh`, `xc.py` and `SKILL.md` lived only in a
  gitignored working copy, so `git ls-files | grep xc-` returned nothing while
  the case study, `CONTRIBUTING.md` and the landing page all told a reader to
  run them. They are now in [`tools/xerj-code/`](tools/xerj-code/), with a
  `!tools/**/*.md` re-include in `.gitignore` — the blanket `*.md` rule was what
  swallowed `SKILL.md`, the same failure mode that hid this repo's pull-request
  template for its entire history.
  - `corpus.json` records the **full 40-character SHA**. The old
    `rev-parse --short` value cannot be fetched from a remote
    (`git fetch --depth 1 origin e449d17` → "couldn't find remote ref"), so a
    manifest could not pin anything. A manifest carrying a short SHA is now
    rejected with a SHA-specific error rather than silently rebuilt at the tip.
  - `xc-corpus.sh --from <manifest>` rebuilds a corpus at its pinned commits —
    fetch-by-SHA at depth 1 with progressively deeper fallbacks — and an
    existing clone is *moved* to the recorded SHA instead of skipped.
  - [`tools/xerj-code/hub/`](tools/xerj-code/hub/) carries vetted definitions
    for the four corpora this repo uses. Each entry has a `review` block a
    human filled in: SPDX expression, `adapt-with-attribution` /
    `approach-only` / `mixed`, reviewer and date.
  - Hosting pre-built *indexes* remains out of scope (on-disk format version,
    embedder identity, redistribution of the indexed source), as does the
    measurement harness — `CASE_STUDY.md` and `SKILL.md` previously pointed at
    a `MEASURE.md` that is not in the repo, and now say plainly what ships.

- **`GET /_ilm/status`, `POST /_ilm/start`, `POST /_ilm/stop`** (#282): the
  operator can halt lifecycle execution without stopping the node; a stopped
  engine's tick touches nothing. The flag is in-memory — a restart resumes
  execution — while the per-index detach tombstone is the durable stop.
- **`POST /{index}/_ilm/remove`** (#282): ES's own detach verb; goes through
  the same tombstoned detach path as the settings-null route. A literal name
  that is not an index answers 404 rather than writing a tombstone for a
  ghost.

- **A dynamically-discovered nested object field is no longer permanently
  opaque to `GET _mapping`/`_field_caps`**
  ([#292](https://github.com/xerj-org/xerj/pull/292),
  [#307](https://github.com/xerj-org/xerj/pull/307)). A subfield like `metadata.kind`
  was already queryable (dotted-path resolution against `_source` doesn't
  care about the mapping) but had no way to be *discovered* by a client that
  reads the mapping instead of already knowing the field name — indistinguishable
  from not existing to Kibana/OSD's own field-list UI. Three compounding gaps,
  all fixed together: dynamic mapping never recursed into `Value::Object`
  (`{"type": "object"}` was a terminal, not real ES/OS default behavior);
  `GET _mapping`/`_field_caps` served the index-creation-time explicit mapping
  blob verbatim forever, so *any* field discovered dynamically after creation
  — flat or nested — never surfaced, no matter how many documents arrived; and
  `_field_caps`'s per-field walk only ever registered top-level names, so a
  nested object's own children were still invisible even once the mapping
  correctly described them. Measured end-to-end on `/_memory`'s own backing
  index: `GET .xerj-memory-{ns}/_field_caps?fields=metadata.kind` returned
  `{"fields": {}}` before, the real entry after. The cluster-wide
  `GET /_field_caps` (as opposed to the per-index `GET /{index}/_field_caps`)
  had the same gaps independently and is fixed the same way — the two no
  longer disagree about which fields exist. Dynamic mapping also now
  re-inspects nested fields on *every* document — a later document adding a
  new nested key merges it into the existing mapping instead of leaving it
  permanently invisible — and `Schema::field_count()` counts nested children
  and multi-fields recursively, closing a `max_fields_per_index` bypass
  where one top-level object could carry unbounded uncounted children. Note
  the tightening this implies: a dynamic text field with its `.keyword`
  multi-field now costs 2 against the default limit of 500, which matches
  how ES counts `index.mapping.total_fields.limit`. Two bounded residuals
  from review (a one-shot budget overshoot on a single deeply-keyed insert;
  array-nested keys deferred to the 100-doc recheck) are filed as
  [#312](https://github.com/xerj-org/xerj/issues/312).
  **Known limitation**: this only benefits indices created after this
  ships. An index (a `/_memory` namespace or otherwise) created before it
  already has its stored mapping written, and that isn't retroactively
  edited — existing indices need either re-creation or an explicit
  `PUT _mapping` to pick up the fix. No automatic migration is included.

- **A crafted filename can no longer forge records on the agent progress
  stream** (the first rc.15 known issue below). Every externally-controlled
  string — in-flight paths, the paths and error text interpolated into human
  notes, the terminal line's reason — is stripped of control characters, bidi
  overrides and zero-width characters and bounded in length before it reaches
  any progress surface. Measured on a corpus holding crafted names: a run that
  previously emitted a forged `xerj-done ok=true exit=0 reason=completed`
  2.2 s ahead of its real terminal line now emits one terminal line and no
  control characters at all. The same sanitisation covers the walker's
  "skipping unreadable entry" warning, which renders a path to the same stderr.
- **`--progress json` paces the bar like `--progress plain`.** The `bar` field
  bypassed the spacing slot entirely: measured at `--progress-interval 1`, 178
  of 178 ticks carried a bar against 18 bars on the plain surface. It is now a
  string on exactly the ticks that owe a bar and `null` in between — 26 of 320
  ticks on the same corpus, against 16 on plain.
- **The bar spacing floor is the 15 s the documentation states.** A half-tick
  tolerance made the enforced floor `interval/2` shorter — 12.5 s at the
  shipped defaults. Measured at `--progress-interval 10`: 11 of 11 same-phase
  gaps under 15 s (min 10.0 s) before, none after (min 19.3 s).
- **`/{index}/_refresh` reports segment-publication failures instead of
  claiming every shard succeeded.** The endpoint previously discarded every
  error returned by the synchronous engine flush and always answered HTTP 200
  with `successful == total`. It now includes the index, status, and underlying
  reason in `_shards.failures`, attempts every resolved index, and returns the
  first failed shard's HTTP status as Elasticsearch 8.13.4 does for refresh.

- **Text field names containing path separators can be flushed and reopened.**
  FTS side-car filenames previously interpolated the raw field name, so a name
  such as `styles_blocks_core/site-title` became a child path and prevented the
  segment from being published. Unsafe or overlong field names now use one
  deterministic bounded filename component. Existing portable names retain
  their byte-identical paths. Each segment has an immutable filename-layout
  discriminator: absence means the historical raw layout; an explicit marker
  means the encoded layout. Readers never infer the layout from side-car file
  existence, preventing partial files and raw names that resemble digests from
  aliasing another field. Before the discriminator or first encoded side-car
  is written, the index durably advances its data-directory marker to format 2;
  older binaries refuse that index instead of silently missing the new layout.
  A retry after a post-rename marker failure must re-establish durable marker
  publication before writing the discriminator or encoded FTS paths. Unix
  confirms the parent-directory fsync; Windows uses a same-directory Win32
  write-through replacement rather than claiming its directory-sync no-op is
  durable. Safe-only indices remain format 1.

### Changed

- **Default `_search` source payloads omit engine-generated embedding
  companions** ([#309](https://github.com/xerj-org/xerj/pull/309)). A search
  with no `_source` now returns ordinary source fields without
  `<field>_vector` and `<field>_vector_chunks` — identified from the
  mapping's embedding config, not by name; measured payload size fell from
  211,432 to 21,069 bytes for 5 hits (about 90%). Explicit `"_source": true`,
  `"_source": ["field"]`, and include/exclude forms are unchanged, vectors
  remain stored and used for kNN/semantic/hybrid scoring, and the
  document-copying paths (`_reindex`, index clone, enrich execution) request
  the full source explicitly so copies keep embeddings byte-for-byte. Add
  `"_source": true` to restore the previous complete payload, including
  vectors. Two residuals disclosed from review: a `fields` request naming a
  companion resolves against the already-filtered source and comes back
  empty under the new default — name it in `_source` instead
  ([#310](https://github.com/xerj-org/xerj/issues/310)) — and the new
  default arm copies each hit's source even on indices with no embedding
  fields, a constant-factor read-path cost filed as
  [#311](https://github.com/xerj-org/xerj/issues/311).

- **`merge.strategy = "log_structured"` is now refused at startup instead of
  silently running the size-tiered policy**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). Nothing in the tree
  ever read `merge.strategy`; `run_merge_once` always builds a
  `SizeTieredMergePolicy`. An operator who chose a levelled policy for its read
  amplification got the other one, quietly. This follows the `storage.backend`
  and `vector.default_quantization` guards already in `Config::validate`.

- **The three dormant `[merge]` settings now say so, in the code, in the shipped
  TOML, on the docs site, and in the log at startup**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). The issue reported
  merge I/O rate limiting as "claimed but dormant" and it is: the `RateLimiter`
  that honours `io_rate_mb_per_sec` is wired only into `xerj-storage`'s legacy
  `MergeExecutor`, which the engine never constructs. `min_segments` and
  `max_concurrent` are unread too — the real per-tier trigger is
  `min_merge_count`, and merge parallelism comes from `XERJ_MERGE_PARALLELISM`.
  `MergeConfig::dormant_overrides` names any of the three the operator has moved
  off its default and `xerj-server` logs each at WARN, so the operator throttling
  merges to protect query latency finds out instead of guessing. They stay
  accepted rather than becoming hard errors because the wrong value costs
  latency, not data, and `io_rate_mb_per_sec = 100` has shipped in
  `xerj.default.toml` since v0.1. Wiring a real throttle into `run_merge_once`
  is the fix that makes this list shorter.

- **`engine/xerj.default.toml` disagreed with the defaults it claims to
  document, in four places.** `max_segment_mb` said 5120 against a real 8192,
  `wal_max_size_mb` 512 against 1024, `flush_size_mb` 256 against 512, and
  `default_quantization` `"scalar8"` against `"none"` — so `cp xerj.default.toml
  xerj.toml`, the documented first step, silently changed four engine behaviours
  including turning on 8-bit vector quantization. The file is corrected and
  `shipped_default_config_documents_the_real_defaults` now diffs every leaf key
  against `Config::default()`, so it cannot drift again.

- **The docs site had drifted the same way, and now has the same guard.**
  `landing/docs/config.html` shipped `wal_max_size_mb = 512`,
  `flush_size_mb = 256` and `default_quantization = "scalar8"` in its
  copy-pasteable `[storage]` and `[vector]` blocks while its own DEFAULT
  table — thirty lines up, on the same page — said 1024, 512 and `"none"`;
  `landing/docs/storage.html` said the WAL rolls at 512 MiB;
  `landing/docs/vectors.html` called `scalar8` "the default" and listed a
  `scalar4` mode that no config or mapping value can reach; and
  `engine/README.md` put `flush_size_mb` at 256 MiB. All four are corrected,
  along with the docs search-index blob duplicated across 44 landing pages,
  which offered `scalar4` as a selectable quantization mode.
  A second guard, `the_docs_site_config_page_agrees_with_the_real_defaults`,
  now diffs *both* halves of that page — every DEFAULT cell in the reference
  table and every assignment in the example blocks — against
  `Config::default()`. An example that deliberately differs (the blocks whose
  point is switching TLS or cluster mode on) has to carry `# not a default` on
  the line, so the reader is told as well as the test.
  - **Known gap, found while correcting the `scalar4` prose and disclosed
    rather than quietly fixed:** the guard reads `config.html` against
    `Config::default()`, so it covers the *config* half only. Nothing checks a
    *mapping*, and a mapping is not validated either —
    `es_compat.rs` matches `"scalar8" | "int8" | "none"` on a `dense_vector`
    field's `quantization` and falls through `_ => {}`, so
    `"quantization": "scalar4"` (or `"binary"`, or a typo like `"sq8"`) is
    accepted with a 200, echoed back verbatim by `GET /_mapping`, and ignored
    — the field is stored at full-precision f32 while the mapping says it is
    quantized. Measured through the ES-compat router at this commit. It is
    pre-existing and outside this diff; it is another instance of the
    accepted-and-ignored class in
    [#204](https://github.com/xerj-org/xerj/issues/204), is filed with the
    repro and a proposed 400 as
    [#275](https://github.com/xerj-org/xerj/issues/275), and
    `landing/docs/vectors.html` now says so on the page instead of leaving
    "not a mode you can select" to imply the value is rejected.

- **Four other claims from [#207](https://github.com/xerj-org/xerj/issues/207)
  now match the source.**
  - *Settings count.* `config.rs` said 38 in its header and 56 twenty lines
    down while the count test asserted 60 — and the test asserted it by
    re-adding a hardcoded sum (`5 + 2 + 3 + … - 1 == 38`), an identity that
    could not fail whatever `Config` held. There was a second copy of the same
    non-test in `config.rs` itself (`count_user_facing_settings`, `61 == 61`).
    Both now serialise `Config::default()` and count leaf keys, section by
    section. The measured figure is **105** (103 when this work was done; the
    rc.14 `server.allow_insecure_network_bind` key made it 104 and #247's
    `lifecycle.tick_interval_secs` makes it 105 — which is the point: it is
    measured, so it moves with the code), and 38 / 50 /
    56 / 60 / 61 / "<50"
    are corrected in `config.rs`, `xerj-common/src/lib.rs`, `engine/README.md`,
    `xerj.default.toml` and the feedback responses, along with the stale
    per-sub-config annotations (limits 3 → 13, storage 5 → 10, embedding 4 → 19,
    merge 5 → 8, cluster 4 → 5, tls 3 → 4, auth 2 → 3). This closes item 10 of
    `user-feedback/ROADMAP-TO-GA.md`. 105 versus 3,000+ is still the winning
    story, told truthfully.
  - *Wire-compatibility test framing.* `journey_es_migration` was described as
    proving "the same curl commands" work. It calls the Rust engine API
    directly and never makes an HTTP request. The response-shape assertions are
    real and stay; the comment now says what the test does and points at the
    ES-compat YAML conformance suite, which is what actually covers the wire.
  - *OpenAPI spec.* `landing/openapi.json` declared `version: 1.0` while
    specifying 7 routes out of the 200-plus the router registers. It is now
    titled and versioned as a partial spec, and says so.

- **`autoindex` estimates the job on the user's own machine and hands the
  decision back before it takes their laptop.** Phase A already reads and
  parses every file to sniff and sample it, so it now *times* that work per
  format family and turns it into a range with its basis printed next to it —
  `code 500 files 749.3 MB at 11.7 MB/s measured over 500 file(s) → 64.2 s` —
  rather than a constant calibrated on someone else's hardware. A family is
  priced only from files phase A **provably** read end to end (whole-file
  parsers always; streaming parsers only when the file was under the sampling
  byte cap *and* stopped short of the record cap; never `sqlite`, never
  gzip), and everything else is named under `unmeasured_families` with its
  bytes instead of being priced at another family's rate. If nothing could be
  measured there is no number at all and no gate. The two ends are the
  classical list-scheduling bounds (Graham, 1969) over the phase-B worker
  count that #240's resource policy chose.
  The number is a **floor**, labelled as one on every surface: it covers
  client-side extraction only, because measuring the server, the network or
  embedding would mean writing to the index the estimate exists to ask
  permission for. Measured on a 68 MB source tree the floor was 0.1 s against
  a real 8.9 s run; on a 793 MB one, 64.2 s against ~350 s. The gate therefore
  under-asks and never over-asks, and says so where a reader would otherwise
  mistake silence for a promise.
- **`--max-minutes` (default 10) stops the run and asks instead of deciding.**
  Past the threshold with no answer, autoindex indexes nothing, writes a JSON
  decision request to stdout and exits **4** — a code of its own, because exit
  1 is already the catch-all for every real failure and an agent must be able
  to tell "choose something" from "your endpoint is down". The payload carries
  the estimate and its basis, file/byte counts, the per-band work order, the
  heaviest directories with real byte counts (flagged when they match the
  vendored/generated rule), and four options: `proceed`, `fast`, `narrower`,
  `cancel`. Answer with `--approve <id>` (`--yes` = proceed), which skips the
  gate; `--max-minutes 0` disables it. A person at a terminal gets the same
  facts as prose plus a prompt; a piped or agent-driven run is **never**
  blocked on stdin, and an unanswerable prompt is a cancel, never consent.
  `--approve fast` really applies `--no-semantic --no-graph`, and `--approve
  narrower` is refused with instructions rather than accepted and ignored
  (#204). The `fast` option states no speed-up factor: it reports the datasets
  and file count it changes and says plainly that this run did not measure the
  factor.
  The gate governs the phase-B route. An **incremental reconcile of an already
  committed generation** (a `--no-graph` re-run over a folder that already has
  one) processes only what changed and publishes from a sealed snapshot, so
  `--max-minutes` does not apply there — and that route now *says so* on
  stderr rather than accepting the flag and quietly not honouring it.

- **`autoindex` indexes what matters first.** Phase B's queue was sorted by
  size alone — right about scheduling, silent about value, so a user who
  stopped early got whatever was largest, which on a source tree is
  `node_modules`. Work is now ordered by value band (source and documents →
  configuration → structured data → logs and line files → vendored, generated
  and minified paths), with the old biggest-first rule kept *inside* each band
  so a large file still runs alongside its band instead of becoming the tail;
  a single-worker run goes smallest-first, where there is no tail to hide in.
  One exception keeps the new order from costing wall clock: a file whose own
  extraction outlasts everything ranked above it (`size × workers > the rest`)
  starts first regardless of band. The bands, their file/byte counts and the
  reason each sits where it does are printed with the plan and included in the
  decision payload as `priority_order` — an unexplained order is
  indistinguishable from an arbitrary one. Verified live on a 28 MB mixed
  corpus at two workers: the first second of phase B was spent on
  `src/mod_*.rs`, and the 1.7 MB `node_modules/**` files — the largest in the
  corpus, and therefore *first* under the old rule — drained last.
  Bytes-based progress and its percent are unaffected: reordering does not
  change the denominator.

## [1.0.0-rc.15] - 2026-08-10

### Known issues in this release

Both were found by adversarial review *after* their pull requests merged, so
they are present in these binaries. Fixes are in flight for rc.16.

- **A crafted filename can forge records on the agent progress stream.** The
  in-flight path is rendered onto the progress surface without control-character
  sanitisation, so a filename containing a newline can inject something that
  looks like a genuine `xerj-progress` or `xerj-done` record — including a false
  `ok=true` completion — into the stream this release tells AI agents to parse.
  Indexing a repository whose filenames you do not control is enough to trigger
  it. Until the fix lands, do not treat that stream as trustworthy when the
  corpus is untrusted.
- **An outer repository's `.gitignore` judges files inside a nested checkout.**
  The ignore lookup walks every layer with no repository-boundary stop, unlike
  git itself, so a vendored or submoduled repository is filtered by rules that
  have no authority over it. Files you expect to be indexed may be skipped.
  `--no-ignore` bypasses it.

Also unfixed and worth knowing: `--progress json` does not pace the bar (a JSON
consumer sees roughly ten times as many bar lines as a plain consumer), and the
documented "at most one bar per 15 s" floor is really 12.5 s.


### Added

- **Index lifecycle policies are executed, not just stored**
  ([#199](https://github.com/xerj-org/xerj/issues/199), contributed by
  @Vinz2168). A single internal engine modeled on OpenSearch's ISM state machine
  — named states, ordered actions, ordered transitions — exposed through two
  REST surfaces: native ISM at `_plugins/_ism/*` and the Elasticsearch-shaped
  `_ilm/*`. `spawn_lifecycle_manager()` runs the tick on
  `lifecycle.tick_interval_secs` (default 300s, matching OpenSearch ISM's own
  job interval), and the managed-index cursor survives restart. Retention now
  actually deletes and rolls over instead of being acknowledged and forgotten.
  One documented divergence: `min_age` is measured from when an index entered
  its current state rather than from rollover time, so a policy that rolls over
  *and* has a downstream phase meant to be measured from the rollover will
  advance on a different clock than Elasticsearch uses.

- **`xerj autoindex` honours `.gitignore` and `.xerjignore`**
  ([#276](https://github.com/xerj-org/xerj/issues/276)). Nested ignore files and
  negation patterns follow git's own precedence, with build-output defaults
  (`node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `__pycache__`)
  on top; `--no-ignore` and `--no-default-ignores` turn them off. Pointing xerj
  directly at an ignored path still indexes it — an explicit instruction beats a
  rule the user did not write for this purpose. `--dry-run` reports what was
  skipped and by which rule, because a user whose files did not appear needs to
  know why. Measured on this repository: 274,826 files walked in 1.18s becomes
  1,385 in 0.25s.

- **Progress is relayed to an AI agent as a drawn bar.** The stream surface
  carries a rendered bar alongside its machine-readable `key=value` fields, so
  an agent driving `xerj autoindex` on someone's machine can show a real
  progress line rather than going silent for minutes. `--progress json` keeps a
  single parseable stream; `--quiet` still writes nothing on either.

### Fixed

- **Tied scores have one total order — a bounded page is a prefix of the full
  page** ([#191](https://github.com/xerj-org/xerj/issues/191)). A `size:5` page
  and a `size:1000` page disagreed about which tied document came back. The
  memtable's bounded candidates are now ranked by the same page key as the
  segment path, so the two agree. The remaining `constant_score` and post-sort
  re-sort cases are tracked separately in
  [#270](https://github.com/xerj-org/xerj/issues/270).

- **`date_histogram` no longer aborts the process on a multi-byte `time_zone`**
  ([#272](https://github.com/xerj-org/xerj/issues/272), contributed by
  @Vinz2168). `aggs::parse_time_zone_offset` sliced a `&str` on a byte index
  that could land mid-character, and with `panic=abort` that took the whole node
  down — an unauthenticated request was enough.

- **The legacy path-parameter form of scroll continuation works again**
  (contributed by @Vinz2168), and **`_cat/*?format=json` plus `_data_stream/**`
  wildcards** are supported (also @Vinz2168) — both are shapes real
  Elasticsearch clients send.

### Changed

- **`autoindex` parses each PDF once per fresh run instead of twice.** Phase A
  already paid a *complete* parse for every PDF — `extract::extract` routes
  `Family::Pdf` straight to the isolated worker and drops the sampling limit,
  so only delivery ever stopped early — and phase B then spawned the worker
  again for the same bytes. Phase A now retains each validated worker response
  in an anonymous temporary file under `--state-dir` and phase B replays it.
  Reuse is an optional accelerator, never correctness-critical state: it is
  bounded to 384 MiB of retained-plus-in-flight bytes and to an artifact-handle
  cap derived from live `RLIMIT_NOFILE` and open-descriptor counts, both
  re-probed at every admission; a 4 GiB-or-half-free filesystem floor is
  reserved for the journal and phase-B staging first; and every artifact is
  verified (physical length, content digest, JSON decode, worker-protocol
  identity) before a single record is published. Anything that cannot be
  measured conservatively declines and reparses — which is why the optimization
  is **Linux-only** today: other platforms have no live descriptor evidence.
  A restart has a frozen plan but no trusted handle, so it parses again; the
  spool is deliberately not journal state. `--json` gains a
  `pdf_extraction_reuse` block so the behaviour is observable rather than
  inferred from timing. Original work by Leonid Bugaev (@buger).

### Changed — autoindex refuses reruns that would strand documents

- **Deleting an indexed file and rerunning `xerj autoindex` now fails instead
  of exiting 0.** Nothing in the pipeline removes the documents, aliases,
  graph edges or catalog entries that the deleted file published, so a rerun
  that ignored the deletion left them live and searchable with no source file
  behind them. The rerun is now refused before any remote call other than the
  endpoint-readiness ping: no mapping, delete-by-query, bulk, refresh, graph
  or catalog write is attempted, and the journal is not appended to. The error
  names the removed files and their content keys — the first ten, then an
  `… and N more` tail, since a whole unmounted subtree can vanish at once —
  and gives three recovery routes: restore the files and rerun, rebuild in
  place by deleting the named indices and the state directory, or rebuild
  under a new state directory, prefix and brain. `--fresh` is refused for the
  same case, because it does not delete those documents either. `--json` emits
  the same facts — every removed entry, uncapped — as
  `xerj.autoindex.unsupported_sync_delta.v1` on stdout with exit 1. Deleting a
  file that was only ever *skipped* is not refused: a junk file publishes no
  documents, and the stale junk-catalog sweep removes its one catalog row, so
  nothing is stranded.
- **Files added after a plan was frozen are now called out on stderr.** They
  are still not indexed by a rerun that resumes an existing plan — the plan is
  a crash-resume boundary, not a folder-sync generation — but the run now
  lists them, records them as skipped (exit 3, completed-with-junk) and points
  at `--fresh`, which rebuilds the plan in place and picks them up. Adding or
  changing files and rerunning keeps working; only removals are refused.
- **`xerj brain` no longer turns an absent or zero node-count probe into an
  automatic reset.** Journal/server disagreement now fails with the journal,
  URL, prefix, and brain identity plus recovery instructions, including the
  explicit `--fresh` rerun for a genuinely wiped data directory. Probe
  transport and malformed-response failures remain errors instead of being
  reported as an empty destination. A nodes index that was deleted out from
  under a surviving brain meta doc reads as absence and reaches that recovery
  text, rather than surfacing as a raw HTTP 404.
- **`--fresh` still recovers a state directory whose journal cannot be
  parsed.** The rerun gate reads the durable plan before the run starts, and a
  plan that is malformed, or recorded for a different root/URL/prefix, is fatal
  to a resume — but not to `--fresh`, which deletes that journal unread. The
  preflight is now no more fatal than the open it precedes: under `--fresh` the
  unreadable plan is reported on stderr and rebuilt from the current folder.
  Without `--fresh` the refusal is unchanged. Because the removal gate has no
  comparison basis in that case, the warning says so: documents already
  published for files that are now gone cannot be identified from an unreadable
  plan and are not deleted.
- **Refusal and skip listings are capped at ten entries plus an "and N more"
  tail.** Unmounting a bind mount under an indexed root vanishes every content
  group at once, so the uncapped listing was one rendered entry per journalled
  file. `--json` still carries every entry.

### Fixed

- **An index whose metadata will not parse is refused instead of silently
  reopened with an empty mapping**
  ([#202](https://github.com/xerj-org/xerj/issues/202)). `Index::open` treated
  "this file is not there" and "this file does not parse" as the same thing:
  `load_schema`/`load_settings` mapped ENOENT, EACCES, EIO and malformed JSON
  alike to one anonymous error, and the caller answered all of them with a
  fresh dynamic mapping. Measured on a field deliberately mapped `keyword`:
  after `schema.json` was truncated to half its bytes the index still opened
  `Ok` with `field_count = 0`, the field came back `None`, and one further
  document re-inferred it as `long` — a mapping silently replaced by a
  different one, with nothing in any response saying so. Absent stays absent
  (indices predating create-time schema persistence legitimately have no
  `schema.json`); present-but-unparseable now fails the open with an error
  naming the file. `es_mapping.json` — the full-fidelity mapping behind
  `GET /_mapping` — was logged-and-ignored on a parse failure and now fails the
  index too, on the boot scan, on snapshot restore and on a retry.

  **Visible on upgrade:** a node whose data dir already holds a corrupt sidecar
  now boots **red** with that index unserved, where it previously came up green
  with an empty mapping. It joins the failed set
  [#206](https://github.com/xerj-org/xerj/issues/206) introduced, so every
  surface that set feeds already reports it — measured on a node holding two
  user indices, `victim` with a truncated `schema.json`:

  ```
  GET  /_cluster/health          -> red, unassigned_primary_shards: 1
  GET  /_cluster/health?level=indices
                                 -> victim red, unassigned_info.details = the
                                    absolute path of schema.json and the parse error
  GET  /_cat/indices             -> "red open victim <uuid> 1 0 0 0 0b 0b 0b"
  GET  /_cluster/indices/failed  -> the reason, plus the retry and delete calls
  PUT  /victim, POST /victim/_doc, POST /victim/_search
                                 -> 503 no_shard_available_action_exception
                                    carrying that same reason
  GET  /health/ready             -> 200 "ready (degraded): 1 of 2 indices
                                    serving, 1 failed to open; see GET
                                    /_cluster/indices/failed"
  POST /healthy/_search          -> 200
  ```

  The readiness counts above are the two-index test router's. A running node
  also holds its internal `.xerj_*` indices, so the same condition on the shipped
  binary reads `ready (degraded): 15 of 16 indices serving, 1 failed to open` —
  the 200 and its meaning are unchanged, only the totals move with what else the
  node holds.

  Recovery is the three doors #206 added. Two were measured end to end on the
  shipped binary: repair the file and
  `POST /_cluster/indices/failed/{index}/_retry` (503 while it is still torn,
  `200 {"reopened": true}` once it is not, health back to green), and
  `DELETE /{index}` (200, directory removed, health back to green). The third —
  restore the index from a snapshot — is the path this change touches (a
  successful restore now clears the recorded failure, which it has to or health
  would stay red after the repair worked); it is covered by an engine-level
  test rather than measured over HTTP. The node stays in kubelet rotation as
  long as any index is still serving.

- **Concurrent sidecar writes can no longer manufacture the torn file the reader
  now refuses.** `write_file_atomic` staged every write in
  `path.with_extension("tmp")` — one shared name for all writers of that file —
  so two concurrent writers of one sidecar (`PUT /{index}/_settings` racing
  another settings update, two API-key mints for `api_keys.json`) both opened it
  `O_TRUNC`, interleaved their bytes, and each renamed it into place. The rename
  is atomic; the content being renamed was not one writer's. Measured with the
  old shared name, 4 threads × two different-length settings bodies × 200
  rounds, over four runs: **86–294 of 800 writes failed with ENOENT** (the loser
  renaming a file the winner had already moved) and, with those errors swallowed
  as the callers did, **1–16 of 200 rounds left an unparseable `settings.json`**.
  The counts are race-dependent and vary with machine load; what does not vary is
  that both are non-zero on every run with the shared name and **0 and 0** on
  every run with a unique staging name. Each write now stages
  in its own sibling file (`<file>.tmp.<pid>.<seq>`) in the target's directory
  and removes its own debris on failure — which for `api_keys.json` is key
  material. `update_settings` also writes `settings.json` through
  `write_file_atomic` rather than a plain `fs::write`; it was the last
  non-atomic sidecar writer left.

- **`index` on the mapping is now honoured instead of silently ignored**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). `"index": false` was
  accepted by `PUT /{index}`, echoed back verbatim by `GET /{index}/_mapping`,
  and then had no effect at all: the field kept a full inverted index and
  `match`, `term`, `match_phrase`, `prefix` and `wildcard` against it all
  returned the document, where Elasticsearch answers 400. It is the sibling of
  the `doc_values` fix in rc.12 and follows its shape.

  The line is drawn where ES draws it. Since 8.1, `"index": false` on a
  keyword / numeric / date / boolean / ip field keeps the doc-values column and
  stays queryable — `MappedFieldType.isSearchable()` is "has postings **or** has
  doc values" — so those queries keep working here, unchanged. Only a field with
  neither (a `text` field, or anything with an explicit `"doc_values": false`)
  is unsearchable, and a query naming one is now rejected with ES's own
  sentence, `Cannot search on field [f] since it is not indexed nor has doc
  values.`, as a 400 `search_phase_execution_exception` /
  `query_shard_exception`. The check lives in one place — `search_inner` — and
  these surfaces were each measured reporting the rejection rather than
  absorbing it, including the three that were swallowing it: `_search`,
  `_count` on all four selectors (single index, `logs-*`, a comma list and the
  cluster-wide `POST /_count`), `_msearch`, `_msearch/template`, `_explain`,
  `_delete_by_query` and `_rank_eval`. `_explain` answers the 400 instead of a
  confident `"matched": false` for a query it never ran; `_msearch` and
  `_msearch/template` render it as a per-response 400 envelope with `type` and
  `root_cause`, leaving the sibling sub-requests in the batch untouched; and
  `_rank_eval` records the refused request under `failures` instead of dropping
  it from `details` *and* `failures`. It still publishes a `metric_score`, and
  that number is still the mean over the requests that actually ran — which is
  ES's behaviour too — but the batch no longer shrinks silently underneath it:
  a two-request batch in which one is refused now answers `details: {good}`,
  `failures: {bad}`, `metric_score: 1.0` (measured), so a relevance gate reading
  a clean score can see what did not contribute to it.

  A multi-index `_count` follows ES's broadcast rule rather than failing
  outright: the failure status is returned only when **no** index answered
  (`RestStatus.status`, semantics only), so `/t/_count` and a `logs-*` whose
  indices all carry the offending mapping both 400 exactly as `_search` does,
  while a mixed selector returns 200 with the partial count and the refusal
  visible as `_shards.failed: 1` plus a `_shards.failures` entry carrying the
  `query_shard_exception`. What it will not do any more is what it did before:
  report `count: 0` with `"successful": 2, "failed": 0`.

  `exists` is unaffected and still returns zero hits rather than an error, which
  is also what ES does.

  The multi-field query types follow ES's own split rather than a blanket rule.
  An explicitly *named* field is rejected like any other named field —
  `multi_match` / `simple_query_string` / `query_string` with
  `"fields": ["note"]`, and `query_string` with `"default_field": "note"` —
  while a *wildcard* spec such as `"fields": ["*"]` is never an error, because
  ES expands a pattern over the searchable fields and silently drops the rest
  (`QueryParserHelper.resolveMappingField`). Before this, the same user intent
  got two different answers: the `simple_query_string` form was rejected (the
  parser lowers it to a `match`) while the `multi_match` form returned the
  document through the unsearchable field.

  Reaching the `query_string` half of that meant fixing a second
  accepted-and-ignored key in the same family: **`query_string`'s `fields`
  array was never read by the parser**, so `{"query":"x","fields":["title"]}`
  searched every field, and — once `index: false` became a real rejection —
  disagreed with `{"default_field":"title"}`, which is the same request written
  the other way. `fields` now selects the targets for a clause that named no
  field of its own (a `dis_max` over one leaf per field when there is more than
  one, `^boost` honoured) — the same job `simple_query_string`'s `fields` list
  was already doing. A clause that does name its own field keeps it, as in ES.

  The field is dropped from the full-text index at flush and merge only where
  the stored-doc fallback is *equivalent* — a non-indexed `text` field. An
  exact-typed field that kept its doc values keeps its whole-value postings,
  because the fallback scan splits on non-alphanumerics and would start matching
  `192.168.0.1` against `192.168.0.2`. So no byte-saving claim is made for this
  change: for those types the footprint the option implies is knowingly forgone
  rather than bought with wrong answers.

  Known gaps, measured and left open rather than claimed shut. **Aggregations
  and sort do not inherit the check**: only the `query` clause is walked, so
  `{"size":0,"aggs":{"a":{"terms":{"field":"note"}}}}` still answers 200 with a
  bucket built from `_source`, and `{"sort":[{"note":"asc"}]}` still answers 200.
  ES rejects those too, but for an unrelated reason and with an unrelated
  sentence — `Fielddata is disabled on [f] in [i]…`, which it raises for *any*
  `text` field whether or not `index: false` was declared — so reusing this
  error there would misreport a wider divergence as this one. **The field-less
  arms of the stored-document scan are schema-free**, walking every `_source`
  key, so a token living only in an unsearchable field can still match when
  **no** field is named: `query_string {"query":"…"}` and `more_like_this` with
  no `fields` both still return the document. (Their *named* forms are
  rejected — `more_like_this` with `fields` lowers to a `bool.should` of
  `match` at parse time and inherits the check.) **A wildcard `fields` spec
  reaches that same field-less arm**: `simple_query_string` / `query_string`
  with `"fields": ["*"]` lowers to the `"*"` placeholder and is answered by the
  schema-free scan, so it can still match through an unsearchable field. It is
  never an *error*, which is ES's rule and deliberate — but it is not "answered
  over the searchable fields" either, and that is a pre-existing property of
  wildcard expansion, identical on `main`. (`multi_match` with a pattern is
  different: it is answered by the FTS projection, which does apply
  `isSearchable()`, and finds nothing in an unsearchable field.) **The opaque
  `query_string` fallback still ignores a multi-field `fields` list**: a query
  string the parser declines to lower keeps the whole string for the FTS path,
  where a one-element `fields` is carried across as `default_field` and a
  longer list has nowhere to go. And **`_validate/query` never opens the
  index** — it answers from the parse alone, so it reports `{"valid": true}`
  for a body `_search` refuses. That too is unchanged from `main`, but it now
  disagrees with `_search`, so it is written down rather than left to be
  discovered. Every one of these gaps is pinned by an assertion in
  `index_false_is_honoured::documented_gaps_are_still_open_and_still_documented`,
  so closing one fails the test that says it is open.

### Added — incremental `--no-graph` corpus reconciliation

- **`xerj autoindex --no-graph` now reconciles a changed folder instead of
  re-deriving it.** A `--no-graph` run commits a durable *corpus generation*: a
  manifest of content groups (stable group identity, content digest and byte
  length, canonical path plus aliases, dataset assignment, validated output
  counts) plus a sealed source snapshot of the prepared records. A later run
  projects the current inventory onto the committed manifest and publishes only
  the difference — additions, content changes, deletions, renames, junk
  transitions and duplicate-alias moves all converge, and a no-op re-run issues
  no data bulk and appends no new generation. A run interrupted mid-generation
  replays from its own sealed snapshot, so the result never depends on whether
  the source tree changed after the crash. Dataset and mapping identity is
  frozen at generation 1: a file that would need a *new* dataset or a mapping
  change is refused before any remote mutation rather than silently widening
  the schema.

  Scope, plainly: only `--no-graph`. Graph-enabled runs are unchanged, and each
  changed generation still copies and prepares the full corpus (O(N) staging) —
  the win is in what gets published and verified, not yet in what gets read.
  `--snapshot-max-gb` (default 64) caps the logical staged payload; it is a
  payload budget, not a disk-space or peak-memory guarantee.

  Original work by Leonid Bugaev (@buger).

- **Junk keeps the contract it always had on the generated path: recorded,
  never fatal.** A file that cannot be read or recognised (`plan.junk_files`)
  and a record that no dataset assignment accepts (`stats.junk`) both used to
  abort a `--no-graph` run outright — the first with
  `plan has no assignment for <file>`, the second with
  `durable preparation of <file> produced N junk records`. A single zero-byte
  file was enough to make a whole folder unindexable. Both are now counted
  instead: the sealed prepared artifact carries its junk-record count, the
  manifest group carries it into the generation, and the catalog's per-file
  `junk` and run-level `junk_records_total` report it instead of a hardcoded
  zero. Such a run exits **3** ("completed with junk"), the same signal the
  legacy path publishes — the generated path previously returned a flat `0`
  and lost it.

- **`--dry-run` is honoured on an already-generated state directory.** It was
  evaluated only after the pending-replay and reconcile branches had already
  published and committed, so on any state directory past generation 1 the flag
  was accepted, silently ignored, and the destination mutated. It is now decided
  before either branch: it prints the plan a real run would act on — the
  projected reconcile plan, or the sealed plan of a pending generation — and
  returns without opening the journal for write, without snapshot GC, and
  without a single bulk request.

### Changed — `autoindex --fresh` is refused on a durable generation

- **`--fresh` no longer silently destroys a committed corpus generation.**
  `--fresh` deletes the resume journal, and the snapshot GC that follows every
  journal open then sees an empty protected set and removes every sealed
  snapshot — so on a generated state directory `--fresh` would discard the
  committed manifest, the pending replay evidence, and the alias/path/
  stale-record knowledge, while cleaning nothing at the destination. It is now
  refused up front, naming the generation that blocks it, and pointing at the
  plain re-run (which reconciles the change) or at an isolated rebuild with a
  new `--state-dir` and `--prefix`.

  **Unchanged for everyone else.** On a legacy (non-generated) journal —
  including every graph-enabled corpus and anything `xerj brain` writes —
  `--fresh` behaves exactly as before: it ignores the journal and restarts,
  which is what `xerj brain`'s documented self-heal for a wiped data directory
  depends on.

### Migration — pre-generation `--no-graph` state directories must be rebuilt

- **A `--no-graph` state directory written before this release cannot be
  adopted as generation zero, and the first `--no-graph` run against it will
  refuse with a followable rebuild command.** This is a deliberate, versioned
  format boundary (`sync::GENERATION_FORMAT_VERSION = 1`), not a validation
  failure: a pre-generation resume plan records no stable group identity, no
  content byte length and no validated per-group output counts, so no amount of
  evidence in it could reconstruct a manifest group. The refusal prints the
  exact `xerj autoindex` argv for an isolated rebuild into a new `--state-dir`
  and `--prefix`; the old target and the shared `autoindex-catalog` are left
  alone and require explicit, validated cleanup once the new target is
  verified. Nothing is migrated in place and no destination data is touched by
  the refusal. Graph-enabled state directories are not affected.

## [1.0.0-rc.14] - 2026-08-10

### Changed

- **One documented core and memory policy, and the worker knobs are live**
  ([#240](https://github.com/xerj-org/xerj/issues/240)). XERJ had no single
  answer to "how much of this machine may I take?", and the scattered answers it
  did have disagreed with each other — and, on Darwin, with the OS.
  `engine.flush_workers`, `engine.merge_workers` and `engine.search_workers`
  were documented but inert; they now drive real pool widths, and a value of `0`
  refuses startup instead of being silently reinterpreted. `--bulk-mb` above 24
  and `--workers 0` are likewise refused rather than clamped, so a request the
  engine will not honour fails loudly. Only hand-written configs are affected:
  the shipped `xerj.default.toml` has no `[engine]` section.

- **One 16 MiB file no longer costs 65 GB of RSS**
  ([#239](https://github.com/xerj-org/xerj/issues/239)). `split_sections` was
  quadratic in both time and memory: a 16 MiB text file with no blank line
  needed **65.6 GB of peak RSS and 16.3 s** to produce 16.8 MB of sections.
  Now linear — the same input takes **8.3 ms and 35 MB**.

- **BREAKING — `server.bind_address` now defaults to `127.0.0.1`, and a
  cleartext node refuses to publish itself to the network**
  ([#228](https://github.com/xerj-org/xerj/issues/228)). The old default was
  `0.0.0.0` while TLS is off by default, so an out-of-the-box node accepted its
  admin API key over plain HTTP on every interface the host had — verified on a
  stock boot, where `curl -H "Authorization: ApiKey …" http://<lan-ip>:9200/…`
  answered `200` and the same URL over `https` had no listener to hand shake
  with. Auth being on did not help: the credential *is* the thing on the wire.

  A fresh node is now reachable from its own host and nowhere else. Exposing it
  is two statements, not zero: set `server.bind_address` (or `--bind` /
  `XERJ_BIND_ADDRESS`), and — while `tls.enabled = false` — also set
  `server.allow_insecure_network_bind = true` (env
  `XERJ_ALLOW_INSECURE_NETWORK_BIND`). Without the second, startup exits
  non-zero before the data directory is created or a first-run admin key is
  minted. `--insecure` does not evade it; it clears `tls.enabled`, which is
  exactly what the check keys on. Same fail-closed shape as
  [#229](https://github.com/xerj-org/xerj/issues/229), whose TLS-on gRPC check
  is untouched and cannot be relaxed by this opt-out.

  **Upgrading:** a deployment that relied on the old default becomes
  unreachable from other hosts until it sets both; one that already wrote
  `bind_address = "0.0.0.0"` without TLS now fails to start with a message
  naming the setting that unblocks it. The shipped Docker image, compose file
  and Helm chart set both, because a container's network namespace is the
  boundary and its published port is where TLS belongs.

  Two documentation claims that were false as written are now true: the CLI and
  security pages both said `--insecure` "refuses to run with a non-loopback
  bind", and it did not — a `--insecure` node on `0.0.0.0` accepted an
  unauthenticated `PUT /leak/_doc/1` from the LAN (`201`). The startup banner
  also prints the bind address on every listener line, so a loopback node and a
  world-facing one no longer look identical.

  A `bind_address` that is not an IP literal is rejected first, ahead of both
  exposure checks, with the fault it actually has. Host names have never been
  resolved — the pre-#228 code parsed `"{bind}:{port}"` as a socket address and
  failed on them too — but the exposure predicates fail closed on anything they
  cannot parse, so without that ordering `bind_address = "localhost"` was
  refused with a message asserting that localhost "is not loopback" and would
  "serve plain HTTP on a network-reachable interface", and pointed at an opt-out
  that fixed nothing: setting it let the boot run on to the bind and fail there
  instead, after the data directory, the `.xerj_*` system indices, the master
  key and a printed first-run `admin.key` already existed.

### Security

- **API key secrets are no longer stored in the clear**
  ([#201](https://github.com/xerj-org/xerj/issues/201)). `api_keys.json` held
  the secret verbatim. 0600 plus atomic rename is right for a secret a *process*
  owns, but a file has more readers than a process: a backup, a snapshot, a
  container layer, a support bundle, a decommissioned disk. Secrets are now
  stored as a salted SHA-256 (`$ssha256$<salt>$<digest>`) and compared in
  constant time, with existing plaintext records migrated on load. The fast hash
  is deliberate: the secret is two v4 UUIDs — 244 bits of CSPRNG output, never
  human-chosen — so offline guessing is out of reach whatever the hash costs,
  while this comparison runs on *every authenticated request*, where an Argon2id
  would add tens of milliseconds per request and hand out a free CPU-exhaustion
  DoS. The same release makes `_security/_authenticate` report the key's real
  identity instead of a hardcoded `superuser`, and gives the audit chain durable
  storage.

- **The node refuses to start when TLS is on but gRPC would answer in cleartext
  off-loopback** ([#229](https://github.com/xerj-org/xerj/issues/229)).
  `tls.enabled = true` encrypted two of the three data-plane listeners; the
  third never participated, because the gRPC server is built without tonic's
  `tls` feature. Nothing surfaced it — gRPC clients connected and worked
  identically either way — so an operator who enabled TLS and bound a network
  interface got no error, no failed handshake and no symptom, while API keys and
  document bodies crossed the network in the clear. Auth was never the gap;
  confidentiality was. Boot now fails closed unless
  `tls.allow_insecure_grpc_h2c` says otherwise.

### Fixed

- **`multi_match` returns the same hit set before and after `_flush`**
  ([#218](https://github.com/xerj-org/xerj/issues/218)). The memtable and
  segment paths disagreed on multi-token semantics, so flushing an index could
  change which documents a query matched — verified live on a one-document
  index where an OR query returned 0 hits before `_flush` and 1 after, with ES
  returning 1 throughout.

- **Dynamic string fields get Elasticsearch's default `.keyword` multi-field**
  ([#209](https://github.com/xerj-org/xerj/issues/209)). Dynamic mapping
  inferred every string as bare `text`, so `GET _mapping` never showed a
  `.keyword` sub-field and `_field_caps` never listed one. Anyone arriving from
  Elasticsearch — or any tool that reads `_field_caps` for field discovery,
  Kibana included — could not see that `category.keyword` existed.

- **`min_score` is honoured at `size:0`, and an external scalar N keeps its
  ghosts** ([#193](https://github.com/xerj-org/xerj/issues/193)). A
  `size:0 + min_score` body never materialised a hit, so the threshold had
  nothing to filter and `hits.total` reported the raw match count — measured at
  **65** where the same query at `size:100` counted **5**. The tie-break
  comparator item from that issue is deliberately not included here; it belongs
  with [#191](https://github.com/xerj-org/xerj/issues/191).

- **Index blocks can be removed, `read_only_allow_delete` means what its name
  says, and a blocked write answers 403 rather than 500.** Three compounding
  defects: `PUT /_settings` wrote only the display-side map while enforcement
  read the per-index `settings.json`, so a block was acknowledged, invisible to
  `GET /_settings`, and — with `_block` registered PUT-only — removable only by
  restarting with a hand-edited file. The one block that fires automatically did
  the inverse of its name, and clients hitting any of it were told the server
  had faulted.

- **`llms.txt` no longer tells agents six things the binary does not do.** The
  "Running an index for a human" section is executed, not just read: an agent
  follows it literally and then reports the result to its user. Corrected: the
  job-size line is not the first line printed; `--dry-run` does not write
  *nothing* (it creates and locks the state dir and appends a journal record);
  a dry run reuses a frozen plan instead of re-costing the job, so it is not a
  floor for the real run; exit `1` is the catch-all for every error, not
  "endpoint unreachable"; `status` needs the same explicit `--state-dir` the run
  used; and `--workers`/`--pdf-workers` bound the client-side extractor, not the
  server (with `--pdf-workers` capped to 1–4).

- **A failed index can now be inspected, deleted and retried without stopping
  the server** ([#206](https://github.com/xerj-org/xerj/issues/206)). An index
  directory that refused to open at boot was recorded in a map that only
  `Engine::health` ever read. Live-reproduced with a corrupt `snapshot.json`:
  it was absent from `_cat/indices` and `_cluster/state`, `DELETE /{index}`
  answered `404 index_not_found` and left the bytes on disk, and the only
  recovery was to stop the server and edit the data directory by hand. A
  failed index is now a real state: listed as `red` in `_cat/indices`, present
  in `_cluster/state` as an `UNASSIGNED` primary with `ALLOCATION_FAILED` and
  the verbatim open error, enumerated with its reason by
  `GET /_cluster/indices/failed`, reopenable via
  `POST /_cluster/indices/failed/{name}/_retry` once the cause is fixed, and
  deletable through the ordinary `DELETE /{index}`. Searches, writes and
  creates against it return `503 no_shard_available_action_exception` carrying
  the reason instead of a `404` that claimed it did not exist, while the
  metadata surfaces — `GET /{index}`, `HEAD /{index}` (the `indices.exists()`
  every ES client calls), `_mapping`, `_settings`, `_cat/indices` — report it
  as an index that exists, the way ES answers those from cluster metadata
  rather than from a shard. Before this, `HEAD` said the name was free and the
  following `PUT` said it was unavailable.

  Two related defects in the same report are fixed with it. **Readiness no
  longer hard-fails on a partly degraded node** — `/health/ready` returned
  `503` for any red status, so one broken index pulled a pod holding 200
  healthy ones out of service permanently; it now reports `200 ready
  (degraded)` while the node can still serve something and `503` only when
  every index it holds failed to open. And **`_cat/health` no longer prints a
  hardcoded `green`** — the one health surface an existing ES dashboard points
  at was the one that could not report a broken node; it and `_cluster/health`
  now count an unopenable index as an unassigned primary, agreeing with
  `/v1/health` and `/v1/cluster/health`.

  Because `red` is now reachable on `_cluster/health` for the first time, the
  two conditions that gate on it are consulted rather than assumed:
  `wait_for_active_shards=all` and `wait_for_status=green|yellow` both answer
  `408` with `timed_out: true` on a red node instead of `200 timed_out: false`.
  `GET /_cluster/health?wait_for_status=green&timeout=30s` is the bootstrap gate
  every docker healthcheck, CI wait loop and Kibana startup uses; a red node
  used to sail straight through it. A green or yellow cluster is unaffected.

- **A `DELETE /{index}` that could not remove the bytes no longer strands the
  index.** The open-index path pulled the handle out of the engine before
  `remove_dir_all`, so a removal that failed (read-only mount, `EACCES`) freed
  the name while the directory survived: no handle, no failed-index entry,
  nothing on `_cat/indices`, and the next `DELETE` answered `404` — the same
  dead end as [#206](https://github.com/xerj-org/xerj/issues/206), reached from
  the other side. The handle is now restored on failure, so the index stays in
  service and addressable and the operator can retry the delete once the cause
  is fixed.

- **Index templates, ingest pipelines, data streams and ILM policies survive a
  restart** ([#203](https://github.com/xerj-org/xerj/issues/203)). Index
  templates, legacy (v1) templates, component templates, ingest pipelines, data
  streams and ILM policies lived only in memory: `PUT /_index_template/logs`
  answered `{"acknowledged": true}` and `GET` answered 404 after the next
  restart, and the next index that should have matched the template was created
  without it — with no error anywhere. All six are now persisted together in
  `<data_dir>/cluster_state.json`, written atomically (tmp → fsync → rename →
  fsync the parent directory) so the write is committed at the request, not in
  a shutdown hook, and restored in `Engine::new` before the listeners come up.
  A write that fails is rolled back in memory and answered with a 500 instead
  of `acknowledged`. Restored pipelines are recompiled, not just re-read —
  storing the definition alone would have left `?pipeline=x` accepted and
  silently inert after a restart.

  Four defects found while verifying it, each reproduced first:

  - Concurrent management writes corrupted each other (all flushes staged
    through one fixed `.tmp` path — 32 parallel template PUTs returned
    `store_exception`). Snapshot-and-write is now serialized.
  - A rollover interrupted between "backing index created" and "generation
    persisted" wedged the data stream forever: every later rollover recomputed
    the same name and got `409 resource_already_exists_exception`. Boot now
    adopts the highest generation actually present on disk, warns, and persists
    the repair once.
  - A `cluster_state.json` that could not be **read** (EACCES after a uid
    change on a container volume, a backup tool's chmod, EIO) came up as empty
    maps, and the next management write renamed a snapshot of those empty maps
    over a file whose bytes were perfectly good. The load failure now latches:
    every management mutation is refused with a 500 naming the file, and the
    file is not touched, until a boot loads it cleanly. The corrupt-parse arm
    refuses too. The later format-compatibility fence above tightens that
    diagnostic boot further: it creates no `cluster_state.corrupt.json` copy.
  - `DELETE /_data_stream/<name>` recorded the removal before destroying the
    backing indices, so a crash in that window stranded `.ds-<name>-00000N`
    directories that no data-stream API could reach — GET and DELETE answered
    404 while `PUT /_data_stream/<name>` answered
    `409 resource_already_exists_exception` permanently. The order is reversed:
    the removal is recorded only once every backing index is gone, and a
    backing index that cannot be deleted now aborts the DELETE with a 500
    (it was previously swallowed and answered `acknowledged`) so the operator
    can retry. Read that 500 as "did not finish", not as "nothing happened":
    a multi-generation stream still loses every backing index that *could* be
    destroyed, the stream itself stays addressable, and the retry after the
    cause is fixed completes the delete.

  **Known gaps, deliberately not closed here.** `.ds-*` indices that no data
  stream claims — left by a data dir written before this change, or by a
  DELETE interrupted by an older build — are **not** cleaned up or adopted
  automatically; boot now names them in a warning and the recovery is
  `DELETE /<backing-index>` per index, because an orphan may hold the only copy
  of real data. And `aliases.json` still has both swallow shapes that were
  fixed here for `cluster_state.json`: an unreadable aliases file is swallowed
  at boot and overwritten by the next alias write, and a *failed* alias write
  is only logged, so an alias can survive on disk after the stream it named was
  deleted with a 200. Both are pre-existing behaviour, not introduced here or
  made worse here, and are tracked separately.

- **`autoindex` no longer leaves immortal catalog entries for skipped files**
  ([#238](https://github.com/xerj-org/xerj/issues/238)). A file added after the
  resume plan was frozen is skipped and reported in the catalog — but that
  report was written from a per-run `Vec` that nothing durable remembered, so
  once the file left the corpus no run could ever remove its `file:{key}`
  document. The catalog is the data map every `map`, `status` and agent query
  reads, and it kept advertising files that were gone. Skipped files are now
  recorded in the durable plan and swept from the catalog when they leave the
  corpus. The sweep is safe to do completely because a skipped file is never
  indexed and never enters the graph corpus — that one catalog document is its
  entire live footprint. Ordering is deliberate: the plan entry is added only
  after the document is written and dropped only after the delete has landed,
  so a failed catalog bulk is retried by the next run instead of stranding the
  document. This does **not** implement add/change/delete reconciliation for
  *indexed* files, which remains open.

- **Repeated `autoindex` scans keep agent-facing map metadata durable.**
  Dataset source bytes, parser-junk counts and coercion-drop notes now derive
  from committed per-file journal records instead of invocation-local worker
  counters, so an unchanged resume does not overwrite them with zero. Run
  timestamps now distinguish invocation start (`started`) from summary
  generation (`summary_generated_at`), and `junk_records_total` on the run
  document is the same durable sum the per-dataset `junk_records` reports
  rather than an invocation-local counter that read `0` after a no-op resume.
  Existing catalogs whose historical `started` field was dynamically mapped
  as text remain usable: the additive mapping upgrade no longer attempts an
  incompatible text-to-date type change. Concretely, this is catalogs written
  by **v1.0.0-rc.4** — the one release that had `autoindex` but not yet
  dynamic ISO-date inference (added in rc.5). Measured against a live engine,
  adding `started` as `date` to such a catalog is refused 400
  `mapper_parsing_exception` (*"field [started] already exists as [text],
  cannot add [date]"*), which aborts the invocation before any document work.
  Catalogs written by rc.5 or later already inferred `started` as `date` and
  were never affected.

  Three consequences worth knowing before you upgrade:

  - **`bytes` is redefined.** It used to be the bytes the latest invocation
    processed, credited to the first dataset a file was assigned to. It is now
    the complete canonical source size, counted once in **every** distinct
    dataset that source is assigned to. Per-dataset values therefore get
    larger, and summing `bytes` across datasets can exceed the corpus size
    when one source feeds several datasets — exactly as one source already
    contributed to several dataset-local file counts. It is a dataset-local
    source footprint, not a partition of physical storage.
  - **Journals written before this release carry no per-dataset coercion
    record**, because the new `dropped_by_dataset` field defaults to empty
    when an older journal is replayed. Byte and junk counts reconstruct fine
    (their journal fields already existed), but a dataset whose files are all
    unchanged loses its `N field values dropped by coercion` note on the first
    resume after upgrading, and only regains it once one of its files is
    reprocessed. Re-run with `--fresh` if the note matters more than the
    rescan.
  - **`junk_records_total` is narrower than the number it replaces**, and one
    class of failure has left it. It used to be an invocation counter that
    also folded in records the *backend* refused per bulk item; it is now
    parser junk only, so `xerj autoindex map`'s header counts exactly what the
    per-dataset `junk_records` values count. No signal is lost in practice: a
    single per-item rejection already aborts the run with
    `autoindex stopped with bulk/backend failures`, before any run document or
    map is written, so the folded-in number was never observable in a map.
    That abort message now also states how many records were refused, which
    the first-five error sample could not convey.

- **The installer is now fail-closed on checksum *verification*, not just on
  checksum *download*.** `landing/get` printed
  `warning: sha256sum/shasum not found — skipping checksum verification` and
  installed anyway — which is not what "refusing to install an unverified
  binary", three lines above it, implies. It now searches `sha256sum`,
  `shasum`, `openssl`, `sha256`, `busybox` and `cksum`, and stops if none is
  present. A machine with no hashing tool at all can still proceed, but only by
  setting `XERJ_INSECURE_SKIP_CHECKSUM=1` — an explicit choice, made by the
  user, not a silent default. The downloaded checksum is also validated as 64
  hex characters before use.

- **`XERJ_LIBC=gnu` makes the glibc Linux builds reachable from the one-line
  installer again.** `landing/get:61` unconditionally overrode the target with
  `unknown-linux-musl` after the `case` arm had set `unknown-linux-gnu`, so the
  script could never request a linux-gnu asset — while the code read as though
  it could. musl remains the deliberate default (static, no glibc-version
  floor, and xerj links jemalloc so musl's allocator is not in the path), and
  the comment now says so; `XERJ_LIBC=gnu` opts into the glibc build.

### Added

- **`autoindex` reports real progress, an honest percent and an ETA — no run is
  silent** ([#241](https://github.com/xerj-org/xerj/issues/241)). Between 47%
  and 64% of a typical run produced no output at all, which is the worst
  possible behaviour for the two audiences that matter: a person watching a
  laptop they cannot use, and an AI agent that has gone quiet on that person's
  behalf. Progress now covers phase, items done and total, rate and ETA, in both
  modes — a terminal gets a live redrawn line, a pipe gets periodic structured
  `xerj-progress` lines an agent can parse, and every run ends with a single
  `xerj-done` summary. The percent is bytes-based so a large file cannot make it
  stall at 99%, the first ETA is withheld for five seconds and labelled `rough`
  until there is enough evidence for it, and the line names the file currently
  being waited on. `--quiet` still produces exactly nothing on either stream,
  and `--progress none` is honoured everywhere — the resource policy's own
  startup notes were moved onto the same surface so they cannot leak into
  `--progress json`'s single parseable stream.

- **`/get` and `/get.ps1` are counted.** `functions/get.js` (a Cloudflare Pages
  Function) serves the installer straight out of `landing/get`, unchanged, and
  records the request. The install path had never had a counter of any kind —
  `curl` runs no JavaScript, so no page beacon could ever observe it, and the
  number of installs was simply unknown. It records timestamp, country,
  User-Agent and a coarse OS guess; it records **no IP address, no cookie and
  no identifier of any kind**, so two installs by one person cannot be
  distinguished from two installs by two people. It is fail-open: if the
  storage binding is missing or the write throws, the installer is still
  served. **No telemetry was added to the xerj binary, and none is planned.**

- **`metrics/release-downloads.jsonl` + a daily GitHub Action.** GitHub reports
  release `download_count` as a running total and keeps no history, so the one
  uninflated adoption number this project has could be read but never trended.
  One committed line per day turns it into a series. `scripts/adoption-snapshot.sh`
  prints the funnel on demand, including which repo-level numbers are
  contaminated and why.

## [1.0.0-rc.13] - 2026-08-08

### Security

- **All 13 open Dependabot advisories against `engine/Cargo.lock` closed**
  (#220). Twelve by lockfile-only version bumps — openssl 0.10.81 /
  openssl-sys 0.9.117 (8 advisories: GHSA-8c75-8mhr-p7r9, GHSA-ghm9-cr32-g9qj,
  GHSA-hppc-g8h3-xhp3, GHSA-pqf5-4pqq-29f5, GHSA-xp3w-r5p5-63rr,
  GHSA-phqj-4mhp-q6mq, GHSA-xv59-967r-8726, GHSA-xmgf-hq76-4vx2),
  rustls-webpki 0.103.13 (GHSA-82j2-j2ch-gfr8), quinn-proto 0.11.16
  (GHSA-4w2j-m93h-cj5j), rand 0.8.7 (GHSA-cq8v-f236-94qc), webauthn-rs 0.5.5
  (GHSA-22w3-693w-x895) — and one by removal: `protobuf` is gone from the
  dependency graph entirely (GHSA-2gh3-rmm4-6rq5; `prometheus` now builds
  without its protobuf feature — XERJ only ever served the Prometheus text
  exposition format, and two new tests pin that format). A reachability
  analysis found none of the 13 exploitable in a default deployment; closed
  anyway.
- npm developer-tooling bumps (#194): basic-ftp 5.3.1, ip-address 10.4.0,
  js-yaml 4.3.1, ws 8.21.3 — transitive from puppeteer, never part of a
  release artifact.

### Changed

- **The Helm chart now defaults to secure mode** (#213). It shipped
  `insecure: true` — TLS and auth off — while the `/get` installer shipped
  auth on. The default is now secure, and a new `NOTES.txt` states at install
  time exactly what insecure mode turns off.

### Documentation

- Roadmap to 1.0.0 GA from a source review against the user-feedback corpus
  (#212).
- Install one-liner moved first on README, xerj.org hero and llms.txt, plus a
  paste-to-your-agent install prompt (#219).

## [1.0.0-rc.12] - 2026-08-07

### Changed

- **`doc_values` on the mapping is now honoured instead of silently ignored.**
  `"doc_values": false` was accepted, echoed back by `GET _mapping`, and had no
  effect — aggregating and sorting on such a field succeeded, where
  Elasticsearch errors. The doc-values sidecar builder was schema-free and filed
  a column for every `_source` string it saw. The default now follows ES: no
  doc-values for `text`, `annotated_text`, `match_only_text`,
  `search_as_you_type`, `semantic_text`, `binary`, `object` and `nested`; an
  explicit `"doc_values": true` on a text field still builds the column and
  still returns whole-value buckets.

  Measured on a 500k-doc corpus (one analyzed `text` field), force-merged: index
  154,413,167 → 99,424,772 bytes (**1.553x smaller**, `index/raw` 0.433x →
  0.279x) and force-merge 139.13 s → 59.69 s (**2.33x faster**, merge re-encodes
  the sidecar and there are 55 MB fewer bytes to build). The `.dv` artifact falls
  95.9%; every other artifact is byte-identical. A `terms` aggregation on a text
  field got **1.34x faster** (3414 → 2547 ms) after losing its column, because
  walking a column of whole document bodies was slower than the brute path it
  falls back to.

  The saving is corpus-dependent: it removes 90-99% of the doc-values sidecar,
  and that sidecar is 2-37% of index size depending on how large text bodies are
  relative to `_source`. On source-code corpora where `_source` dominates it is
  nearer the low end.

### Performance

- **The per-segment FTS reader is cached instead of rebuilt on every query.**
  `FtsIndexReader::open` sat inside the per-segment search loop and performed two
  zstd decompressions per field — the whole `.post` blob and the whole `.meta`
  array — into owned buffers, then discarded them. Measured on one production
  field: ~50 ms and ~41 MB per (segment, field), per query.

  Readers are now cached per (segment, field-set), charged against the segment
  hydration budget under a new `fts_reader` category (visible in
  `_nodes/stats`), and evicted at merge completion. Segments are immutable, so
  no invalidation is needed; if the budget refuses the charge the reader is
  returned uncached, so an oversized corpus degrades to the previous behaviour
  rather than growing without bound.

  Measured p50 on 500k docs with the query cache disabled: `match` on text
  166.29 → 57.85 ms (2.87x), 500-doc page 168.96 → 64.05 ms (2.64x), `prefix`
  170.91 → 69.22 ms (2.47x), `wildcard` 180.52 → 74.79 ms (2.41x), `fuzzy`
  306.16 → 195.25 ms (1.57x), `match_phrase` 495.58 → 339.90 ms (1.46x).
  Brute-scan families (`function_score`, `boosting`) are unchanged — their cost
  is scanning and scoring, not opening a reader.

### Fixed

- **`xerj autoindex` no longer duplicates an index when an extractor improves**
  (#178). Datasets were inferred by comparing each file's whole field-name set,
  including names the extractor invents rather than reads from the file
  (`defs`, `symbols`, `symbol_count`, `title`, `page`, …). A better parser makes
  those names appear, which moved the file into a different dataset — and the
  dataset slug is an ingredient of every document `_id`, so the file was
  re-indexed under a new id in a new index while its previous document stayed
  behind, unreferenced. Measured on a 846-file Rust corpus, re-planning with an
  improved symbol extractor grew the index from 53,873 to 53,902 documents with
  29 stale leftovers and exit code 0; on a 2,328-file C corpus, 7,923 to 8,023.
  Clustering now uses only the field names that came from the file, so gaining
  or losing extractor fields can no longer re-home a document: both re-plans
  hold at 53,873 and 7,923.

  Nothing is deleted, so the first *re-plan* after upgrading (`--fresh`, a new
  `--state-dir`, or a lost journal) re-homes the files whose datasets merge and
  leaves their previous documents in place — 53,873 → 58,855 on that Rust
  corpus, a one-time cost paid once instead of once per extractor change. An
  ordinary re-run reuses the journal's frozen plan and is unaffected. Datasets
  inferred from real schemas are untouched: that corpus went from 9 datasets to
  7 (the two symbol-presence variants of one code dataset merged, likewise two
  section-presence variants of one prose dataset), and the C corpus from 407 to
  405, with every document accounted for in both.

## [1.0.0-rc.11] - 2026-08-04

### Added

- **`xerj autoindex` pins the server's embedding execution identity** (#160).
  Vectors produced by two different embedding backends — or by two different
  models, tokenizers, or vector widths within one backend — are not comparable,
  so mixing them in one index silently degrades every similarity result rather
  than failing. A semantic autoindex run now fetches
  `GET /v1/embedding/identity`, records the returned opaque digest in its
  journal before the index is created, and requires the same digest on every
  resume; a mismatch aborts before the next index-create or `_bulk`. There is
  no new command or setup step — `xerj autoindex ./reports` calls the endpoint
  itself over the already-configured `--url`, and plans with no semantic fields
  skip it entirely.

  The endpoint is authenticated (cluster-read) and deliberately opaque: it
  returns a version, the *effective* backend, a digest, the semantic contract,
  whether that backend is resumable, and a sanitized reason when it is not. It
  never returns credentials, provider URLs, model names, or local paths.
  `dimensions` is reported only for the backends whose width the server
  actually pins (`lexical`, and `onnx-experimental` at 384); it is omitted for
  `neural`, whose width comes from the loaded model's `hidden_size`, and for
  `proxy`, whose width is whatever the remote returns.

  `lexical` and `onnx-experimental` are resumable. `neural` and `proxy` are
  not: neither attests immutable model bytes, so a fresh semantic run is
  allowed but a resume is refused.

- **A detector that connects documents by what they say, not only by what they
  cite** (#164). Every shipped detector was structural — `wikilink`, `mdlink`,
  `href`, `pathcite` and `cratecite` need an explicit citation, `sequence` and
  `samedir` need a filesystem relationship — so a folder of PDFs or saved pages,
  where no document links to another, produced a graph whose only edges ran
  inside single documents and along directory chains. `sharedterm@1` emits a
  `shared_term` edge (weight 0.45, confidence 0.5) between two documents that
  use the same distinctive words, and carries those words as the edge evidence
  so a wrong link is inspectable exactly like a wrong `mdlink`.

  Measured on 240 arXiv PDFs filed in 24 topic folders (236 extractable), same
  binary, same corpus: before, 11,854 edges — 11,642 `sequence` inside single
  documents plus 212 `same_dir`, with 212 cross-document edges and **none**
  crossing a folder. After, 462 `shared_term` edges (271 further candidates
  refused by the density cap), 377 of them joining documents in different
  folders and 296 joining different top-level topics. Two independent runs
  produced identical counts and identical evidence.

  Density is capped rather than trusted: a term in more than 10% of the corpus
  is that corpus's vocabulary and is ignored, a link needs at least two shared
  word families (so a singular and its plural are not two signals), and no
  document takes more than 5 `shared_term` edges. What the cap refused is
  reported by the run as `edges_capped` instead of vanishing. A corpus of ten
  documents or fewer therefore produces no `shared_term` edges at all, which is
  the 10% rule being honest rather than an exception.

  The honest limit, which the docs and the console now state: this links
  documents that share VOCABULARY, not documents that share MEANING. Terms are
  compared as strings with no stemming and no model — XERJ's built-in embedder
  is lexical feature hashing and this detector does not use even that. Against
  the corpus's own topic filing, a random pair of those documents shares a
  top-level topic 17.0% of the time; `shared_term` links do so 35.9% of the
  time. Real signal, and a noisy one.

### Changed

- **BREAKING: an existing semantic `autoindex` state directory cannot be
  resumed** (#160). A journal written before this change carries no pinned
  identity, so there is no way to prove the server still produces the same
  vector space; rather than assume it, the resume fails closed with *"this
  semantic autoindex journal predates embedding identity pinning and cannot be
  resumed safely"*. The same path fires when a previous `--no-semantic` run is
  followed by a semantic run over the same state directory. Recover by
  rebuilding with `--fresh` and a new `--prefix`; before reusing the old
  prefix, delete and recreate its prior autoindex indices. Non-semantic runs
  and brand-new state directories are unaffected.

- `EmbeddingProxy::new` now rejects an endpoint that is not `http(s)` at
  construction time instead of at first request, so an invalid
  `embedding.default_endpoint` falls back to lexical at startup and
  `/v1/embedding/identity` reports `lexical` — the backend the server will
  really use — rather than `proxy`.

## [1.0.0-rc.10] - 2026-08-03

### Security

- **The Console data-sources proxy is no longer a way around a brain** (#149).
  The Console router is merged onto the engine routers after their layers are
  applied, so `authz_middleware` never ran on it and no index-visibility scope
  was installed; its only filter matched `.xerj_` (underscore) and therefore
  missed the reserved `.xerj-memory-` namespace. Any authenticated Console
  session of any role could read another tenant's second-brain documents, and
  the indices/fields listings leaked brain names and mappings. All three
  handlers now refuse the reserved namespace with the same `404` a system index
  gets, and the prefix moved to `xerj-common` so there is one definition.

- **Snapshot and restore can no longer reach another tenant** (#152).
  `authorize_expression` waved every wildcard through before consulting the
  principal's grants. That is sound where the engine expands a pattern over the
  caller's visible set, but `create_snapshot` walks the index map itself and
  `restore_snapshot` expands against the snapshot manifest and then removes and
  rewrites the index directory, never passing the visibility funnel. A
  non-superuser `POST /_snapshot/{repo}/{snap}/_restore {"indices":".xerj-memory-*"}`
  therefore rolled every tenant's brain back to the backup instant. Patterns on
  those two verbs are now decided up front: one that may reach the reserved
  namespace requires an explicit grant, exactly as an index template's
  `index_patterns` already did. Narrower patterns are untouched.
  The create handler also read `indices` only in its array spelling, so the
  string form ES equally accepts fell through to "absent" and captured the whole
  node; both spellings now parse, and patterns resolve against the
  visibility-filtered index list. (This also fixes `{"indices":["*"]}`
  previously producing an empty snapshot.)

- **Scripted updates are bounded by the same script budget as search** (#153).
  `_update` and `_update_by_query` reach Painless through
  `transform_document_serialized`, which established neither the request
  deadline nor the fault sink, so every evaluation fell back to its own full
  slice. With a fresh context per `;`-separated statement, a script's cost
  multiplied by its statement count and again by the hit count, and
  `wait_for_completion=false` detached the whole thing with no ceiling. One
  deadline now governs the request, faults are captured once rather than per
  document, and the hit loop stops at the deadline and reports `timed_out`
  honestly instead of a hardcoded `false`.

### Fixed

- **A client disconnect can no longer brick an index** (#150).
  `index_document` held the collection-publication guard across
  `self.schema.read().await`, the only suspension point between `begin()` and
  `commit()`. The PUT handler runs inline on the connection task, so a
  disconnect dropped that future; parked behind schema evolution's write lock
  the drop landed inside the interval and left the guard `Pending`, setting a
  sticky poison flag. Every later read and write on that index then failed
  permanently, and `_close`/`_open` do not reconstruct the index, so only a
  process restart recovered. The schema guard is now taken before the interval,
  which contains no `await` at all and cannot be cancelled part-way. Storage
  failures that occur before any visibility mutation now cancel the publication
  instead of poisoning it, so a transient disk error fails one request.

- **Columnar aggregations no longer drop a segment's documents** (#151, #143).
  `field_needs_brute_fallback` kept the fast path whenever any flushed segment
  carried a field's doc-values column, but array suppression is decided per
  segment, and the executors skip a column-less segment. Indexing scalar values,
  refreshing, then indexing array values and refreshing produced a terms
  aggregation that silently omitted the second segment's documents; term
  predicates and numeric metrics undercounted the same way. Coverage must now be
  total, and the check moved into `seg_field_kind` so it also covers sub-metric
  fields, `top_hits` sort fields, `matrix_stats` fields and composite sources.

### Thanks

- @Nicolas0315 for four detailed, measured performance reports (#144, #145,
  #146, #147) on fuzzy/wildcard term expansion, top-k scoring and the unused
  skip table, the `bbq_*`/`int8_hnsw` mapping contract, and filtered kNN never
  reaching the HNSW graph. Each arrived with a standalone harness and honest
  caveats. They are tracked for the following release rather than rushed into
  this one.


### Changed — runtime-field types (can break existing mappings)

- **`mappings.runtime` is now validated against ES's runtime-field type
  registry**, not against the mapping-property type list. Runtime fields
  have their own, much smaller registry in ES: `boolean`, `composite`,
  `date`, `double`, `geo_point`, `ip`, `keyword`, `long`, `lookup`.
  Anything else is refused at index creation with the same
  `mapper_parsing_exception` ES emits ("The mapper type [x] declared on
  runtime field [f] does not exist…").

  This cuts both ways, and both directions were wrong before:

  - **Newly rejected.** Types that are perfectly valid *mapping
    properties* but illegal under `runtime` — `text`, `match_only_text`,
    `nested`, `object`, `geo_shape`, `shape`, `point`, `dense_vector`,
    `sparse_vector`, `binary`, `percolator`, `completion`, `histogram`,
    `alias`, `join`, `version`, `constant_keyword`, `wildcard`,
    `date_nanos`, the range types, and the narrow numerics (`integer`,
    `short`, `byte`, `float`, `half_float`, `scaled_float`,
    `unsigned_long`, which collapse into `long`/`double` at runtime) —
    used to be accepted with a `200`. **A mapping that declared any of
    these under `mappings.runtime` will now fail to create.** Change the
    entry to the runtime type ES would have required (usually `keyword`,
    `long` or `double`), or move the field into `properties`.
  - **Newly accepted.** `composite` and `lookup` are runtime-ONLY types
    and are absent from the property list, so they were previously
    rejected outright — even though `lookup` runtime fields are
    implemented on the search path (via search-body `runtime_mappings`)
    and covered by the conformance suite. Both are now accepted under
    `mappings.runtime`.

  Scope: this is `PUT /{index}` **validation** only. It does not change
  evaluation: `mappings.runtime` is still stored and never evaluated —
  runtime fields are only computed when supplied in the search body as
  `runtime_mappings`. Search-body `runtime_mappings` and
  `PUT /{index}/_mapping` still do not validate runtime types at all
  (they never did) — unchanged here.

  Prior to this, the rule was half-enforced: `flat_object`/`flattened`
  were special-cased into a `400` while every other ES-forbidden type
  passed.

### Security

- **`x-forwarded-for` is no longer trusted for client identity** (#76 S5-4).
  The per-IP auth rate limiter and the audit-log source address used to be
  read straight out of `x-forwarded-for` — a header the caller writes. A
  rotating `x-forwarded-for: 1.2.3.$RANDOM` therefore reset the quota on
  every request, so the unauthenticated magic-link redeem and bootstrap-claim
  endpoints could be brute-forced without limit, while honest clients (who
  send no such header) stayed throttled. Client identity now comes from the
  TCP peer: `ConnectInfo<SocketAddr>` is threaded through both the plain and
  the TLS listener, and a handler reads it via the new `ClientIp` extractor.

  Forwarding headers are still honoured behind a real reverse proxy, but only
  from a proxy the operator has declared: the new **`server.trusted_proxies`**
  setting takes IP addresses or CIDR blocks (`["10.0.0.0/8", "::1"]`) and is
  **empty by default — an unconfigured node believes nobody**. When the peer
  is a declared proxy the forwarded chain is read right-to-left (the left end
  is caller-authored) and the right-most address that is not itself a listed
  proxy wins; a malformed element stops the walk rather than being stepped
  over. Malformed entries in the setting fail startup instead of silently
  widening or narrowing trust.

  Operators terminating TLS or load-balancing in front of XERJ must set
  `server.trusted_proxies` to their proxy's address, otherwise every user
  behind it shares one rate-limit bucket.
- **Cluster control frames are now authenticated (issue #75).** The inter-node
  transport on `cluster.port` accepted any TCP connection that spoke its wire
  format: anyone who could reach the port could send Raft `RequestVote` /
  `AppendEntries` frames and steer consensus. Every frame now carries an
  HMAC-SHA256 tag over a per-connection random challenge issued by the
  receiver, the sender's node id, the frame's position in the connection, and
  the payload; tags are compared in constant time, and the tag is verified
  before the payload reaches the JSON deserialiser. The challenge makes a
  captured frame non-replayable, including against a different node.
- **Cluster mode fails closed.** `[cluster] enabled = true` now requires
  `cluster.auth_secret` (or `XERJ_CLUSTER_AUTH_SECRET`), minimum 16
  characters. With cluster mode on and no secret from either source the node
  **refuses to start** instead of running an unauthenticated control port.
  Single-node mode — the default — is unaffected and needs no secret.

### Breaking

- **The cluster wire format changed and is not backward compatible.** A node
  running this version cannot talk to a node running rc.9 or earlier in either
  direction: the receiver now speaks first (magic, version, challenge) where
  the old protocol expected the sender to open with a length-prefixed JSON
  frame. Upgrading a cluster requires stopping every node and restarting them
  on the new version with a shared secret configured — a rolling restart will
  not interoperate. Only cluster mode is affected; it is off by default, so
  single-node deployments need no action.
- Scope note: the HMAC authenticates frames, it does not encrypt them. Cluster
  traffic is still plaintext and the secret is cluster-wide, so it proves
  cluster membership, not per-node identity. `cluster.port` still belongs on a
  trusted network segment; mTLS remains unimplemented.

### Added

- **`script` query support** (#87). A Painless predicate evaluated per document,
  wired into the doc-scan path. Missing fields fail closed the way modern
  Elasticsearch does: `doc['missing'].value` errors rather than coercing to a
  default, because since ES 7.0 reading `.value` off an empty `ScriptDocValues`
  throws (6.x returned a type default with a deprecation warning and 7.0 removed
  that fallback). Coercing would mean a script filter with a typo'd field name
  matched *every* document. The guard idiom ES documents still works, because
  that idiom is `doc['x'].size() == 0 ? <default> : doc['x'].value` and
  `.size()`, `.length` and `.empty` are total on a missing field.

  Compiled scripts are now cached, bounded at 128 entries and 512 KiB of source
  per thread. A script is evaluated once per document, so re-parsing per
  document made the per-document cost scale with the script's *size*. The cache
  key is attacker-supplied source, so the bound is a security property rather
  than an optimisation detail.

- **`getDayOfWeekEnum().getDisplayName()` in Painless** (#92).

### Fixed

- **Painless resource-limit trips surface instead of returning a wrong score**
  (#97, #123). `MAX_CALL_DEPTH = 32` is a real ceiling on legitimate recursion,
  and in the scoring paths it failed *silently*: a script recursing past it did
  not error, it returned a wrong score.

  Two failure classes had been collapsed into one and they need opposite
  treatment. Unsupported or unparseable script syntax still degrades quietly,
  which is what ES does and what seven of nine call sites rely on. A tripped
  resource limit now propagates, because a silently wrong score is undetectable.
  `_search`, `_msearch`, both template variants, scroll, async_search,
  `_rank_eval`, `_explain` and pivot transforms report it; `_count`, `_reindex`,
  `_delete_by_query` and `_update_by_query` refuse rather than act on a selection
  a fail-closed script truncated. All nine `eval_painless` call sites were
  audited and behaviour changed at none of them.

  `_rank_eval` records the fault per request in `failures` rather than failing
  the batch, and `_explain` — a diagnostic endpoint whose `matched` verdict does
  not depend on scoring — answers and discloses the fault rather than refusing.
  `_delete_by_query` and `_update_by_query` previously returned HTTP 200 with a
  400-shaped body, so a client branching on the status saw success.

  `MAX_CALL_DEPTH` stays 32: re-measured in release, the parser's worst case is
  40,720 bytes per call level, so depth 32 already uses 1.20 MiB of a 2 MiB
  worker stack.

- **`/_msearch` no longer skips the request-time script guard** (#111, #124). It
  built its sub-requests straight off `parse_request`, bypassing the validation
  the single-search path applies. Measured: a 240,531-byte `_msearch` body whose
  items carried scripts that `_search` rejects with a 400 came back 200 on every
  item. `_search/template` and `_msearch/template` had the same hole.

  Closing it initially made `_msearch` *stricter* than `_search`, because the
  new guard walked the whole body while `_search` checks six specific fields. A
  single `GuardedField` definition now drives every entry point, with the
  `aggs`/`aggregations` pair resolved exactly as the executor resolves it
  (`aggs` wins when both are present, regardless of document order). A
  compile-time assertion pins the guarded set at six, because removing an entry
  was not a type error and silently reopened the bypass.

- **`<field>.keyword` predicates resolve against the parent column** (#110).
  Four raw lookups inside `resolve_pred` failed closed, asserting that a flushed
  segment held no matching row, while the in-memory arm resolved the same field
  correctly. Measured: a `filters` aggregation on `extension.keyword` reported
  200 documents out of 3,200, and a `term` query on `group.keyword` reported 0
  hits out of 6,000.

- **`fast_aggs` falls back to brute force for unresolvable nested fields**
  (#104). Found live on real Kibana and OpenSearch Dashboards sample dashboards:
  `geo.dest` terms, `machine.ram` avg and the doubly-nested `machine.os.keyword`
  terms all returned empty or null through the fast path while the brute path
  computed the right answer.

- **The columnar histogram paths honour `config.limits.max_buckets`** (#121,
  partial). `exec_date_histogram` and `exec_histogram` carried their own
  hardcoded `65_536`. Measured at cap 37 on a 12,000-document index: 38 buckets
  returned, no error. The brute paths ignored the setting too, so this was live
  in both executors. **Still open:** `exec_terms` has no bucket cap at all, which
  is the actual OOM vector; see #121.

- **`bucket_script` resolves `_count` and evaluates parenthesised ternaries**
  (#95, #105). A `_doc_count`-weighted bucket and the `bucket_script` inside it
  disagreed about how many documents the bucket held. Verified per aggregation
  type. Parenthesised ternaries silently resolved to a null bucket value,
  because the split only looked at paren depth 0. Two equal infinities compared
  *unequal*, because `inf - inf` is NaN and an epsilon-only `==` rejected
  identical values.

- **Named ES date formats are resolved for `ignore_malformed` validation**
  (#89). Named shorthands were matched as literal text rather than expanded, so
  a value genuinely valid under the declared format was silently dropped on
  ingest. Confirmed against a real OpenSearch 2.11.1 node, which accepts the ISO
  strings and rejects the bare numbers — the opposite of what XERJ did.

- **Two `xerj-engine` tests no longer flake under parallel load** (#100). One
  mutated a process-wide bucket cap under a lock that only protected
  participants; the other depended on wall-clock timing. Measured on a 32-core
  box under 14 CPU hogs: 12/12 and 10/10 green, where the prior tree was 18%
  red.

- **`_field_caps` reports `flat_object`/`flattened` rather than generic
  `object`** (#86), and **`flat_object` is treated as an alias for `flattened`**
  for OpenSearch callers.

- **Keyed (object-shaped) bucket aggregations merge across multi-index search**
  (#103), and **`X-OpenSearch-Version` is sent to OpenSearch callers** (#101).

### Performance

- **`parse_date_ms` no longer pays an unbounded scan per document** (#91, #96).
  The no-colon zone-offset branch added in #91 sat before the naive fast paths
  and cost roughly 2x on offsetless ISO values, a hot per-document ingest path.
  The probe now reads a fixed six-byte window at the end of the value and
  nothing else — a constant window is sufficient because `%z` closes the pattern
  and chrono requires the whole value to be consumed.

  Measured across three independently built, CPU-pinned harnesses: offsetless
  ISO 217.0 → 110.2 ns/op, and the long-value classes that an interim scan-based
  guard regressed (4 KB prose 1171.4 → 198.3, 64 KB 16278.0 → 1009.2) are back
  at or below the pre-guard cost. Best-or-tied on every class, with zero
  mismatches across 72,462 constructed values.

## [1.0.0-rc.9] - 2026-08-01

Ninth release candidate: the **cross-platform correctness release**.
Headline for users, stated plainly because it is the most serious defect
this project has shipped: **the Windows binaries published as rc.4
through rc.8 could not start.** Every index creation returned
`Access is denied. (os error 5)`, and because the console bootstrap
creates a system index on first boot, the server aborted before it ever
listened. `xerj.org/get.ps1` installed those binaries for three weeks.
The cause was one unguarded Unix idiom in the durability path; the fix
is four lines, and a Windows runner now boots the binary, writes a
document, reads it back and indexes 400 datasets on every pull request.
Nothing else caught this because, until this release, no CI job had ever
*run* the binary anywhere except Ubuntu — `release.yml` built eight
targets and executed none of them.

The release also closes the self-contained half of the post-audit
security backlog — a snapshot location that could still escape
`data_dir`, index-name validation missing at the create boundary, a
field limit that dynamic ingest enforced and explicit mapping updates
did not, an unserialised magic-link redemption, and node identity leaking
from an unauthenticated endpoint — and it carries the consequences of
taking the Windows lesson seriously: a test pass over the surfaces that
shipped untested, and the four real defects writing those tests exposed.
A create-time file-descriptor exhaustion reachable from an index-create
request, inline `<script>` contents indexed as prose, XML record election
that changed shape between runs of the same file, and a core dump on
`xerj … | head`. The Rust suite grows from 1,420 to 1,486 test
functions; conformance is unchanged at 1360 passed / 0 failed / 3
skipped. The only performance change is a memory regression fix in the
ONNX embedding path; no speed measurement was taken and none is claimed.
The `_reindex` keyset behaviour is untouched — a test around it was
wrong, not the code, and that distinction is spelled out below because
it is easy to misread as a data-loss bug.

### Fixed

- **The server can start on Windows.** `xerj_common::fsio::fsync_dir`
  was `std::fs::File::open(dir)` followed by `sync_all()`, with no
  platform gate. On Windows, obtaining a handle to a *directory* that
  way always fails with `ERROR_ACCESS_DENIED (os error 5)`: the standard
  library cannot pass `FILE_FLAG_BACKUP_SEMANTICS`, which that call
  requires. `IndexStore::save_snapshot` fsyncs the parent directory
  after publishing the segment manifest, so **every index creation
  returned an I/O error**, and the unconditional `xerj-console`
  bootstrap — which creates `.xerj_users` on first boot — turned that
  into a fatal `xerj-console bootstrap` error at startup. Introduced in
  `297be60` (2026-07-12) as part of the power-loss-ordered publish
  chain, so it affected rc.4, rc.5, rc.6, rc.7 and rc.8. Windows now
  takes a separate `fsync_dir` that returns `Ok(())`, because Windows
  exposes no directory-flush primitive at all — `FlushFileBuffers` is
  not defined for directory handles. **State the consequence honestly:
  durability on Windows is weaker than on Unix.** The file contents are
  still fsynced by the callers before the rename; what is not forced is
  the rename itself, which relies on NTFS metadata journalling. That is
  the best available behaviour on the platform, and it is now a
  documented property rather than an error returned on every call.
- **`xerj … | head` no longer aborts with a core dump.** The Rust
  runtime installs `SIG_IGN` for `SIGPIPE` before `main`, so once the
  reader closed the pipe the next write returned `EPIPE`, `println!`
  panicked with `failed printing to stdout: Broken pipe`, and the
  release profile's `panic = "abort"` turned that into a core dump —
  which also swallowed the command's real exit status. The binary now
  restores the default disposition before anything can write to stdout
  (unix only), so the kernel terminates the process on the signal and
  shells report the conventional 141. Found because `xerj autoindex map
  | head -80` core-dumped on every run of a use-case harness. **Two
  consequences worth knowing.** Client sockets are unaffected: the
  standard library writes those with `MSG_NOSIGNAL` (`SO_NOSIGPIPE` on
  macOS), so a client disconnecting mid-response still surfaces as
  `EPIPE` and cannot signal the server. The server's own stdout does
  change — previously tracing discarded write errors and a server
  outlived a vanished stdout; now the next log line terminates it.
  Supervised deployments redirect stdout to a file or `/dev/null` (the
  `brain` boot path included), so in practice this reaches only
  `xerj | reader-that-leaves`.
- **Inline `<script>` and `<style>` contents are no longer indexed as
  prose.** The HTML extractor's `skip_until` state suppressed the
  *tags* inside those elements but the tokenizer's text branch appended
  their contents to the buffer unconditionally, and the closing tag
  continued without discarding it — so the buffered CSS and JavaScript
  were flushed into the document body at the next tag boundary.
  Minified stylesheets and scripts became indexed prose in every HTML
  record: BM25 noise, and — the reason this is more than a quality bug
  — inline configuration blocks carrying API keys, tokens and internal
  endpoints were stored in `_source`. For the "point xerj at my project
  folder" use case that is a real exposure. Script and style elements
  are now treated as the raw-text elements they are — the scanner jumps
  to the literal end tag instead of tokenising inside them, which also
  bounds an unclosed `<script>`: the tail of the file is discarded
  rather than buffered and indexed. Two narrower ways the leak survived
  a first attempt were caught in review and are closed here. A trailing
  `/` inside an *unquoted* attribute value (`<script src=/cdn/lib/>`)
  was read as a self-closing tag, so the element was never entered —
  legal HTML5, since `script` is never a void element. And an end tag
  whose name ran on into a non-alphanumeric character
  (`</script-foo>`) was accepted as the terminator, ending raw text
  early and indexing everything after it. The tag scanner now tracks
  attribute state to decide self-closing, and the end-tag test follows
  the spec's appropriate-end-tag rule: whitespace, `/`, or `>`.
- **XML record election is deterministic.** `elect_record_tag` chose the
  repeating element with `max_by_key` over a `HashMap`, and `max_by_key`
  settles ties by iteration order — which is seeded randomly per map.
  When a wrapper element and one of its children were both structured
  and equally frequent, elections over the *same bytes* chose different
  tags: 200 in-process elections over one fixture split 89 / 111. The
  record count and the locators held, so documents kept their `_id`s,
  but each build gave them a completely different field set, and field
  inference for that dataset never settled across builds. The comparison
  is now total — most occurrences, then the outermost tag, then the
  lowest name — with the outermost tag as the principled key: when a
  wrapper repeats as often as its child, the wrapper is the record and
  the child is one of its fields. **Two limits worth being precise
  about.** This bites across *independent* builds — a `--fresh` run, a
  new state directory, a second machine, a CI reproducibility check —
  not across ordinary repeated runs, because the resume journal skips
  files whose content digest has not changed. For the same reason an
  index built before this fix does **not** heal itself: it keeps
  whatever shape it rolled until the XML file's bytes change or the
  index is rebuilt with `--fresh`.
- **Three further inference elections are deterministic.** Reviewing the
  fix above found the same defect — `max_by_key` over a `HashMap`, ties
  settled by a per-instance random hash seed — at three more decisions,
  each with a wider blast radius than the XML one. Each now has a
  tie-break argued from what the decision means rather than a copied
  rule. The log extractor elects the template an *entire file* is parsed
  with, and every line missing that template is demoted to a
  continuation, so a flip changes the record count; ties now go to the
  most specific template, in the order `parse_kind` already tries them
  (app before clf before syslog), so an ambiguous file resolves the way
  an ambiguous line does. Field inference elects the date encoding
  written into the mapping, the catalog and the coercion plan; ties go
  to the lowest `DateEnc` in declaration order, which is `parse_date_str`'s
  own richest-first priority, so a field mixing `…T00:00:13` with
  `… 00:00:13` names the encoding that concedes the least. The third
  elects a field's entity type, keyword versus text.

- **The ONNX embedding path no longer materialises every window upfront.**
  `embed_semantic_jobs` built and cloned the passage text of every window
  before embedding any of them, so peak memory scaled with the whole job
  rather than with what was in flight. Windows now carry
  `(start, end, passages)` and their texts are built lazily and moved
  into `embed_batch`, with at most two batches in flight. This closes a
  memory regression at default settings (#71); no throughput measurement
  was taken and no speed claim is made.

### Security

Five items from the post-audit backlog, each with a regression test
where the defect is testable, plus one found while writing this
release's tests. The three that remain open are listed under Known
limitations rather than quietly omitted.

- **Snapshot repository locations could still escape `data_dir`
  (#73, High).** The residual half of F-PATH-02: a `settings.location`
  containing `..` that resolved to a *nonexistent* target slipped past
  the guard, because canonicalisation fell back to a lexical path and
  the containment check compared components of a path that had never
  been resolved. `..` components are now rejected outright, before any
  resolution. The regression test covers both the direct form and
  laundering it through an explicit allowlist entry.
- **Index-name validation is enforced at the create-index boundary
  (#80).** `IndexName::validate` existed and was applied elsewhere;
  the ES-compatible `create_index` handler did not call it. Wiring it up
  is defense in depth against path traversal through index names — the
  accepted set is unchanged, so no previously valid name is now
  rejected.
- **The field limit applies to explicit mapping updates (#76 S5-5).**
  `max_fields_per_index` was enforced on dynamic ingest but not on the
  explicit `add_fields` path, so `PUT _mapping` and schema evolution
  could push an index past the ceiling that exists to bound mapping
  explosion.
- **Concurrent magic-link redemptions cannot both mint a session
  (#76 S5-3).** The check-then-consume sequence in the redeem handler
  was not serialised, so two requests carrying the same token could both
  pass the used-check. Redemption is now serialised behind a gate.
  Stated precisely, because it is easy to overclaim: in the shipped
  engine the second session was already being blocked incidentally, by
  the index's optimistic concurrency rejecting the colliding write — so
  the observable pre-fix exposure was a 500 with internal detail instead
  of a clean 401, plus a single-use guarantee that rested on what the
  store happened to do with racing writes rather than on the handler.
  The fix moves that guarantee into the handler where it belongs. This
  is covered by 25 rounds of 6 concurrent redemptions.
- **The unauthenticated `cluster/info` body no longer leaks the node
  identifier or exact build version (#76 AUTHZ-2).** It now carries only
  mode and uptime.
- **A per-index setting could exhaust file descriptors at index-create
  time.** `index.xerj_ingest_shards` — added in rc.8 to *reduce*
  descriptor usage — was accepted with only a `>= 1` check, and the
  ES-compatible create-index handler threads the request body's
  `settings` through verbatim. `IndexStore::open` eagerly creates a
  directory and opens a WAL file descriptor per shard, so
  `{"settings":{"index":{"xerj_ingest_shards": 100000}}}` was a
  single-request descriptor and inode exhaustion — precisely the failure
  the setting exists to prevent. The override is now bounded to
  `1..=256`, the same ceiling `EngineConfig::validate` already enforced
  on the global `engine.ingest_shards`. Out-of-range values are
  **refused, not clamped**, so a typo falls back to the engine default
  rather than silently taking a different one. Found while writing the
  tests that rc.8 shipped without.

### Added

- **CI runs the binary on macOS and Windows.** A new
  `autoindex-fd-smoke` job on an `[ubuntu, macos, windows]` matrix boots
  the server under a constrained descriptor budget (soft 256 / hard
  4096 — between the pre-fix need of roughly 6,400 for 400 datasets and
  the post-fix 600), asserts a create-index / index-document / search
  round-trip, autoindexes a 400-dataset corpus and fails on any
  `EMFILE`. macOS runners default to `ulimit -n 256`, the exact
  condition the rc.8 descriptor bug regressed under, so the job
  reproduces it rather than approximating it. The explicit round-trip
  exists so that the next platform-specific break names the operation
  that failed instead of surfacing as an opaque "server exited during
  boot".
- **The use-case harnesses run in CI.** A new `usecase-smoke.sh` gate
  drives the second-brain transcript (overview counts and detector
  families, ego evidence and `not_shown`, idempotent link, unlink
  retiring rather than deleting, `as_of` replay, and the normative
  `hops > 2` / self-edge 400s and unknown-brain 404), the MCP tool
  surface (initialize handshake, the four `xerj_brain_*` tools listed
  with their honesty strings, and a real `tools/call` round-trip), and
  autoindex discovery with post-discovery search. These harnesses
  already existed but were manual, so a regression in the brain API or
  the MCP tool list was caught by nothing. Wiring them up immediately
  found two assertions that could never have passed: the transcript
  pinned detector versions at `@1` when five of them are at `@2`, and
  the autoindex idempotency check compared whole `_cat/indices` rows
  including on-disk size, which legitimately changes across a re-extract.
- **THE MAP's bounded-graph claims are tested.** The knowledge-graph
  pipeline shipped in rc.7 on two load-bearing claims — at most 13
  groups at any scale, and byte-identical output across runs — with no
  automated test in any language behind either; the only exercise was a
  manual browser script needing a live server and a real Chrome. The
  pipeline is pure dependency-free JavaScript, so 23 offline cases now
  run in under a second over four corpus scales (8, 12, 40 and 120
  folders, straddling the 12-cluster cap): the group bound with at most
  one pooled "everything else" body, every file landing in exactly one
  group with the membership lists and the index agreeing both ways, link
  conservation against the honesty row the UI computes from,
  run-to-run determinism, the pairwise bundle bound, and `as_of` replay
  making a retired link live again.
- **Regression cover for four surfaces that shipped without any.** The
  DOCX decompression and paragraph caps from rc.7 (both verified by
  removing each guard in turn and confirming the matching test fails);
  the six autoindex extractors that had no direct tests at all — HTML,
  CSV, JSON, JSONL, XML and SQLite, 49 cases; the magic-link redemption
  race hardened in rc.9's Phase-2 work, driven with 25 rounds of 6
  concurrent redemptions; and the per-index WAL shard override,
  including a reopen test that pins a shard count, writes documents that
  live only in the memtable and WAL, restarts, and asserts both the
  layout and every document survive. An installer lint gate covers
  `landing/get` and `landing/get.ps1`, which had no verification of any
  kind despite being the only user-facing entry point.

### Changed

- **A test was measuring the CI runner's core count, not the code.**
  `reindex_pages_past_10k_via_keyset` failed on any machine with real
  core count (`left: 0, right: 10050`) and passed on CI's two-core
  runners, so `cargo test --workspace` was red for every developer and
  green for the project. Turbo ingest routes documents to memtable
  shards by worker thread: on two cores they all land in one
  incidentally-visible shard, on a real box they scatter into
  unpublished shards that the search the reindex pages over cannot see.
  The sanity assertion above it passed either way because
  `live_doc_count` reads the version map rather than the search surface.
  The test now refreshes the source. **The `_reindex` product path was
  never affected** — driven end to end over HTTP, bulk-indexing 10,050
  documents and reindexing with `size: 1000` reports
  `total: 10050, created: 10050, batches: 11` and the destination counts
  10,050, both with and without an explicit refresh. This was the test
  reaching under the API, not a reindex defect, and it is recorded here
  because a bare changelog line about "reindex returning 0 documents"
  would badly misrepresent it.

### Known limitations

- **DOCX truncation at the decompression cap is silent.** When a
  document's XML inflates past the 72 MiB cap the reader simply hits
  end-of-file; because a truncated tag reads as a clean `Eof` rather
  than an error, nothing increments the junk counter. A bomb-truncated
  document is therefore indistinguishable from a legitimately short one
  — the caller gets a well-formed record that is quietly missing
  content. The memory bound itself holds and is tested; only the
  *reporting* is missing. Making truncation observable would change the
  statistics autoindex reports, so it is deliberately left for a
  separate change.
- **`index.xerj_ingest_shards` is only read in its nested spelling.**
  `{"index": {"xerj_ingest_shards": 1}}` is honoured;
  `{"index.xerj_ingest_shards": 1}` is not, unlike the neighbouring
  `index.sort.*` settings which accept both. Nothing in the product
  emits the flat form, so this is an inconsistency rather than a break.
- **A brain is still not an authorization boundary.** Any authenticated
  caller can read any brain's `overview` and `ego`; with a shared engine
  that is a multi-tenant exposure. This needs an access model rather
  than a patch and is tracked in #79, deliberately not rushed into a
  release candidate. The concrete part of that issue — the walker
  indexing `.env` and other dotfiles — was fixed in rc.7.
- **Cluster transport control messages are still unauthenticated**
  (#75). Reachable only with cluster mode enabled, which is off by
  default.
- **`x-forwarded-for` is still trusted for client identity** (#76 S5-4),
  which means a caller can spoof the address the per-IP rate limiter
  keys on. The fix needs `ConnectInfo` threaded through the shared
  serve and TLS path and is deliberately not bundled with the smaller
  items above. *(Fixed after this release — see Unreleased.)*
- **Windows durability is weaker than Unix durability**, as described
  under Fixed. The platform provides no way to make it otherwise.

## [1.0.0-rc.8] - 2026-07-31

Eighth release candidate: an **autoindex robustness + code-AST release**.
Headline for users: `xerj autoindex` no longer crashes with `Too many open
files` on large source trees, and source code is now parsed into searchable
symbols instead of plain text.

### Added

- **AST code extraction.** Source files are parsed with tree-sitter — Python,
  JavaScript, TypeScript, TSX, Rust, Go, Java, C, C++, Ruby, PHP, C#, Bash —
  rather than indexed as prose. Each file carries its `language`, a structured
  `symbols` array (`{name, kind, line}`), and a searchable `defs` list, so a
  query like `class Model` retrieves the file that defines it. Grammars are
  version-decoupled via `tree-sitter-language`, so one tree-sitter 0.25 core
  serves all of them and adding a language is a grammar dep plus one row.

### Fixed

- **Autoindex `Too many open files` (os error 24) crash** on large repos. Each
  index holds segment mmaps, a WAL and a merge task, so discovering a repo that
  infers hundreds of datasets (WooCommerce ≈ 413 indices, ~16 descriptors each)
  exhausted file descriptors on a default macOS soft limit (256), failing near
  the start. At startup the server now raises `RLIMIT_NOFILE` toward the hard
  limit using a **concrete** `rlim_max`: macOS rejects `setrlimit(RLIMIT_NOFILE)`
  when `rlim_max` is `RLIM_INFINITY` (its launchd default), so passing that back
  was a silent no-op and the limit stayed at 256 — the raise now sets a concrete
  ceiling and steps down for the `kern.maxfilesperproc` cap (best-effort).
  Reproduced and fixed against real django, redis and WooCommerce clones.
- **jemalloc `background_thread currently supports pthread only`** printed on
  every macOS launch — the `background_thread` option is now Linux-only, where
  it is supported; the decay policy is unchanged.
- **Empty console dashboards on a fresh or brain-only engine.** The AI, RAG,
  vector, agent-memory and logs dashboards fell back to mock data when their
  backing index was empty; each dashboard's nav entry is now gated on its data
  existing, so a brain-only launch opens on Second Brain and an empty engine
  shows only System.

## [1.0.0-rc.7] - 2026-07-30

Seventh release candidate: the **second brain and security-hardening
release**. Headline for users: point one binary at a folder with
`xerj brain <folder>` and get a bi-temporal, evidence-carrying link graph
over your own documents, with a console view — THE MAP — that stays
readable at any corpus size. Alongside it, six network-reachable security
findings from a whitebox self-audit are fixed.

### Added

- **`xerj brain <folder>`** — one command turns a folder into a queryable
  knowledge graph. Structural detectors (wiki-links, markdown links,
  hrefs, folder adjacency, reading order) record, for every link, the
  exact sentence that created it and the byte offset it came from.
  Measured: 10 notes → 25 links in 0.1s; a 943 MB repo copy → 7,571 files,
  2,128,360 notes, 20,930 links in 322.7s.
- **Graph HTTP API** — `POST`/`DELETE /_graph/{brain}/link`,
  `GET /_graph/{brain}/ego`, `GET /_graph/{brain}/overview`. Bi-temporal:
  links are retired, never deleted, and any past moment is replayable via
  `as_of`. Traversal is capped at 2 hops per call by design — XERJ is a
  search engine with a graph-shaped index, not a graph database.
- **THE MAP** (console) — a Canvas knowledge-graph view whose default
  "helicopter" altitude is bounded by construction: any corpus collapses
  to at most a dozen named groups plus one catch-all. Measured 19,299
  links / 1,550 documents → 13 groups in 44 ms; a synthetic 50,000-link
  budget → 13 groups / 60 bundles in 122 ms, byte-identical across runs;
  60 fps pan/zoom/expand. Small corpora skip clustering and draw real
  notes, so the view is adaptive rather than one-size.
- **Cross-file-type links** — new `pathcite@1` and `cratecite@1`
  detectors relate documents to the code and files they cite, so a PDF or
  a design note can be shown next to the crate it references, with the
  citing text as evidence.
- **Agent surface** — four MCP tools (`xerj_brain_ego`, `xerj_brain_link`,
  `xerj_brain_unlink`, `xerj_brain_overview`) under
  `contract: xerj-second-brain/1`.
- `limits.snapshot_repo_allowlist` — an Elasticsearch `path.repo`
  equivalent bounding where snapshot repositories may live.

### Fixed

- **Security (unauthenticated):** `POST /_sql` with a deeply nested
  `WHERE` clause overflowed the stack and aborted the whole process. The
  WHERE parser now bounds recursion depth (`MAX_SQL_DEPTH`), as the
  query_string parser already did.
- **Security:** search-template parameters were spliced into the query
  JSON unescaped and re-parsed, so a parameter could inject query
  structure and drop the template's own filter. Parameters are now
  JSON-escaped and can no longer emit structural tokens.
- **Security:** a snapshot repository's `settings.location` was an
  unvalidated absolute path, so snapshots could read and write index data
  outside `data_dir`. Locations are now bounded by `data_dir` or the new
  allowlist.
- **Security:** the console's session `last_seen` refresh blind-wrote a
  stale copy of the session, which could resurrect a session revoked
  moments earlier. The refresh now re-reads and skips revoked sessions.
- Console: the Second Brain dashboard is no longer always present — it
  appears once at least one brain exists, so a user who has never run
  `xerj brain` is not shown an empty dashboard.
- Console: several second-brain counts reported the engine's 10,000-hit
  floor as an exact total on multi-million-note brains; totals are now
  exact, and floors are shown as `≥`.

### Notes

- The second brain's link detection is **structural and lexical** — there
  is no LLM and no semantic model in the loop, and the node-store embedder
  remains a lexical feature hash. Scale beyond the measured corpus above
  is not claimed.

## [1.0.0-rc.6] - 2026-07-28

Sixth release candidate: the **semantic-analytics and bounded-memory
release**. Headline for users: a `knn`/`semantic` query can now carry
`aggs` in the same `_search` request — aggregations run over the
retrieved top-k neighbour set, forced onto the exact path so bucket
counts are exact — and a semantic hit can opt into a `_passage` field
that returns the winning chunk's text with byte-exact provenance.
Underneath, the seven immutable-segment hydration caches now share one
process-wide, cgroup-aware memory budget, and the storage layer gains a
bounded, cancellable selective-hydration primitive (deliberately not
wired to any engine route yet). Two correctness holes are closed: kNN
could return a superseded vector when a document ID was re-inserted
while owning the HNSW entry point (fix arrived from the probelabs fork),
and an oversized Painless expression could exhaust the native stack
instead of returning a bounded error. One change arrived from outside
the org: transparent gzip request-body decompression, contributed by
Vinz2168, which unblocks Filebeat and the other ES clients that compress
by default. ES-YAML conformance holds at 1360 passed / 0 failed / 3
skipped. Each change states its own limits in place; the aggregation
gaps left open are listed under Known limitations.

### Added

- **kNN + aggregations in a single request.** `POST /{index}/_search`
  with a `knn`/`semantic` query plus `aggs` now returns aggregations;
  previously the `aggs` field on kNN responses was hardcoded `null`. No
  new API surface — the existing `aggs` request field is honored on the
  kNN path, across all three executors (HNSW, exact brute-force,
  multi-kNN). Semantics follow ES top-level-knn: aggregations run over
  the retrieved top-k neighbour set after the `num_candidates` fan-out,
  independent of `from`/`size` paging — `"size": 0` returns aggs only
  with empty hits, and `hits.total.value` reports the neighbour pool
  (`k`), not a match count, so bucket counts scale with `k`, never with
  index size. To keep those counts exact, an aggs-bearing kNN request
  always executes the exact brute-force path even when it would
  otherwise qualify for HNSW — ANN recall is below 100% and approximate
  bucket counts would be silently wrong — trading ANN speed for
  exactness (no performance claims are made for this path). This
  removes the previous two-step workaround (kNN → collect ids →
  separate aggregation request). Limitation, stated up front:
  `significant_terms` over a kNN slice returns empty — the vector path
  does not yet supply a background corpus; use `terms` for raw in-slice
  counts, or the two-step pattern when statistical significance is
  needed (filed as a follow-up).
- **Winning-passage provenance (`_passage`).** Requesting
  `"fields": ["_passage"]` on a single `semantic` or kNN query adds
  `fields._passage[0]` to each hit: the winning chunk's source `field`,
  zero-based `ordinal`, UTF-8 byte `start_offset`/`end_offset`, the
  `text` slice, and `page` (only when the source carries a numeric
  `page`; omitted otherwise). The text is reconstructed by slicing the
  authoritative `_source` at compact ingest-time offset metadata rather
  than storing the chunk a second time — the committed measurement over
  256 varied Unicode documents reports 80.26 B/doc of raw metadata,
  3.98 B/doc after ZBS2 compression. All three ES `fields` wire shapes
  (scalar, string array, object form) are accepted and normalized
  identically, and scroll continuations keep the passage. Multi-kNN and
  hybrid-fusion queries requesting `_passage` are a deliberate HTTP 400
  with an actionable message — summed or fused contributions do not
  define one winning passage, and rejecting is more honest than
  guessing. Scope: provenance covers the exact per-chunk max-sim
  scoring path; the HNSW-served and SQ8-quantized paths reconstruct a
  passage only for single-chunk values, and a kNN clause on an
  arbitrary `dense_vector` field has no passage metadata, so `_passage`
  may be absent rather than erroring. The internal metadata (reserved
  prefix `__xerj_passage_meta__`) is engine-owned: supplying it at
  ingest is rejected per-document, and it is stripped from `_source`,
  Painless, field discovery, full-text indexing, and aggregations. The
  native `_search` endpoint accepts `_passage` (returned as a hit-level
  key) and rejects any other `fields` entry. The default embedder
  remains lexical feature hashing — neural semantics still require
  opting into neural embedding mode or an embedding endpoint.
- **Process-wide segment hydration cache budget.** The seven
  immutable-segment hydration caches (stored slices, doc values, parsed
  stored values, sort shadows, id positions, row sequences, decoded
  stored bytes) — previously independent or unbounded — now share one
  cgroup-aware retained-payload budget with CAS admission and
  last-reader refunds. Configure with
  `[limits] max_segment_hydration_cache_mb` (default `0` = automatic:
  20% of the effective cgroup/system memory limit, no floor and no
  fixed cap; an explicit value is clamped to 50% with a startup
  warning) or the `XERJ_SEGMENT_HYDRATION_CACHE_MB` env override
  (`auto`, `off`, or a MiB value; `off` refuses every admission — a
  diagnostic switch). Refusal is invisible to queries: the request
  keeps the value it already built and returns the same result,
  uncached; publish-time warm-only values are simply dropped, and a
  counter increments. `GET /_nodes/stats` gains
  `indices.segment_hydration_cache` with limit/current/peak, refusal
  and accounting-error counters, the budget source, and per-category
  breakdowns. Automatic sizing now honors the *tightest* cgroup-v2
  `memory.max` from leaf to root (previously the nearest finite value
  won, overstating the ceiling when a parent limit was smaller). A
  release-build accounting underflow that left counters unchanged is
  also fixed — both counters now clamp atomically and record exactly
  one accounting error. Honesty note, repeated in the docs: this is a
  retained-payload/key ceiling computed from deliberately conservative
  estimates, **not** an RSS bound — query-owned materialization, decode
  scratch, mmaps, memtables, vectors, and allocator behavior stay
  outside it, and a single highly compressed segment can still create a
  transient decode peak before admission (bounded/streaming decode is a
  named follow-up). The motivating profiles (~206 MiB of publish-warming
  RSS on one 4,096-document diagnostic run; 226.35 MiB of cumulative
  live allocations under a merge warm) are attributed measurements, not
  a claimed end-to-end memory reduction.
- **Bounded selective stored-field hydration (storage primitive).** The
  `xerj-storage` crate can now hydrate selected rows and selected
  top-level `_source` fields from a ZBS2 V2 stored section without
  materializing the whole canonical JSON array, with cancellable entry
  points (deterministic checkpoints every 128 rows), a retained-size
  preflight computed from framing metadata alone (fails closed on
  historical zstd frames that omit a content size), and
  malformed-input hardening — checked arithmetic, fallible reserves,
  and framing/varint/bit-width validation keep hostile headers on the
  error path instead of aborting in the allocator. The on-disk stored
  format is unchanged and historical segments remain fully readable.
  This is deliberately **not** wired to any engine route or cache in
  this release — no endpoint, no query change, and no end-to-end
  speedup is claimed. The committed report measures the trade-off on
  its deterministic 20,000-row fixture: one-row hydration at roughly
  59× lower peak RSS (~335 MB full decode vs ~5.6–5.7 MB selective) but
  roughly 4× slower wall time, because RAW/zstd JSON columns still
  stream through all rows — a memory primitive with a CPU cost,
  measured as a single-host storage microbenchmark, and LZ4 columns
  still decompress whole before selective materialization.
- **gzip request-body decompression (external contribution).** Both
  HTTP routers now transparently decompress `Content-Encoding: gzip`
  request bodies before parsing; previously the raw gzip bytes reached
  handlers and failed JSON parsing (the commit reports
  `PUT /_ilm/policy` returning 400 and `POST /_bulk` returning 500).
  This unblocks the ES clients that compress by default — the
  reproduced failure was Filebeat 8.13.4 with default config 400'ing on
  its first ILM setup call, marking the output connection permanently
  failed, and retrying forever with zero documents ingested; after the
  fix the same default run completes and every test event lands and is
  queryable. Semantics note: the decompression layer sits outside the
  body-size limit, so the configured request-body limit now bounds the
  *decompressed* size, not the on-wire size. Only gzip is handled
  (deflate/br/zstd remain unsupported), response compression is
  unchanged, and verification is the PR's own curl + Filebeat runs — no
  automated test was added in the diff. Contributed by Vincenzo
  Lombardo (`Vinz2168`).

### Fixed

- **kNN under document-ID reuse (HNSW entry point).** Re-inserting a
  vector under the external ID that owned the HNSW graph entry point
  could return the old, superseded vector as a hit for that ID, and the
  v2 graph writer could persist a CRC-valid file whose entry header
  paired the current slot's external ID with the orphaned slot's higher
  layer. Both are fixed (arrived from the probelabs fork, PR #64,
  landed via #66): a slot counts as live only when the reverse ID map
  resolves its external ID back to that exact slot, the entry point is
  re-selected as the highest-layer live slot, search excludes
  superseded slots from results (they stay traversable for graph
  connectivity — excluded, not reclaimed), and the writer derives the
  entry header from live topology, repairing a stale in-memory header
  on save. This is a write/search-time fix, not a retroactive file
  repair: graph files written before the fix still load as-is and are
  corrected the next time the fixed writer saves them. The on-disk
  graph format and byte layout are unchanged, and no authoritative
  documents or latest vectors are discarded.
- **Painless: oversized expressions return a bounded 400 instead of
  aborting the process.** Flat shapes the parser can build without
  recursing (`1+1+…` binary chains, `.member` chains, `[index]` chains)
  bypassed the parser's recursion guards, and the evaluator's depth
  guard counted frames only after entering evaluation — so a 5,001-node
  chain could exhaust the native stack before the intended error fired
  (reproduced as a process abort on the default test-thread stack).
  Expression depth is now computed exactly at parse time — 500 accepted
  and evaluated, 501 rejected, covered for all three shapes — and flat
  binary and member/index chains evaluate iteratively. Scripts found
  under `script`/`*_script` keys in `_search` bodies that exceed the
  limit get an up-front HTTP 400: "script evaluation exceeded maximum
  depth; split the expression into smaller statements". The supported
  depth contract itself is unchanged (still 500) — this enforces the
  existing contract before evaluator recursion instead of during it —
  and semantics of accepted expressions (precedence, associativity,
  short-circuiting and error order, `doc`/`params`/`Math` dispatch)
  are test-covered as unchanged. Statement nesting no longer consumes
  the expression-depth budget; it stays bounded by the parser's
  separate limit.

### Changed — internal (no behavior change claimed)

- **HNSW read path behind a storage-view seam.** Search, construction
  traversal, diverse-neighbor selection, diagnostics, and graph
  serialization in the vector index now run through a private read-only
  storage trait with static dispatch, so a future immutable graph
  backend can implement the same read contract without rewriting the
  search path or adding runtime branching. Public API, mutation model,
  distance behavior (cosine/L2/dot), filtered-search and tombstone
  semantics, and graph format v2 are all unchanged — a checked-in
  pre-seam format fixture loads, rewrites, and reopens correctly, and
  parity tests compare exact result ordering and exact serialized
  bytes. The commit's controlled A/B (one 10K × 64-d graph, 30K
  deterministic queries, nine alternating CPU-pinned runs) reports
  −0.31% median qps with identical checksums, stated to be within run
  noise: explicitly not a speedup, and this refactor does not by itself
  reduce ingest memory.

### Docs

- **calltree.ai case study**
  (`docs/case-studies/calltree-analytics/`): three runnable scripts +
  README showing one `POST /conversations/_search` combining
  `query.knn` with `terms`/`percentiles`/`avg` aggregations for
  deep-research questions, over a 130-conversation generated corpus,
  with a primitives table mapping sub-questions to query shapes.
- **RAG hybrid retrieval example**
  (`docs/examples/rag-hybrid-retrieval/`): pure-cosine vs BM25 vs
  `query.hybrid` (RRF) over an 18-chunk synthetic corpus with real
  768-dim EmbeddingGemma vectors, documenting the pgvector-to-XERJ
  drop-in migration (same chunks and embeddings, swap the cosine
  `SELECT` for the hybrid `_search`). The honest result is included:
  on the identifier query, equal-weight RRF ranks the right chunk #3,
  not #1, and the README recommends weight tuning or a query router.
- **daily.dev Postgres CDC case study**
  (`docs/case-studies/daily-dev-postgres-cdc/`): logical-replication
  sync from a pgvector-equipped `post` table into XERJ via `_bulk`,
  with both a polled `test_decoding` consumer and a push-streaming
  consumer with LSN checkpointing (live-verified resuming from
  `confirmed_flush_lsn` after a stop/change/restart). A correction is
  recorded in place: the first draft's "no fused RRF node" limitation
  was the author's own request-syntax error, not an engine gap —
  `query.hybrid` with `fusion: {type: rrf}` is the real syntax.
- **Semantic-analytics use case** on xerj.org
  (`landing/use-cases/semantic-analytics.html`, wired into the
  use-case subnav, hub, solutions index, sitemap, and
  `llms.txt`/`llms-full.txt`): documents the rc.6 kNN+aggregations
  behavior, including every gap listed under Known limitations below.
  All corpora in this Docs group are small and synthetic, and nothing
  in them was timed at any corpus size; daily.dev and calltree.ai are
  the parties whose questions motivated the case studies, not
  customers.
- **Production-deployment recipe** gains "Bound retained segment
  hydration" (the budget knob, env override, clamping, refusal
  semantics, and the stats fields); the passage-retrieval recipe gains
  the `fields: ["_passage"]` example and response shape.

### Known limitations

Known-open items in this release's own features, stated here rather
than left implied:

- **`significant_terms` over a kNN slice returns empty buckets** — the
  vector path does not yet supply a background corpus. Use `terms` for
  raw in-slice counts, or the two-step kNN → ids → aggregation pattern
  when statistical significance is needed. Filed as a follow-up.
- **Aggregations on a kNN query are bounded by `k`:** buckets count the
  retrieved neighbour set only, never the full matching corpus, and
  `hits.total.value` echoes the neighbour-pool size, not a match count
  — the documented top-level-knn semantics.
- **An aggs-bearing kNN request always runs the exact brute-force
  scan:** cost scales with corpus size × dims, `num_candidates` has no
  effect there, and HNSW never serves it. A hybrid (RRF) query carrying
  `aggs` is rejected with a 400, and a kNN clause inside `bool.filter`
  is not peeled and returns no buckets — slice aggregation works for
  pure kNN only.
- **The hydration budget is not an RSS bound** — it caps conservatively
  estimated retained payload and keys only, and a single highly
  compressed segment can still create a transient decode peak before
  admission; bounded/streaming decode is a separate follow-up.
- **The selective-hydration primitive is not wired** into any engine
  read path or the shared cache budget yet, and LZ4 columns still
  require whole-column decompression before selective materialization.
- **Request decompression handles gzip only** (deflate/br/zstd request
  bodies remain unsupported); response compression is unchanged.
- **HNSW graph files written before the entry-header fix** still load
  with the inconsistent header retained; a file is only corrected the
  next time the fixed writer saves it.

## [1.0.0-rc.5] - 2026-07-27

Fifth release candidate: the **real-client compatibility release**. Much of
it came from pointing real Elasticsearch and OpenSearch tooling at
XERJ — Kibana 8.13, Kibana OSS 7.10.2, OpenSearch Dashboards 2.11.1/3.6,
and their own shipped sample datasets (flights, eCommerce, logs) — and
fixing the wrong answers, 500s, and stalls those clients produced. The rest
is storage, ingest and `autoindex` work that did not come from client
testing. Headline for users: the query classes that silently matched
**zero** documents (booleans, `.keyword` multi-fields, `match_phrase` on
arrays — and keyword arrays before a flush, see the qualification below)
now return what ES returns; a real Kibana/OSD instance boots,
logs in, and saves objects against XERJ end to end; and a dashboard firing
several panel queries at once no longer stalls the node. ES-YAML
conformance holds at 1360 passed / 0 failed / 3 skipped. Zero-hit defects
found during this cycle that are **not** fixed here are listed under Known
limitations.

### Fixed — query semantics (classes that silently matched zero docs)

- **Keyword arrays (partial — memtable half only):** a `term`/`terms` query
  on a multi-valued keyword field only ever compared element `[0]` — the
  memtable stored just the first value and the segment keyword column is
  one ordinal per doc — so exact lookups returned silent false negatives
  while `terms` aggregations and `match` (which join all elements) looked
  correct. The memtable path is fixed: those doc-values lookups now bail
  for array fields and fall through to the array-aware source scan, so a
  `term` on a multi-valued keyword field is correct **before** a flush.
  Flushed segments are **not** fixed — the segment keyword column is still
  one ordinal per document, so after a flush a `term`/`terms` on a
  non-first array element can still silently miss. The complete fix needs
  multi-valued segment keyword columns (a storage-format change); the
  regression test
  (`xerj-engine` `test_term_matches_non_first_array_element`) is committed
  but `#[ignore]`d until that lands.
- **Booleans:** `term`/`match_phrase` on a boolean field undercounted to 0
  while a `terms` aggregation on the *same* field bucketed `true`/`false`
  correctly. The trigger was the count shortcut over memtable-resident
  data — i.e. any bulk import before an explicit `_flush`, which is what
  every real importer (OSD's own sample-data loader included) produces.
- **`.keyword` multi-fields:** the brute-force scan's field resolver had no
  multi-field fallback, so `term`/`match_phrase` on `category.keyword`,
  `manufacturer.keyword` and friends matched nothing — `_source` never
  contains a literal `"category.keyword"` key. It now strips the trailing
  segment and retries against the parent when the parent is a leaf value
  (guarded, so a genuinely absent nested/object field is still absent).
- **`match_phrase` / `match_phrase_prefix` on arrays:** both arms only
  handled scalar values, so an array-valued field never matched any
  phrase. ES semantics restored: the doc matches if any element does.
- **`match_phrase` with a non-string query value:** `{"query": true}` was a
  hard parse error surfaced as `search_phase_execution_exception` — the
  exact shape OSD's filter bar sends for a boolean filter pill, which
  broke every panel on the dashboard at once. Scalars are now coerced the
  way `match` already coerced them.
- **Empty query strings:** `match`/`match_phrase`/`match_phrase_prefix`
  with `""` returned 400; ES treats an empty analyzed query as zero terms.
  They now resolve to `match_none` (200, no hits) — this is what Kibana's
  saved-objects `_find?search=*` builds, so that endpoint works again.
- **`geo_point`:** aggregations and `geo_distance`/`geo_bounding_box`
  rejected string-encoded `{"lat": "50.03", "lon": "8.57"}` coordinates
  (the shape the flights sample data ships), and `geo_centroid`/
  `geo_bounds` used a flat lookup that never found a nested geo field.
  Both now behave like ES.
- **Aggregation scripts:** `terms` required a top-level `field` and
  returned `{"buckets": []}` whenever it was absent — any script-based
  terms aggregation silently produced no buckets. `script` is now a real
  key source alongside `field`.
- **Painless date accessors:** `doc['t'].value` returned a plain string, so
  `.getHour()`, `.getDayOfWeek()`, `.getYear()` and the rest of the common
  accessor set failed the whole script. They now parse and extract the
  requested UTC component.
- **Dynamic date mapping:** ISO-8601 strings are inferred as `date` instead
  of `text`; `date_detection: false` was accepted-but-ignored and is now
  honored; a non-date value written into an inferred date field returns
  `mapper_parsing_exception` instead of being silently stored and then
  being invisible to range queries, sorts, and time filters; and
  `date_detection` survives the `PUT /_mapping` merge path.
- **Segment-path parity:** projected FTS bool queries dropped
  `minimum_should_match` (so `match_bool_prefix` with `mm=3` matched every
  single-term hit after flush) and doc-values wildcards did not case-fold;
  nested kNN returned zero hits for any parent doc living in a segment
  because the reassembled `{_id, _seq_no, _source}` shape was not
  unwrapped. `_count` now resolves `terms` lookups.

### Fixed — API surface (what real clients call)

- **Aliases:** single-index endpoints (`GET /{alias}`, `/_mapping`,
  `/_settings`, …) never resolved an alias to its backing index, and
  aliases were never persisted at all — index data survived a restart but
  the alias pointing at it did not. Aliases now resolve everywhere ES
  accepts them and are written to `aliases.json` (atomic temp+rename).
  Together this unsticks a fresh OSD container looping on "Another
  OpenSearch Dashboards instance appears to be migrating the index".
- **`_field_caps`:** never listed declared multi-fields (so `fields=*`
  omitted every `.keyword` entry and index-pattern refresh reported
  "field not found"), and only special-cased the literal `*`/`_all`
  wildcards — any real glob such as `wiki-test*` returned empty caps.
  Both fixed; wildcard/comma index specs also now resolve on 9 further
  endpoints that previously 404'd, 400'd, or — for `_refresh` and
  `_cat/count` — silently reported success / zero documents while doing
  nothing (`_refresh` answered `{"_shards":{"successful":1,…}}` without
  refreshing; `_cat/count` returned 0, indistinguishable from a genuinely
  empty index).
- **`POST /{index}/_update`** never returned a `get` block, so every Kibana
  saved-object write crashed client-side reading `body.get._source`. It is
  now returned exactly when the caller passes `_source`/`_source_includes`/
  `_source_excludes`, matching ES (absent otherwise, so `_doc` responses
  are byte-identical to before).
- **`_source` vs `stored_fields`:** the implicit `_source` suppression that
  `stored_fields` triggers is now only a default — an explicit top-level
  `_source` in the same request wins, as in ES. This is what starved
  almost every column in OSD's Discover.
- **`_bulk`** reports each item's real `_seq_no` and `_version` instead of
  a wall-clock microsecond timestamp and a hardcoded `1`.
- **`_update_by_query` / `_delete_by_query`** honor
  `wait_for_completion=false` and return the async `{"task": …}` form;
  `GET /_tasks/{id}` reports completion with the final response.
- **`/_xpack`** respects `--compat-version` (it had its own hardcoded
  `8.13.0`, which Kibana OSS 7.10.2 surfaced as an opaque "license not
  available" refusal to start), and `/_xpack` + `/_xpack/usage` report the
  real auth state instead of `security.enabled: true` — so `--insecure`
  no longer makes Kibana render a login screen for auth that is off.
- **Missing HTTP verbs:** `_refresh`, `_analyze`, `_msearch` accept GET and
  `_clone`/`_shrink`/`_split` accept PUT, as ES does — the PUT `_clone`
  405 was a fatal error during Kibana's saved-object migration.
- **Login-path endpoints:** HTTP Basic auth (Kibana's interactive realm),
  `POST /_security/profile/_activate`, `GET /_security/profile/{uid}`,
  `GET`/`POST /_security/user/_has_privileges`, and a real
  `GET`/`PUT`/`DELETE /_security/privilege` store with ES-shaped 404s —
  each of these was a 500 or a hang on the Kibana login and home pages.
- **`GET /_cat/templates/{pattern}`** (a 404 that crashed OSD) and
  **`POST /_index_template/_simulate`** (body-only template preview) added.

### Changed — index resolution (can break existing callers)

These three align index addressing with ES 8 and turn some
previously-succeeding requests into errors. Review any caller that relies
on the old behavior.

- **A comma-separated index spec now 404s when a concrete name is
  missing**, implementing ES's default `ignore_unavailable=false`. The
  missing name used to be dropped silently — `POST /real,typo/_refresh`
  answered `200` with `_shards.total: 1`. Wildcard/`_all` specs that match
  nothing are still a valid empty result (ES's `allow_no_indices` default).
- **Wildcard and `_all` expansion no longer sweeps in hidden
  (dot-prefixed) indices**, so an operation like `POST /*/_close` can no
  longer hit `.xerj_users`-class system indices. A dot-prefixed pattern
  opts back in, as ES's hidden addressing does.
- **`POST /_close` refuses a wildcard or `_all` target outright** with ES
  8's `action.destructive_requires_name` `illegal_argument_exception`.

### Added

- **OpenSearch client auto-sensing.** XERJ answers the identity endpoints
  (`GET /`, `GET /_nodes`) per request based on the caller's User-Agent, so
  one running instance serves an `opensearch-py`/`opensearch-js`/OSD client
  and an Elasticsearch client simultaneously, each seeing a
  version/distribution block its own compatibility gate accepts. Explicit
  `--compat-distribution` / `--compat-version` (and the matching env vars)
  still pin the identity for every client. The `x-elastic-product` header
  is no longer sent to a detected OpenSearch caller.
- **Opt-in ingest memory attribution (developer feature).** Setting
  `XERJ_INGEST_MEMORY_TRACE=summary` emits a bounded `xerj.ingest_memory.v1`
  NDJSON ledger — per-owner logical bytes across HTTP bodies, raw and
  parsed semantic sources, prepared docs and vectors, active memtables and
  drained flush snapshots — alongside jemalloc allocated/active/resident,
  RSS, CPU time, and accounting/dropped-event counters, plus a separate
  `/proc`-derived `xerj.process_sample.v1` stream. Default is fully off.
  Merge and read-cache owners are reported `unavailable`, not zero. This
  makes ingest retention *measurable*; it is not itself a memory bound.
  A deterministic bounded diagnostic suite
  (`demo/usecases/autoindex/scale/bounded/`) drives ingest → refresh →
  flush → force-merge → restart with exact count and sentinel checks.
- **Private pprof debugging toolkit.** A Linux-only `debug-profiling`
  feature compiles in bounded CPU and jemalloc heap profiling with no
  network endpoint; artifacts are written mode-0600 to an operator-created
  directory, with `capture.py`/`inspect.py` wrappers that hash the binary
  and every artifact. Not in any shipped feature set; no runtime overhead
  or speedup is claimed.
- **Experimental ONNX embedding backend**, wired through `autoindex` behind
  the `onnx-experimental` feature. Its lazy session initialization is
  cancellation-safe: model loading moved to one process-owned thread whose
  single terminal result is shared, so a cancelled first request can no
  longer strand every later one.
- The ES-YAML conformance runner now exits non-zero when any case fails
  (it previously reported failures but exited 0).

### Changed — performance (concurrent dashboard bursts)

- **Full-corpus aggregations no longer deep-clone the memtable under the
  shard lock.** The `need_full_corpus` path (any request with `aggs` that
  the columnar fast path can't serve) cloned every buffered document's JSON
  tree while holding the per-shard read guard, serialising concurrent
  panels behind O(docs) work each. It now Arc-shares out under the lock and
  clones after releasing it. Measured: 15 concurrent `date_histogram`
  queries against a real ~14k-doc memtable-resident index, 1.2–2.7 s →
  ~150–170 ms (commit `6a54cf5`).
- **Full-corpus aggregations no longer re-decode segments per query.** The
  same path did an unconditional open + decompress + full `serde_json`
  parse of a segment's entire stored section on every request once data had
  flushed. Segments are immutable, so it now uses the existing
  single-flight stored-value cache. Measured: 20 concurrent requests
  against a real flushed eCommerce segment, 15–26 s → 1–4 ms including the
  cold first hit (commit `f6daf70`). A segment read failure during
  full-corpus assembly is now a hard error rather than a silent skip that
  undercounted every bucket.
- **The scan path no longer stalls on cold, concurrent bursts.** Two
  compounding costs: every scanned doc's `_source` was deep-cloned to
  splice `_id` in even though only a deeply-nested `ids` clause needs it
  (now computed once per scan from the query shape), and the raw-fallback
  decode had no single-flight protection, so N concurrent requests against
  a cold segment each paid the full open+decompress. Measured against real
  Kibana OSS 7.10.2 and eCommerce sample data: a 24-concurrent
  `query_string`+`match_phrase` burst immediately after restart (worst-case
  cold caches) went from ~4.5 s per request to 100–165 ms, with the warm
  repeat at 1–5 ms (commit `7e963ff`).
- **Semantic and vector work is bounded by the request.** One absolute
  deadline computed at search entry is carried through single-flight
  waiting, admission, embedding, hybrid/multi-kNN recursion, and exact
  scanning; partials set `timed_out` and `hits.total.relation: gte`, and
  the ES handler aborts its child task when the request is dropped.
  Previously a cancelled semantic search could drain for minutes past its
  timeout. Cold vector segment loads also moved to bounded `spawn_blocking`
  producers so they can no longer pin every async worker.

### Fixed — storage, durability & ingest

- **Raw ingest validates before it publishes.** Caller bytes were appended
  to the WAL before being proven to be complete JSON, so malformed input
  could reserve sequence numbers and become durable, with the later parse
  path silently substituting `{}`. Whole-batch UTF-8/JSON/nesting
  validation now completes before any sequence, WAL, version-map, memtable,
  or schema mutation, and malformed bulk sources return per-item
  `document_parsing_exception`. The parsed `Value` is now the single
  authority for turbo ingest (live indexing and WAL replay could previously
  disagree), and `copy_to` is applied before WAL publication so GET,
  search, flush, replay, and restart all observe the same source.
- **Per-shard WAL buffers bounded to 64 KiB** (was 8 MiB). Every index
  eagerly opens one writer per ingest shard, so a 16×16 default reserved
  ~2 GiB of allocator capacity before a single document was written;
  capacity above a frame cannot batch across requests because every
  acknowledged append already drains the buffer. Measured on 256 empty
  writers: jemalloc allocated 2,147,503,496 → 16,797,064 bytes (−99.2%),
  with ingest throughput ratios of 0.976 (single shard) and 1.020
  (eight shards) and exact replay preserved (commit `761b915`). Cost:
  records larger than the 64 KiB buffer issue more write syscalls
  (measured 1,372 → 3,372 on 131 KiB × 1000), with throughput unchanged.
- **Same-ID writes are serialized.** A keyed publication coordinator now
  spans the current-state/CAS check through WAL, version map, FTS, and HNSW
  publication for single-document paths, closing races that could admit two
  creates, lose an update patch, or let a delete overtake a write. Scripted
  `_update` and `_update_by_query` run through the same boundary, so two
  concurrent `ctx._source.n += 1` requests can no longer both read `n=0`.
  Distinct IDs do not contend. (Turbo batch paths remain follow-up work.)
- **Semantic vectors stay out of full-text indexes.** The dynamic-field
  walkers treated pooled embeddings and their `_chunks` companions as
  ordinary text and fed every float into FTS as a decimal token. In one
  failed diagnostic run that produced 2.338 GiB of vector FSTs inside a
  3.449 GiB partial index. Embedding outputs are now excluded by schema
  designation (not a name heuristic) across pre-analysis, flush, and merge.
  Existing vector FST sidecars are not rewritten on upgrade — reindexing is
  the safe way to reclaim them.
- **The admin API key persists across restarts.** The server wrote
  `data_dir/admin.key` but never read it back, minting a fresh key on every
  start and locking out every client configured against the previous one.
  An existing well-formed key is now reused; anything missing or malformed
  still falls through to fresh generation.

### Fixed — autoindex

- **Content deduplication with crash-safe replacement.** Byte-identical
  paths were indexed independently (one reproduction journaled 2,442
  records for 1,221 live documents). Sources are now hashed with streaming
  XXH3-128 and digest peers byte-verified before one canonical path is
  chosen; every current name is preserved in `ax_paths`. Publication became
  an explicit durable generation transaction (synced `file_replace_start`
  → staged, source-verified extraction → synced `file_done` commit), and
  journal replay repairs torn tails and rolls back failed appends.
  Ambiguous legacy prefix collisions fail closed before any backend
  mutation instead of guessing.
- **Follow-on fixes to that rework:** resume now guarantees exclusive
  plan-key ownership (two files resolving to one key each ran the
  replacement transaction and deleted the other's documents);
  `delete_by_query` repeats until a pass deletes nothing, because the
  server implements it as a single `size:10000` pass and >10k-doc
  generations left permanent ghost documents; and both recoverable error
  paths now name the offending files or journal offset instead of
  advising a blanket `--fresh` re-extract.
- **PDFs are parsed by `pdf_oxide` in isolated same-binary workers.** The
  previous byte scanner interpreted font character codes without resolving
  page resources or ToUnicode maps, producing NUL-separated and shifted
  text that schema inference (correctly) refused to elect as
  `semantic_text`. Workers get process groups, a 1536 MiB `RLIMIT_AS`, and
  descendant kill/reap on timeout — documented as resource isolation, not
  a security sandbox, with no OS memory cap on non-Unix. New
  `--pdf-workers` and `--pdf-timeout-secs` flags. Cost: the no-default
  release binary grows 34,160,328 → 39,018,184 bytes (+4.6 MiB).
- **Text files are classified by sentence density, not line length.** The
  old `avg_len > 60` rule split markdown across two datasets with two
  different field names — adding `## headings` to a document made it *more*
  likely to be treated as a record stream, which split BM25 statistics
  across two corpora. Terminal-punctuation density separates prose
  (0.43–0.57 on the measured corpus) from logs and source (0.00–0.20);
  CSV rows fall on the record-stream side of the same threshold.

### Changed — autoindex (what it produces)

- **Line-oriented text is chunked into overlapping windows** (40 lines, 10
  overlap) instead of one document per line, and prose sections dropped
  from 32 KB to 2 KB with overlap, because BM25 scores per document and a
  single line rarely contains the caller's own wording. Measured on this
  repository (234 files, 170k LOC + docs + 460 commit messages) with 8
  "where/why is X" questions: 3/8 → 7/8 answered, 162,883 → 5,508 records,
  indexing 234 s → 1.9 s (commit `4510189`). Every chunk carries
  `start_line`/`end_line`.
- **`semantic_text` election is by language, not length.** The rule was
  "largest `text` field with `avg_len >= 200`", which elected 300-character
  base64 and concatenated-id columns while skipping a genuinely semantic
  150-character summary. A field must now actually look like natural
  language — `word_ratio >= 0.55` (tokens matching `[A-Za-z]{3,}`) **and**
  `mean_tokens >= 3`. Measured `word_ratio` is 0.00 for
  trace_id/user_id/order_id/numeric columns and 0.78–1.00 for prose, log
  messages and source. This changes which field gets embedded, so it
  affects both semantic-search quality and ingest cost (the built-in neural
  backend measures ~2.8 docs/s). The election note records the numbers that
  drove the decision.

### Added — autoindex

- **`--bulk-timeout-secs`** (default 300, range 1–3600) applies only to
  `POST /_bulk`, so a legitimately slow neural bulk can be accommodated
  without loosening the deadline on control requests. Retries are bounded
  at six attempts with the identical body and deterministic document IDs.

### Docs

- **WordPress core security audit case study**
  (`docs/case-studies/wordpress-security-audit/`): a reproducible record of
  an AI agent auditing real WordPress core (1,492 PHP files, ~619k lines)
  with XERJ as the retrieval substrate — sink census, interprocedural taint
  analysis, authorization graph, POP-gadget hunt — plus a copyable Claude
  Code skill and a step-by-step playbook. The honest result is that **core
  came back hardened**: the documented findings are three Medium-severity,
  known-class items (an incomplete SSRF deny-list missing `169.254.0.0/16`,
  an ImageMagick parse surface, and an unguarded `role` sink in
  `user-new.php`), a fourth downgraded to not-reachable-in-core, and
  verified negatives for IDOR, SQL de-escaping, and sanitizer composition.
  None of it is claimed as a novel 0-day. The published grep-wins
  counter-examples are shown alongside the wins, and the XERJ bug the audit
  surfaced (the keyword-array `term` defect partially fixed above — the
  memtable half) is disclosed.
  Site use case 06 (`/use-cases/code-security-audit.html`) is built from it.
- **Token-usage guidebook** (`docs/TOKEN_USAGE.md`): a measured decomposition
  of what a XERJ answer costs an agent in tokens — envelope overhead,
  answer, and materialized intermediate data — instead of hand-waving.
- **AST + graph + FTS vulnerability research**
  (`docs/research/ast-graph-vuln-detection.md`, `docs/examples/ast-vuln-graph/`),
  including a multi-language tree-sitter taint scanner tested at real
  WordPress scale.
- Verified embedding examples: XERJ with Google AI (EmbeddingGemma, Gemini
  API, ADK) and with any OpenAI-compatible `/v1/embeddings` endpoint.

### Known limitations

Known-open items found while validating this release and **not** fixed in
it. The first three are zero-hit defects — a query returns no documents
that ES would return:

- **`multi_match` looks inverted.** On a 6,022-document index a long query
  returned 0 hits with `operator: "or"` (the ES default) and 2 hits with
  `operator: "and"`. OR must be a superset of AND.
- **`match` against a `semantic_text` field returns 0 hits** for a long
  natural-language string, while the same query against a plain `text`
  field returns thousands.
- **`term`/`terms` on a multi-valued keyword field can still miss after a
  flush** — only the memtable half of that fix landed (see the first bullet
  of "Fixed — query semantics").
- **`autoindex` dataset clustering merges same-shape files** with different
  subjects: 213 source files sharing the schema `{text}` collapse to one
  index, and embedding centroids separate the subject groups only weakly.
- 3 of 1,363 ES-YAML conformance cases are skipped.

## [1.0.0-rc.4] - 2026-07-22

Fourth release candidate: the **production-hardening release**. A 9-review
release-readiness audit produced 17 blockers and ~60 follow-on items; all
were fixed across four hardening waves and verified against the live binary
(ES-YAML conformance 1360/1363, full-matrix benchmark 52 WIN / 0 LOSE / 25
TIE vs live ES 8.13.4). Headline for users: acknowledged writes survive
crashes, wrong-but-200 responses are gone, the node defends itself under
resource pressure, and the bundled Console gains Kibana-quality editable
dashboards.

### Fixed — durability (acknowledged writes survive failure)

- **Acked-write loss closed:** verified WAL prune + power-loss-ordered
  publish chain; `wal_sync="sync"` honored on ALL bulk paths and the
  `wal_batch_ms` fsync loop actually implemented; torn-frame recovery so a
  disk-full/crash tear cannot poison a WAL generation; consecutive `_bulk`
  delete actions no longer dropped; acked deletes survive restart
  (WAL-shard pinning); delete tombstones end WAL pinning segment-durably.
- **Merge-window reads:** GET never 404s during the merge-publish window;
  merges can never silently drop docs; `_forcemerge` is synchronous and
  quiescent like ES.
- **Startup/data safety:** exclusive `node.lock` on the data dir (second
  process fails fast); data-dir format marker refuses newer-than-supported
  or corrupt dirs BEFORE any destructive GC; refuse-on-corrupt snapshot
  restore; HNSW persistence fsyncs file + dir around rename; periodic
  background flusher no longer aborted at spawn; sharded-WAL FTS replay
  restored on reopen.

### Fixed — correctness (no silent wrong answers)

- **Fail-loud sweep:** the silent-wrong-query classes on `_search` are
  rejected with real 400s (unknown fields, unsupported constructs), as are
  CCR auto-follow (501), remote reindex, `has_child`/`has_parent`, learned
  fusion, and SQL `HAVING` — previously all silently returned wrong data.
- **Doc CRUD wire semantics:** real per-doc `_version` and ES seq_no
  convention; `POST /{index}/_doc/{id}` route added; malformed bulk docs
  rejected per-item with ES-shaped 400s instead of stored as empty `{}`.
- **Aggregations:** real `sum_other_doc_count`; composite bucket keys typed
  from the source field mapping; `multi_terms` raises `too_many_buckets`
  as a real 400 past the cap; `top_hits` emits the doc's real `_seq_no`.
- **Query semantics:** ES-exact date resolution for range bounds (rounding,
  format, date math); Painless compares strings as strings (every string
  previously compared equal) with depth + source-length guards; highlight
  offsets correct on multibyte text; `combined_fields` OR pooling;
  `query_string` fallback discloses operator handling; kNN threads
  filter+boost through top-level kNN and honors similarity cutoffs.
- **Doc-values counting (P0):** a `range` filter on non-numeric values
  admitted every memtable document in `size:0`/`_count`/filter-agg paths
  (a one-day date window over-counted 3.4×); date/keyword range bounds now
  compile to the columnar fast path instead of falling to the brute scan.
- **Multi-valued fields:** a field that is multi-valued anywhere in a
  segment no longer ships a lying doc-values column that silently dropped
  those docs from count shortcuts — consumers fall back to the exact scan.

### Added — resource governance (the node defends itself)

- Parent circuit breaker keyed on ACTUAL RSS, global search pool, disk
  flood-stage watermark, per-query memory guard, ANN coverage guard, and a
  search timeout that actually preempts term-dictionary walks; scroll and
  async-search contexts are TTL-swept and capped. Classic node-killers
  (huge `size`, deep pagination windows, bucket explosions) return bounded
  400/429 instead of taking the process down.

### Added — security

- gRPC listener authenticated; health probes exempt from auth; constant-time
  compare for the admin API key; `admin.key` and TLS private keys created
  0600; CORS configurable and restrictive by default; API keys persist
  across restart with an honest role surface; `/_memory` list paginated
  with a documented auth model.

### Added — Console: Kibana-quality editable dashboards

- Durable backend CRUD for dashboards (create/replace/patch/delete with
  ETag optimistic concurrency) — user dashboards survive localStorage
  clears AND server restarts; a real panel builder with live preview
  (11 viz types, index/query/metric pickers); free-form `{x,y,w,h}` panel
  resize + move; first-launch seeding of 13 built-in dashboards as durable
  managed rows; edit-mode chrome no longer overlaps titles or the sub-nav.

### Added — observability

- ES `_stats`/`_cat` surfaces and the 101-series Prometheus endpoint
  reflect real load (docs, bytes, search/indexing counters); slow-query
  log; structured logging minors; `_cat/indices` uuid + bytes columns and
  ES-shaped snapshot responses.

### Changed — performance

- **kNN flipped:** HNSW-served top-level kNN — official benchmark cell
  23,325 ms → 1.87 ms at recall@10 1.00 (vs ES 0.80).
- **Date-filtered aggregations:** 41–49× (one-day window 9.9 s → 241 ms)
  via keyword/date columnar range predicates; filtered `extended_stats` /
  `percentiles` / `percentile_ranks` / `median_absolute_deviation` served
  columnar with filter-aware gathers (11–264×).
- **Scored-columnar family at the ES floor:** multi_match, query_string,
  fuzzy, prefix/wildcard, highlight, match_phrase, deep pagination,
  `more_like_this`, `function_score`, composite aggs, `rare_terms` /
  `significant_terms` / percentile families — full-matrix result
  52 WIN / 0 LOSE / 25 TIE against live ES 8.13.4.
- Mixed read-under-write hardening: one memtable walk per query, flush cap,
  merge-publish count seeding, open-loop iso-load writer for honest
  measurement.

### Fixed — autoindex & agent search path

- `xerj autoindex` no longer aborts the whole run on ordinary UTF-8 in the
  SQL-dump sniffer (byte-buffer accumulation; junk files are skipped and
  recorded, never fatal) and no longer mojibakes non-ASCII SQL values.
- `highlight` is applied before `_source` filtering, so fragment-only
  responses work (measured: 3.2× fewer tokens into an agent context at
  equal recall).

### Docs

- Honesty ledger: canonical audited scorecard, ROADMAP claims flipped to
  measured reality, phantom-claim purge across README/site/docs.
- Production recipes: TLS + auth hardening, air-gapped deploy, ES→XERJ
  migration.

## [1.0.0-rc.3] - 2026-07-10

Third release candidate. Headline: XERJ gains a **built-in neural embedder** —
real in-process BERT semantics with no Python and no external service — behind a
single backend-agnostic embedding handle, plus two new end-to-end-validated
retrieval recipes.

### Added

- **Built-in neural BERT embedder — shipped in the binary.** A pure-Rust sentence
  encoder via `candle` (default `all-MiniLM-L6-v2`, 384-dim) that runs in-process
  and **downloads its weights (~90 MB) automatically on first use** (or reads them
  from `embedding.local_model_dir` for air-gapped deployments). It is compiled into
  the default release binary — end users just add `--embed-mode neural` at runtime,
  no special build and no separate binary. A progress bar and one-time-download log
  make the first run legible. The binary is ~36 MB as a result; a
  `--no-default-features` slim build without the neural backend is ~23 MB.
- **Unified three-backend embedding handle (`xerj_ai::Embedder`).** `semantic_text`
  ingest and `semantic`/`hybrid` queries run through one of three interchangeable
  backends — **lexical** (default, zero-dep feature-hash), **neural** (built-in
  BERT), or **proxy** (external OpenAI-compatible `/v1/embeddings`) — selected with
  `embedding.mode`, the `--embed-mode` flag, or `XERJ_EMBED_MODE`. Misconfiguration
  degrades to lexical, never a crash; `auto` preserves the historical behaviour.
- **Recipe — All-you-can-eat search.** One corpus retrieved five ways from a single
  index: full-text (BM25), semantic, vector kNN (more-like-this), hybrid (RRF), and
  semantic-scoped-by-keyword-filter. Guide `docs/recipes/all-way-search.md`,
  runnable `recipes/all_way_search.py`.
- **Recipe — Zero-config folder → neural semantic search.** `xerj autoindex` a
  mixed-format folder against a `--embed-mode neural` server, then search the
  discovered prose by meaning while structured files stay exactly filterable. Guide
  `docs/recipes/autoindex-semantic-search.md`, runnable `recipes/autoindex_semantic.sh`,
  sample corpus `demo/data/support-folder/`.

### Changed

- `--embed-mode {lexical|neural|proxy|auto}` CLI flag and `XERJ_EMBED_MODE` env on
  the server; new `embedding.{mode,neural_model,model_cache_dir,local_model_dir}`
  config keys.
- Documentation updated for honesty consistency (README, AGENTS.md, ROADMAP.md,
  llms.txt, recipe guides): the **default** embedder is lexical; the neural embedder
  is an **opt-in** upgrade — output is only described as neural when that mode runs.

## [1.0.0-rc.1] - 2026-07-06

First public release candidate of XERJ — an Elasticsearch-wire-compatible search,
vector, and log-analytics engine written in Rust and licensed under Apache-2.0. This
is a release candidate: the wire protocol and on-disk format are considered stable
for evaluation, but may still change before the final 1.0.0.

### Added

- **Elasticsearch-compatible REST API.** Drop-in wire compatibility with the ES
  8.x HTTP surface, served from `xerj-api` (`es_compat.rs`) on port `9200`:
  - Document APIs: `PUT`/`GET`/`DELETE /{index}/_doc/{id}` and
    `POST /{index}/_update/{id}`.
  - Search: `POST /{index}/_search` with `query`, `from`, `size`, `sort`, `aggs`,
    `_source`, and `highlight`.
  - Bulk API: `POST /_bulk` with `index`, `create`, `update`, and `delete` actions.
  - Scroll API: `POST /{index}/_search?scroll=1m` and `POST /_search/scroll`.
  - `POST /{index}/_delete_by_query`, index templates (`PUT /_index_template/{name}`),
    and aliases (`POST /_aliases` with `add`/`remove`).
- **Full-text search (`xerj-fts`).** BM25 scoring with an analyzer registry and
  on-disk postings lists. Supported query types include `match_all`, `match_none`,
  `match`, `match_phrase`, `match_phrase_prefix`, `multi_match`, `term`, `terms`,
  `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`,
  `query_string`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`,
  and `geo_distance`.
- **Vector search (`xerj-vector`).** Dense-vector HNSW index for k-NN and semantic
  search, exposed through the `knn`, `semantic`, and `hybrid` query types.
- **Aggregations.** `terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`,
  `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`,
  `missing`, and `composite`, with a columnar fast path for `size: 0` aggregations.
- **Sharded ingest and storage (`xerj-storage`).** Write-ahead log with a single
  monotonic sequence-number writer, a 16-shard in-memory memtable
  (`shard = xxh3_64(doc_id) & 15`), flush to immutable segments, and background
  segment merging. WAL replay rebuilds both the storage and FTS memtables on restart.
- **Log analytics (`xerj-logs`).** Columnar log ingestion with retention.
- **AI helpers (`xerj-ai`).** Text chunking, an embedding proxy, and a memory store
  for semantic workflows.
- **Clustering (`xerj-cluster`).** Embedded Raft consensus for cluster metadata with
  no external dependencies.
- **Bundled console (`xerj-console-api`).** Dashboards, auth, preferences, and
  cluster awareness, compiled into the `xerj` binary and mounted under
  `/_xerj-console/api/v1/*`.
- **Transform pipeline (`xerj-wasm`).** Built-in transform plugins with an optional
  WASM backend.
- **Block compression (`xerj-compress`).** LZ4 and Zstd codecs for segment blocks.
- **Single static binary.** `cargo build --release -p xerj-server` produces `xerj`;
  run with `./target/release/xerj --data-dir ./data --insecure`.
- **ES-YAML conformance harness.** A workspace test runner (`es-yaml-runner`) that
  executes the ES 8.13 REST-API-spec YAML suites (search, aggregations, vectors,
  bulk, indices, scroll, cluster) against a live server. XERJ passes 1,326 of 1,329
  cases.
- **Reproducible head-to-head benchmarks.** A 91-cell XERJ-vs-Elasticsearch-8.13
  matrix (ingest, read, vector, and disk dimensions), published and reproducible at
  <https://xerj.org/benchmarks>. The scorecard is honest about both wins and losses.

### Changed

- `_forcemerge` is now synchronous and quiescent, matching Elasticsearch semantics,
  and merge status is exposed through `_stats`.
- Search hit materialization for `size > 0` is bounded to the top `from + size`
  candidates, reducing per-query cost from O(N) toward O(from + size).
- Bulk ingest avoids redundant JSON round-trips and batches schema evolution to
  raise throughput under concurrent load.

### Fixed

- Consecutive `_bulk` `delete` actions that were previously dropped are now applied
  correctly.
- `hits.total` for `size > 0` searches is delete-aware, resolving a conformance
  regression.
- Corrected top-N sort behavior and delete-awareness across the memtable/segment
  merge path.

### Known limitations

- 3 of 1,329 ES-YAML conformance cases do not yet pass.
- This is a release candidate; some Elasticsearch APIs and query/aggregation options
  outside the list above are not yet implemented. See
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and `engine/CLAUDE.md` for the
  current supported surface.

[Unreleased]: https://github.com/xerj-org/xerj/compare/v1.0.0-rc.5...HEAD
[1.0.0-rc.5]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.5
[1.0.0-rc.4]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.4
[1.0.0-rc.3]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.3
[1.0.0-rc.2]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.2
[1.0.0-rc.1]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.1
