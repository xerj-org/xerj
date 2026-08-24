---
title: "Why is reindexing my files taking forever?"
h1: "Reindexing my files is taking forever. What usually causes that?"
description: "A measured XERJ _reindex moved 100,000 documents in 20,836 ms on one node. Read the batches, created and failures fields to tell slow progress from no progress."
slug: "reindex-taking-forever"
cluster: "Operations: reindex"
question: "Reindexing my files is taking forever. What usually causes that?"
intent: "troubleshooting"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a single-node XERJ, count the source index, POST /_reindex into a fresh destination index, then compare the response took, batches, created and failures fields against a _count of the destination before you report progress."
commands:
  - cmd: "curl -s http://127.0.0.1:9200/reindex_src/_count"
    note: "Count the source index before the copy starts."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/_reindex -H 'content-type: application/json' -d '{\"source\":{\"index\":\"reindex_src\"},\"dest\":{\"index\":\"reindex_dst\"}}'"
    note: "Copy every document from the source index into a fresh destination index."
  - cmd: "curl -s http://127.0.0.1:9200/reindex_dst/_count"
    note: "Count the destination index after the call returns."
links_out:
  - "read-autoindex-progress"
  - "/docs/migration-from-es"
  - "cheap-low-volume-log-search"
  - "resume-interrupted-autoindex-run"
faq:
  - q: "Reindexing my files is taking forever. What usually causes that?"
    a: "Corpus size, document size, host load and mapping complexity set the wall time. A _reindex call blocks and answers only at the end, so read a _count of the destination before you call it stuck."
  - q: "Why is my local search index rebuild so slow?"
    a: "Corpus size, document size, host load and mapping complexity all change the time. The measured run used a fixed 100,000-document corpus at 1-minute load average 2.63."
  - q: "What should I measure when folder reindex never finishes?"
    a: "For a _reindex, read took, batches, created, version_conflicts, retries and failures, and count the destination while the call runs. For a folder re-run, read xerj autoindex status, which reports files done, records and the journal state."
  - q: "Is PDF extraction the thing that makes reindex hang?"
    a: "Not measured here. This page captures a _reindex of a fixed document corpus, not a per-format split, so it cannot blame PDF extraction. A folder run does bound each PDF with a size cap, a page cap and a worker timeout, and refuses a file past a cap instead of hanging on it."
  - q: "How long does a XERJ reindex take?"
    a: "One measured run copied 100,000 documents in 20,836 ms on a single node. That is one run on one shared host, not a rate you can extrapolate."
  - q: "Does XERJ reindex across nodes?"
    a: "No. XERJ is single-node, so _reindex is a local copy. There is no cross-node parallelism to add and no failover if the node stops."
  - q: "Does reindexing use more memory as it runs?"
    a: "Yes. XERJ has an open defect that retains heap per indexed document, so a large reindex grows resident memory. Do not plan corpora beyond a few million documents."
---

**TL;DR** — A XERJ `_reindex` of a fixed 100,000-document corpus reported `"took": 20836` milliseconds, 100 batches and 0 failures on one node. Read `created` against a `_count` of the destination index to separate slow progress from a stalled call. XERJ is single-node, so `_reindex` is a local copy.

## Two different jobs are called reindexing

A folder re-run and `POST /_reindex` are different calls. A folder re-run of `xerj autoindex` reads the journal in the state directory and skips the files it already finished. A `_reindex` copies documents from one index into another inside the same node.

This page measures the second call. The resume article covers the first.

## What one measured reindex looked like

A `_reindex` of 100,000 documents into a fresh destination index returned in 20,836 milliseconds. Harness wall-clock time for the same call was 20.893 s, and the source count and the destination count both read 100,000 afterward.

The captured response is small, and every field in it is a progress signal.

```json
{
  "took": 20836,
  "total": 100000,
  "created": 100000,
  "updated": 0,
  "batches": 100,
  "version_conflicts": 0,
  "noops": 0,
  "retries": { "bulk": 0, "search": 0 },
  "throttled_millis": 0,
  "timed_out": false,
  "failures": []
}
```

Treat this as one run on one shared host at 1-minute load average 2.63, not as a throughput promise.

## Read the fields before you assume a stall

`created` is the field that answers "is it moving". A `_reindex` call blocks until it finishes, so the response arrives only at the end. During the call, read progress from the destination index instead.

Count the destination index while the copy runs.

```sh
curl -s http://127.0.0.1:9200/reindex_dst/_count
```

A count that climbs means the copy still makes progress. A count that stays flat while the node still answers means one batch is still in flight.

## Why the wall time is what it is

The measured call moved 100,000 documents in 100 batches, so each batch carried 1,000 documents. Batch count, document size and mapping complexity set the wall time more than any single tuning flag does.

Host load matters too. The capture recorded 1-minute load average 2.63 before the call and 2.27 after it.

For context, XERJ measured 1.72× the bulk ingest rate of Elasticsearch 8.13.4 on a separate 100,000-document board. The same board publishes 4 read p99 losses under sustained write, including 13.57 ms for XERJ against 3.45 ms for Elasticsearch. Neither result predicts your `_reindex` time.

## Memory grows while a reindex runs

XERJ retains heap per indexed document, which is an open engine defect rather than a tuning problem. A long `_reindex` therefore grows the resident set as it proceeds. The project's own instruction is to not plan corpora beyond a few million documents.

Watch free memory on the host, not only the response.

## Check size the way that works

Two Elasticsearch habits for reading index size fail on XERJ, and the capture keeps both responses. `GET /_cat/indices?format=json&bytes=b` still returns human-formatted values such as `716.1kb`. The metric-filtered `GET /{index}/_stats/store` returns 404.

Use the plain form, then read `_all.total.store.size_in_bytes` from the response.

```sh
curl -s http://127.0.0.1:9200/reindex_dst/_stats
```

## What single-node means for a reindex

XERJ is single-node, so `_reindex` copies inside one process on one host. There is no data-plane replication and no failover, so a node that stops takes the in-flight batch with it. Re-run the call after a restart and compare the destination `_count` with the source `_count`.
