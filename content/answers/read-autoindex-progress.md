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
  - "autoindex-dataset-refused-by-server"
  - "autoindex-server-back-pressure-429"
evidence:
  - claim: "With --no-graph, a 231-file repository reported 9 phases in order: walk, hash, scan, prepare, snapshot, index, finalize-catalog, finalize-refresh, finalize-verify, with 0 progress lines reading scan at pct=100.0, and ended xerj-done ok=true exit=3 reason=completed-with-junk wall=3.9s files=231 records=1663."
    source: "benchmarks/autoindex-resilience/after-fix.small-repo.stderr.txt"
  - claim: "The same run's index phase read phase=index basis=bytes pct=81.7 items=169/231 bytes=1881018/2303369, 1.0 s into the phase, with eta_s=unknown."
    source: "benchmarks/autoindex-resilience/after-fix.small-repo.stderr.txt"
  - claim: "The terminal bar drew the same 9 phases under a pseudo-terminal, including index at 78.1% with 160/231 items and 1.7MB/2.2MB."
    source: "benchmarks/autoindex-resilience/after-fix.small-repo.tty.txt"
  - claim: "Resuming a full-corpus generation interrupted with 12,890 of 47,444 operations committed reported starting, then replay, then opened index at items=0/34554 bytes=0/805561870, and never reported scan or snapshot."
    source: "benchmarks/autoindex-resilience/after-fix.resume-probe.stderr.txt"
  - claim: "A probe of 60 index refreshes on a node holding 1,526 datasets took 6.36 s, about 106 ms each, which projects to about 160 seconds for all 1,526."
    source: "benchmarks/autoindex-resilience/README.md"
  - claim: "Resuming a full-corpus generation whose 47,444 operations were all applied, on a node restarted onto the same data, finalize-catalog took 49.7 s, finalize-refresh 9.2 s for 1,527 indices and finalize-verify 331.6 s for 47,444 read-backs, and the run ended xerj-done ok=true exit=3 reason=completed-with-junk wall=415.0s."
    source: "benchmarks/autoindex-resilience/after-955.full-corpus-resume.stderr.txt"
  - claim: "On v1.0.0-rc.74 the --no-graph path reported only walk, hash and scan: 48 progress lines read phase=scan pct=100.0 eta_quality=stalled, since_progress_s climbed to 250.0, and the run ended exit=1 aborted wall=270.0s on a 48,533-file corpus."
    source: "benchmarks/autoindex-resilience/before-rc74.stderr.txt"
  - claim: "A full-corpus run aborted at 60.4% of its index phase after 5122.5 s when one bulk came back with 747 items rejected 429 by the node's memory circuit breaker."
    source: "benchmarks/autoindex-resilience/before-944.full-corpus.stderr.txt"
  - claim: "A --no-graph run whose node keeps rejecting for the whole 600 s a bulk waits ends with reason=server-backpressure and exit 1, and its terminal line carries ops_applied and ops_remaining."
    source: "engine/crates/xerj-autoindex/src/sync_executor.rs"
faq:
  - q: "How do I know autoindex is still working?"
    a: "Read the xerj-progress line on stderr. The elapsed_s and since_progress_s fields advance even when the percentage does not, and waiting_on names the current file."
  - q: "Why does autoindex progress stay at 0%?"
    a: "The percentage counts items, and one large file is one item. The captured 16 MB run held 0.0% with items=0/1 for 15 seconds while it indexed normally."
  - q: "Which phases does autoindex report?"
    a: "The default capture recorded 12 in order: walk, hash, scan, prepare, graph, index, graph-corpus, finalize-refresh, finalize-count, finalize-correlate, finalize-histogram and finalize-catalog. With `--no-graph` a capture recorded 9: walk, hash, scan, prepare, snapshot, index, finalize-catalog, finalize-refresh and finalize-verify."
  - q: "The progress says scan 100% and stalled. Is autoindex hung?"
    a: "On a current build, `scan` at 100% means scan, so read `since_progress_s` and `waiting_on`. On v1.0.0-rc.74 and earlier, a `--no-graph` run printed that line for the whole indexing phase while it worked normally. Check `_count` on the node before you stop it."
  - q: "How do I parse autoindex progress in a script?"
    a: "Pass --progress plain and read the xerj-progress lines from stderr. Each line is a flat set of key=value pairs with no colors and no cursor control."
  - q: "What does the final autoindex line say?"
    a: "The xerj-done line carries ok, exit, reason, wall, files, records, datasets and junk_files. The captured run ended ok=true exit=0 reason=completed."
  - q: "What does a `server is shedding load` line mean?"
    a: "The node answered some bulk items with HTTP 429 and the run is re-sending only those after a backoff. `since_progress_s` climbs while it waits; the line names how long the wait has run and when the run gives up. The terminal line then carries `bulk_retries=N`."
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

## The 9 phases of a --no-graph run

`--no-graph` takes a different route. It seals every file into a snapshot first, then publishes from the snapshot. A capture of a 231-file repository recorded 9 phases in this order.

| Phase | What it does | Counted in |
| --- | --- | --- |
| `walk`, `hash`, `scan` | Find, fingerprint and sample the files | files and source bytes |
| `prepare` | Install one mapping per dataset | datasets |
| `snapshot` | Verify, copy and extract every file into a sealed snapshot | source bytes |
| `index` | Send the sealed bulk bytes to the node, one file at a time | sealed bulk bytes |
| `finalize-catalog` | Publish the catalog and read it back | datasets |
| `finalize-refresh` | Refresh every dataset index, so the read-back is exact | indices |
| `finalize-verify` | Read every file's documents back from the node | files |

The `index` phase names the file it is inside and carries a real denominator in both units:

```text
xerj-progress phase=index basis=bytes pct=81.7 items=169/231 bytes=1881018/2303369 rate=1899235.6 eta_s=unknown eta_quality=unknown since_progress_s=0.0 phase_elapsed_s=1.0 elapsed_s=2.0 waiting_on=core/src/stopwords/ori.rs(1.2KB)
```

The byte total of `index` is larger than the byte total of `snapshot`. That is correct. `snapshot` counts source bytes, and `index` counts the extracted NDJSON that is actually sent.

`eta_s` reads `unknown` there because the phase was 1.0 s old. That is the honest answer. The estimate appears once it has settled.

`finalize-refresh` is a phase of its own for a reason. On a node holding 1,526 datasets, a probe of 60 refreshes took 6.36 s, about 106 ms each, while that node was also indexing. That projects to about 160 seconds for all of them. Folded into `finalize-verify`, that time would have held the phase at zero with a climbing `since_progress_s`.

The finalize phases were later timed on the full corpus, on a node restarted onto the same data. Resuming a generation whose 47,444 operations were all applied, `finalize-catalog` took 49.7 s, `finalize-refresh` took 9.2 s for 1,527 indices, and `finalize-verify` took 331.6 s for 47,444 read-backs. Those are one run on a shared machine, not a benchmark, and the refresh time depends on how busy the node is.

A resumed run skips `walk`, `hash`, `scan` and `snapshot`. It reports `replay`, then `index`. Its `index` phase counts only the operations still to apply, so it starts at 0% of what remains. It does not credit this run with an earlier run's writes. In a capture of a full-corpus run interrupted with 12,890 of 47,444 operations committed, the resumed `index` phase opened at `items=0/34554`.

Every run shows `phase=starting` until its first real phase opens. On that resumed run, with a large journal to load, `starting` lasted about 13 seconds.

The `index` phase applies one file at a time, so it is the slow one on a large corpus. Read `eta_s` once `eta_quality` leaves `unknown`.

## scan at 100% now means scan

Through v1.0.0-rc.74 the `--no-graph` route reported only `walk`, `hash` and `scan`. Mapping install, sealing, indexing and the read-back all ran with no phase of their own, so the stream kept describing the scan that had already finished:

```text
xerj-progress phase=scan basis=bytes pct=100.0 items=48533/48533 bytes=520892779/520892779 rate=17050775.5 eta_s=unknown eta_quality=stalled since_progress_s=15.0 phase_elapsed_s=25.7 elapsed_s=30.0
```

The rc.74 capture holds 48 such lines, with `since_progress_s` climbing to 250.0. A real hang at the end of the scan prints exactly the same thing. Neither a person nor an agent could tell the two apart.

On a current build each of those steps is a phase, and the small-repository capture holds 0 lines that read `scan` at `pct=100.0`. If you are on rc.74 or earlier and see that line, check `_count` on the node before you conclude the run is hung.

## The line that ends the run

`xerj-done` is the only line a script must parse to decide success. The captured terminal line reads in full:

```text
xerj-done ok=true exit=0 reason=completed wall=22.2s files=1 records=164441 datasets=1 junk_files=0
```

`reason` distinguishes `completed`, `dry-run`, `completed-with-junk`, `aborted` and, on the `--no-graph` path, `server-backpressure`, and the exit code follows it. Exit 3 with `completed-with-junk` means the run refused some files, and the catalog holds a reason for each one. `server-backpressure` is exit 1: the node kept rejecting for the whole 600 s a bulk waits, and the line adds `ops_applied` and `ops_remaining` so you know how much the same command still has to do.

If the server refused a whole dataset, the line also carries `datasets_refused` and `files_refused`. They appear only when it happened. The [refused-dataset page](/answers/autoindex-dataset-refused-by-server) covers that case.

If the node pushed back with HTTP 429 during the run, the line carries `bulk_retries`, the number of bulks the run re-sent. It also appears only when it happened. If the node refused a request for its size (HTTP 413) and the run cut it in two, the line carries `bulk_splits`, also only when it happened.

## When the node pushes back

A node that crosses its memory watermark answers writes with HTTP 429 until memory drops back, usually within seconds. The run lowers its bulk concurrency, re-sends only the rejected items after a backoff, and says so on stderr at most once every 5 seconds:

```text
autoindex: server is shedding load — [parent] real memory circuit breaker tripped: …; re-offering 747 rejected record(s) (waited 30s, giving up after 600s)
```

During the wait `since_progress_s` climbs, because nothing is landing. That is the honest reading, and the line above is what tells it apart from a hang. Before this change a full-corpus run aborted at 60.4% of its `index` phase, after 5122.5 seconds, on the first bulk that came back with 747 items rejected 429. The [back-pressure page](/answers/autoindex-server-back-pressure-429) covers the rules and the exit-1 case.

## Progress and the decision gate are separate runs

`--quiet` means no progress output, so it cannot share an invocation with `--progress plain`. The capture ran the 2 recipes as separate commands for that reason.

Use `--progress plain` to follow a run. Use `--quiet` with `--max-minutes` to get the machine-readable decision JSON on stdout.
