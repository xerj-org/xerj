# index-size — on-disk footprint of a force-merged XERJ index

Measures where the durable bytes are, so encoding changes (bitpacking,
deltas, dictionary reuse, zstd dictionaries) land with before/after
evidence instead of estimates. Design and staged plan:
[DESIGN.md](./DESIGN.md). Epic: issue #1038.

## Run it

```sh
cargo build --release -p xerj-server           # scoped build, per repo rules
benchmarks/index-size/run.sh engine/target/release/xerj /data/sizebench 9530 100000
```

`<work-dir>` must be on a real disk (the script refuses tmpfs — du figures
would be RAM figures). It boots a throwaway node on the given port block
with its own data dir, the default lexical embedder and auth on, indexes
100k docs, force-merges to one segment, waits for du-stable-30s, and
writes `result-<LABEL>-<LEVEL>.json` plus a printed table:

```text
== index-size base (level=balanced, 100000 docs, forcemerge 1) ==
durable (excl .wal):     10,415,477 B   (.wal 552,154 B, excluded)
  .post            5,570,604 B  53.5 %
  .seg             2,072,355 B  19.9 %
  .dv              1,819,138 B  17.5 %
  ...
```

(the numbers above are the 2026-07-09 measured split, shown as shape —
see the comparability caveat).

`LEVEL=best` re-runs with `[compression] level = "best"` in the node
config — the merge-path encoder knob, honoured at merge only, never at
flush (the flush path is pinned to zstd 3 after the ingest regression;
see the test `compression_level_reaches_the_merge_encoder_but_never_the_
flush_path`). That is the level ceiling any format-level change must beat
or coexist with.

## Corpus

`demo/data/extras/chat-events.ndjson` (4,008 events, seed-42
regenerable), cycled to `DOCS`, each doc enriched with a deterministic
multi-sentence `body` (in-script pool, see run.sh) and a unique `doc_id`.
Mapping is explicit: 6 keyword (incl. `doc_id`), 1 text, 4 long
+ 1 double, 1 date, 1 boolean — the same shape as the 2026-07-09
diagnosis corpus.

## Baseline (measured, cited — do not re-derive)

From [demo/playbooks/DISK_SIZE_2026-07-09.md](../../demo/playbooks/DISK_SIZE_2026-07-09.md),
100k docs force-merged to one segment:

| zstd level | total | .seg | .dv | .post | .meta |
|---|---:|---:|---:|---:|---:|
| 3 (flush default) | 10,415,477 | 2,072,355 | 1,819,138 | 5,570,604 | 229,560 |
| 9 | 9,789,136 | 1,726,778 | 1,733,544 | 5,406,891 | 198,112 |
| 12 | 9,737,700 | 1,725,519 | 1,698,279 | 5,394,763 | 195,328 |
| 19 | 9,022,248 | 1,561,830 | 1,436,158 | 5,119,175 | 181,281 |

`body.post` alone was 3,928,612 B (37.7 % of the total at level 3).

**Comparability caveat:** that run's body-text generator was not
committed, so this harness's absolute totals are NOT comparable with the
table above; the component structure is. For a before/after on any
change, run THIS harness on the base commit and on the change, same
`DOCS`, same `LEVEL`, and commit both `result-*.json` files here under
`results/` — the mbox-ingest before/after convention.

## CI

`demo/playbooks/ci-check.sh` runs a 20k-doc pass informationally on every
PR (same job as the smoke suite — no new CI job) and prints the
per-extension table into the log. CI numbers use `[profile.ci-test]`
builds, which must never produce a published number
(`engine/Cargo.toml` profile comment); they gate regressions and back PR
evidence only. The public disk comparison vs Elasticsearch stays on the
manual `bench-matrix.mjs` → `SCORECARD.md` path, which needs a real ES
node and a release-profile binary.

## What is deliberately NOT measured here

- **WAL bytes** — reclamation is async and dominates whole-dir variance;
  every figure here is durable-excluding-`.wal` (same convention as the
  in-tree integration harness).
- **The public 1.61× vs ES cell** — its basis is an un-force-merged
  ~2.38M-doc index after the mixed-writer benchmark; a force-merged 100k
  CI number must never drift into that cell's context.
