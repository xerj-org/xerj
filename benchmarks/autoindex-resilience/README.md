# autoindex resilience: before / after captures

Raw captures behind the numbers quoted for
[#929](https://github.com/xerj-org/xerj/issues/929) (one refused dataset aborted
the whole run), [#931](https://github.com/xerj-org/xerj/issues/931) (progress
read `scan 100% / stalled` for the whole indexing phase) and
[#930](https://github.com/xerj-org/xerj/issues/930) (`xc-index.sh --fresh`
failed on every previously indexed corpus).

This is a correctness record, not a performance benchmark. Nothing here is a
speed claim. Timings are included because they are in the captures; they were
taken on a 32-core machine shared with other builds and are not reproducible
to the second.

Every number in the docs that cites this folder is derived from these files by
`summarize.py`, not copied by hand:

```sh
python3 summarize.py                 # prints results.json from the two full-corpus captures
python3 summarize.py --check         # exit 1 if results.json is stale
python3 summarize.py some.stderr.txt # summarize any `--progress plain` capture
```

Local paths in the captures were shortened (`~/.xerj-code/corpora/…`, `./…`).
Nothing else was changed.

## Files

| File | What it is |
| --- | --- |
| `before-rc74.stderr.txt` | v1.0.0-rc.74, full corpus, `--no-graph --progress plain`. #929 and #931 in one run. |
| `after-fix.stderr.txt` | This branch, same command, same corpus. |
| `results.json` | `summarize.py` over the two files above. |
| `after-fix.small-repo.stderr.txt` | This branch, a 231-file repository, `--progress plain --progress-interval 1`: a complete stream that fits on a screen. |
| `after-fix.resume-probe.stderr.txt` | This branch: resuming the full-corpus generation after it was interrupted. Stopped on purpose after 100 s. |
| `after-fix.small-repo.tty.txt` | The same repository under a pseudo-terminal: what the TTY bar draws. |
| `refusal-e2e.run1.stderr.txt`, `refusal-e2e.run1.stdout.txt` | A forced refusal on a throwaway node (see below). |
| `refusal-e2e.run2-noop.stderr.txt` | A no-op re-run of the same command. |
| `fresh-before-rc74.legacy-state.txt` | rc.74 refusing a state directory written before `generation-v1`. |
| `fresh-before-rc74.committed-generation.txt` | rc.74 refusing `--fresh` over a committed generation. |

## The corpus and the command

The reference-coding corpus `xerj-search`: five repositories (tantivy,
meilisearch, quickwit, sonic, elasticsearch). Both captures report the same
inventory: 50,061 files walked after ignore rules, 48,533 unique files scanned
(520,892,779 bytes), 1,526 datasets inferred and 1,089 junk or skipped files.
Indexed into a throwaway node with the default lexical embedder:

```sh
xerj autoindex ~/.xerj-code/corpora/xerj-search \
  --url http://localhost:9540 --prefix xc-xerj-search --no-graph \
  --state-dir ./state --progress plain --yes
```

## Before (v1.0.0-rc.74)

- Phases reported: `walk`, `hash`, `scan`. Nothing else, for the whole run.
- 48 progress lines read `phase=scan pct=100.0 eta_quality=stalled`, with
  `since_progress_s` climbing to 250.0. The run was doing real work the whole
  time.
- One dataset's mapping was refused with an HTTP 400. The run ended
  `xerj-done ok=false exit=1 reason=aborted wall=270.0s`. Nothing was indexed:
  not the refused dataset, and not the other 1,525.

## A forced refusal (this branch)

The field name that triggered the rc.74 refusal is no longer elected
`semantic_text`, so the full corpus does not reproduce a refusal any more. To
exercise the structural fix against a real node, a refusal was forced: a
three-file corpus (two CSV files, one Markdown file), with the CSV dataset's
index created beforehand so that `value` was mapped as `long` where the
inferred mapping wants `keyword`.

```text
autoindex: dataset csv REFUSED by the server — 2 file(s) recorded as junk and NOT indexed; every other dataset continues. …
xerj-done ok=true exit=3 reason=completed-with-junk wall=0.6s files=1 records=1 generation=1 datasets_refused=1 files_refused=2 …
```

The other dataset was indexed (`_count` 1), the refused index held 0 documents,
the two CSV files carried `status: junk` with the server's reason in
`autoindex-catalog`, and `xerj autoindex map` printed a "Refused datasets — NOT
indexed" section above the dataset table. A no-op re-run exited 3 again with
the same counts.

Not covered by a run: a refusal caused by anything other than a field-type
conflict. Every HTTP 400 takes the same code path, but only this cause was
forced end to end.

## After, small repository (this branch)

`sonic`, 231 files after ignore rules, on an otherwise idle node.

- Phases reported, in order: `walk`, `hash`, `scan`, `prepare`, `snapshot`,
  `index`, `finalize-catalog`, `finalize-refresh`, `finalize-verify`.
- 0 lines read `scan` at `pct=100.0`.
- `index` carried a denominator in both units, 1.0 s into the phase:
  `pct=81.7 items=169/231 bytes=1881018/2303369 eta_s=unknown`. The ETA is
  `unknown` because the phase was one second old, which is the honest answer.
- Ended `xerj-done ok=true exit=3 reason=completed-with-junk wall=3.9s files=231 records=1663`.
- Under a pseudo-terminal the bar drew the same nine phases; `index` reached
  `78.1% | 160/231 items | 1.7MB/2.2MB`.

## After, a resumed run (this branch)

The first full-corpus attempt on this branch was interrupted on purpose with
12,890 of 47,444 operations committed (the binary was rebuilt after a review
finding, and "verified" should mean the binary that ships). Resuming that
generation with the final binary:

- reported `starting` for about 13 s while the journal loaded, then `replay`,
  then `index`;
- never reported `scan` or `snapshot`;
- opened `index` at `items=0/34554 bytes=0/805561870` — the 34,554 operations
  still to apply, not all 47,444 and not pre-credited with the 12,890 an
  earlier attempt wrote. The byte total dropped from 1,099,789,983 to match.

## Probes taken while a full-corpus run was in flight

Single ad-hoc measurements against the throwaway node, on a busy machine. They
explain two decisions; they are not benchmarks.

| Probe | Result | What it decided |
| --- | --- | --- |
| 60 x `POST /<index>/_refresh` on a node holding 1,526 datasets | 6.36 s, about 106 ms each; projects to about 160 s for all 1,526 | The pre-verify refresh loop became the `finalize-refresh` phase instead of holding `finalize-verify` at `0/N`. |
| `POST /<index>/_delete_by_query` matching nothing, largest index (18,734 docs), 8 samples | 46-57 ms, the same with and without `?refresh=true` | Recorded in #933: the per-file pre-delete is about a third of each file's cost; the refresh flag is not the cost. |
| `_bulk` of one small document, 5 samples | about 4 ms after the first | Recorded in #933: the bulk round trip is not the cost. |
| journal-style append + `fsync`, 20 samples | 0.5 ms median | Recorded in #933: the journal is not the cost. |

## After, full corpus (this branch)

PENDING — the run was still in its `index` phase when this file was first
committed. This section is replaced with the terminal line, the record count
and the per-phase summary when it ends.

## What the after-run also showed

With the `index` phase visible for the first time, its speed is readable: about
6 files per second on this corpus, because the generated path applies one file
at a time. That is a pre-existing property of the path, not a regression from
this change, and it is tracked separately in
[#933](https://github.com/xerj-org/xerj/issues/933).
