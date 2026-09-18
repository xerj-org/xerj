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
| `after-fix.small-repo.stderr.txt` | This branch, a 231-file repository, `--progress plain`: a complete stream that fits on a screen. |
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

`sonic`, 231 files after ignore rules.

- Phases reported, in order: `walk`, `hash`, `scan`, `prepare`, `snapshot`,
  `index`, `finalize-catalog`, `finalize-verify`.
- 0 lines read `scan` at `pct=100.0`.
- `index` carried a denominator in both units:
  `items=203/231 bytes=2159904/2301706 eta_s=0.6 eta_quality=good`.
- Ended `xerj-done ok=true exit=3 reason=completed-with-junk wall=13.1s files=231 records=1663`.
- Under a pseudo-terminal the bar drew the same eight phases; `index` reached
  `96.8% | 221/231 items | 2.1MB/2.2MB | eta 1s`.

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
