---
title: "Search JSON logs and gzip logs in one folder"
h1: "What's the easiest way to search JSON logs plus some old gzip text logs in the same folder?"
description: "xerj autoindex detects JSON arrays, JSONL logs and gzipped logs in one folder, infers the date field and answers exact filters. One run returned 386 ERROR hits."
slug: "search-json-and-jsonl-logs"
cluster: "Files and formats"
question: "What's the easiest way to search JSON logs plus some old gzip text logs in the same folder?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./json-logs --url http://127.0.0.1:9200 --prefix jlog --progress plain`, GET /jlog-*/_mapping to find which field XERJ inferred as a date, then POST a filtered search for level=ERROR and a terms aggregation on service, and report the hit total and the bucket counts."
commands:
  - cmd: "xerj autoindex ./json-logs --url http://127.0.0.1:9200 --prefix jlog --progress plain"
    note: "Index a folder holding both JSON-array and JSONL log files."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/jlog-*/_mapping"
    note: "Read the inferred mapping, including the date field and the keyword fields."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jlog-*/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"level\":\"ERROR\"}}]}},\"size\":3,\"_source\":[\"level\",\"service\",\"message\",\"timestamp\",\"ts\"],\"track_total_hits\":true}'"
    note: "Filter both log indices for errors and get an exact total."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jlog-*/_search -H 'content-type: application/json' -d '{\"size\":0,\"aggs\":{\"by_service\":{\"terms\":{\"field\":\"service\",\"size\":20}},\"by_level\":{\"terms\":{\"field\":\"level\",\"size\":20}}}}'"
    note: "Count log lines per service and per level, exactly."
links_out:
  - "search-gzip-logs-without-zgrep"
  - "search-kubernetes-pod-logs-on-disk"
  - "cheap-low-volume-log-search"
  - "search-engine-without-docker"
faq:
  - q: "What's the easiest way to search JSON logs plus some old gzip text logs in the same folder?"
    a: "Index the folder once. Gzip is transparent to `autoindex`, so a `.log.gz` file is read like the plain file beside it, and one index pattern such as `/jlog-*/_search` queries both."
  - q: "How do I search JSON or JSONL log files without grepping blindly?"
    a: "Run `xerj autoindex` on the folder, then send a filtered `_search` request. XERJ maps each JSON key to a typed field, so a `term` filter on `level` works at once."
  - q: "I just want to search logs on my laptop. I don't want Elasticsearch in Docker."
    a: "Yes. XERJ is one native binary that serves the Elasticsearch-compatible port itself, so the log folder is indexed and queried on the host with no container runtime."
  - q: "Does XERJ handle JSONL as well as a JSON array?"
    a: "Yes. XERJ detects both from file content and gives each its own index. The captured run produced `jlog-jsonl` and `jlog-json` from one folder."
  - q: "Does XERJ find the timestamp field by itself?"
    a: "Yes. The captured run inferred `timestamp` as a date in one file and `ts` as a date in the other, with no configuration."
  - q: "Are the error counts exact or estimated?"
    a: "Exact. The captured filter returned 386 hits and the aggregations reported `doc_count_error_upper_bound` 0 and `sum_other_doc_count` 0."
  - q: "Do gzipped log files work the same way?"
    a: "Yes. Gzip is transparent on every parsed family, so a `.jsonl.gz` file indexes like the plain file next to it and lands in the same query pattern."
---

**TL;DR** — `xerj autoindex` detects JSON-array and JSONL log files from their content and gives each its own index. In a captured run over 2 files, XERJ inferred `timestamp` and `ts` as date fields with no configuration. A filter for `level=ERROR` returned exactly 386 hits.

## Index the log folder in one command

`xerj autoindex <folder>` reads both JSON layouts from file content, so the file extension does not decide anything. Each layout becomes its own dataset and its own index.

```sh
xerj autoindex ./json-logs --url http://127.0.0.1:9200 --prefix jlog --progress plain
```

The captured folder held `app.jsonl`, a line-per-event file of 2,000 lines, and `audit.json`, a 500-element JSON array. XERJ produced `jlog-jsonl` with 2,001 documents and `jlog-json` with 501 documents.

## The date field is inferred, not configured

XERJ elected a `date` type for the time field in both files, under two different key names. That inference is what makes a date range query work straight after the run.

| index | date field | other inferred fields |
| --- | --- | --- |
| `jlog-jsonl` | `timestamp` | `level` and `service` as `keyword`, `message` as `text`, `duration_ms` as `long` |
| `jlog-json` | `ts` | `level` and `service` as `keyword`, `message` as `text` |

XERJ recognizes 8 date encodings, including RFC 3339, common log format, RFC 2822, epoch milliseconds and epoch seconds. A guard holds epoch-number guessing to a 1990 to 2100 value window, a floor of 20 distinct values and a span under 20 years. An ordinary integer column therefore stays a `long`.

## Filter both indices for errors

One `term` filter on `level` across `/jlog-*/_search` returned exactly 386 hits. XERJ maps `level` and `service` as `keyword`, so an exact filter needs no analyzer and no wildcard.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/jlog-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"level":"ERROR"}}]}},"size":3,"_source":["level","service","message","timestamp","ts"],"track_total_hits":true}'
```

Pass `track_total_hits` when the number itself is the answer. Without it a client sees the page of hits, and an agent reporting a count must have the total.

## Count by service, exactly

A `terms` aggregation on `service` returned 5 buckets of exactly 500 documents, and a second aggregation split the same corpus by level. XERJ reports `doc_count_error_upper_bound` 0 and `sum_other_doc_count` 0 on both, because every aggregation in XERJ is exact.

```json
{"by_service": {"buckets": [{"key": "auth",      "doc_count": 500},
                            {"key": "billing",   "doc_count": 500},
                            {"key": "checkout",  "doc_count": 500},
                            {"key": "inventory", "doc_count": 500},
                            {"key": "search",    "doc_count": 500}],
                "doc_count_error_upper_bound": 0, "sum_other_doc_count": 0},
 "by_level":   {"buckets": [{"key": "INFO",  "doc_count": 1714},
                            {"key": "WARN",  "doc_count": 400},
                            {"key": "ERROR", "doc_count": 386}],
                "doc_count_error_upper_bound": 0, "sum_other_doc_count": 0}}
```

Exactness matters for a log count. An approximate bucket count makes an error budget or an alert threshold unreliable. XERJ returned the true count on this corpus.

## Compare the totals before you report one

The 2 files held 2,500 log lines, and the run reported 2,502 documents live. The `service` buckets total 2,500, so the 2 extra documents carry no `service` value.

Compare `_count` with the sum of the aggregation buckets whenever the number is the deliverable. The gap is the count of documents that lack the field you grouped on.

## What this capture does not show

This is a single-node run over 2 files and 2,500 log lines on 1 host, so it demonstrates inference and exactness rather than log-scale throughput. XERJ has no replication and no failover in this configuration.

Gzip is transparent on every parsed family, so a compressed log file indexes like the plain file beside it. Full-text search on `message` ranks with BM25. The default embedder in XERJ is lexical feature hashing and cannot connect a query to a synonym; neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from `RUN-A`, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host.
