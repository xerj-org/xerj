# mbox ingest — measured on a 1 GB synthetic Google Takeout

What `xerj autoindex` does with a Takeout-shaped mailbox, measured, with the
numbers that came out and the ones that did not. Everything here is from a
**synthetic** mailbox written by [`scripts/synthetic-takeout.py`](../../scripts/synthetic-takeout.py)
(seed 42, profile `mixed`, `--target-bytes 1G --with-archive`), so that every
planted needle can be checked against a ground-truth file. Nothing here has
run against a real Takeout export.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen AI MAX+ 395 w/ Radeon 8060S — 32 threads (16 cores × 2) |
| RAM | 119 GiB usable |
| Disk | NVMe, ext4 (`/dev/nvme0n1p4`); the work dir is on it, NOT on tmpfs (the harness refuses a RAM-backed dir) |
| OS | Linux 7.0.0-27-generic (Ubuntu), x86_64 |
| XERJ | v1.0.0-rc.74 + branch `feat/ingest-mbox-takeout`, `cargo build --release -p xerj-server` |
| Node | throwaway, own data dir, TLS off, auth off, **`--embed-mode lexical`** (the default feature-hashing embedder — not neural) |
| Node memory | governor auto tier for a 64–128 GiB machine: **16 GiB process cap**, 4 GiB memtable budget, RSS watermark 95 % = 15,564 MB (`memory_limit_source=Auto`) |
| autoindex | default settings: 32 scan threads, 32 index workers, 4 PDF workers, `--bulk-mb 8`, `--yes` to pass the estimate gate |
| Box load | **shared**: other agents were building and benchmarking on it during every run. The 1-minute load average before/after each run is recorded in its result file (`loadavg_before` / `loadavg_after`; 9–31 before the runs below). Wall times are therefore upper bounds for an idle machine, not typical figures. |

## The corpus

`tree-mixed-1G` — one `Takeout/` folder: `Mail/All mail Including Spam and
Trash.mbox` (1,073,777,879 bytes, sha256 `4098bf60…aec0323`, **40,799
entries**: 5,698 binary attachments, 4,261 PDF attachments with a text layer,
4,300 text attachments, plus the malformed block), 12 Keep notes (each as
`.json` + `.html` twin), `Labels.txt`, Drive documents, `archive_browser.html`,
and an unextracted `.zip` and `.tgz` beside the folder. 18 files, 1,024 MB.

Expected output of a complete run, from the truth file: ~107k records
(messages, body sections, attachment pages/sections/cards), 21,544 resolvable
replies, 2,530 planted needles.

## How to reproduce

```sh
# from the repo root, with a release binary built
benchmarks/mbox-ingest/run.sh engine/target/release/xerj /path/on/a/real/disk mixed 1G 9520
# a 20 MB smoke first is a good idea:
benchmarks/mbox-ingest/run.sh engine/target/release/xerj /path/on/a/real/disk mixed 20M 9520
# the node's default memory cap does not let the 1 GB run finish (#948); the
# complete run lifts it — spell it "off", a bare 0 is ignored by the governor:
XERJ_MAX_PROCESS_MEMORY_MB=off LABEL=uncapped benchmarks/mbox-ingest/run.sh engine/target/release/xerj /path/on/a/real/disk mixed 1G 9520
# then, read-only, against the still-running node (run.sh does this itself):
python3 benchmarks/mbox-ingest/verify.py --url http://127.0.0.1:9520 --prefix bench --brain bench \
    --truth /path/on/a/real/disk/tree-mixed-1G.truth.json
```

`run.sh` boots the node, generates the tree if it is not there, runs
`xerj autoindex` under `/usr/bin/time -v`, samples the autoindex process's
`VmHWM` every 0.5 s and the server's at the end, waits for the index size to be
stable for 30 s, runs the verifier, and writes `result-<profile>-<size>.json`.
`RESUME=1` reruns the same command against the same node and state dir — what
the tool tells a user to do after a failure. `AX_FLAGS` / `LABEL` name
variants. Result files from the runs below are in [`results/`](./results/).

What each number is:

- **wall** — `xerj autoindex` start to exit, including Phase A (walk, sniff,
  sample, infer), the graph phase, Phase B (extract + bulk), and finalize.
- **docs/s** — documents on the node at the end ÷ wall. Not records/s during
  Phase B.
- **peak RSS, autoindex** — the `xerj autoindex` process's own `VmHWM`;
  **largest in tree** — `getrusage(RUSAGE_CHILDREN)` max RSS, i.e. the largest
  single process autoindex waited for (itself or a PDF worker).
- **peak RSS, server** — the node's `VmHWM` after the run, minus nothing.
- **index on disk** — `du -sb` of the node's data dir at autoindex exit and
  after merges settled; ratio is settled ÷ mbox bytes.
- **verify** — every planted needle must come back exactly once and from the
  right message; `replies_to` edges must equal the truth's resolvable replies;
  `attachment_of` edges must equal the attachment documents. `--per-kind 60`
  checks the first 60 needles of each kind (all of them for the 20 MB tree).

## Before: the run did not complete

Binary at `2d5f0af6` (this branch before the thread pool and the 429
re-offer). Three runs, default settings, same result each time.

| run | wall | docs on node at abort | autoindex peak RSS | server VmHWM | index at exit | exit |
|---|---|---|---|---|---|---|
| 1 — fresh ([json](./results/before-mixed-1G-run1.json)) | 226.9 s | 54,192 | 184.5 MB (largest in tree 182.7 MB) | **19,685.5 MB** | 366.2 MB | 1 |
| 2 — `RESUME=1` of run 1, i.e. "rerun the same command" ([time](./results/before-mixed-1G-run2-resume-time.txt)) | 298.7 s | — (verify aborted: 0.4 s/query on a merging node) | 179.7 MB | ~19.6 GB (13–14 GB RSS still 20 min after the abort) | — | 1 |
| 3 — fresh, meant to be uncapped ([json](./results/before-mixed-1G-run3-capped.json)) | 228.5 s | 54,861 | 183.4 MB | **21,797.2 MB** | 365.7 MB | 1 |

The abort line, identical in all three:

```
error: autoindex stopped with bulk/backend failures: bulk backend failed for 2439 item(s):
{"type":"engine_exception","reason":"[parent] real memory circuit breaker tripped: rss=15580MB >=
watermark=15564MB (94% of limit=16384MB); writes rejected to prevent an out-of-memory kill","status":429}
```

Two things were wrong, one on each side of the wire:

1. **The client treated the breaker as fatal.** The server answers a memory
   episode with HTTP 200 and per-item `status: 429`; the client retried only
   HTTP-level 429s and aborted on the first per-item one, leaving the mailbox
   un-journaled. The server released the breaker 0.3–10 s after each trip
   (four trips in run 1, 471 in run 2 — the node opened run 1's index and sat
   at the watermark for the whole retry). Fixed on this branch: rejected items
   are re-offered for up to 10 minutes.
2. **The server needs ~20 GB of RSS to ingest a 1 GB mailbox.** ~54k
   documents, 366 MB on disk, 19.7–21.8 GB VmHWM; the client peaked at
   184 MB. That is the engine's ingest memory, not the client's, and it is
   NOT fixed on this branch; it is characterised here and filed as an issue
   (see the PR). On a 16 GiB laptop the auto cap is 8 GiB and the breaker
   trips earlier.

Also observed in these runs, and fixed on this branch: the mailbox was
extracted on **one thread** (Phase B is one worker per file), at 4.7 MB/s of
source, with one PDF parser subprocess per attached PDF in series — the
`waiting_on=…mbox(1.0GB)` line sat at `pct=0.0 eta_s=unknown` for 190 s
because bytes were credited only when a file finished.

Run 3 was launched with `XERJ_MAX_PROCESS_MEMORY_MB=0` intending to lift the
cap; the governor treats a bare `0` as ambiguous and ignores it (spell it
`off`), so it is a third capped run and is reported as one. It reproduces run
1 to within 2 % on every figure.

### 20 MB smoke, same binary ([json](./results/before-mixed-20M.json))

790 entries, 21.1 MB mbox: **exit 3** (completed-with-junk: the two archives),
9.7 s wall, 2,102 documents (216.9 docs/s), autoindex peak 145.9 MB, server
peak 1,874.1 MB, index settled 15.2 MB (0.72 × mbox). Verify: every needle
kind exactly-once — body 9/9, latin-1 31/31, undeclared cp1252 12/12,
after-unquoted-`From` 1/1, **pdf-attachment 5/5** (a needle on a page inside
a PDF inside a base64 part inside the mbox — the chain a unit test cannot run,
because the test binary has no PDF worker), text-attachment 6/6, keep-note
12/12, drive-markdown 1/1; `replies_to` 413 = truth 413; `attachment_of`
773 = attachment documents 773; 0 empty "(no subject)" documents.

## After

Binary at `22605dd0`: the mailbox parsed on a `--workers`-wide pool, per-item
429s re-offered for up to 10 minutes. Same corpus, same node settings, same
shared box (1-minute load 58 at the start of these runs).

### 20 MB smoke ([json](./results/after-mixed-20M.json))

**exit 3**, 10.5 s wall, 2,102 documents, autoindex peak 181.7 MB (largest in
tree 178.7 MB), server peak 2,012.0 MB, index settled 15.2 MB (0.72 × mbox).
Verify, every kind exactly-once: body 9/9, latin-1 31/31, undeclared cp1252
12/12, after-unquoted-`From` 1/1, **pdf-attachment 5/5**, text-attachment
6/6, keep-note 12/12, drive-markdown 1/1; `replies_to` 413 = 413;
`attachment_of` 773 = 773; 0 empty documents. Identical to the pre-fix smoke
on every verify figure — the pool changes nothing about what comes out, which
is what the ordered-forwarding test promises. Wall time is the same at this
size (10.5 s vs 9.7 s, both under load): the run is dominated by the fixed
phases and the node, not by the 20 MB of extraction.

### 1 GB, run A ([json](./results/after-mixed-1G-runA.json)) — found the third defect

Same binary (`22605dd0`). **exit 1 at 236.4 s**, 61,442 documents on the
node (vs 54,192 before the fixes), autoindex peak 227.2 MB, server VmHWM
**22,112 MB**, index 390.6 MB at exit. The breaker tripped seven times; the
per-item re-offer engaged once ("re-offering 3092 rejected record(s)") and
the second offer was taken. Then the third trip held the server above the
watermark for **57 s** (RSS 19,526 MB against a 15,564 MB watermark — the
breaker stops admission, not work already admitted) and during it the server
answered a bulk with an **HTTP 429 status carrying a full bulk body** — a
shape the client had not seen: it went through the HTTP-level retry (six
attempts, ~8 s of backoff) and aborted the run. Fixed in the next commit
(`retry_loop`: a loading run re-asks a 429 for up to 10 minutes; a 429 whose
body is a bulk response is read item by item so only rejected items go
again; a one-shot client keeps the bounded budget). Verify on what landed: 60
of 60 checked needles of every kind found exactly once, except
after-unquoted-`From` 24/41 — the rest sat in the un-sent tail, as did every
edge (edges are written after a file's nodes are accepted).

What the pool changed, visible in the server log rather than in wall time
(the breaker bounds both runs): documents started arriving **74 s** after
the index was created, against **148 s** before the pool — the whole
mailbox is staged before its first bulk, and that staging halved on a box at
load 40–58 with PDF parsing capped at 4 subprocesses. The progress bar
reported `pct=40.0 eta_quality=good` at 60 s where it used to sit at 0.0.

### 1 GB, run B — capped, the final binary ([json](./results/after-mixed-1G-runB-capped.json))

Binary at `4c0c4685` (thread pool; per-item AND HTTP-level 429 re-asked for
up to 600 s). Node at its default auto tier (16 GiB cap, watermark
15,564 MB). Load 13 at the start, 0.7 at the end — a quiet box. **The run
does not complete**: exit 1 at **1,281 s**, 82,422 documents on the node,
autoindex peak 224.0 MB, server VmHWM **26,529 MB**, index 577 MB at exit.
The breaker engaged 27 times; from 87.1 % of the mailbox on, the server's
RSS sat at 15,548–15,633 MB against the watermark — engaged for 118 s and
147 s at a stretch, released for 3–4 s between — and the client, having
waited the full 600 s, aborted with the mailbox un-journaled. Verify on what
landed: every checked needle exactly-once except after-unquoted-`From`
32/41 (the tail was never sent); 0 edges (edges are written after a file's
nodes are accepted).

The client-side fixes on this branch turned "dies on the first 429" into
"waits ten minutes and then dies". They cannot make this run complete,
because the memory is the server's — see
[#948](https://github.com/xerj-org/xerj/issues/948).

### 1 GB, run C — uncapped, complete ([json](./results/after-mixed-1G-runC-uncapped.json))

Same binary, `XERJ_MAX_PROCESS_MEMORY_MB=off` (no process cap; memtable
budget 30,527 MB; the breaker never engaged). 1-minute load 11.3 at the
start (a build of ours finishing), 25.6 at the end (the node's own merges).

| | |
|---|---|
| exit | **3** — completed-with-junk (the two archives) |
| wall | **279.0 s**: ~170 s index phase, ~1 s graph resolution, **~97 s `finalize-count`** (autoindex waiting on the node's counts) |
| documents | **106,581** (106,551 from the mbox, 39,619 of them attachment records) |
| docs/s | **382** over the whole wall; 3.85 MB of source per second |
| peak RSS, autoindex | **296.3 MB** (largest in tree 296.3 MB) |
| peak RSS, server | **68,527.6 MB** VmHWM, read after the post-run settle; 27.9 GB observed at 83 % of the index phase |
| index on disk | 937.7 MB at exit, **760.4 MB settled** — **0.71 × the mbox** |
| state dir peak | 252 MB |
| verify | every needle kind exactly-once: body 60/60, latin-1 60/60, undeclared cp1252 60/60, **after-unquoted-`From` 41/41**, pdf-attachment 60/60, text-attachment 60/60, keep-note 12/12, drive-markdown 1/1; **`replies_to` 21,544 = truth 21,544**; **`attachment_of` 39,619 = attachment documents 39,619**; 0 empty documents |

The mailbox was fully staged **39 s** after its index was created (the bar
crossed its 45 % seam at 39.2 s), so the remaining ~130 s of the index phase
is the node accepting 106k documents and 61k edges — ~820 docs/s at the file
level. The node flushed its memtable 305 times (average 550 documents,
largest 3,973) and ran 24 merges during ingest and 2 after.

## What this says, plainly

- **The extractor is correct on this corpus and cheap.** Under 300 MB of
  client memory for a 1 GB mailbox, every planted needle found exactly once,
  every edge accounted for, and the parallel path emits the same records as
  the sequential one.
- **The server is not cheap.** 26.5 GB under the 16 GiB cap — and the run
  never finishes — or 68.5 GB uncapped, for an index that is 760 MB on disk.
  On a 16 GiB laptop the auto cap is 8 GiB, so the same wall arrives with a
  smaller mailbox; that is an expectation, not a measurement. Filed as
  [#948](https://github.com/xerj-org/xerj/issues/948) with these numbers. It
  is an engine issue; nothing in the extractor can fix it, and the docs on
  this branch say so instead of promising a laptop-sized run.
- **`docs/s` is a whole-run figure** and a third of the run is
  `finalize-count`.
- **Nothing here has run on a real Takeout export.** The corpus is synthetic
  by design so that it can be verified; a real export may differ in ways the
  generator did not think of.

