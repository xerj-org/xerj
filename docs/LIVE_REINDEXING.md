# Live reindexing — `xerj autoindex <folder> --watch`

`xerj autoindex` indexes a folder once. To keep that index current you had two
choices: re-run it, or accept a stale index. `--watch` is the third: index once,
then stay resident and reindex what the filesystem says changed.

```sh
xerj autoindex ~/notes --watch --no-graph \
  --url http://127.0.0.1:9200 --prefix notes --state-dir ./state-notes
```

Source: `engine/crates/xerj-autoindex/src/watch.rs`. The flags are `--watch` and
`--debounce <ms>` (`engine/crates/xerj-autoindex/src/cli.rs`).

## `--watch` needs `--no-graph`, and that is a real limitation

The run refuses `--watch` without `--no-graph`. This is not a formality, so it
is worth stating exactly what it means:

* On the **`--no-graph` (generated) route**, a re-run reconciles the folder
  against a committed generation. Added, changed, renamed and deleted files are
  all handled (`lib.rs::project_reconcile_plan`,
  `reconcile_plan::reconcile_plan`).
* On the **default graph path**, a re-run *resumes a frozen plan*, and what that
  costs depends on the kind of change. Measured on a 10,000-file tree
  (`docs/measurements/autoindex-watch-2026-09-19.md`, section 4):

  | Graph-route re-run after… | Result |
  |---|---|
  | a file's **content** changed | indexed: 3.07 s, `files=1`, exit 0, the new text searchable |
  | a file was **added** | 3.32 s, **exit 3**, `1 file(s) appeared after the resume plan was frozen and were NOT indexed`; the new file stays unsearchable until `--fresh` |
  | a file was **deleted** | **exit 1, aborted in 0.24 s** — "removing files from an indexed folder is not reconciled yet". Its documents stay live, and every later re-run aborts the same way until the corpus is rebuilt |

So the limitation is **additions and deletions**, not edits. A watcher on the
graph path would serve a folder that goes stale on the first new file and stops
reindexing entirely on the first deletion — worse than no watcher at all. That is
why `--watch` requires the route that reconciles all four cases, and says so
instead of quietly downgrading. (An earlier version of this page said a *changed*
file is not indexed on the graph path. It is; that claim came from a measurement
whose shell append had created a file instead of modifying one.) The cost of
`--no-graph` is relationship detection: no wikilink, local-link, section-order or
directory-chain edges, so no second-brain graph over a watched corpus.

## What it costs

Measured on one box (32 cores, NVMe, one local node) on a tree of **10,000 files
/ 4,576,300 bytes / 101 directories**, one file modified, added and deleted on
each of three routes. Every number comes from
`docs/measurements/autoindex-watch-2026-09-19.md`, which holds the commands, the
captured output and the verification that each change really was searchable
afterwards. The box was **shared** with other agents' fat-LTO builds
(one-minute load average 7.6–17.7 across the runs, recorded per run), and every
figure is a single sample: read the order-of-magnitude gaps, not the small ones.

| Keeping a 10,000-file index current | Wall | Client CPU | Server CPU |
|---|---|---|---|
| index the folder again from scratch (one file modified) | 293.8 s | 127.9 s | 167.5 s |
| re-run the same `--no-graph` command, nothing changed | 1.7 s | 2.1 s | 0.01 s |
| re-run it, one file modified | 44.3 s | 7.6 s | 27.9 s |
| re-run it, one file added | 42.7 s | 7.9 s | 28.3 s |
| re-run it, one file deleted | 39.1 s | 7.5 s | 23.1 s |
| `--watch`, idle | — | **0.00 s per minute** | 0.07 s per minute |
| `--watch`, one file modified | 47.6 s | 6.6 s | 34.5 s |
| `--watch`, one file added | 45.9 s | 6.9 s | 32.8 s |
| `--watch`, one file deleted | 40.8 s | 6.5 s | 27.6 s |

The `--watch` wall times are measured from the change hitting the filesystem to
the session's own `watch: pass N finished` line, so the 400 ms debounce is inside
them. `Client CPU` is `user+sys` of the indexing process; `server CPU` is the
node's own `utime+sys` over the same window — work a client-side timer cannot see.

Read the table honestly, because it says three things and only one of them is the
obvious one:

1. **Against re-indexing the folder from scratch, incremental wins by an order of
   magnitude** — 47.6 s against 293.8 s, 6.6 CPU-seconds against 127.9. That gap
   is far larger than the load noise, and it holds for a modify, an add and a
   delete alike.
2. **Against a re-run of the same command, `--watch` is not faster per change.**
   47.6 / 45.9 / 40.8 s against 44.3 / 42.7 / 39.1 s. In principle it skips the
   re-hash of the whole corpus, which the no-op re-run above measures on its own
   at 2.1 CPU-seconds for 4.58 MB — but **that saving is smaller than the noise on
   this corpus and we could not measure it**: an independent re-run of the modify
   case on the same binary and node came out the other way (re-run 5.61 CPU-s
   against watch 7.19), and the re-run case alone moved 7.64 → 5.61 between runs.
   Read only the order-of-magnitude gap in point 1. On a corpus of large files,
   where the hash is minutes rather than a second, the skipped term is the one
   that grows — that is an argument from what the code does, not a measurement.
3. **At idle, `--watch` is free and a timer is not.** 0.00 CPU-seconds per minute
   against 1.7–2.2 s of wall and a full 4,576,300-byte re-read on every tick,
   forever, plus a mean staleness of half the tick. A watcher's staleness is the
   debounce plus the pass.

So the case for `--watch` is idle cost, detection latency and not having to
remember to re-run — not throughput per change.

### Why a one-file pass still costs ~40 s, and what it is not

Sampling the staging snapshot and the node once a second through a one-file
re-run splits that 42-second run in two (section 3 of the measurement record):

* **~13 s, client side**: `sync_executor.rs::create_snapshot_inner` seals a
  snapshot over the *whole* inventory — per file, `content::verify` on the source,
  `copy_synced` (an `fsync` each) into the staging blob store, `content::verify`
  again on the copy, serially. The snapshot for a one-file change holds **10,000
  blobs**, one per corpus file.
* **~27 s, server side**, while the client is nearly idle: the catalog is rewritten
  file-by-file per generation — after the run, all 10,000 `doc_kind: file` catalog
  documents carry the newest `run_id`.

Both are O(corpus) per change, both are shared with a plain re-run, and neither is
touched by `--watch`; the per-change lever is there, not in the walk `--watch`
removes. It belongs to the transactional publish path and is filed as its own
issue ([#971](https://github.com/xerj-org/xerj/issues/971)). (An earlier version of this page attributed the whole tail to the snapshot.
That was an inference and the sampling above disproved it.)

### Idle cost, stated as numbers

Idle cost is a reported customer problem, so it was sampled directly, twice — once
after the first pass and once after three change passes:

* One OS watch per **indexed** directory — 101 on the tree above, not one per
  directory on disk.
* No polling thread, no timer, no busy loop. The session blocks on the event
  channel (`watch.rs::next_burst`).
* **0.00 CPU-seconds over 60 s** of an untouched tree, both times; the node itself
  spent 0.07 s in the same minute.
* **157 MiB resident** between passes (160,712 kB, unchanged over the 60 s sample;
  158 MiB after the three passes), against a 258 MB peak *during* a pass.
* **295 threads** resident at idle — the scan and index worker pools sized from
  this box's 32 cores. They cost no measurable CPU, but they are not free RAM.

## What it watches, and what it ignores

The watch set is the set of directories the indexing walk **admits**
(`walk::walk_dirs_opts`), not the root recursively. Same traversal, same
hidden-name rule, same `.gitignore`/`.xerjignore` stack. Two consequences:

* An ignored directory costs no watch and produces no events. A `cargo build`
  writing into an ignored `target/` cannot wake the watcher at all.
* A watched run and a plain re-run agree on what is indexed, because they apply
  the same rules from the same code — not from a second implementation.

Editing `.gitignore` or `.xerjignore` invalidates the directory it governs, so
a rule you add or remove takes effect on the next pass.

Events on hidden names (`.file.swp`, `.git/index.lock`) are dropped without a
pass, because no run indexes them either. The two exceptions are the ignore
files themselves.

## Editor and tool behaviour

One "save" is several filesystem events, and every editor does it differently
(truncate-then-write, write-temp-then-rename, plus a chmod). `--debounce`
(default 400 ms) waits for the tree to be quiet for that long before starting a
pass, so one save is one pass. A tree that never goes quiet — a log being
appended to inside the corpus — still gets a pass at least every 5 s
(`watch.rs::max_hold`).

Handled, with a test for each (`incremental_reconcile_http_tests.rs`,
`watch.rs`):

| What happens | What the watcher does |
|---|---|
| atomic save (write temp, rename over) | indexes the new content; the temp name never becomes a document |
| truncate-then-write | indexes the new content |
| chmod / metadata touch only | a pass runs (a file that just became unreadable stops being indexable) but the digest is not invalidated, so nothing is re-read and nothing is republished |
| file deleted | its records stop appearing in search |
| file replaced by a directory | the subtree is re-read and converges |
| whole directory moved | the subtree is invalidated; the documents follow the new path |
| whole directory deleted | every document under it stops appearing in search |
| burst of thousands of events | coalesced into one pass; above 20,000 distinct paths the pass re-hashes everything instead of tracking them (bounded memory) |
| dropped events (kernel queue overflow) | the platform's rescan notice forces a full re-hash for that pass |

### The watcher's own reads are not changes

Worth knowing because it decides whether a session settles at all. On Linux the
watch mask `notify` installs includes `IN_OPEN` and `IN_CLOSE`
(`notify-8.2.0/src/inotify.rs:418`), so **every file a pass reads reports an
event** — and a pass reads the whole corpus (the hash, then the generation
snapshot's copy and its two verifications). Treated as changes, those events make
every pass trigger the next one: measured, before this was fixed, as 48 passes
over an untouched 3-file tree with the digest cache invalidated every time.

So `watch.rs::classify` decides per event kind: reads are dropped,
`Access(Close(Write))` is a finished write and is kept, `Modify(Metadata)` is the
weak signal above, and anything unknown is treated as a change. The same loop is
possible from a plausible command line — `--state-dir ./state` inside the watched
folder, where every pass writes the journal — so `--watch` refuses a state
directory inside the tree it watches.

## The correctness contract

Stated precisely, because the loose version of it is false:

> **After any sequence of changes, a plain `xerj autoindex` re-run — which
> re-hashes every byte and trusts nothing — changes not one document.**

That is the property `--watch` has to have, and it is the one that guards the
digest cache: if a carried digest ever let a changed file skip its re-hash, the
verifying re-run reads the bytes, sees the difference and republishes, and the
assertion fails. It is asserted after every convergence test in
`engine/crates/xerj-autoindex/src/incremental_reconcile_http_tests.rs`, including
after a randomised create/modify/rename/delete/recreate sequence
(`watch_converges_under_a_randomised_change_sequence`,
`a_rerun_changes_nothing`).

The second, weaker property — **a watched index equals an independently built
fresh full index**, same document ids, same document count, same sources — is
asserted too, and holds for every change that does not move dataset identity:
modifications, deletions, atomic saves, creations, and the randomised sequence
above.

Where it does **not** hold, and why: an incremental run deliberately preserves
the committed dataset and schema identity, while a fresh run re-elects dataset
slugs from the corpus it sees (`reconcile_plan.rs`, module docs). Rename the
directory a dataset was named after and a fresh rebuild files `to/one.csv` under
a dataset called `to`, while the incremental corpus still calls it `from` — same
paths, same bytes, a different dataset name and therefore a different index and
document id. That is the incremental route's rule and a manual re-run does
exactly the same thing; it is not something `--watch` introduces. If you want
the slug re-elected, rebuild the corpus.

How the shortcut stays safe: a plain run hashes every byte on every run because
size and mtime cannot prove byte identity (`content.rs`). `--watch` does not
weaken that to "trust mtime". A file skips its re-hash only when **both** hold:

1. no event since that hash named the file, its directory, or any ancestor; and
2. its `(size, mtime, inode)` fingerprint is exactly what it was when the digest
   was taken.

The digest cache is in memory and per process. A restart re-hashes in full.

**The one hole, stated plainly:** a write that produces no event *and* leaves
size, mtime and inode identical is not noticed until something else touches the
file. `touch -r` and a same-size rewrite with a restored timestamp do that. A
plain `xerj autoindex` re-run hashes everything and repairs it. Watching is also
not supported on every filesystem — some container bind mounts and network
shares report no events at all, and `--watch` says so if the watcher cannot
start.

## Crash and restart

A pass is an ordinary incremental run, so the journal's resume contract applies
unchanged: a process killed mid-pass leaves a resumable journal, and the next
start re-hashes in full (empty cache) and reconciles. Proven by
`a_pass_that_fails_mid_publish_converges_on_the_next_one`, which fails a pass
mid-publish, restarts with an empty cache and asserts the result equals a fresh
full index — no lost and no duplicated documents.

## The watch limit is a hard failure, not a warning

On Linux each watched directory consumes one `inotify` watch from
`fs.inotify.max_user_watches`, shared with every other watcher running as the
same user (editors and language servers hold thousands). When the kernel refuses
one, `--watch` **stops** with a message naming the limit, the current values of
`max_user_watches` and `max_user_instances`, how many directories this tree
needs, the `sysctl` that raises it, and the two ways out that need no root
(watch a subdirectory, or exclude directories with `.xerjignore`).

It stops rather than continuing because a half-watched tree is the worst
outcome available: the index looks live and silently is not.

Honest scope of that claim: the message's content is asserted by a unit test
(`the_watch_limit_message_names_the_limit_the_count_and_a_way_out`), and the
`ErrorKind::MaxFilesWatch` branch it comes from is the one `notify` raises for
`ENOSPC` (`notify-8.2.0/src/inotify.rs:449`). The kernel refusal itself was not
provoked on the machine these numbers come from: doing that means lowering
`fs.inotify.max_user_watches` while the run is in flight, which needs root.

## Progress and machine-readable output

Each pass reports through the ordinary progress surface, so `--progress plain`
and `--progress json` behave exactly as they do for a normal run, including
per-phase ETA. The hash phase's totals are the files *that pass* will read, so
the percentage describes the work being done rather than the work a full run
would have done. Per pass the session adds one line:

```
watch: 101 directories watched under /home/me/notes (debounce 400 ms); one watch per indexed directory, no polling thread
autoindex: --watch: re-hashing 1 file(s) (0 MB); 9999 file(s) (4 MB) carried from the previous pass
watch: pass 1 finished in 47.0s exit=0 events=3 paths=1 hashed=1/0MB carried=9999/4MB cache=10000 files
```

`hashed=` versus `carried=` is the operation count this feature has to make
visible: it is how you see whether the watcher is doing incremental work or
falling back to full re-hashes.

With `--json`, each pass prints its own result object on its own line (one
JSON object per pass, NDJSON), rather than one object for the process.

## Cost of the alternative, and why polling is the expensive shape

The reason this feature exists in event-driven form rather than as a built-in
timer is that polling costs money and CPU in proportion to how fresh you want
the index to be, while watching does not:

| Strategy | Detection latency | Work per minute on the tree above |
|---|---|---|
| `--watch` | debounce (0.4 s) + the pass | 0 CPU while nothing changes |
| re-run every 60 s | 30 s mean | 1 full walk + 4.58 MB re-hashed, ~2.1 CPU-s |
| re-run every 5 s | 2.5 s mean | 12 full walks + 55 MB re-hashed, ~25 CPU-s |

The same arithmetic is much harsher when the source is object storage rather
than a local disk, which is worth stating here because it is the reason not to
"just poll": a `ListObjects` call is a Cloudflare R2 **Class A** operation, one
per 1,000 objects listed, and the free tier is 1,000,000 Class A operations per
month. Polling an empty bucket every 5 s spends ~518,000 of them (half the free
tier) finding nothing; every 60 s spends ~43,000 (4%); and polling a
100,000-object bucket every 60 s needs 100 list pages per cycle — ~4.3 million
operations a month, over four times the entire free allowance. A local `--watch`
session makes **zero** API calls while idle. (Watching an object-storage source
is not implemented; `--watch` watches a local folder.)

## What is NOT implemented

* No watching on the default graph path — `--watch` requires `--no-graph`.
* No object-storage or remote source watching. `--watch` takes a local folder.
* No incremental generation snapshot: each pass still seals a snapshot over the
  whole corpus, which is what dominates per-change latency on a large tree
  (measured above).
* No periodic full-hash safety sweep. The repair for the fingerprint hole above
  is a plain `xerj autoindex` re-run, run when you want it.
* The watcher does not watch the root's parent, so deleting the watched folder
  itself is reported by the pass, not by an event.
* No `--watch` for `xerj autoindex map` or `status`; they are single-shot reads.
* No automatic retry of a failed pass. A pass that fails — the endpoint went
  away, a bulk was rejected — is reported as a warning and the session keeps
  watching, but the work is retried on the **next change**, not on a timer. The
  journal's resume contract means nothing is lost when it is retried; it does
  mean an index can stay behind while the tree is quiet and the endpoint is
  broken. Watch the `watch: pass N FAILED` lines.
* No dataset re-election. A watched corpus keeps the dataset names it was built
  with, like any incremental run; a rename that would have produced a different
  dataset name on a fresh build does not rename the dataset.
