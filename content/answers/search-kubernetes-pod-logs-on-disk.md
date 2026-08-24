---
title: "How do I search Kubernetes pod logs?"
h1: "How do I search Kubernetes pod logs?"
description: "Collect the pod logs to disk first, then index the folder. A captured run answered 1 phrase query across 3 pods and returned 18 hits, 6 per pod."
slug: "search-kubernetes-pod-logs-on-disk"
cluster: "Files and formats"
question: "Grep for specific text from kubernetes multiple pods"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, copy or stream the pod logs you care about into a local folder that keeps the var/log/pods/<namespace>_<pod>_<uid>/<container>/0.log layout, start a local XERJ node, run `xerj autoindex ./pod-logs --url http://127.0.0.1:9200 --prefix pl --progress plain`, POST a match_phrase on body with size 100, and report the per-file hit counts read from the returned hits rather than from an aggregation."
commands:
  - cmd: "xerj autoindex ./pod-logs --url http://127.0.0.1:9200 --prefix pl --progress plain"
    note: "Index a folder of already-collected pod log files from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/pl-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"tapir connection reset\"}},\"size\":100,\"_source\":[\"ax_path\",\"ax_locator\"],\"track_total_hits\":true}'"
    note: "Run 1 phrase query across every pod, and read the spread from the hits."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"doc_kind\":\"file\"}}],\"must\":[{\"wildcard\":{\"path\":\"*var/log/pods*\"}}]}},\"size\":10,\"_source\":[\"path\",\"format\",\"status\",\"records\"],\"sort\":[{\"path\":\"asc\"}]}'"
    note: "Read the family XERJ gave each log file, and the document count per file."
links_out:
  - "search-json-and-jsonl-logs"
  - "search-gzip-logs-without-zgrep"
  - "cheap-low-volume-log-search"
faq:
  - q: "Can 1 query search logs from several pods at once?"
    a: "Yes. In the captured run 1 `match_phrase` request returned 18 hits spread 6, 6 and 6 across 3 pod log files, with the file path on every hit."
  - q: "Does XERJ collect logs from a cluster?"
    a: "No. XERJ is not a log collector and not a log shipper. Copy or stream the logs to disk first with `kubectl logs`, a DaemonSet or your existing agent."
  - q: "Does XERJ parse the timestamp and level from a pod log?"
    a: "Not for the CRI format. All 3 files were detected as `txt-prose`, so no `ts`, `level` or `message` field was produced. The line text is searchable as `body`."
  - q: "How do I count matches per pod?"
    a: "Read the counts from the returned hits with a large `size`, then group on `ax_path` in your client. The captured run did that and got 6 per file."
  - q: "Did the aggregation agree with the hits in this run?"
    a: "Yes. The terms aggregation on the same request reported 6 documents per file, the same as the hits. Compare both before you publish a count."
  - q: "What if I convert the logs to JSONL first?"
    a: "Keep every line under 4,096 characters. A captured reproduction indexed 30 documents at 4,095 characters per line and 0 documents at 4,096."
  - q: "Where do these results come from?"
    a: "All results come from run RUN-B, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host."
---

**TL;DR** — Collect the pod logs to local disk first, then run `xerj autoindex` on the folder. In a captured run, 1 `match_phrase` request returned 18 hits spread 6, 6 and 6 across 3 pod log files. XERJ is not the collector, and the CRI format was read as prose text.

## Get the logs onto disk

XERJ indexes files that already exist on a filesystem. Use `kubectl logs`, a node-level DaemonSet or your existing agent to write the pod logs into a folder, then index that folder.

The captured fixture kept the kubelet layout, `var/log/pods/<namespace>_<pod>_<uid>/<container>/0.log`, and the CRI line format, `<RFC3339Nano> <stdout|stderr> <F|P> <message>`. Each of the 3 files held 240 lines.

```sh
xerj autoindex ./pod-logs --url http://127.0.0.1:9200 --prefix pl --progress plain
```

The run read 3 files into 1 dataset and 57 documents live in `pl-docs`, with 0 junk files. Each log file produced 18 documents.

## One query covers every pod

A single phrase query across `/pl-*/_search` returned 18 hits. The hits named all 3 files, 6 hits each, and each hit carried the full pod path.

| pod log file | hits |
| --- | --- |
| `var/log/pods/default_checkout-7d9f8b6c4-2xk7l_.../checkout/0.log` | 6 |
| `var/log/pods/default_billing-5c8d7a9b3-qq4mz_.../billing/0.log` | 6 |
| `var/log/pods/payments_ledger-6b4c9d2e8-h7t3v_.../ledger/0.log` | 6 |

```sh
curl -s -XPOST 'http://127.0.0.1:9200/pl-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"tapir connection reset"}},"size":100,"_source":["ax_path","ax_locator"],"track_total_hits":true}'
```

Read the per-pod spread from the returned hits. Set `size` large enough to hold every hit. The terms aggregation on the same request also reported 6 per file, but no general rule follows from 1 agreement.

## The CRI format is read as prose, not as logs

All 3 files were detected as `txt-prose` in the `autoindex-catalog`. The `logs` family did not fire on this format, so no `ts`, `level` or `message` field was produced.

Plan for that. The line text is searchable through `body`, so a full-text query for `level=error` finds the lines that hold that string. No typed level filter and no date range query exist on this index.

If you want typed fields, convert the lines to JSON or JSONL before you index them. Keep every JSONL line under 4,096 characters. A captured reproduction indexed 30 documents at 4,095 characters per line and 0 at 4,096, with the family flipping from `jsonl` to `json`.

## What XERJ is not doing here

XERJ is not a log collector, not a log shipper and not a cluster component. The node fetched nothing over the network and observed 0 distinct non-loopback peers across 185 samples over its whole life.

No container runtime and no cluster existed on the capture host. The 3 log files use the real kubelet path layout and the real CRI line format, but the fixture generator wrote them. The claim under test was reading such files from disk after you gather them, and that claim holds.

## What this capture does not tell you about scale

The captured run indexed 3 files and 720 log lines on 1 host. That size demonstrates the cross-pod query and the family assignment, not log-scale throughput. XERJ runs single-node here, with no replication and no failover.

Full-text ranking on `body` uses BM25. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-B, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
