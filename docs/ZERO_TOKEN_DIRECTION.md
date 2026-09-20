# The zero-token direction

**Zero-token** is the name for work XERJ does locally, inside the engine, so
that a person or an agent spends no model tokens *finding*, *judging*,
*watching for* or *handing over* an answer. Plain search already works that
way: no model runs to answer a query, and nothing leaves the machine.

This page is the long form of the *zero-token direction* section in
[ROADMAP.md](../ROADMAP.md). The roadmap is authoritative for status; this page
holds the reasoning, the measured baseline and the design constraints. It is a
plan and a status report, **not a feature list** — most of what it describes
does not exist yet, and each section says so first.

Every "today" statement below was checked against `main` on 2026-09-18
(`4d8dadbf`, 32 commits after `v1.0.0-rc.74`) by reading the file named beside
it. Every number comes from a run whose harness and literal output are in
[`benchmarks/neural-path-triage/`](../benchmarks/neural-path-triage/).

## Status at a glance

| Item | On `main` today | State |
|---|---|---|
| Judged search (rerank stage) | ES `rescore` and the `hybrid` query type only | in flight: `feat/rerank-stage` |
| Share links, guest reading room | no share or guest code in the tree | in flight: `feat/share-links`, `feat/console-corpus-reader` |
| Mail ingest | `.eml` / MIME extraction and, since #949, streaming `mbox` / extracted Google Takeout ingest with reply edges, in `xerj autoindex` (unreleased) | on `main`; synthetic mailbox only; server memory is the limit (#948) |
| `autoindex` resilience | one refused dataset aborts the run (#929) | in flight: `fix/autoindex-resilience` |
| Semantic detections | `percolate` works; `_watcher` stores and never evaluates; alert rules have schemas and no evaluator | planned |
| Object-storage backend | real S3/R2/MinIO client + read-through cache land in `xerj-storage`; the index path still does not use them, so `storage.backend = "s3"` refuses to start | in flight: `feat/object-storage-backend` |
| Block index mode for logs | `xerj-logs` crate is compiled in and called from no non-test code | planned |
| User-code ingest plugins | built-in native plugins run; no wasmtime backend in the tree | planned |
| Corpus hub (signed packs) | nothing | planned |

"In flight" means a pushed branch exists and nothing of it is on `main`.
"Planned" means no code exists.

## 1. Judged search

Retrieval answers "which documents mention this". Judged search adds a second
stage that answers "which of these actually answer it", over the top *N* only.

**Today.** The ES `rescore` block (a query rescorer) and the `hybrid` query
type with `rrf` or `linear` fusion. There is no rerank stage on `main`. The
branch `feat/rerank-stage` adds a `rerank` block on `_search` that hands the
top hits to an external relevance judge; it is opt-in per request and inert
until an operator configures a key, and it is the one feature that would send
document text off the machine. That is exactly why a *local* judge is on this
list.

**The bar a local model has to clear.** Before building one we measured what
already ships. BEIR test splits, `--embed-mode neural` (all-MiniLM-L6-v2, CPU),
nDCG@10:

| Arm | SciFact (300 queries, 5,183 docs) | NFCorpus (323 queries, 3,633 docs) |
|---|---|---|
| BM25 (`multi_match` over `title`, `text`) | 0.657 | 0.302 |
| MiniLM vectors only (`semantic`) | 0.676 | 0.329 |
| BM25 top-30 re-ordered by the same bi-encoder | 0.686 | 0.332 |
| **`hybrid`, RRF, server-side** | **0.699–0.704** (three runs) | **0.345** |

Two conclusions. Re-ordering a BM25 shortlist with the embedder XERJ already
ships is *worse* than the hybrid query XERJ already ships, on both corpora, so
that is not worth building. And **a local cross-encoder ships only if it beats
0.699 / 0.345 on this harness** by more than the run-to-run spread described
below; if it does not, it does not ship.

Read these numbers with their limits. They are with the opt-in neural
embedder; the **default embedder is lexical feature hashing** and was not part
of this run. They are two small public corpora on one machine. The hybrid
figure moves between runs of unchanged indices — 0.6993, 0.7023 and 0.7044 on
SciFact; 0.3448, 0.3446 and 0.3450 on NFCorpus — because documents with an
equal fused score are ordered by a per-process hash seed. That is a bug with
its own issue. The other three arms reproduce to four decimals every time.

On NFCorpus, BM25 returns no hits at all for 25 of the 323 queries. That is
legitimate: in all 25, no query token occurs in any document (18 absent terms
such as `Zoloft`, 7 plurals whose singular is present, such as `bagels`). The
`standard` analyzer does not stem. The defect found there is a different one —
see *What the measuring found* below.

## 2. Share links and a guest reading room

Hand one person a link to one corpus or one document, read-only, without
creating them an account.

**Today.** Nothing. There is no share or guest code in the tree; reading a
corpus needs an account session or an API key. Two branches are in flight:
`feat/share-links` (scoped, expiring links; a share can never name a system
index) and `feat/console-corpus-reader` (the corpus browser, and the reader a
link opens). Both carry commits their authors marked unverified. They are not
claims yet.

Design constraints that are not negotiable: a link grants read access to the
named scope and nothing else; it expires; it can be revoked; opening one is
audited; and a guest never receives a credential that works anywhere else.

## 3. Mail ingest

**Today, on `main`, unreleased.** `xerj autoindex` parses `.eml` / MIME
messages: one record for the message with decoded header fields and the
plain-text body, and separate records per attachment linked back to the parent
message — a PDF attachment goes through the PDF extractor
(`engine/crates/xerj-autoindex/src/extract/eml.rs`).

**On `main` since #949, unreleased.** Streaming `mbox` (Takeout, Thunderbird,
Apple Mail, mutt), extracted Google Takeout exports — archives are never
opened; the run names each one with the command to extract it — and the
`email-thread@1` detector's `replies_to` / `attachment_of` edges. Measured on
a synthetic mailbox only (`benchmarks/mbox-ingest/README.md`); the node's
ingest memory is the limit (#948).

## 4. Semantic detections

A standing question asked of every incoming document, in three stages:

1. **Prefilter** — a `percolate` query selects the few stored detections a
   document could possibly match. Cheap, lexical, no model.
2. **Typed judgment** — for the survivors only, a judge returns one of a small
   closed set of outcomes. Not free text.
3. **Alert with a calibrated probability** — *calibrated* means measured on
   held-out labelled data and published with its reliability curve. An
   uncalibrated score is not shipped as a probability.

**Today.** Stage 1 is real: `percolate` is a dispatched query type and matches
stored queries against a supplied document. Stages 2 and 3 do not exist, and
**XERJ has no alerting**:

- `PUT /_watcher/watch/{id}` stores the body in an in-memory map and answers
  `"condition": {"met": true, "type": "always"}`. `GET` and `DELETE` work on
  that map. No scheduler exists and no watch is ever evaluated
  (`engine/crates/xerj-api/src/es_compat.rs`, `put_watch`; nothing else reads
  `engine.watches`).
- The console owns `.xerj_alert_rules` and `.xerj_alert_fires`. Both have
  schemas and are created at bootstrap
  (`engine/crates/xerj-console-api/src/indices.rs`). No evaluator reads the
  first or writes the second.

So the first piece of work here is not new: `_watcher` must either run watches
or refuse them. Today it is an accepted-and-ignored surface.

## 5. A real object-storage backend

**Today.** The client is real. `engine/crates/xerj-storage/src/s3.rs`
implements `StorageBackend` over `aws-sdk-s3`: ranged `GetObject`,
`PutObject`, paginated `ListObjectsV2`, `HeadObject`, `DeleteObject`, with an
endpoint override so one type covers Cloudflare R2, MinIO and AWS S3. It is
tested against both MinIO and R2, and it counts every billed request by class
so an operator can see what it spends. `SegmentCache` serves byte ranges
through it with a bounded local cache. The old local-directory simulation is
kept as `SimulatedObjectStore`, a test double, because unit tests want a
backend with no network and no cost.

**Still missing, and this is why the selector still refuses.** The index does
not use any of it. `StorageMode::ObjectStore` exists with a flush-upload and a
read-through open path, but the only `StorageMode` built outside tests is in
`engine/crates/xerj-engine/src/index.rs` and it is `Local`. The upload path
sends one file per segment — the `.seg` — where a real 25-field segment writes
104, and `snapshot.json` never leaves local disk, so a fresh node pointed at
the bucket sees zero segments. That is measured, not assumed:
`object_store_mode_does_not_yet_make_an_index_stateless` runs exactly that
experiment. So `storage.backend = "s3"` still stops the server at startup, now
with a message that says which half is missing
(`engine/crates/xerj-common/src/config.rs`).

The same test found the encouraging half: asked for a segment by id, a fresh
node with an empty disk does fetch it from the bucket and read it correctly.
The bytes survive losing the node; the catalogue that tells a node what to ask
for does not.

**Planned.** Bundle a segment's files into a single object with a byte-offset
footer — one object per segment rather than 104, which the free-tier arithmetic
in `docs/OBJECT_STORAGE.md` shows is the difference between 9% and 899% of
Cloudflare R2's monthly Class A allowance. Then atomic snapshot publication to
the bucket, and crash-consistency tests against a real endpoint **before** the
selector is allowed to accept the value.

## 6. A block index mode for logs

An index mode for log-shaped data: rows live in time-partitioned columnar
blocks, and a query skips whole blocks using their min/max metadata, instead
of building a per-term inverted index over every field.

**Today.** The `xerj-logs` crate implements that design — columnar encoding,
log-template extraction, time-range queries with block skipping, retention. It
is compiled into `xerj-engine` and `xerj-server` as a dependency and **called
from no non-test code**. Log-shaped analytics run through the general columnar
segment format and the aggregation suite.

**Planned.** Wire it behind an explicit index setting, measured against the
general path on the same data, or remove the crate.

## 7. User-code ingest plugins

**Today.** Ingest-pipeline transforms are built-in native Rust plugins —
rename, drop, add, JSON parse, timestamp parse, PII redaction, grok, route —
and they do run on `_bulk`. The crate is called `xerj-wasm`, but **the wasmtime
backend is not in the tree**: `engine/crates/xerj-wasm/Cargo.toml` has no
`wasmtime` dependency and no `wasm` feature; `lib.rs` only notes that one could
be added behind the same trait. A pipeline naming a processor this build does
not implement is stored as unrunnable, and every ingest through it is refused
— it is never quietly run as a shorter pipeline.

**Planned.** A sandboxed runtime for user-supplied transforms: fuel and memory
limits per document, no ambient filesystem or network access, and the same
refuse-loudly rule when a module cannot be loaded.

## 8. A corpus hub of signed, pre-indexed packs

Download a reference corpus — a standard library, a specification set —
already indexed, and mount it, instead of every user spending the same CPU
hours indexing the same public text.

**Today.** Nothing. No pack code exists.

**The pack format is the design work.** A pack is a directory or archive with
a manifest carrying:

- a **format version**. A reader that does not know the version refuses the
  pack. Segment headers already follow this rule
  (`engine/crates/xerj-storage/src/segment.rs`: a header whose `format_version`
  is not the reader's returns `StorageError::UnsupportedVersion`).
- **per-file checksums** for every file in the pack.
- a **detached signature** over the manifest, which in turn covers the
  checksums.
- **licence and provenance on every record**: where the text came from, at
  which revision, under which licence — so a hit can say so.

**Three risks decide whether this ships at all.**

- **Redistribution licence.** An index is a derived copy of its source text. A
  pack may carry only what its licence lets the project redistribute, and that
  excludes much of what people most want pre-indexed.
- **Pack safety.** A pack is untrusted input to the segment readers. It needs
  the fuzzing the on-disk formats get. A signature proves who built a pack; it
  does not prove the pack is safe to open.
- **Format stability.** A published pack outlives the release that built it.
  The pack format therefore needs a compatibility promise that the internal
  segment format has never had to make.

## What the measuring found

Measuring the baseline for section 1 turned up four defects. Each is a filed
issue with a literal reproduction; the harness and the raw output are in
[`benchmarks/neural-path-triage/`](../benchmarks/neural-path-triage/). Stage 1
as a whole is tracked in [#941](https://github.com/xerj-org/xerj/issues/941).

- **A declared analyzer stops applying at flush**
  ([#937](https://github.com/xerj-org/xerj/issues/937)). A default analyzer declared
  in `settings.analysis` is honoured by the in-memory table only; after a
  flush the segment is written and queried with `standard`, so the same
  `match` query returns a different hit set. A per-field `analyzer` in the
  mapping is accepted, echoed back and ignored, and an unknown analyzer name
  is accepted. This is why stemming cannot currently be turned on.
- **Neural ingest leaves the machine idle**
  ([#938](https://github.com/xerj-org/xerj/issues/938)). One `_bulk` stream into a
  `semantic_text` field runs at 6.1 documents per second on ~1,470-character
  abstracts while the server keeps 3.4 of 32 hardware threads busy. The same
  node reaches 29.5 documents per second when eight clients send concurrently,
  for the same total CPU.
- **A `semantic` query over multi-passage documents is an exact scan that
  copies every stored document**
  ([#939](https://github.com/xerj-org/xerj/issues/939)). ~410 ms p50 on 5,183 documents, of which the
  model's forward pass is ~14 ms. The same query on a 10,003-document index of
  one-sentence documents is ~14 ms in total.
- **Tied RRF scores are ordered by a per-process hash seed**
  ([#940](https://github.com/xerj-org/xerj/issues/940)). After a restart
  on unchanged data, 32 of 40 SciFact queries returned a different order and
  21 of 40 a different top 10, with identical hit sets.

All four were measured on a shared 32-thread machine under load from other
jobs. The proportions are the finding; the absolute milliseconds are
conservative.

XERJ is single-node. Every figure on this page is one process on one host.
