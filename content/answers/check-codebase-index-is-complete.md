---
title: "How do I check my code index actually finished?"
h1: "My codebase indexer says indexed but I don't see my code. How do I check it actually finished?"
description: "Compare 4 numbers: files on disk, the dry-run plan, autoindex status, and the index count. A captured run cut 64 files to a 3-file plan, naming a rule per gap."
slug: "check-codebase-index-is-complete"
cluster: "Operations: completeness"
question: "My codebase indexer says indexed but I don't see my code. How do I check it actually finished?"
intent: "troubleshooting"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, count the files under the target folder, run xerj autoindex with --dry-run, run it for real, then reconcile the dry-run file count with xerj autoindex status and with a _cat/indices count before you report the codebase as indexed."
commands:
  - cmd: "xerj autoindex ./my-repo --url http://127.0.0.1:9200 --prefix comp --state-dir ./state-comp --dry-run"
    note: "Print the plan and the ignore accounting without writing anything."
  - cmd: "xerj autoindex ./my-repo --url http://127.0.0.1:9200 --prefix comp --state-dir ./state-comp"
    note: "Index the folder and print the final document counts."
  - cmd: "xerj autoindex status --url http://127.0.0.1:9200 --state-dir ./state-comp"
    note: "Read the journal state and the live indices for this run."
links_out:
  - "index-monorepo-for-agent"
  - "catalog-files-with-autoindex-map"
  - "read-autoindex-progress"
faq:
  - q: "My codebase indexer says indexed but I don't see my code. How do I check it actually finished?"
    a: "Reconcile 4 numbers: files on disk, the dry-run plan, the autoindex status journal, and the index document count. A gap between the first two is always an ignore rule."
  - q: "How can I tell if folder indexing is still running or if it died?"
    a: "Run xerj autoindex status with the same --state-dir. The journal reports files done, records and its state; the captured output read 3 files done and FINISHED."
  - q: "How do I verify a local code index actually contains my files?"
    a: "Query for a symbol you know exists. The captured query on the defs field for merge_segments returned 2 hits, src/lib.rs in Rust and src/ingest.py in Python, each with its path."
  - q: "The indexer exited. Did it finish or just stop?"
    a: "Read the terminal line and then the journal. Exit 0 with reason=completed means nothing was refused, and exit 3 with reason=completed-with-junk means the run finished and refused at least one file."
  - q: "Why does the index hold more documents than files?"
    a: "XERJ writes one document per extracted row, and for source files it adds one document per declaration on top of the whole-file document. The document count therefore follows how many declarations your code holds, not a fixed multiple of the file count, so read it from your own run."
  - q: "Why does the plan hold fewer files than the folder?"
    a: "Ignore rules pruned them. The captured dry run named target/, node_modules/ and a .gitignore entry, with an exact count of files inside each."
  - q: "Does _cat/indices report the index size in bytes?"
    a: "No. Even with bytes=b it returned human-formatted values such as 58.5kb, so read GET /{index}/_stats and use _all.total.store.size_in_bytes."
---

**TL;DR** — Reconcile 4 numbers before you trust an "indexed" status: files on disk, the `--dry-run` plan, `xerj autoindex status`, and the document count in `_cat/indices`. A captured XERJ run turned 64 non-hidden files into a 3-file plan, and every missing file had a named ignore rule.

## The 4 numbers that must agree

A completeness check is arithmetic, not a status word. Count the files on disk, read the planned file count, read the journal state, then read the index count, and account for every difference.

| Number | Where it comes from | Captured value |
| --- | --- | --- |
| Files on disk | `find . -type f` | 65 total, 64 non-hidden |
| Files in the plan | `autoindex --dry-run` | 3 |
| Files done | `xerj autoindex status` | 3 files done, `FINISHED` |
| Documents live | `_cat/indices` | set by the emission model below, not by the file count |

The gap between 64 and 3 is the interesting one, and `autoindex` explains it rather than hiding it.

## Read the plan before the run

A dry run prints the plan and the ignore accounting, and writes nothing to the node. Run it first on any folder that later looks under-indexed.

```sh
xerj autoindex ./my-repo --url http://127.0.0.1:9200 --prefix comp --state-dir ./state-comp --dry-run
```

The captured dry run named every exclusion with an exact file count.

```text
autoindex: 3 files (0 MB) under .../fixtures/code
autoindex: ignore rules: skipped 2 files and pruned 3 directories (60 non-hidden files inside them); 1 ignore file read
autoindex: ignore rules:   <built-in>:target/ — 1 directory pruned (30 non-hidden files inside)
autoindex: ignore rules:   <built-in>:node_modules/ — 1 directory pruned (25 non-hidden files inside)
autoindex: ignore rules:   .gitignore:coverage/ — 1 directory pruned (5 non-hidden files inside)
```

3 planned files plus 2 skipped files plus 60 files inside pruned directories accounts for 65 paths. Every exclusion carries a rule name and a count.

## Documents outnumber files, and that is normal

XERJ writes one document per extracted row, and for most families that includes a whole-file document alongside the rows. Source code goes one step further. For each source file XERJ writes the file document, carrying the full text and a `defs` list of everything the file declares. It then writes one more document for every declaration it found, keyed by a `code:<line>:<name>` locator. That is what makes a constant or a one-line signature retrievable on its own, instead of only inside the class or method enclosing it. A declaration captured twice at the same line and name collapses to a single document.

The document count for a folder of source files therefore follows the number of declarations in it. No fixed ratio to the file count exists to carry over from another run. Read the count from `_cat/indices` for your own run, and reconcile it against the count `xerj autoindex status` prints.

State the convention whenever you quote a count, because a reader comparing a file count with a much larger document count will otherwise assume an error.

## Ask the journal, not your memory

`xerj autoindex status` reads the state directory the run wrote and prints both the journal state and the live indices.

```sh
xerj autoindex status --url http://127.0.0.1:9200 --state-dir ./state-comp
```

The journal prints as one line: the journal path, the root it indexed, how many files are done, how many documents were written, and either `FINISHED` or `in progress`. A run that wrote a graph adds an indented `graph:` line naming the edge count, the edges index and the brain. Below that, `status` lists every live index carrying your prefix with its document count, read from the node rather than from the journal.

Compare those two sides rather than trusting `FINISHED` on its own. The journal says what the run believed it wrote; the live index list says what the node actually holds.

## Finish with a query you can predict

A count proves quantity; a query proves retrievability. Pick a symbol you know exists in the tree and search the `defs` field for it.

```sh
curl -s -XPOST http://127.0.0.1:9200/comp-*/_search -H 'content-type: application/json' -d '{"query":{"match":{"defs":"merge_segments"}},"size":10,"_source":["ax_path","language","defs"],"track_total_hits":true}'
```

The captured response returned 2 hits, `src/lib.rs` in Rust and `src/ingest.py` in Python, each with its full `defs` list.

## Two size checks that fail on XERJ

An Elasticsearch user checking index size reaches for 2 forms that fail here. `GET /_cat/indices?format=json&bytes=b` still returned `58.5kb`, not a raw byte count. `GET /{index}/_stats/store` returned 404.

Use the plain statistics form instead, then read `_all.total.store.size_in_bytes`.

```sh
curl -s http://127.0.0.1:9200/comp-docs/_stats
```

## One more surprise in `_cat/indices`

`autoindex` creates one `.xerj-memory-<brain>-edges` index per indexed folder, named after the folder. A first look at `_cat/indices` therefore shows more indices than folders you indexed. XERJ is single-node, so all of them live in one process.
