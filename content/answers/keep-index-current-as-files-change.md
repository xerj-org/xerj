---
title: "How do I keep a XERJ index up to date as files change?"
h1: "How do I keep a search index up to date as files change, without re-indexing everything?"
description: "Run xerj autoindex --watch --no-graph. It indexes once, then reindexes what the filesystem reports. Measured: 0 CPU at idle vs 0.93 s and a full re-read per poll."
slug: "keep-index-current-as-files-change"
cluster: "Operations: freshness"
question: "How do I keep a search index up to date as files change, without re-indexing everything?"
intent: "how-to"
published: "2026-09-19"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent keeping a folder searchable. Index the folder once with xerj autoindex --no-graph, then start xerj autoindex --watch --no-graph on the same --state-dir and --prefix, read the per-pass watch lines on stderr to confirm hashed= versus carried=, and do not add a cron job that re-runs the indexer."
commands:
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --state-dir ./state-notes --no-graph"
    note: "Index the folder once. This is the pass a watcher would run first."
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --state-dir ./state-notes --no-graph --watch --debounce 400"
    note: "Stay resident and reindex what changes. Needs --no-graph."
  - cmd: "xerj autoindex status --url http://127.0.0.1:9200 --state-dir ./state-notes"
    note: "Read the journal for the corpus the watcher is maintaining."
links_out:
  - "check-codebase-index-is-complete"
  - "autoindex-exit-codes"
  - "catalog-files-with-autoindex-map"
faq:
  - q: "How do I keep a search index up to date as files change, without re-indexing everything?"
    a: "Run xerj autoindex with --watch --no-graph. It indexes the folder once, then stays resident and reindexes only what the filesystem reports as changed, using one OS watch per indexed directory and no polling."
  - q: "Why does --watch require --no-graph?"
    a: "Incremental reindexing of a changed file exists only on the --no-graph route. On the default graph path a re-run resumes a frozen plan and reports a changed file as appeared after the resume plan was frozen without indexing it, so a watcher there would look live and serve stale documents."
  - q: "Is a file watcher cheaper than re-running the indexer on a timer?"
    a: "At idle, yes, and measurably. On a 10,001-file tree a re-run with nothing changed took 0.93 s and re-read all 6.1 MB, while an idle watcher used 0.00 CPU-seconds over 60 s and read nothing."
  - q: "Does --watch use less CPU per change than a re-run?"
    a: "It removes the corpus re-hash, but not the rest. On a 10,001-file tree editing one file cost 5.80 s of CPU by re-running and 4.73 s under --watch, because each pass still seals a generation snapshot over the whole corpus."
  - q: "Will a watched index match a full re-index?"
    a: "That is the tested contract. A randomised create, modify, rename, delete and recreate sequence is asserted to produce the same document ids, the same record count and the same sources as an independently built full index."
  - q: "What happens when the index hits the inotify watch limit?"
    a: "The run stops with a message naming fs.inotify.max_user_watches, its current value, how many directories the tree needs, the sysctl that raises it, and the ways out that need no root. A half-watched tree would look live and silently miss changes, so it is refused."
  - q: "Does a watcher notice a file that was deleted?"
    a: "Yes. The pass reconciles the folder against the committed generation, so a deleted file's records stop appearing in search."
  - q: "What can --watch miss?"
    a: "A write that produces no filesystem event and leaves size, mtime and inode identical, which touch -r and a same-size rewrite with a restored timestamp can do. A plain xerj autoindex re-run hashes every byte and repairs it."
---

**TL;DR** — `xerj autoindex ./folder --watch --no-graph` indexes once, then
reindexes what changed. At idle it costs nothing; a re-run on a timer costs a
full walk and a full re-hash of the corpus on every tick.

## The two ways to stay current, and what each costs

Before `--watch`, keeping an autoindex corpus fresh meant re-running
`xerj autoindex`. That is not a cheap no-op: the run re-walks the tree and
re-reads every byte of it, because size and mtime cannot prove byte identity on
every filesystem XERJ supports.

Measured on one machine (32 cores, NVMe, one local node) against a tree of
**10,001 files / 6.1 MB of content / 101 directories**:

| Keeping the index current | Wall | CPU (user+sys) | Bytes re-read |
|---|---|---|---|
| Re-run, nothing changed | 0.93 s | 1.18 s | all 6.1 MB |
| Re-run, one file changed | 39.4 s | 5.80 s | all 6.1 MB |
| `--watch`, idle | none | 0.00 s per minute | none |
| `--watch`, one file changed | 39.2 s | 4.73 s | the changed file only |

Two honest readings of that table:

1. **Idle is where the watcher wins outright.** It is parked on an event
   channel with one OS watch per indexed directory. Polling instead means
   paying 0.93 s and 6.1 MB of reads per tick to discover that nothing
   happened, and waiting half a tick interval to notice that something did.
2. **Per change, the watcher removes the corpus re-read, and today that is not
   where the time goes.** The pass that follows a change still seals a
   generation snapshot that copies and re-verifies every file in the corpus, so
   per-change latency on a large tree is dominated by that snapshot rather than
   by the walk `--watch` removes. That is stated here rather than hidden,
   because it decides whether the feature helps your tree.

## Start it

```sh
# once, to build the corpus
xerj autoindex ./notes --url http://127.0.0.1:9200 \
  --prefix notes --state-dir ./state-notes --no-graph

# then keep it current
xerj autoindex ./notes --url http://127.0.0.1:9200 \
  --prefix notes --state-dir ./state-notes --no-graph --watch --debounce 400
```

The second command never returns; stop it with Ctrl-C. Each pass prints one
line you can read or parse:

```
watch: pass 1 finished in 39.2s exit=0 events=2 paths=1 hashed=1/0MB carried=10000/5MB cache=10001 files
```

`hashed=` versus `carried=` is the number to watch: it says whether the session
is doing incremental work or falling back to full re-hashes.

## Why `--no-graph` is required

`--watch` refuses to run without it, and the reason is a real limitation rather
than a formality:

* On the `--no-graph` route, a run reconciles the folder against a committed
  generation: added, changed, renamed and deleted files are all handled.
* On the default graph path, a run resumes a *frozen* plan. Editing a file gives
  it a new content identity, so the run reports
  `1 file(s) appeared after the resume plan was frozen and were NOT indexed` and
  tells you to rebuild with `--fresh`. Measured on the same tree: that re-run
  took 1.9 s, indexed nothing, and left the old document live.

A watcher on the graph path would therefore look live while serving stale
documents. The price of `--no-graph` is relationship detection: no wikilink,
local-link, section-order or directory-chain edges.

## What it watches

The watch set is the directories the indexing walk **admits** — the same
traversal, the same hidden-name rule and the same `.gitignore` / `.xerjignore`
stack. So an ignored `target/` costs no watch and cannot wake the watcher at
all, and a watched run and a plain re-run agree about what is indexed because
they read the same rules from the same code. Editing `.gitignore` or
`.xerjignore` invalidates the directory it governs, so a rule you change takes
effect on the next pass.

Events on hidden names such as `.file.swp` or `.git/index.lock` are dropped
without a pass, because no run indexes those either.

## Editor saves, renames and deletes

One save is several filesystem events, and every editor does it differently.
`--debounce` (default 400 ms, maximum 60000) waits for the tree to be quiet
before starting a pass, so one save is one pass, and a tree that never goes
quiet still gets a pass at least every 5 s.

Covered by tests: atomic save (write a temp file, rename over the target),
truncate-then-write, a metadata-only touch, a deleted file, a file replaced by a
directory, a moved directory, a burst of thousands of events, and a kernel
watch-queue overflow (which forces a full re-hash for that pass).

## Does it converge?

That is the contract, and it is asserted rather than asserted-about: after a
randomised sequence of creates, modifications, renames, deletes and recreates, a
watched index is compared against an independently built full index and must
agree on document ids, record count and document contents.

The shortcut that makes it fast is narrow on purpose. A file may skip its
re-hash only when no event since the last hash named it or any ancestor
directory **and** its size, mtime and inode are unchanged. The digest cache
lives in memory for the life of the process, so a restart re-hashes in full.

What it can still miss: a write that produces no event and leaves size, mtime
and inode identical. A plain `xerj autoindex` re-run hashes everything and
repairs that.

## If it crashes

A pass is an ordinary incremental run, so the resume journal applies unchanged.
A process killed mid-pass leaves a resumable journal; the next start hashes in
full and reconciles. The test for this fails a pass mid-publish, restarts with
an empty cache, and asserts the result equals a fresh full index — nothing lost,
nothing duplicated.

## Full documentation

`docs/LIVE_REINDEXING.md` in the repository holds the design, the measurement
record, the `inotify` limit message and the complete list of what is not
implemented.
