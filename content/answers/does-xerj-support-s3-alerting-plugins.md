---
title: "Does XERJ support S3, alerting or custom plugins?"
h1: "Does XERJ support S3 storage, alerting or custom plugins yet?"
description: "Since v1.0.0-rc.77 the index CAN live in an S3-compatible bucket (storage.backend = \"s3\": one immutable bundle object per segment family plus snapshot.json). _watcher never runs a watch; plugins are built-in."
slug: "does-xerj-support-s3-alerting-plugins"
cluster: "Capabilities and limits"
question: "Can XERJ store its index in S3, send alerts, or run my own ingest plugin?"
intent: "informational"
published: "2026-09-18"
updated: "2026-09-26"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt and https://github.com/xerj-org/xerj/blob/main/ROADMAP.md before you plan around XERJ. Object storage works in BOTH directions: xerj autoindex s3://bucket/prefix reads documents out of a bucket, and since v1.0.0-rc.77 storage.backend = \"s3\" stores the index itself in one (needs an existing storage.s3_bucket; the WAL stays local). Do not design for alerting, user-supplied ingest plugins or downloadable corpus packs: none of those is implemented. Tell the human which of their requirements falls on that list, and use only what the roadmap lists under shipping today."
commands:
  - cmd: "curl -s -XPUT http://127.0.0.1:9200/_watcher/watch/demo -H 'content-type: application/json' -d '{\"trigger\":{\"schedule\":{\"interval\":\"10s\"}},\"input\":{\"simple\":{}},\"condition\":{\"never\":{}}}'"
    note: "The request is accepted and the watch is stored. Nothing will ever evaluate it, and the reply says the condition was met although the watch asked for never."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/_watcher/watch/demo"
    note: "The stored body comes back unchanged. That is all this API does today."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/detections/_search -H 'content-type: application/json' -d '{\"query\":{\"percolate\":{\"field\":\"query\",\"document\":{\"message\":\"disk full on node 7\"}}}}'"
    note: "The percolate query is real. It matches stored queries against the document you supply, which is the piece a future detection feature builds on."
links_out:
  - "search-files-in-an-s3-bucket"
  - "what-is-xerj"
  - "how-xerj-combines-search"
  - "filter-knn-exact-scan-caveat"
  - "cheap-low-volume-log-search"
  - "local-embeddings-without-openai-api"
evidence:
  - claim: "storage.backend = \"s3\" stores the index in the bucket: one immutable ZBM1 bundle object per segment family plus a per-index snapshot.json catalogue; merges publish before retiring inputs; a fresh node adopts the bucket."
    source: "docs/OBJECT_STORAGE.md"
  - claim: "The one remaining startup refusal is the loud kind: storage.backend = \"s3\" requires storage.s3_bucket to name an existing bucket — XERJ never creates one."
    source: "engine/crates/xerj-common/src/config.rs"
  - claim: "The source side is implemented: xerj autoindex s3://bucket/prefix lists a prefix and streams each changed object into a local mirror — the mirrored object bytes land on local disk under the state directory."
    source: "docs/OBJECT_STORAGE.md"
  - claim: "PUT /_watcher/watch/{id} inserts the body into an in-memory map and answers condition met true; no code evaluates a stored watch."
    source: "engine/crates/xerj-api/src/es_compat.rs"
  - claim: "The console's .xerj_alert_rules and .xerj_alert_fires indices have schemas and are created at bootstrap; no evaluator reads or writes them."
    source: "engine/crates/xerj-console-api/src/indices.rs"
  - claim: "Ingest transforms are built-in native Rust plugins; xerj-wasm has no wasmtime dependency and no wasm feature."
    source: "engine/crates/xerj-wasm/Cargo.toml"
  - claim: "The xerj-logs crate is compiled in as a dependency and is not wired: it has zero call sites in non-test code."
    source: "ROADMAP.md"
  - claim: "The roadmap section that carries these statuses, and the design page behind it."
    source: "docs/ZERO_TOKEN_DIRECTION.md"
faq:
  - q: "Can XERJ store its index in S3, send alerts, or run my own ingest plugin?"
    a: "One of the three, since v1.0.0-rc.77: an index can live in an S3-compatible bucket (`storage.backend = \"s3\"`). Alerting and user-supplied plugins are still not implemented, and each has a surface in the code that looks closer to done than it is. This page says exactly what exists."
  - q: "Does XERJ support S3 or object storage?"
    a: "Yes, in both directions. `xerj autoindex s3://bucket/prefix` reads documents OUT of a bucket as a source, and since v1.0.0-rc.77 `storage.backend = \"s3\"` stores the index itself in the bucket: one immutable ZBM1 bundle object per segment family plus a `snapshot.json` catalogue, written by a real S3-compatible client (Cloudflare R2, MinIO, AWS S3). Before rc.77 the index side refused to start on purpose — an earlier version of this page said it always would."
  - q: "Does XERJ have alerting or a working watcher?"
    a: "No. XERJ has no alerting. `_watcher` stores watches and no scheduler ever evaluates them, and the console alert-rule indices have schemas and no evaluator."
  - q: "Can I write my own ingest plugin for XERJ?"
    a: "Not yet. The ingest pipeline runs built-in native transforms only. The crate is named `xerj-wasm`, but the wasmtime backend is not in the tree."
  - q: "Why does the server refuse to start when I set the storage backend to s3?"
    a: "Only one case refuses now, and it is deliberate: `storage.s3_bucket` is empty or does not name an existing bucket. XERJ never creates a bucket, so the config check fails loudly instead of writing to local disk in silence. With a named, existing bucket the server starts and the index lives in it (since v1.0.0-rc.77)."
  - q: "Is there anything real to build a detection on today?"
    a: "Yes, one piece. The `percolate` query is a dispatched query type and matches stored queries against a document you supply. The judging and alerting stages that would sit after it do not exist."
  - q: "Can I download a pre-indexed corpus for XERJ?"
    a: "No. A hub of signed, pre-indexed packs is planned and no code exists. The open questions are redistribution licence, pack safety and format stability, and they decide whether it ships."
  - q: "Where is the authoritative status?"
    a: "`ROADMAP.md` in the repository. If this page and the roadmap disagree, the roadmap wins, and the disagreement is a bug worth an issue."
---

**TL;DR** — One of the three shipped in v1.0.0-rc.77: an index can live in an S3-compatible bucket (`storage.backend = "s3"`). Alerting and user-supplied plugins are still not implemented, and each has a surface in the code that looks closer to done than it is, so this page says exactly what exists, with the file that proves it. The authoritative list is [`ROADMAP.md`](https://github.com/xerj-org/xerj/blob/main/ROADMAP.md).

## Why this page exists

A search engine that speaks a familiar wire protocol invites assumptions. An agent that sees a `_watcher` route may plan around it, and it should not: XERJ does not run watches. An operator who sees `backend = "s3"` in a config schema may expect a bucket to work — since v1.0.0-rc.77 it does, and this page is the one place that used to say otherwise.

XERJ's rule is that an input is either honoured or refused loudly. Two of the three follow that rule today and one does not (`_watcher` accepts a watch and never evaluates it — the accepted-and-ignored surface). All of them were checked against `main` by reading the named file: 2026-09-18 originally, with the S3 status re-verified on 2026-09-26 against v1.0.0-rc.77.

## S3 and object storage: both directions work

This section said the opposite until 2026-09-26 — it described `S3Backend` as a local-directory simulation with no network client, and the startup refusal as permanent. That was true when it was checked (2026-09-18, before rc.77) and stopped being true in v1.0.0-rc.77. The correction is stated in the open rather than quietly rewritten.

**Reading documents out of a bucket works.** `xerj autoindex s3://bucket/prefix` lists the prefix, streams in each object whose ETag or size changed, and indexes it with the same extractors it uses for a folder. `r2://` and any S3-compatible store behind `--endpoint-url` work the same way. The [S3 indexing page](/answers/search-files-in-an-s3-bucket) has the commands and the request arithmetic.

**Storing the index in a bucket works, since v1.0.0-rc.77** ([#1008](https://github.com/xerj-org/xerj/pull/1008), closing [#965](https://github.com/xerj-org/xerj/issues/965)). Set `storage.backend = "s3"` with an existing `storage.s3_bucket`, and:

- the segment path packs each segment family into **one immutable ZBM1 bundle object** — not one PUT per file (the layout it replaced would have issued ~104 PUTs per segment);
- a per-index `snapshot.json` catalogue is the publication point: merges publish their outputs before retiring their inputs, and a fresh node adopts the bucket by reading it;
- the client is a real S3-compatible client over `aws-sdk-s3` — Cloudflare R2, MinIO and AWS S3 — in `engine/crates/xerj-storage/src/s3.rs`. The local-directory simulation survives only as a test double;
- the read-through segment cache, per-request cost accounting by billing class, and the budget that stops rather than warns landed in v1.0.0-rc.75;
- the WAL stays local. [`docs/OBJECT_STORAGE.md`](https://github.com/xerj-org/xerj/blob/main/docs/OBJECT_STORAGE.md) is the full design page.

One refusal remains, and it is the loud kind: with `storage.backend = "s3"` and no `storage.s3_bucket`, the server does not start. It prints that `storage.backend = "s3"` requires `storage.s3_bucket` to name an existing bucket — XERJ never creates one. That check is in `engine/crates/xerj-common/src/config.rs`.

## Alerting: there is none

XERJ has no alerting. Two surfaces make it look otherwise.

The first is `_watcher`. `PUT /_watcher/watch/{id}` is accepted. The handler puts the body into an in-memory map and replies that the condition was met, whatever condition you sent. `GET` and `DELETE` work on the same map. There is no scheduler, and no code ever evaluates a stored watch. This one is an accepted-and-ignored input, and the roadmap says it must either run watches or refuse them.

The second is the console. It owns two system indices, `.xerj_alert_rules` and `.xerj_alert_fires`. Both have schemas and both are created at start-up. No evaluator reads the first or writes the second, so there are no alerts and no notifications from them either.

One piece underneath is real. The `percolate` query is a dispatched query type: you store queries as documents, and a `percolate` search returns the stored queries that match a document you supply. A future detection feature would use that as its cheap first stage. The stages after it, a typed judgment and an alert that carries a calibrated probability, are planned and do not exist.

## Custom ingest plugins: built-in only

The ingest pipeline does run transforms on `_bulk`. They are built-in native Rust code: rename, drop, add, JSON parse, timestamp parse, PII redaction, grok and route.

The crate that holds them is named `xerj-wasm`, which suggests more than is there. Its `Cargo.toml` has no `wasmtime` dependency and no `wasm` feature. You cannot load your own module.

The refusal rule holds here. A pipeline that names a processor this build does not implement is stored as unrunnable, and every ingest through it is refused. It is never run as a shorter pipeline in silence.

## Two more that are planned, not shipped

**A log-specific index mode.** The `xerj-logs` crate is in the workspace and is compiled in as a dependency, but it is not wired: it has zero call sites outside its own tests. Logs you index today go through the general segment format and the ordinary aggregations. The [low-volume log search page](/answers/cheap-low-volume-log-search) describes what does work.

**Downloadable corpus packs.** A hub of signed, pre-indexed packs is on the roadmap and no code exists. The design work is the pack format: a format version that readers refuse when they do not know it, per-file checksums, a signature, and licence and provenance on every record. Three risks decide whether it ships: whether the source licence allows redistribution, whether a pack is safe to open, and whether the format can stay stable after packs are published.

## What to do instead today

| You wanted | What works now |
| --- | --- |
| index data in S3 | works since v1.0.0-rc.77: `storage.backend = "s3"` plus an existing `storage.s3_bucket` — segment bundles and `snapshot.json` live in the bucket, the WAL stays local |
| search documents that live in S3 | works: `xerj autoindex s3://bucket/prefix` mirrors the changed objects to local disk and indexes them |
| an alert when a document matches | there is no alerting; run a `percolate` or an ordinary search on your own schedule and act on the result yourself |
| a custom transform at ingest | one of the built-in transforms, or transform the document before you send it |
| a ready-made reference corpus | clone the source and run `xerj autoindex` on it |

## What this page does not claim

It does not claim a date for the two that remain — alerting and user-supplied plugins are planned, which means no code exists yet. S3 as an index home shipped in v1.0.0-rc.77; before that release the same setting refused to start, and this page said so.

It does not claim the search side is limited in the same way. Full-text search, the `hybrid` query and vector search are shipping, and the default embedder is lexical feature hashing, not a neural model. The [hybrid retrieval page](/answers/how-xerj-combines-search) covers that.

XERJ is single-node. Everything on this page describes one process on one host.
