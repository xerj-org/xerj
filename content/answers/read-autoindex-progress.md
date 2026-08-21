---
title: "How do I read autoindex progress?"
h1: "How do I read autoindex progress?"
description: "XERJ prints xerj-bar, xerj-progress and a final xerj-done line. On a single large file the percentage stays at 0.0%, so read since_progress_s and waiting_on."
slug: "read-autoindex-progress"
cluster: "Operations: progress"
question: "How can I see whether folder indexing is still working?"
intent: "how-to"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run xerj autoindex with --progress plain, parse the xerj-progress lines on stderr into phase, pct, since_progress_s and waiting_on fields, and report the terminal xerj-done line rather than guessing from the bar."
commands:
  - cmd: "xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix log16 --state-dir ./state-log16 --progress plain"
    note: "Print machine-readable progress lines on stderr for the whole run."
  - cmd: "xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix gate --state-dir ./state-gate --max-minutes 1 --quiet"
    note: "Print the decision JSON on stdout instead; this cannot be combined with progress output."
  - cmd: "xerj autoindex status --url http://127.0.0.1:9200 --state-dir ./state-log16"
    note: "Read the journal state after the run finishes."
links_out:
  - "check-codebase-index-is-complete"
  - "resume-interrupted-autoindex-run"
  - "estimate-autoindex-time-before-running"
faq:
  - q: "How do I know autoindex is still working?"
    a: "Read the xerj-progress line on stderr. The elapsed_s and since_progress_s fields advance even when the percentage does not, and waiting_on names the current file."
  - q: "Why does autoindex progress stay at 0%?"
    a: "The percentage counts items, and one large file is one item. The captured 16 MB run held 0.0% with items=0/1 for 15 seconds while it indexed normally."
  - q: "Which phases does autoindex report?"
    a: "The capture recorded 12 in order: walk, hash, scan, prepare, graph, index, graph-corpus, finalize-refresh, finalize-count, finalize-correlate, finalize-histogram and finalize-catalog."
  - q: "How do I parse autoindex progress in a script?"
    a: "Pass --progress plain and read the xerj-progress lines from stderr. Each line is a flat set of key=value pairs with no colors and no cursor control."
  - q: "What does the final autoindex line say?"
    a: "The xerj-done line carries ok, exit, reason, wall, files, records, datasets and junk_files. The captured run ended ok=true exit=0 reason=completed."
  - q: "Can I combine --quiet with --progress plain?"
    a: "No. --quiet means no progress output, so the decision-JSON recipe and the progress-parsing recipe are separate invocations of autoindex."
---

**TL;DR** — Pass `--progress plain`, then read the `xerj-progress` lines on stderr. A captured 22.2 s XERJ run emitted 12 phases in order and ended with `xerj-done ok=true exit=0 reason=completed`. On a single 16 MB file the percentage stayed at `0.0%`, so read `since_progress_s` and `waiting_on` instead.

## Three kinds of line, one contract

`autoindex` prints 3 shapes on stderr, and each answers a different question. Pass `--progress plain` to get all 3 without terminal control codes.

| Line | What it is for |
| --- | --- |
| `xerj-bar` | A human-readable bar with the phase and the current file |
| `xerj-progress` | Flat `key=value` pairs for a parser |
| `xerj-done` | One terminal line with the exit reason and the totals |

```sh
xerj autoindex ./logs --url http://127.0.0.1:9200 --prefix log16 --state-dir ./state-log16 --progress plain
```

## The fields that move when the bar does not

A captured `xerj-progress` line carries the phase, the basis, the percentage, item and byte counters, a rate, an estimate, and 3 clocks.

```text
xerj-progress phase=index basis=bytes pct=0.0 items=0/1 bytes=0/16777292 rate=unknown eta_s=unknown eta_quality=unknown since_progress_s=9.8 phase_elapsed_s=9.8 elapsed_s=10.0 waiting_on=service-00.log(16.0MB)
```

`elapsed_s` and `since_progress_s` advance on every line. A live run therefore differs from a hung one on the clock alone. `waiting_on` names the file the run is inside.

## Why the percentage can sit at zero

The captured 16 MB run held `pct=0.0` with `items=0/1` for the whole index phase, and it was healthy the entire time. One large file is one item, so the item counter cannot move until that file completes.

The 3 index-phase lines in the capture show `since_progress_s` at 4.8, then 9.8, then 14.8, against `elapsed_s` of 5.0, 10.0 and 15.0. A percentage of 0.0 next to a rising `elapsed_s` means work in flight, not a stall.

Split a large corpus into several files if you want the percentage to move.

## The 12 phases, in the captured order

A run passes through fixed phases, and the capture recorded all of them in this order.

1. `walk`, `hash`, `scan` — find and fingerprint the files.
2. `prepare`, `graph` — infer datasets and prepare relationship detection.
3. `index`, `graph-corpus` — write documents and edges to the node.
4. `finalize-refresh`, `finalize-count`, `finalize-correlate`, `finalize-histogram`, `finalize-catalog` — settle and catalog the result.

A run that sits in `finalize-count` waits on the node, not on the disk. The captured line names the index it waits for, as `waiting_on=log16-logs`.

## The line that ends the run

`xerj-done` is the only line a script must parse to decide success. The captured terminal line reads in full:

```text
xerj-done ok=true exit=0 reason=completed wall=22.2s files=1 records=164441 datasets=1 junk_files=0
```

`reason` distinguishes `completed`, `dry-run`, `completed-with-junk` and `aborted`, and the exit code follows it. Exit 3 with `completed-with-junk` means the run refused some files, and the catalog holds a reason for each one.

## Progress and the decision gate are separate runs

`--quiet` means no progress output, so it cannot share an invocation with `--progress plain`. The capture ran the 2 recipes as separate commands for that reason.

Use `--progress plain` to follow a run. Use `--quiet` with `--max-minutes` to get the machine-readable decision JSON on stdout.
