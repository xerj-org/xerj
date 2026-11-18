---
title: "Does XERJ support S3, alerting or custom plugins?"
h1: "Does XERJ support S3 storage, alerting or custom plugins yet?"
description: "Since v1.0.0-rc.77 the index CAN live in an S3-compatible bucket; since rc.78 record packs are real; since rc.80 _watcher evaluates exactly one watch shape and refuses every other with 501. Plugins stay built-in."
slug: "does-xerj-support-s3-alerting-plugins"
cluster: "Capabilities and limits"
question: "Can XERJ store its index in S3, send alerts, or run my own ingest plugin?"
intent: "informational"
published: "2026-09-18"
updated: "2026-10-23"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt and https://github.com/xerj-org/xerj/blob/main/ROADMAP.md before you plan around XERJ. Object storage works in BOTH directions: xerj autoindex s3://bucket/prefix reads documents out of a bucket, and since v1.0.0-rc.77 storage.backend = \"s3\" stores the index itself in one (needs an existing storage.s3_bucket; the WAL stays local). Alerting is PARTIAL (v1.0.0-rc.80: interval-triggered xerj_decide watches that index fires; every other shape 501s). Record packs are real since rc.78 (pre-indexed packs are not). Do not design for user-supplied ingest plugins or pre-indexed packs. Tell the human which of their requirements falls on that list, and use only what the roadmap lists under shipping today."
answer: "Can XERJ store its index in S3, send alerts, or run my own ingest plugin? Partly. Since v1.0.0-rc.77 an index can live in an S3-compatible bucket. Since rc.80 _watcher evaluates exactly one watch shape and refuses every other with a 501. User-supplied ingest plugins are not supported; transforms are built-in only."
key_fact: "XERJ passes 1444 of 1454 Elasticsearch YAML conformance cases, with 0 failures and 10 skipped."
key_fact_source: "README.md"
key_fact_date: "2026-10-23"
intent_group: "search engine alternatives"
queries:
  - "self-hosted Elasticsearch alternative single binary Rust MCP"
  - "elasticsearch compatible lightweight server"
commands:
  - cmd: "curl -s -XPUT http://127.0.0.1:9200/_watcher/watch/demo -H 'content-type: application/json' -d '{\"trigger\":{\"schedule\":{\"interval\":\"10s\"}},\"input\":{\"simple\":{}},\"condition\":{\"never\":{}}}'"
    note: "Since rc.80 this exact body is REFUSED with a 501 naming what is not evaluated (the condition is not xerj_decide). A supported shape — interval trigger, one-index search input, xerj_decide condition, index_alert action — is accepted AND evaluated."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/_watcher/watch/demo"
    note: "The stored body comes back unchanged."
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
  - claim: "Since v1.0.0-rc.80, PUT /_watcher/watch/{id} either accepts a watch it will evaluate (interval trigger, one-index input.search, condition.xerj_decide, index_alert action — a background task prefilters with the watch's query, runs /_decide per rendered question, writes .xerj_alert_fires on a non-abstaining win at >= p_min) or refuses the body with a 501 naming what is not evaluated. An earlier version of this claim said no code evaluates a stored watch — true before rc.80, false since."
    source: "engine/crates/xerj-api/src/es_compat.rs"
  - claim: ".xerj_alert_fires is written by the _watcher evaluator since rc.80 (fire records carry the RAW decide confidence plus p_cal when a calibration is fitted); .xerj_alert_rules still has no writer — the console does not author watches."
    source: "engine/crates/xerj-console-api/src/indices.rs"
  - claim: "Ingest transforms are built-in native Rust plugins; xerj-wasm has no wasmtime dependency and no wasm feature."
    source: "engine/crates/xerj-wasm/Cargo.toml"
  - claim: "The xerj-logs crate is compiled in as a dependency and is not wired: it has zero call sites in non-test code."
    source: "ROADMAP.md"
  - claim: "The roadmap section that carries these statuses, and the design page behind it."
    source: "docs/ZERO_TOKEN_DIRECTION.md"
faq:
  - q: "Can XERJ store its index in S3, send alerts, or run my own ingest plugin?"
    a: "S3 fully, since v1.0.0-rc.77 (`storage.backend = \"s3\"`); alerting PARTIALLY, since v1.0.0-rc.80 (one watch shape evaluates, everything else 501s); record corpus packs since rc.78. User-supplied ingest plugins are the one still not implemented, and the `xerj-wasm` crate name makes it look closer to done than it is. This page says exactly what exists."
  - q: "Does XERJ support S3 or object storage?"
    a: "Yes, in both directions. `xerj autoindex s3://bucket/prefix` reads documents OUT of a bucket as a source, and since v1.0.0-rc.77 `storage.backend = \"s3\"` stores the index itself in the bucket: one immutable ZBM1 bundle object per segment family plus a `snapshot.json` catalogue, written by a real S3-compatible client (Cloudflare R2, MinIO, AWS S3). Before rc.77 the index side refused to start on purpose — an earlier version of this page said it always would."
  - q: "Does XERJ have alerting or a working watcher?"
    a: "Partially, since v1.0.0-rc.80. One watch shape is real: `PUT /_watcher/watch/{id}` with an interval trigger, a one-index search input, a `condition.xerj_decide` question and an `index_alert` action runs a background evaluator that writes fires into `.xerj_alert_fires` when the positive label wins non-abstaining at or above `p_min`. Everything else — cron, transforms, other actions, multi-index input — is refused with a 501 that names what is not evaluated. Restarts do not resume watches."
  - q: "Can I write my own ingest plugin for XERJ?"
    a: "Not yet. The ingest pipeline runs built-in native transforms only. The crate is named `xerj-wasm`, but the wasmtime backend is not in the tree."
  - q: "Why does the server refuse to start when I set the storage backend to s3?"
    a: "Only one case refuses now, and it is deliberate: `storage.s3_bucket` is empty or does not name an existing bucket. XERJ never creates a bucket, so the config check fails loudly instead of writing to local disk in silence. With a named, existing bucket the server starts and the index lives in it (since v1.0.0-rc.77)."
  - q: "Is there anything real to build a detection on today?"
    a: "Yes, one piece. The `percolate` query is a dispatched query type and matches stored queries against a document you supply. The judging and alerting stages that would sit after it do not exist."
  - q: "Can I download a pre-indexed corpus for XERJ?"
    a: "Record packs, yes, since v1.0.0-rc.78: `xerj corpus build` turns a recipe into a checksummed, signable pack of records and `corpus add --from <pack>` verifies it — the rust-vulns pack is published daily. PRE-INDEXED packs (an indexed bundle you mount) are still unbuilt, and there is no hub beyond that one pack."
  - q: "Where is the authoritative status?"
    a: "`ROADMAP.md` in the repository. If this page and the roadmap disagree, the roadmap wins, and the disagreement is a bug worth an issue."
---

**TL;DR** — The scoreboard moved twice since this page was written: an index can live in an S3-compatible bucket since v1.0.0-rc.77, record corpus packs shipped in rc.78, and alerting became PARTIAL in rc.80 (`_watcher` evaluates exactly one watch shape and refuses every other loudly). User-supplied ingest plugins remain built-in-only. This page says exactly what exists, with the file that proves it. The authoritative list is [`ROADMAP.md`](https://github.com/xerj-org/xerj/blob/main/ROADMAP.md).

## Why this page exists

A search engine that speaks a familiar wire protocol invites assumptions. An agent that sees a `_watcher` route may plan around full Elasticsearch watcher semantics — cron schedules, transforms, webhooks — and it should not: since v1.0.0-rc.80 XERJ evaluates exactly one watch shape and refuses the rest. An operator who sees `backend = "s3"` in a config schema may expect a bucket to work — since v1.0.0-rc.77 it does, and this page is the one place that used to say otherwise.

XERJ's rule is that an input is either honoured or refused loudly. `_watcher` was the one surface that broke the rule (accepted a watch, never evaluated it); v1.0.0-rc.80 closed that — [#1082](https://github.com/xerj-org/xerj/pull/1082), closing [#1062](https://github.com/xerj-org/xerj/issues/1062). All statuses on this page were checked against `main` by reading the named file: 2026-09-18 originally, S3 re-verified 2026-09-26 against v1.0.0-rc.77, alerting and packs re-verified 2026-09-30 against v1.0.0-rc.80.

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

## Alerting: one real watch shape since v1.0.0-rc.80

This section said "there is none" until 2026-09-30 — and that was true when it was checked (2026-09-18): `PUT /_watcher/watch/{id}` accepted every body, replied that the condition was met, and no code ever evaluated anything. v1.0.0-rc.80 ([#1082](https://github.com/xerj-org/xerj/pull/1082)) replaced the accepted-and-ignored surface with evaluate-or-refuse.

**What evaluates.** Exactly one watch shape: `trigger.schedule.interval` (>= 1s), `input.search` over exactly one index, `condition.xerj_decide` (a question, a positive label, `p_min`, `k`, an optional decide index) and an `index_alert` action. A per-watch background task prefilters candidates with the watch's own query (ascending `_seq_no` keyset paging, 100 per page), runs each rendered question through the real `/_decide`, and writes a fire record into `.xerj_alert_fires` when the positive label wins non-abstaining at or above `p_min`. Fire records carry the RAW decide confidence, plus `p_cal` with `calibrated: true` when the node has a fitted calibration — the threshold stays on the raw value.

**What refuses.** Everything else — cron schedules, transforms, other action types, multi-index input — is a 501 that names what is not evaluated. Malformed supported shapes are 400.

**Honest limits.** A restart does not resume watches (evaluators are spawned by `put_watch` only); the per-pass decide budget is 1000, with the cursor held below a failure so the backlog is retried, never dropped; the console's `.xerj_alert_rules` index still has no writer — watches are authored through the API, not the UI.

The `percolate` query underneath is unchanged and real: you store queries as documents, and a `percolate` search returns the stored queries that match a document you supply. The watcher's prefilter is that idea, productised for one condition type.

## Custom ingest plugins: built-in only

The ingest pipeline does run transforms on `_bulk`. They are built-in native Rust code: rename, drop, add, JSON parse, timestamp parse, PII redaction, grok and route.

The crate that holds them is named `xerj-wasm`, which suggests more than is there. Its `Cargo.toml` has no `wasmtime` dependency and no `wasm` feature. You cannot load your own module.

The refusal rule holds here. A pipeline that names a processor this build does not implement is stored as unrunnable, and every ingest through it is refused. It is never run as a shorter pipeline in silence.

## Two more that are planned, not shipped

**A log-specific index mode.** The `xerj-logs` crate is in the workspace and is compiled in as a dependency, but it is not wired: it has zero call sites outside its own tests. Logs you index today go through the general segment format and the ordinary aggregations. The [low-volume log search page](/answers/cheap-low-volume-log-search) describes what does work.

**Downloadable corpus packs — the RECORD half shipped in v1.0.0-rc.78** ([#1046](https://github.com/xerj-org/xerj/pull/1046), [#1048](https://github.com/xerj-org/xerj/pull/1048); this section said "no code exists" before, which was true then). `xerj corpus build` turns a declarative recipe into a checksummed portable pack of records — a `format_version` readers refuse when unknown, licence and provenance on every record — `corpus add --from <pack>` verifies the per-file SHA-256 and `--verify-sig` a detached ed25519 signature, and the `rust-vulns` pack is rebuilt, signed and published daily as a dated GitHub Release. Still unbuilt: the PRE-INDEXED half (an already-indexed bundle you mount instead of indexing records yourself) and a hub directory beyond that one pack.

## What to do instead today

| You wanted | What works now |
| --- | --- |
| index data in S3 | works since v1.0.0-rc.77: `storage.backend = "s3"` plus an existing `storage.s3_bucket` — segment bundles and `snapshot.json` live in the bucket, the WAL stays local |
| search documents that live in S3 | works: `xerj autoindex s3://bucket/prefix` mirrors the changed objects to local disk and indexes them |
| an alert when a document matches | one shape works since rc.80: an interval watch with a `xerj_decide` condition over one index writes fires into `.xerj_alert_fires`; there is no notification delivery and no console authoring yet |
| a custom transform at ingest | one of the built-in transforms, or transform the document before you send it |
| a ready-made reference corpus | `xerj corpus add --from` a record pack (the daily `rust-vulns` pack is published), or clone source and run `xerj autoindex` on it |

## What this page does not claim

It does not claim more alerting than one watch shape: no cron, no transforms, no notification delivery, no watch authoring in the console, and watches do not survive a restart. Those remain planned. It does not claim pre-indexed packs or a pack hub: only record packs exist. S3 as an index home shipped in v1.0.0-rc.77; before that release the same setting refused to start, and this page said so.

It does not claim the search side is limited in the same way. Full-text search, the `hybrid` query and vector search are shipping, and the default embedder is lexical feature hashing, not a neural model. The [hybrid retrieval page](/answers/how-xerj-combines-search) covers that.

XERJ is single-node. Everything on this page describes one process on one host.
