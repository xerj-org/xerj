---
title: "The indexer died overnight. Do I start over?"
h1: "The indexer died overnight. Do I have to start over?"
description: "XERJ keeps a resume journal in the state directory, so re-running autoindex is idempotent. A capture reached 82,231 documents and matched a clean control run."
slug: "resume-interrupted-autoindex-run"
cluster: "Operations: recovery"
question: "The indexer died overnight. Do I have to start over?"
intent: "troubleshooting"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run xerj autoindex against a folder with an explicit --state-dir, interrupt it, then re-run the identical command with the same --state-dir and compare the final _count and the first 20 document ids with a clean control run into a fresh prefix."
commands:
  - cmd: "xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix sig --state-dir ./state-sig --progress plain"
    note: "Start the run with an explicit state directory so it can resume."
  - cmd: "xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix sig --state-dir ./state-sig --progress plain"
    note: "Re-run the identical command after an interruption to resume from the journal."
  - cmd: "curl -s 'http://127.0.0.1:9200/sig-*/_count'"
    note: "Count the documents the run left behind."
links_out:
  - "read-autoindex-progress"
  - "estimate-autoindex-time-before-running"
  - "catalog-files-with-autoindex-map"
faq:
  - q: "The indexer died overnight. Do I have to start over?"
    a: "No. Re-run the identical command with the same --state-dir. XERJ reads the resume journal, prints how many files it already holds, and indexes only what remains."
  - q: "What happens if folder indexing stops halfway through? Can I resume it?"
    a: "The finished files stay indexed and the journal in --state-dir records them. In the capture the re-run submitted 0 records and the document count was unchanged."
  - q: "I killed the search indexer. Is the work lost?"
    a: "Not the part the journal already recorded. The interrupted run, the resumed run and a clean control run all ended at 82,231 documents on the same corpus."
  - q: "Will restarting an indexer duplicate documents?"
    a: "No. Document ids are derived from content, so the resumed run and a clean control run produced identical first-20 ids and identical totals of 82,231."
  - q: "What happens if I lose the state directory?"
    a: "The re-run starts from the beginning and re-reads every file. Ids stay stable, so the index converges to the same content rather than doubling."
  - q: "Does SIGINT stop autoindex immediately?"
    a: "Not in this capture. The signal arrived 6 s into a 12.0 s run and the client still reported reason=completed, so a mid-file abort was not demonstrated."
  - q: "What happens if the node stops instead of the client?"
    a: "The client reports xerj-done ok=false exit=1 reason=aborted and a connection error. Re-run with the same --state-dir once the node is back."
---

**TL;DR** — XERJ writes a resume journal into the `--state-dir`, so re-running the identical `autoindex` command is idempotent. In a capture the interrupted run, the resumed run and a clean control run all ended at 82,231 documents. The first 20 document ids matched exactly.

## The state directory carries the recovery

`autoindex` writes every finished file into a journal under `--state-dir`. The next run reads that journal before it plans any work. Pass an explicit `--state-dir` on the first run, because recovery depends on the same directory.

```sh
xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix sig --state-dir ./state-sig --progress plain
```

The resumed run says what it found, on its first lines.

```text
resuming from journal ./state-sig/journal.ndjson (1 files already done)
estimate: no estimate — nothing to index
xerj-done ok=true exit=0 reason=completed wall=1.2s files=0 records=82231
```

## What the capture actually observed

The harness sent `SIGINT` 6 s into an 8 MB log-indexing run. The client did not abandon the work: it printed `xerj-done ok=true exit=0 reason=completed wall=12.0s files=1 records=82231` and exited 0.

State that plainly, because it bounds the claim. This capture proves that a re-run is safe and idempotent. This capture does not prove a mid-file abort, and no page can claim one.

## The convergence proof

The capture compared 3 runs over the same 8 MB corpus. The 3 were the interrupted run, the resume against the same `--state-dir`, and a clean control run into a fresh prefix.

| Run | Documents | Notes |
| --- | --- | --- |
| After the interrupt | 82,231 | Exit code 0 |
| After the resume | 82,231 | 0 source rows submitted, 1.2 s wall |
| Clean control, fresh prefix | 82,231 | Independent state directory |

The first 20 document ids sorted ascending are identical between the resumed run and the clean control run, beginning `000012965601f7179a683a4f8e346dc4`. Identical ids are the strong result here, because equal counts alone leave different content possible.

## Why re-running is safe

XERJ derives each document id from content rather than from arrival order. A second pass over the same file therefore rewrites the same ids, which replaces documents instead of adding them.

Losing the state directory costs time, not correctness. The run re-reads every file, and the index converges on the same document set.

## Prove it on your own corpus

Run the same checks the capture ran. Compare a resumed index with a control index in a fresh prefix.

```sh
curl -s 'http://127.0.0.1:9200/sig-*/_count'
```

Then read the first ids from both indices and compare them.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/sig-*/_search' -H 'content-type: application/json' -d '{"query":{"match_all":{}},"size":20,"sort":[{"_id":"asc"}],"_source":false}'
```

## When the node goes away instead

A stopped node produces a different terminal line, and an earlier capture preserves one. The client reported `xerj-done ok=false exit=1 reason=aborted wall=416.4s` and a `Connection refused` error against `/_bulk`, which means the node was no longer listening.

XERJ is single-node, so there is no failover and no second node to take the write. Restart the node, then re-run the identical `autoindex` command with the same `--state-dir`.
