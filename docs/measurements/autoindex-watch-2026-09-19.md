# Measurement record — `xerj autoindex --watch` vs re-running, 2026-09-19

This file exists so every number in `docs/LIVE_REINDEXING.md` and in
`content/answers/keep-index-current-as-files-change.md` can be traced to a
command someone else can run.

## Machine and build

| | |
|---|---|
| Host | 32 cores, 119 GB RAM, NVMe, Linux 7.0.0-27-generic |
| Binary | `cargo build --release -p xerj-server` on branch `feat/autoindex-watch` |
| Server | one throwaway node, `xerj --port 13240 --data-dir <tmp>`, auth on, default (lexical) embedder |
| Corpus | 10,002 files / 6,150,458 bytes of content / 101 directories, synthetic mixed `.md`/`.txt`/`.py`/`.json`/`.csv` |
| Load | **shared box.** Other agents were running `cargo build --release` (fat LTO) throughout; `uptime` read `load average: 9.40, 10.93, 11.63` during the watch session. Section 1's numbers were captured earlier, on a quieter box; sections 2-4 come from one script run under that load. Where the two overlap the difference is stated rather than averaged away. |

The embedder is XERJ's default **lexical** feature hashing. No neural embedding
ran, and none of these numbers is a semantic-search measurement.

`--profile quick` was used for the test gate and for the functional smoke test;
every number below is from a `--release` binary, per `engine/Cargo.toml`'s rule
that `quick` is never valid for published measurement.

## Corpus

```sh
# 100 directories x 100 files: .md / .txt / .py / .json / .csv, ~600 bytes each,
# plus two files a shell append created (see section 4 for why that matters)
find /home/claude/scratch-watchlocal/corpus10k -type f | wc -l     # 10002
find /home/claude/scratch-watchlocal/corpus10k -type d | wc -l     # 101
find ... -type f -printf '%s\n' | awk '{t+=$1} END{print t}'       # 6150458
```

`find` and the run agree: `autoindex` prints `10002 files` for this tree.

## 1. The default graph path does not index a content change on a re-run

Captured earlier in the session, on the quieter box (same commands, same corpus,
before the concurrent builds started).

**Read the caveat in section 4 before quoting this section as "a CHANGED file":**
the shell append below wrote `pkg042/file017.md` for a file whose generated
extension is `.txt`, so it created a NEW file rather than modifying one. Both
cases take the same branch — a new content identity is not in the frozen plan —
but only section 4 measures the modification itself.

```
$ xerj autoindex $CORPUS --url http://localhost:13240 --state-dir state-base \
    --prefix base --progress plain --max-minutes 0        # first run
xerj-done ok=true exit=0 reason=completed wall=13.85s files=10001 records=60000 …
  /usr/bin/time: wall=13.85 user=3.58 sys=1.77 rss=326496kB

$ …same command again, nothing changed
xerj-done ok=true exit=0 reason=completed wall=1.8s files=0 records=60000 …
  /usr/bin/time: wall=1.85 user=0.25 sys=0.20 rss=136768kB

$ echo "changed body zeta zeta zeta" >> $CORPUS/pkg042/file017.md
$ …same command again
resuming from journal …/journal.ndjson (10000 files already done)
autoindex: 1 file(s) appeared after the resume plan was frozen and were NOT indexed:
  not in plan, skipped: pkg042/file017.md
autoindex: re-run with --fresh to rebuild the plan in place and index them
xerj-done ok=true exit=3 reason=completed-with-junk wall=1.9s files=0 … skipped_appeared=1
  /usr/bin/time: wall=1.88 user=0.27 sys=0.22 rss=138608kB
```

This is why `--watch` requires `--no-graph`: on the graph path the edit is not
indexed at all, and the only documented repair is a full `--fresh` rebuild.

## 2. The `--no-graph` generated route, without a watcher

```
$ X="xerj autoindex $CORPUS --url http://localhost:13240 --state-dir state-gen \
     --prefix gen --no-graph --progress plain --max-minutes 0"

$ $X                       # genesis
xerj-done ok=true exit=0 reason=completed wall=247.7s … generation=1
  /usr/bin/time: wall=247.74 user=105.16 sys=3.69 rss=329184kB

$ $X                       # nothing changed
xerj-done ok=true exit=0 reason=completed wall=0.9s files=10001 records=50001 generation=1
  /usr/bin/time: wall=0.93 user=0.79 sys=0.39 rss=234848kB

$ echo "watch marker unique-token-qqzz" >> $CORPUS/pkg042/file018.md && $X
xerj-done ok=true exit=0 reason=completed wall=39.4s files=10002 records=50002 generation=2
  /usr/bin/time: wall=39.39 user=3.69 sys=2.11 rss=298324kB

$ curl …/gen-*/_search?q=unique-token-qqzz   → "total":{"value":1}
```

The change IS indexed and IS searchable. Cost: 39.4 s for one edited file in a
10,001-file tree, of which 38 s is spent after the per-file scan phase — the
generation seals a snapshot that copies and re-verifies every file in the corpus
(`sync_executor.rs::create_snapshot_inner`: one `copy_synced` plus two
`content::verify` calls per file, serially, each fsynced). `--watch` does not
change that; it is the measured next lever for this feature.

