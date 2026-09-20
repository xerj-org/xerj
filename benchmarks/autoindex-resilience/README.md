# autoindex resilience: before / after captures

Raw captures behind the numbers quoted for
[#929](https://github.com/xerj-org/xerj/issues/929) (one refused dataset aborted
the whole run), [#931](https://github.com/xerj-org/xerj/issues/931) (progress
read `scan 100% / stalled` for the whole indexing phase) and
[#930](https://github.com/xerj-org/xerj/issues/930) (`xc-index.sh --fresh`
failed on every previously indexed corpus), and the two defects the full-corpus
verification found after them:
[#944](https://github.com/xerj-org/xerj/issues/944) (an HTTP 429 from the
node's memory circuit breaker ended the run) and
[#955](https://github.com/xerj-org/xerj/issues/955) (the catalog was one
`_bulk` request, over the engine's 50,000-action limit).

This is a correctness record, not a performance benchmark. Nothing here is a
speed claim. Timings are included because they are in the captures; they were
taken on a 32-core machine shared with other builds and are not reproducible
to the second.

Every number in the docs that cites this folder is derived from these files by
`summarize.py`, not copied by hand:

```sh
python3 summarize.py                 # prints results.json from the four complete captures
python3 summarize.py --check         # exit 1 if results.json is stale
python3 summarize.py some.stderr.txt # summarize any `--progress plain` capture
```

Local paths in the captures were shortened (`~/.xerj-code/corpora/…`, `./…`).
Nothing else was changed.

## Files

| File | What it is |
| --- | --- |
| `before-rc74.stderr.txt` | v1.0.0-rc.74, full corpus, `--no-graph --progress plain`. #929 and #931 in one run. |
| `slice-rc74.stderr.txt` | v1.0.0-rc.74 client on the slice (see below): the same #929 refusal on the same field, 39.6 s in. |
| `slice-after.stderr.txt` | This branch's client, same slice, same command: completes, exit 3. |
| `slice-after.node.txt` | The node side of both slice runs: counts per repository, the trigger field's mapping, memory. |
| `after-955.full-corpus-resume.stderr.txt` | This branch with #955 fixed, resuming the full-corpus generation that `before-955.stderr.txt` could not finish. Complete, nothing trimmed. |
| `after-955.node.txt` | The node side of that resume: counts, catalog size, request-body peak, memory, how the client reached the node. |
| `before-955.stderr.txt` | This branch before #955: the full corpus, every operation applied, then exit 1 in `finalize-catalog`. First 30 and last 40 of 2,815 lines. |
| `before-955.node.txt` | The node side of that run: governor lines, ingest-memory peaks, sampled resident memory. |
| `limits-real-node.txt` | #955 and the #944 terminal case against a real engine: `max_actions_per_bulk = 64`, `max_body_bytes` of 96 KiB and 64 KiB, a 64 MiB memory cap, and the resume after it, each beside a control run. |
| `results.json` | `summarize.py` over the four complete captures: `before-rc74`, `slice-rc74`, `slice-after`, `after-955.full-corpus-resume`. |
| `after-fix.small-repo.stderr.txt` | This branch, a 231-file repository, `--progress plain --progress-interval 1`: a complete stream that fits on a screen. |
| `after-fix.resume-probe.stderr.txt` | This branch: resuming the full-corpus generation after it was interrupted. Stopped on purpose after 100 s. |
| `after-fix.small-repo.tty.txt` | The same repository under a pseudo-terminal: what the TTY bar draws. |
| `refusal-e2e.run1.stderr.txt`, `refusal-e2e.run1.stdout.txt` | A forced refusal on a throwaway node (see below). |
| `refusal-e2e.run2-noop.stderr.txt` | A no-op re-run of the same command. |
| `fresh-before-rc74.legacy-state.txt` | rc.74 refusing a state directory written before `generation-v1`. |
| `fresh-before-rc74.committed-generation.txt` | rc.74 refusing `--fresh` over a committed generation. |
| `before-944.full-corpus.stderr.txt` | This branch before #944 was fixed: the full corpus, aborted at 60.4% by a per-item 429. Trimmed to its first 30 and last 40 lines (the 1,479 lines in between are `index` progress and bulk-concurrency lines); the marker line says so. |
| `before-944.node.governor.txt` | The node's governor log lines for that run: the memory cap it chose and every circuit-breaker engagement and release. ANSI colour stripped, nothing else changed. |
| `before-944.whole-request-429.stderr.txt` | This branch with the per-item fix, resuming that generation on the same node: aborted 1039.6 s in by a 429 on the *whole* bulk request, which the transport retry gave up on after six attempts. Complete, nothing trimmed. |

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

## The full-corpus run found a fourth defect (#944)

The full-corpus verification with the final binary of #929/#930/#931 did not
finish. At 60.4% of the `index` phase, 5,122.5 s in, one bulk came back
HTTP 200 with 747 of its items answered `status: 429` by the engine's real
memory circuit breaker, and the client aborted:

```text
xerj-done ok=false exit=1 reason=aborted wall=5122.5s
error: prepared bulk contained 747 rejected items: {"type":"engine_exception","reason":"[parent] real memory circuit breaker tripped: rss=15679MB >= watermark=15564MB (94% of limit=16384MB); writes rejected to prevent an out-of-memory kill","status":429}
```

`before-944.node.governor.txt` is the node's side: a 16 GiB automatic cap on a
119.2 GiB machine, and 14 breaker engagements in 27 minutes, every one
released within 0.1–3.1 s. The condition the run died on had cleared about a
second later. The same capture holds 117 `raising bulk concurrency` lines for
11 shrinks.

The fix (re-send only the rejected items, give up 600 s after the bulk was
first offered, `bulk_retries=N` on the terminal line) is verified by unit tests on a stub server and by an end-to-end test on
each of the two indexing paths.

Resuming the interrupted generation with that fix found the second shape of
the same defect (`before-944.whole-request-429.stderr.txt`): 1039.6 s in, at
20.8% of the 17,398 operations that remained, the engine answered a whole bulk
HTTP 429 and the client's transport retry gave up after six attempts:

```text
xerj-done ok=false exit=1 reason=aborted wall=1039.6s
error: _bulk: HTTP 429 Too Many Requests: {"took":49,"errors":true,"items":[{"index":{"_index":"xc-xerj-search-elasticsearch-x-pack-25",…"status":429,"error":{"type":"engine_exception","reason":"[parent] real memory circuit breaker tripped: rss=15634MB >= watermark=15564MB (94% of limit=16384MB); …
```

A whole-request 429 now joins the same patience loop as a per-item one
(unit tests: re-sent like a per-item one; items in the 429 body honoured; handed
back after patience, not after six attempts; and a generated-path end-to-end
run answered three whole-request 429s in a row).

The node's side of that second abort is not a client defect and is filed as
[#950](https://github.com/xerj-org/xerj/issues/950): the resumed run began
with the node's resident memory already at the watermark — 14.8 GB of anonymous
memory for 1.2 GB on disk, unchanged 2.5 hours after the last write — so
nothing was ever accepted again. On the default 16 GiB tier this corpus cannot
finish on that node; the full-corpus result below was taken with a raised
`XERJ_MAX_PROCESS_MEMORY_MB`, and says so.

## The full-corpus run found a fifth defect (#955)

The verification run on a raised cap (`XERJ_MAX_PROCESS_MEMORY_MB=49152`,
fresh node, this branch at `702d4188` as server and client) indexed the whole corpus. It
applied all 47,444 sealed operations, 821,840 records, and its node never
engaged the breaker. Then it failed at the very end, 10,336 s in
(`before-955.stderr.txt`):

```text
xerj-progress phase=finalize-catalog basis=items pct=99.7 items=1521/1526 … elapsed_s=10335.5
xerj-done ok=false exit=1 reason=aborted wall=10336.0s
error: prepared bulk contained 1 rejected items: {"type":"engine_exception","reason":"bulk request contains 102258 lines (~51129 actions); exceeds max_actions_per_bulk of 50000","status":413}
```

The catalog, one document per file, per dataset and per run, went out as ONE
`_bulk`: 51,129 actions in 31.9 MB (`http_body` peak 31,910,392 bytes in
`before-955.node.txt`), over the engine's default `limits.max_actions_per_bulk`
of 50,000. The fix windows every body at 10,000 actions (the catalog also at
`--bulk-mb`) and halves a request the node still refuses for its size.

## After, full corpus (this branch)

Resuming the generation above, with its journal and its node's data copied, on
the #955 binary (`after-955.*`):

```text
autoindex: resumed and committed pending corpus generation from durable source
xerj-done ok=true exit=3 reason=completed-with-junk wall=415.0s files=47444 records=821840 generation=1 code_files=34324 code_files_indexed=34324 code_files_junked=0
```

- Phases: `starting`, `replay`, `index` (0/0, nothing left to apply),
  `finalize-catalog` 49.7 s, `finalize-refresh` 9.2 s for 1,527 indices,
  `finalize-verify` 331.6 s for 47,444 read-backs.
- `autoindex-catalog`: 0 documents before, 51,129 after.
- The node's largest request body: 8,388,241 bytes, under `--bulk-mb 8`
  (was 31,910,392).
- The rc.74 trigger field, `Check streams can't be enabled with existing
  logs.otel indices` in `xc-xerj-search-elasticsearch-modules-10-basic-13`, is
  mapped `text`; `xerj autoindex map` prints no refused datasets.
- The exit is 3 because 1,089 files were junk or skipped, as in every capture
  of this corpus.

How the client reached the node: the journal pins
`url=http://localhost:9540`, and a resume must use the URL the journal was
created with. The restarted node listened on a private port, and the client
reached it through a local TCP forwarder (`HTTP_PROXY`). Nothing listened on
9540 during the run.

### What was and was not verified at full scale

| | Verified? | Evidence |
| --- | --- | --- |
| #929/#931 on the full corpus: no refusal, the phases through `finalize-catalog`, 47,444 operations applied | yes, with the branch at `702d4188` | `before-955.stderr.txt` |
| #955: the catalog write that failed commits, on the same data | yes, with the #955 binary, by resuming | `after-955.*` |
| One uninterrupted run from an empty node to a commit with the final binary | **no** | — |
| Any full-corpus run on the default 16 GiB memory tier | **no, and it cannot finish there today** | `before-944.*`, [#950](https://github.com/xerj-org/xerj/issues/950) |

## The size and back-pressure paths against a real engine (this branch)

The end-to-end tests for #955 and #944 run against a stub. `limits-real-node.txt`
runs the same paths against the engine, on the sonic repository, one fresh
node per setting, each compared with a control run on default limits
(`records=1663`, 236 catalog documents). Every row was RE-RUN on 2026-09-20,
after the reconciliation with #949 replaced this branch's 120 s patience with
main's 600 s: the numbers below come from that run, not from the 2026-09-19
one they replace.

| Node setting | Result |
| --- | --- |
| `max_actions_per_bulk = 64` | one 69-action request refused (item 413), halved; `ok=true exit=3 records=1663 bulk_splits=1`, 236 catalog documents |
| `max_body_bytes = 98304` | one 120,443-byte request refused (HTTP 413, `length limit exceeded`), halved; `ok=true exit=3 records=1663 bulk_splits=1`, 236 catalog documents |
| `max_body_bytes = 65536` | a single 70,471-byte record cannot be cut: `exit=1 reason=aborted`, error names `limits.max_body_bytes` |
| `XERJ_MAX_PROCESS_MEMORY_MB=64` | breaker engaged from start-up; 600 s of re-sends (19 `server is shedding load` notices, one per 30 s), then `ok=false exit=1 reason=server-backpressure wall=609.1s ops_applied=0 ops_remaining=231` |
| the same state directory, node restarted on the default cap, same command | `resumed and committed`; `ok=true exit=3 records=1663`, 236 catalog documents |

## After, a slice that holds the rc.74 trigger (this branch)

A corpus that completes in minutes and still contains what the full corpus
failed on: tantivy, quickwit, meilisearch and sonic whole, plus 419 files of
`elasticsearch/modules` (every `src/yamlRestTest` file and all of
`modules/streams`, which holds the YAML test whose key rc.74 elected
`semantic_text`). 4,479 files, fresh node per run, default configuration
(16 GiB auto tier).

- rc.74 client (`slice-rc74.stderr.txt`): `xerj-done ok=false exit=1
  reason=aborted wall=39.6s`, on the same mapping refusal of the same field,
  `nested semantic_text field [Check streams can't be enabled with existing
  logs.otel indices] is not supported`, in
  `slice-elasticsearch-modules-10-basic-13`. 0 records. Its stream also shows
  #931: 4 lines of `scan` at `pct=100.0` reading `stalled`.
- This branch's client (`slice-after.stderr.txt`): `xerj-done ok=true exit=3
  reason=completed-with-junk wall=525.6s files=3680 records=159666
  generation=1`, all nine phases, 0 `scan` lines at 100%. `_count` 159,666:
  tantivy 111,612, quickwit 29,883, meilisearch 13,607, sonic 1,663,
  elasticsearch 2,901. The trigger dataset holds 8 records and maps the field
  as `text`. The node's peak resident memory was 3.8 GB and its breaker never
  engaged.

## What the after-run also showed

With the `index` phase visible for the first time, its speed is readable: about
6 files per second on this corpus, because the generated path applies one file
at a time. That is a pre-existing property of the path, not a regression from
this change, and it is tracked separately in
[#933](https://github.com/xerj-org/xerj/issues/933).
