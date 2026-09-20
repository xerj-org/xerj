# Measurement record — `xerj autoindex --watch` vs re-running, 2026-09-19

This file exists so every number in `docs/LIVE_REINDEXING.md`, in
`content/answers/keep-index-current-as-files-change.md` and in the pull request
can be traced to a command someone else can run.

Every figure below is a **single sample on a shared box**. The one-minute load
average at the start of each run is printed with it, and it ranged from 7.6 to
17.7 on 32 cores while other agents were running fat-LTO `cargo build`s. Do not
read small differences as signal; the comparisons this record makes are the
ones where the gap is an order of magnitude, or where the direction is the same
in all three change cases.

## Machine, build and corpus

| | |
|---|---|
| Host | 32 cores, 119 GB RAM, NVMe, Linux 7.0.0-27-generic; shared with other agents' builds |
| Binary | `CARGO_TARGET_DIR=target-watchlocal cargo build --release -j 10 -p xerj-server`, branch `feat/autoindex-watch` |
| Node | one throwaway node, `xerj --port 14040 --data-dir .scratch/node`, auth on, default (**lexical**) embedder |
| Corpus | 10,000 files / 4,576,300 bytes / 101 directories, synthetic `.md` `.txt` `.py` `.json` `.csv`, ~460 bytes mean |

No neural embedding ran: the default lexical feature-hashing embedder was used,
so none of these numbers is a semantic-search measurement. The corpus is
generated deterministically (100 directories × 100 files, `random.Random(20260919)`),
and three byte-identical copies were made — `c-full`, `c-inc`, `c-watch` — so
that no route ever saw another route's edits.

The same three changes were applied to each copy, in this order:

* **modify** — append one line to `pkg007/file031.txt`, a file that already exists
* **add** — create `pkg007/added-new.md`, a path that did not exist
* **delete** — `rm pkg007/file033.json`

The file the change is applied to is checked for existence (or absence) before
the change, because the previous version of this record got that wrong: it
appended to `pkgNNN/fileNNN.md` for a file whose generated extension was `.txt`,
so the shell **created** a file and the run measured an addition while the table
called it a modification. Those numbers are gone; everything here was re-measured.

## The three routes

| Route | What it is |
|---|---|
| **full** | index the folder again from scratch: a new `--state-dir` and a new `--prefix` per run (`--fresh` refuses to discard a committed generation, so a rebuild is a new destination) |
| **re-run** | run the *same* `xerj autoindex … --no-graph` command again against the same state dir — the incremental reconcile that exists today |
| **`--watch`** | one resident `xerj autoindex … --no-graph --watch --debounce 400` session; the pass it runs for each change |

All three were measured with `/usr/bin/time`; the watch route's wall clock is
measured from the moment the change hits the filesystem to the moment the
session's own `watch: pass N finished` line appears (so it includes the 400 ms
debounce), and its CPU is the delta of `utime+stime` from `/proc/<pid>/stat`.
`server cpu` is the same delta taken on the node process over the same window —
it is the work the change caused on the server, which client-side `/usr/bin/time`
cannot see.

## 1. Keeping a 10,000-file index current after ONE file changes

| Route | Case | Wall | Client CPU (user+sys) | Server CPU | load1 at start |
|---|---|---|---|---|---|
| full | modify | 293.8 s | 127.9 s (122.72 + 5.16) | 167.5 s | 7.63 |
| full | add | 347.7 s | 146.9 s (141.05 + 5.80) | 206.6 s | 8.60 |
| full | delete | 312.4 s | 137.9 s (132.32 + 5.53) | 178.4 s | 20.22 |
| re-run | *nothing changed* | 1.68 s | 2.05 s (1.44 + 0.61) | 0.01 s | 14.24 |
| re-run | modify | 44.3 s | 7.64 s (4.81 + 2.83) | 27.9 s | 14.30 |
| re-run | add | 42.7 s | 7.92 s (5.17 + 2.75) | 28.3 s | 11.71 |
| re-run | delete | 39.1 s | 7.49 s (4.97 + 2.52) | 23.1 s | 10.26 |
| re-run | *nothing changed* (after the three) | 2.18 s | 2.62 s (1.96 + 0.66) | 0.01 s | 7.95 |
| `--watch` | modify | 47.6 s | 6.61 s | 34.5 s | 8.77 |
| `--watch` | add | 45.9 s | 6.92 s | 32.8 s | 9.26 |
| `--watch` | delete | 40.8 s | 6.50 s | 27.6 s | 7.93 |

Genesis, for scale: the `--no-graph` first pass over this corpus took **485.1 s**
wall / 252.6 s client CPU / 241.9 s server CPU at load 17.7 (re-run route), and
**324.3 s** at load 8.6 for the watcher's own first pass.

Each change was verified, not assumed:

```
search tokinc-mod: 1     search tokinc-add: 1     deleted path hits: 0   (re-run route)
search tokfull-mod: 1    search tokfull-add: 1    deleted path hits: 0   (full route)
search tokw-modify: 1    search tokw-add: 1       deleted path hits: 0   (--watch)
```

and the watcher's own per-pass lines show the cache doing its job:

```
watch: pass 0 finished in 324.3s exit=0 events=0 paths=0 hashed=10000/4MB carried=0/0MB  cache=10000 files
watch: pass 1 finished in  47.0s exit=0 events=3 paths=1 hashed=1/0MB   carried=9999/4MB cache=10000 files
watch: pass 2 finished in  45.3s exit=0 events=6 paths=1 hashed=1/0MB   carried=10000/4MB cache=10001 files
watch: pass 3 finished in  40.4s exit=0 events=3 paths=2 hashed=0/0MB   carried=10000/4MB cache=10000 files
```

### What the table says, stated plainly

* **Against a full rebuild, the incremental routes win by an order of magnitude**
  — 47.6 s against 293.8 s wall for one modified file, 6.6 s against 127.9 s of
  client CPU. That gap is far larger than the load noise and holds for all three
  change cases.
* **Against a re-run of the same command, `--watch` is not faster per change.**
  44.3 / 42.7 / 39.1 s (re-run) against 47.6 / 45.9 / 40.8 s (`--watch`), one
  sample each, with the debounce inside the watch figure. What it does save is
  client CPU — 7.64 → 6.61, 7.92 → 6.92, 7.49 → 6.50 — about **1 CPU-second per
  change**, which is the re-hash of the whole corpus it skips. The no-op re-run
  (2.05 CPU-seconds to read all 4.58 MB and find nothing) is the same term
  measured on its own.
* **So the per-change case for `--watch` is not throughput.** It is idle cost,
  detection latency, and not having to run anything on a timer.

## 2. Idle cost of a watch session

Users report idle CPU and RAM as a real cost, so this was sampled twice: once
after the genesis pass, once after the three change passes.

| Sample | Watcher CPU over 60 s | Watcher RSS | Threads | Server CPU over 60 s |
|---|---|---|---|---|
| after genesis | **0.00 s** | 160,712 kB (157 MiB), unchanged start to end | 295 | 0.07 s |
| after the three passes | **0.00 s** | 162,040 kB (158 MiB) | — | — |

Peak RSS *during* a pass was 258 MB; the 157 MiB above is what the process holds
between passes. 295 threads are resident at idle (the scan and index worker
pools, sized from the 32 cores) and they cost no measurable CPU, but they are not
free RAM. There is no polling thread and no timer: the session blocks on its
event channel (`watch.rs::next_burst`).

The alternative — a re-run on a timer — costs 1.68–2.18 s wall and ~2.0–2.6 s of
CPU *per tick*, re-reads all 4,576,300 bytes on every tick, and leaves a mean
detection latency of half the tick interval.

## 3. Where the 40+ seconds of a one-file pass actually go

The previous version of this record said "~38 s of the 39 s is the snapshot".
That was an inference, not a measurement, and it is wrong. It was measured by
sampling the staging snapshot's blob count and the server's CPU once a second
through a one-file-modify re-run (client wall 42.4 s, user 4.82, sys 2.35, load ~7):

```
t=  0.0-  5.1  staged blobs=2227   server CPU +0.00 s
t=  5.1- 10.1  staged blobs=5818   server CPU +0.00 s
t= 10.1- 15.2  staged blobs=9858   server CPU +0.01 s
t= 15.2- 20.3  staging gone        server CPU +4.59 s
t= 20.3- 25.3                      server CPU +5.27 s
t= 25.3- 30.4                      server CPU +6.43 s
t= 30.4- 35.4                      server CPU +4.96 s
t= 35.4- 40.5                      server CPU +4.98 s
t= 40.5- 41.5                      server CPU +1.01 s
```

So a one-file change on a 10,000-file corpus is **two** O(corpus) costs, not one:

1. **~13 s of client-side snapshot** (t ≈ 3–16 s). `sync_executor.rs::create_snapshot_inner`
   walks the whole inventory serially and, per file, runs `content::verify` on the
   source, `copy_synced` into the staging blob store (which `sync_all()`s it), and
   `content::verify` again on the copy. The sealed snapshot for a one-file change
   holds **10,000 blobs** — one per corpus file — confirmed by `ls` on the state dir.
2. **~27 s of server CPU** (t ≈ 15–42 s), while the client is nearly idle. The
   catalog is rewritten file-by-file for the whole corpus every generation: after
   the run, all 10,000 `doc_kind: file` catalog documents for that prefix carry the
   newest `run_id`, and the node logs a flush of ~600-document segments followed by
   a 16-segment merge with `live_docs=50021`.

Neither cost is touched by `--watch`, and both are shared with a plain re-run.
Filed as [issue #971](https://github.com/xerj-org/xerj/issues/971); it belongs to
the transactional publish path, not to this feature.

A third, smaller observation from the same run: the progress bar reports
`phase=scan 100% … (stalled)` for the entire 39-second tail, because neither the
snapshot nor the publish is a reported phase. That is a UX gap against the
resource-aware-progress directive, not a correctness one.

## 4. The default (graph) route, re-measured — and a correction

The previous record claimed the graph path does not index a **changed** file on a
re-run. That claim came from the same mislabelled-extension mistake: the shell had
*created* a file. Re-measured on its own corpus copy with a file that genuinely
exists (`pkg007/file031.txt`, 516 → 553 bytes):

| Graph-route re-run after… | Result |
|---|---|
| genesis | 17.0 s, `files=10000 records=140000 datasets=3`, exit 0 |
| **modify an existing file** | **3.07 s, `files=1`, exit 0 — the new content IS indexed and IS searchable** (`hits: 1`), with no stale duplicate: the path holds 2 documents, the same as an untouched file |
| **add a new file** | 3.32 s, **exit 3** `completed-with-junk`, `1 file(s) appeared after the resume plan was frozen and were NOT indexed`; the added token is not searchable (`hits: 0`) |
| **delete a file** | **exit 1, `aborted` after 0.24 s**: "1 file(s) indexed under this resume plan no longer exist in the folder, and their documents are still live in the destination; removing files from an indexed folder is not reconciled yet". The documents stay live, **and every later re-run aborts the same way** — a second modify after the delete aborted in 0.20 s and was never indexed. |

So the honest reason `--watch` requires `--no-graph` is **additions and
deletions**, not modifications: on the graph route an added file is skipped until
a `--fresh` rebuild, and a deleted file aborts the run and keeps aborting it, which
leaves the whole corpus frozen. A watcher there would go stale on the first new
file and stop working on the first deletion. The refusal message in `cli.rs` said
"a file whose content changed … is NOT indexed"; that wording was corrected on this
branch in the same commit as this record.

## 5. Commands

```sh
# node
engine/target-watchlocal/release/xerj --port 14040 --data-dir .scratch/node

# corpus (three identical copies)
python3 .scratch/gen_corpus.py .scratch/c-full && … c-inc && … c-watch

# re-run route (genesis, then the same command again after each change)
xerj autoindex .scratch/c-inc --url http://localhost:14040 \
  --state-dir .scratch/st-inc --prefix inc --no-graph --progress plain --max-minutes 0

# full route (a new state dir and prefix per rebuild)
xerj autoindex .scratch/c-full --url http://localhost:14040 \
  --state-dir .scratch/st-f1 --prefix f1 --no-graph --progress plain --max-minutes 0

# watch route (one session for all three changes)
xerj autoindex .scratch/c-watch --url http://localhost:14040 \
  --state-dir .scratch/st-w --prefix w --no-graph --watch --debounce 400 \
  --progress plain --max-minutes 0
```

The driver that applied the changes, waited for each pass and sampled CPU/RSS was
`.scratch/measure.sh`; the attribution sampler in section 3 was `.scratch/attrib.sh`;
the graph-route probe was `.scratch/graph.sh` and `.scratch/graph2.sh`. They live
outside the repository because they hard-code this box's paths and ports; their
captured output is what the tables above quote.
