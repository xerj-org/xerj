---
title: "What can I use for low-volume log search?"
h1: "What can I use for low-volume log search?"
description: "XERJ indexes plain-text logs on one node with exact error counts and sub-millisecond filters. A measured ladder stops at 16 MB: memory grows about 200x the corpus."
slug: "cheap-low-volume-log-search"
cluster: "Operations: low-volume logs"
question: "Free or cheap alternative for low volume log management and searching?"
intent: "tool-selection"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index a small folder of plain-text logs with xerj autoindex, then run a term filter on level and a terms aggregation on service, and record the node's peak resident memory from /proc before you plan a larger corpus."
commands:
  - cmd: "xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix log16 --state-dir ./state-log16 --progress plain"
    note: "Index a folder of plain-text log files into a typed index."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/log16-*/_search' -H 'content-type: application/json' -d '{\"size\":0,\"query\":{\"bool\":{\"filter\":[{\"term\":{\"level\":\"ERROR\"}}]}},\"track_total_hits\":true}'"
    note: "Count error lines exactly, with no sampling."
  - cmd: "curl -s http://127.0.0.1:9200/log16-logs/_stats"
    note: "Read the real on-disk size from _all.total.store.size_in_bytes."
links_out:
  - "search-json-and-jsonl-logs"
  - "search-gzip-logs-without-zgrep"
  - "search-kubernetes-pod-logs-on-disk"
  - "/docs/recipes/log-analytics"
faq:
  - q: "Can XERJ replace a hosted log service?"
    a: "For low volume on one node, yes. A measured 16 MB corpus indexed in 22.237 s and answered exact error counts in under a millisecond. Large volumes are a different question."
  - q: "How much memory does log indexing use?"
    a: "About 200x the corpus size. The measured ladder reached 2,755.9 MB peak resident memory after 28 MB of cumulative log data on one node."
  - q: "How large a log corpus can XERJ handle?"
    a: "This capture measured up to 16 MB per step and stopped there for memory headroom. Plan for corpora up to a few million documents, and measure your own host."
  - q: "Are XERJ log counts exact or estimated?"
    a: "Exact. At every ladder step the level=ERROR document count matched the generator's own count exactly: 424, 848 and 1,696."
  - q: "How fast are queries over an indexed log corpus?"
    a: "On the 16 MB corpus, p50 was 0.488 ms for an error filter, 0.536 ms for a terms aggregation and 0.676 ms for a date range with an aggregation."
  - q: "Does a match query on the text field find log lines?"
    a: "No. The logs family writes ts, level and message fields with no generic text field, and message is a keyword. Query message or level instead."
  - q: "How much disk does an indexed log corpus need?"
    a: "The 3 measured steps stored 1.045, 1.319 and 1.039 bytes per source byte. The ratio is not stable, so do not publish a single storage multiplier."
---

**TL;DR** — XERJ indexes plain-text logs on one node, returns exact error counts, and answered a `level=ERROR` filter at p50 0.488 ms on a 16 MB corpus. Memory is the limit, not speed: peak resident memory grew to about 200× the corpus, and the measured ladder stopped at 16 MB.

## What was actually measured

One single-node XERJ process indexed 3 log corpora, cold, one step at a time. Every step recorded documents, exact error counts, wall time, on-disk bytes, peak resident memory and the host load average.

| Corpus | Documents | ERROR documents | Wall time | On-disk bytes | Peak RSS | Load |
| --- | --- | --- | --- | --- | --- | --- |
| 4 MB | 41,116 | 424 | 7.558 s | 4,383,772 | 895.2 MB | 2.99 |
| 8 MB | 82,231 | 848 | 12.198 s | 11,064,437 | 1,828.7 MB | 3.00 |
| 16 MB | 164,441 | 1,696 | 22.237 s | 17,437,773 | 2,755.9 MB | 2.51 |

Read the peak-RSS column cumulatively. `VmHWM` is a high-water mark that never decreases. The 2,755.9 MB value therefore covers the whole 28 MB of log data the node had seen by then.

## The counts are exact, at every step

The expected error count came from the fixture generator, not from the engine. The 2 numbers matched at every step: 424, 848 and 1,696. Exact counting is the reason to index logs rather than sample them.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/log16-*/_search' -H 'content-type: application/json' -d '{"size":0,"query":{"bool":{"filter":[{"term":{"level":"ERROR"}}]}},"track_total_hits":true}'
```

## Query latency on the 16 MB corpus

3 query shapes were timed against the finished 16 MB index, 30 runs each, with 5 warmup runs discarded. The host sat at 1-minute load average 2.51 before and after.

| Query | Hits | p50 | p95 |
| --- | --- | --- | --- |
| `level=ERROR` filter | 1,696 | 0.488 ms | 0.772 ms |
| `service` terms aggregation | 10,000 | 0.536 ms | 0.941 ms |
| Date range with `host` aggregation | 64,603 | 0.676 ms | 1.12 ms |

This is a repeated-query observation on one fixed corpus on one shared host. The numbers are not a benchmark and do not transfer to another machine.

## Memory is the real limit

Indexing plain-text logs costs about 200× the corpus size in resident memory. Memory, not speed, is the operational headline of this capture.

The harness derived a headroom rule from the ladder's own growth, `2000 + 200 × corpus MB` in MiB. The harness then refused the 32 MB step, which needed about 8,400 MiB against 6,104 MiB available.

Two earlier attempts ran the 64 MB step without that rule and lost the node process.

```text
xerj-done ok=false exit=1 reason=aborted wall=416.4s
error: write catalog: bulk send (request timeout 300s): ... tcp connect error: Connection refused (os error 111)
```

`Connection refused` means the node had stopped listening. No `dmesg` access was available on that host, so the cause is not confirmed and this page does not call it an out-of-memory kill.

## No 1 GB figure, on purpose

This capture deliberately did not attempt a 1 GB corpus, and no 1 GB number appears on this page. The server has an open defect that retains heap per indexed document. A 16 MB ladder extrapolated to 1 GB produces a number with no measurement behind it.

Plan corpora up to a few million documents on one node, and run the ladder yourself before you commit to a size.

## Disk cost, and why there is no single multiplier

The 3 steps stored 1.045, 1.319 and 1.039 bytes on disk per source byte. Background segment merging had not settled at the 8 MB step, which is why the middle row is higher. Do not publish a single storage multiplier from these 3 rows.

Read the real size from the plain statistics endpoint, because `_cat/indices?bytes=b` still returns human-formatted values and `/_stats/store` returns 404.

```sh
curl -s http://127.0.0.1:9200/log16-logs/_stats
```

## Field names that surprise Elasticsearch users

The `logs` family parses each line into `ts`, `level` and `message`. There is no generic `text` field, and `message` is a `keyword`, so a habitual `{"match": {"text": "timeout"}}` returns 0 hits on a log index.

XERJ also writes one document per line plus one document per file, which is why 41,115 lines produced 41,116 documents.

## What this is not

Log analytics here runs on the ordinary segment write path and generic aggregations. The dedicated `xerj-logs` module has zero call sites and is not wired into the engine, so no page can sell it as a shipped feature.

XERJ is single-node. There is no data-plane replication, no failover and no multi-region mode. A log index on XERJ therefore needs a snapshot and restore plan.
