---
title: "How do I estimate folder-indexing time?"
h1: "How do I estimate folder-indexing time?"
description: "XERJ measures a client-side extraction floor during a dry run and can stop at a decision gate with exit 4. On 2,500 PDFs the floor was 2.2 min, the run 157.7 s."
slug: "estimate-autoindex-time-before-running"
cluster: "Operations: estimation"
question: "How long will it take to index a folder for search?"
intent: "cost"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run xerj autoindex with --dry-run to read the measured extraction floor, then re-run with --max-minutes and --quiet so the decision gate returns exit 4 and a machine-readable decision JSON on stdout before any document is written."
commands:
  - cmd: "xerj autoindex ./mixed --url http://127.0.0.1:9200 --prefix est --state-dir ./state-est --dry-run"
    note: "Print the file count, the measured extraction floor and the work order."
  - cmd: "xerj autoindex ./pdfs --url http://127.0.0.1:9200 --prefix gate2 --state-dir ./state-gate2 --pdf-workers 1 --max-minutes 1 --quiet"
    note: "Stop at the decision gate and print the decision JSON on stdout with exit 4."
  - cmd: "xerj autoindex ./pdfs --url http://127.0.0.1:9200 --prefix gate2 --state-dir ./state-gate2 --pdf-workers 1 --max-minutes 1 --progress plain --yes"
    note: "Answer the gate and run the same job to completion."
links_out:
  - "search-file-contents-in-a-folder"
  - "resume-interrupted-autoindex-run"
  - "read-autoindex-progress"
faq:
  - q: "How do I estimate how long indexing will take?"
    a: "Run xerj autoindex with --dry-run. XERJ reads a sample of the files end to end and prints a measured client-side extraction floor for that machine."
  - q: "Is the autoindex estimate a prediction?"
    a: "No. The estimate is a floor for client-side extraction only. It excludes server indexing, embedding, network round trips and relationship detection, and the run is always longer."
  - q: "How far off was the measured floor?"
    a: "On 2,500 PDFs the floor was 134.5 s to 135.0 s and the finished run took 157.7 s. The floor under-predicted the run by about 23 seconds."
  - q: "What is the autoindex decision gate?"
    a: "A pre-flight stop. If the measured floor exceeds --max-minutes, autoindex exits 4 with a decision JSON and writes nothing to the node."
  - q: "How do I answer the decision gate?"
    a: "Re-run the identical command with --approve proceed, fast, or cancel. --yes is an alias for --approve proceed, and the captured re-run exited 0."
  - q: "Why does the dry run say 0.0 s for a small folder?"
    a: "Because the measured extraction floor for 6 small files rounds to zero. The gate did not trigger, and the real run still took 1.5 s including server time."
---

**TL;DR** — `xerj autoindex --dry-run` prints a measured extraction floor for your own machine, and it writes nothing. Add `--max-minutes` and `--quiet`, and the decision gate exits 4 with a JSON decision request. On 2,500 PDFs the floor was 2.2 min and the finished run took 157.7 s.

## The estimate is a floor, not a forecast

`autoindex` reads a sample of the target files end to end during phase A, then prints a measured floor for client-side extraction. The line says so in its own words, and the wording matters more than the number.

```text
estimate: at least 0.0 s–0.0 s — a MEASURED FLOOR for client-side extraction, not a prediction of the whole run: server indexing, embedding and network time are not in it
```

XERJ lists what the floor leaves out. The exclusions are server-side indexing and merge time, network round trips, relationship detection, and any unmeasured file family.

## Read the basis line, not only the number

Every estimate carries a basis line that states how much of the job XERJ actually measured. Coverage under 100% means the floor rests on a sample rather than on the whole folder.

```text
estimate: basis — measured on this machine during phase A: 6 file(s) / 1.8 KB read end to end across 6 family/families, scheduled over 10 worker(s). Covers 1.8 KB of the 1.8 KB planned (100% of planned bytes). Client-side extraction only.
```

A family with no end-to-end read appears under `unmeasured_families` and carries no time at all. That omission is the main way a floor understates a job.

## How far the floor sat from the run

Two captured pairs show the gap between the floor and the finished run. The floor is honest about direction: it is always low.

| Corpus | Measured floor | Actual wall time |
| --- | --- | --- |
| 6 mixed files, 1.8 KB | 0.0 s to 0.0 s | 1.5 s |
| 2,500 PDFs, 1 PDF worker | 134.5 s to 135.0 s | 157.7 s |

The PDF job finished about 23 seconds above its floor. Use the floor as a lower bound and a trigger, never as a completion time you can promise anyone.

## The decision gate stops before it writes

Pass `--max-minutes` and the gate compares the measured floor with your budget. When the floor exceeds the budget, `autoindex` exits 4 and writes nothing to the node.

```sh
xerj autoindex ./pdfs --url http://127.0.0.1:9200 --prefix gate2 --state-dir ./state-gate2 --pdf-workers 1 --max-minutes 1 --quiet
```

The captured reason names both sides of the comparison.

```text
the measured extraction floor alone is 2.2 min–2.3 min; its upper end 2.3 min already exceeds --max-minutes 1. The real run is longer than this — server, network and embedding time are not in the number
```

## The decision JSON is built for an agent

With `--quiet` the gate writes one JSON object to stdout and exits 4. The object carries the estimate, the priority order, the heaviest directories and 4 options.

| Option | What it does |
| --- | --- |
| `proceed` | Index everything as planned; `--yes` is an alias |
| `fast` | Adds `--no-semantic --no-graph`, keeping typed BM25 and keyword indices |
| `narrower` | Re-run against a subdirectory; `autoindex` has no `--exclude` flag |
| `cancel` | Index nothing and exit 0 |

The `fast` option states its own limit in the capture. The default embedder is lexical feature hashing, so the saving stays small unless the node runs `--embed-mode neural`.

## Answering the gate

Re-run the identical command with an answer. The captured re-run with `--yes` exited 0 and indexed 2,500 files into 5,000 documents in 157.7 s.

```sh
xerj autoindex ./pdfs --url http://127.0.0.1:9200 --prefix gate2 --state-dir ./state-gate2 --pdf-workers 1 --max-minutes 1 --progress plain --yes
```

An unanswered exit 4 writes nothing, so the node stays untouched until you answer the gate.

## What the estimate cannot see

The floor measures your client, not the node. XERJ is single-node, so server indexing, merging and embedding queue behind one process on one host. None of that time appears in the floor.

Host load changes the answer too. The captured PDF run held 1 PDF worker because the memory safe zone allowed only 1. A host with more free memory schedules more workers.
