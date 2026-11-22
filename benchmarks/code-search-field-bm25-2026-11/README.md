# `xerj code` field lanes, and markdown behind code in `xerj search`

> **Round 7 (2026-11-22): the name lane was dropped.** A held-out
> re-measure put its cost at +135% to +138% `took` p50 on the 188k-record
> corpus, against a decision bar of 25%. On held-out questions it added
> +0.023 nDCG@10. The identifier and exact-ID lanes stay. They cost +5%
> `took` p50 at load 5-16 and +18% at load 24-68, and they keep a held-out
> gain with zero losses. See [Round 7](#round-7-held-out-questions-and-the-name-lane-decision).
> The sections below are the original run, which still measured the name lane.

**Run:** 2026-11-21, one Linux x86_64 host shared with other builds. Load
averages are next to every timing; they ran from 58 to 88, so the timings are
upper bounds, not a speed claim. Node: one private `--insecure` node on port
24100 with a throwaway data dir, default `--embed-mode lexical`. The server
binary is the same for both arms (this change is client-side only); each arm
is a client binary:

| Arm | Source | sha256 |
|---|---|---|
| base | main `f22ddd5b2` | `275deabfb3702b60975a7facfa4a949424262211dc1713da76f40c337da49c00` |
| new | this branch | `e833ad9d8b27e4545612d68ac912ffa4038a8a3117ab4e8df2513fd4e5a49cf3` |

Issues: #1788 (field-aware ranking and exact IDs in `xerj code`), #2513
(docs above code; this run covers the markdown follow-up and the batch tool).
Losses are listed, not dropped.

## What changed

`xerj code` (bm25 mode, the default) sent one `multi_match` over `body`,
`defs`, `title` (plus the recall legs). On an autoindex code corpus `title`
is a **keyword**: the file name on a file record, the symbol name on a symbol
record. So the `multi_match` matched it only when the whole query was the
name, and a symbol record (no `body`) was reachable only by its exact name.
The body now adds three lanes, each gated on the mapping that was read:

| Lane | Clause | Score | Can match alone |
|---|---|---|---|
| name | per query word (3+ chars, stop words out, at most 8): case-insensitive `*word*` wildcard on a keyword `title` | constant 5 | no (adds score) |
| identifier | 2-6 plain words spelled `snake_case`, `camelCase`, `PascalCase`, `SCREAMING_SNAKE`: `term` on keyword `name` | constant 500 | yes |
| exact ID | CVE / GHSA / RUSTSEC / PYSEC / GO IDs and `pkg@version` (the #3336 detector): `term` on keyword `id`/`aliases`/`name` (3000), phrase on `body`/`text` | constant 3000 / 2500 | yes |

Weights: the coverage tiers (#2327) add 1000 for half the words and 1000 more
for all of them, so the name lane only reorders inside a tier, an identifier
match beats files that share some words but not a file that has all of them,
and a file that cites an ID beats both tiers. `xerj search` keeps its #3336 ID
phrase (BM25-scaled, boost 6); only `xerj code` uses the 2500 constant,
because a code corpus has no keyword `id` to carry it.

For `xerj search` / `xerj_search` / `xerj_batch_search` (#2513): a prose markup
file (`.md`, `.markdown`, `.mdx`, `.rst`, `.adoc`, `.asciidoc`) counts as
documentation wherever it sits (`user-feedback/`, `tools/`, `CHANGELOG.md`),
not only under `docs/` or as a README. A question that names `markdown`,
`changelog` or `notes` is not demoted. `xerj_batch_search` items now run the
same doc/sparse-definition pass `xerj_search` runs (before: test penalty only).

## Question sets (all frozen before any arm ran on them)

| Set | n | Corpus | Query | Relevant |
|---|---:|---|---|---|
| `sep` A/B/C | 300 | `engine/crates/**/*.rs` at `f9dcf626` (537 files, 18,379 records) | the committed [`search-exact-phrase`](../search-exact-phrase/) questions: A exact string, B words of a `fn` name, C the `fn` name | the file |
| `lucene` | 8 | Lucene checkout (7,547 files, 188,125 records) | the 8 single-file questions of [`grep-vs-retrieval-lucene`](../grep-vs-retrieval-lucene/questions.json) | `truth_file` |
| `javadoc` | 200 | same Lucene corpus | first sentence of a top-level class Javadoc in `lucene/core/src/java`, seeded sample ([`make_javadoc_set.py`](harness/make_javadoc_set.py)) | the declaring file |

Caveats, stated up front:

- `sep` B is built from `fn` names, so the identifier lane fits it by
  construction. `javadoc` sentences often name the class
  (`An Analyzer builds TokenStreams`), which favours the name lane. The
  8 hand-written Lucene questions are the only set built for neither.
- `crates/xerj-engine/src/index.rs` (2.7 MB) is over the autoindex size cap,
  so the 21 `sep` questions about it score 0 in both arms.
- The weights were picked on these sets (sweep below), then the final binary
  was run once per arm. There is no separate held-out set; `javadoc` was
  created after the first sweep and only used for the last five variants.

## Results: `xerj code`, nDCG@10 (linear gain, as `xerj eval`)

[`harness/code_eval.py`](harness/code_eval.py), k=10, distinct `ax_path`,
paired sign-flip test ([`harness/compare.py`](harness/compare.py), 10,000
rounds, seed 42). The node was restarted before each pass; passes ran
base, new, new, base and the two passes of each arm returned identical
rankings ([`passes.log`](results/passes.log)).

| Set | base | new | delta | wins/losses/ties | p | rank 1 | top 10 |
|---|---:|---:|---:|---|---:|---|---|
| sep all (300) | 0.6699 | **0.6917** | +0.0218 | 16/21/263 | 0.044 | 167 → 175 | 237 → 243 |
| sep A exact string | 0.5505 | 0.5600 | +0.0094 | 7/14/79 | 0.62 | 32 → 33 | 78 → 79 |
| sep B words of a name | 0.5392 | 0.5952 | +0.0560 | 9/7/84 | 0.039 | 43 → 50 | 67 → 72 |
| sep C identifier | 0.9200 | 0.9200 | 0 | 0/0/100 | 1 | 92 → 92 | 92 → 92 |
| lucene (8) | 0.4814 | 0.5561 | +0.0747 | 3/0/5 | 0.24 | 2 → 3 | 6 → 6 |
| javadoc (200) | 0.8597 | **0.9280** | +0.0683 | 33/3/164 | 0.0001 | 147 → 170 | 191 → 197 |

Raw: [`code-compare.txt`](results/code-compare.txt), per query
`results/code-<set>-<arm>-p<pass>.json`.

**Losses.** `sep` A loses more queries than it wins (14 vs 7): the name lane
lifts test files whose names hold a query word (`restart must recover exactly
once`: `cluster_metadata_survives_restart.rs` and `failed_index_recovery.rs`
now sit above the expected `index_store.rs`, rank 1 → 3). `sep` B
`run multi terms` falls 1 → 6 behind `multi_valued_keyword_terms.rs` and other
test files named `multi_*`, and `read search fields` 9 → out of the top 10. The mean still rises on
A because the wins are larger (5 → 1, 6 → 2, absent → 1).

**Compared with the issue's prototype.** #1788 cites +0.065 nDCG@10 over 18
corpora from a Python prototype. Those corpora are not in this repository, so
that number was **not** reproduced; the numbers above are a different
measurement on different sets.

### Exact IDs (fixture)

[`fixtures/ids`](fixtures/ids): `advisory_gate.rs` cites `RUSTSEC-2021-0003`;
two decoys hold `rustsec`, `2021`, `0003`, `affect` apart. No autoindex code
corpus maps an `id` keyword, so this is the citation lane.
[`fixture-ids.txt`](results/fixture-ids.txt):

| Query | base rank of `advisory_gate.rs` | new |
|---|---:|---:|
| `RUSTSEC-2021-0003` | 2 | **1** |
| `what does RUSTSEC-2021-0003 affect` | 3 | **1** |
| `refuse vulnerable pin` (control) | 1 | 1 |

A first build used the #3336 BM25-scaled phrase (boost 6) here: no change
(rank 3), because the decoy collects both coverage boosts (2008 vs 3.2).
That is why `xerj code` uses a constant.

### Sweep that chose the weights

Run through a body-rewriting proxy in front of the node with the base client
(same pipeline, only the server body changed); nDCG@10 for `sep` / `lucene`
/ `javadoc`:

| Variant | sep | lucene | javadoc |
|---|---:|---:|---:|
| base | 0.6699 | 0.4814 | 0.8597 |
| `multi_match` `most_fields` instead of `best_fields` | 0.6532 | 0.4955 | – |
| `tie_breaker` 0.3 | 0.6699 | 0.4814 | – (no effect) |
| `*word*` on `ax_path`, 2 / 5 / 15 | 0.6736 / 0.6712 / 0.6587 | 0.5561 / 0.5561 / 0.6734 | 2: 0.9105 |
| `*word*` on `title`, 2 / 3 / 5 / 8 / 15 | 0.6743 / 0.6773 / 0.6762 / 0.6724 / 0.6630 | 0.5561 / 0.5561 / 0.5561 / 0.5702 / 0.6327 | 3: 0.9241, 5: 0.9317, 8: 0.9298 |
| title 5 + `ax_path` 2 | 0.6754 | 0.5647 | 0.9319 |
| identifier `term` as an extra `should` (cannot match alone), 50 / 500 | 0.6733 | 0.4814 | 500: 0.8597 |
| title 5 + identifier 500 as a recall alternative | **0.6917** | 0.5561 | **0.9317** |

`most_fields` lost on `sep` and was not taken. The proxy run had no 8-word cap
on the name lane; with the cap the release binary scores 0.9280 on `javadoc`
(0.9317 uncapped). The cap stays because each wildcard costs a scan (below).

## Results: `xerj search` with markdown outside `docs/` (#2513)

The `sep` corpus plus every `.md` file of the repository at `f9dcf626` outside
`docs/` and `engine/crates/` (574 files: 168 `user-feedback/`, 92 `landing/`,
92 `content/`, 74 `demo/`, 37 `benchmarks/`, 33 `scripts/`, 10 `tools/`, ...),
autoindexed as prefix `mx` (21,211 records). The 300 `sep` questions through
`xerj eval --id-field ax_path` (the #3334 eval, i.e. the `xerj search` query
path) ([`search-mixed-compare.txt`](results/search-mixed-compare.txt)):

| | base | new |
|---|---:|---:|
| nDCG@10 | 0.6310 | 0.6344 (+0.0035, 4/0/296, p = 0.12) |
| queries with a `.md` file at rank 1 | 6 | 2 |
| `.md` slots in the 300 top-10 lists | 106 | 11 |

Wins: `CHANGELOG.md` and `ROADMAP.md` no longer sit above `state.rs` and
`xccode/mod.rs`. The nDCG change is small because these questions target
code and rarely collide with notes; the markdown that left the top 10 was
mostly noise below the answer. No query lost.

## Latency

`took` reported by the node per query, the node restarted before each pass
(an earlier run that alternated the two clients query by query was discarded:
the second client of each pair reused clause caches the first had warmed).

| Set | base p50 / p95 ms | new p50 / p95 ms | load (1-min) during passes |
|---|---|---|---|
| sep (18k records) | 262 / 405, 271 / 411 | 312 / 475, 320 / 486 | base 57.9, 70.9; new 83.8, 87.7 |
| javadoc (188k records) | 172 / 310, 178 / 286 | 388 / 465, 389 / 469 | same passes |

The cost is the name lane: one standalone `*graph*` case-insensitive wildcard
on `title` of the Lucene corpus took 226-454 ms at load ~80. That is a scan
in the engine's wildcard path, not a client cost, and it grows with the
corpus. On the 188k-record corpus it doubles `took` at this load; on the 18k
one it adds ~50 ms. A quiet-host number is still owed.

## Reproduce

```sh
xerj --insecure --data-dir "$(mktemp -d)" --port 24100 &
export XERJ_CODE_HOME=$PWD/xch            # corpora/<name>/ holds each corpus
xerj corpus index sep    --url http://localhost:24100
xerj corpus index lucene --url http://localhost:24100
python3 harness/make_javadoc_set.py xch/corpora/lucene data/javadoc   # byte-identical
python3 harness/code_eval.py ./xerj http://localhost:24100 sep \
    data/sep-queries.tsv data/sep-qrels.tsv out-sep.json 10
python3 harness/compare.py base-sep.json new-sep.json
xerj eval --url http://localhost:24100 --prefix mx --id-field ax_path \
    --qrels data/sep-qrels.tsv --queries data/sep-queries.tsv --json
harness/fixture_ranks.sh ./xerj http://localhost:24100 ids "RUSTSEC-2021-0003"
```

`data/sep-*` and `data/lucene-*` are the existing question files reshaped
into `xerj eval` TSVs (qid prefix = set); `data/javadoc-*` is the generator's
output.

## Round 7: held-out questions and the name-lane decision

**Run:** 2026-11-22. Same harness ([`code_eval.py`](harness/code_eval.py)),
same corpus (Lucene, 188,125 records, re-indexed from the same checkout). One
private `--insecure` node, restarted before every pass. The server binary is
main `136e0d141` for all three arms. Each arm is a client binary built with
`cargo build --release -p xerj-server`:

| Arm | Source | sha256 |
|---|---|---|
| main | `136e0d141` | `546bfc23033fd0a7581dfa11a4b0f870c7acb77ae9d55981274689bd6e05dd05` |
| A | this PR at `ba3307558` (all lanes) | `433a2a8a8d1b9f2342b0593c39bbd3318dea3974b61ff7703fd98918e002af90` |
| B | A without the name lane (this commit) | `9a399bee23e4491b30277aeb4e826a5a370f5a522200273c6bf47ad980edf5f7` |

Decision rule, recorded before the run:
1. Land A if the `xerj code` p50 regression is 25% or less.
2. Otherwise land B, if B alone keeps the ranking gain.
3. Otherwise hold.

### Held-out question sets (built after the weights were frozen)

| Set | n | Built by | Relevant | Notes |
|---|---:|---|---|---|
| `h` | 200 | [`make_heldout_set.py`](harness/make_heldout_set.py): the `javadoc` recipe, but over every module **except** `core` (and test-framework/benchmark/luke), seed 20261122 | declaring file | no file overlaps the tuning `javadoc` set; 24/200 sentences contain the class name |
| `hm` | 200 | `h` with the class name removed from the sentence | same | takes away the name-lane advantage on those 24 |
| `id` | 200 | [`make_ident_set.py`](harness/make_ident_set.py): a public/protected camelCase method name from a non-core module, declared in exactly one file and written as `name(` at most 3 times in the whole checkout, split into words (`get hyphenation points`), seed 1122 | declaring file | built to fit the identifier lane, the way `sep` B was |

### Ranking, nDCG@10 (paired sign-flip, 10,000 rounds)

| Set | main | A | B | A W/L/T | B W/L/T | A p | B p |
|---|---:|---:|---:|---|---|---:|---:|
| `h` held-out | 0.8836 | **0.9067** | 0.8836 | 12/1/187 | 0/0/200 | 0.0023 | 1 |
| `hm` held-out, name masked | 0.8756 | **0.9032** | 0.8756 | 15/2/183 | 0/0/200 | 0.0002 | 1 |
| `id` held-out | 0.2556 | 0.4724 | **0.4856** | 60/13/127 | 56/0/144 | 0.0001 | 0.0001 |
| `javadoc` (tuning) | 0.8597 | **0.9280** | 0.8597 | 33/3/164 | 0/0/200 | 0.0001 | 1 |
| `lucene` 8 (tuning) | 0.4814 | **0.5561** | 0.4814 | 3/0/5 | 0/0/8 | 0.24 | 1 |
| `sep` all (tuning, 18k-record corpus) | 0.6699 | **0.6917** | 0.6887 | 16/21/263 | 6/0/294 | 0.044 | 0.031 |
| … `sep` B words of a name | 0.5392 | 0.5952 | **0.5956** | 9/7/84 | 6/0/94 | 0.039 | 0.032 |

The main and A numbers on the tuning sets match the original run exactly.
Rankings were identical across the 8 passes of each arm on `h`.

- **The name lane's gain is smaller on held-out questions:** +0.023 on `h`,
  against +0.068 on the tuning `javadoc` set.
- **B never loses a query on any set.** On Lucene prose questions it is
  identical to main: the identifier lane needs the words of one name. On
  `id` it gains slightly more than A (+0.230 vs +0.217), because A's name
  lane loses 13 queries there. It keeps 86% of A's `sep` gain (+0.0188 vs
  +0.0218) with no losses; A lost 21 queries on `sep`.
- ID fixture ([`r7/fixture-ids.txt`](results/r7/fixture-ids.txt)): A and B
  both rank `advisory_gate.rs` first for both RUSTSEC queries; main ranks it
  2 and 3. The exact-ID lane is unchanged in B.

### Latency, set `h`

`took` is the node's own figure; wall is the client process end to end.
Each pass is 200 queries, and the node is restarted before each pass. Load
is the 1-minute load average at start / peak / end. The host is shared, and
part of the peak during A passes comes from A's own wildcard scans.

**Loaded window:** 5 rounds, order rotated (main A B, B A main, A main B, B main A, main B A).

| Arm | `took` p50 per pass (ms) | median `took` p50 / p95 | median wall p50 / p95 | load range |
|---|---|---|---|---|
| main | 163, 170, 173, 163, 174 | 170 / 278 | 252 / 639 | 23.6 to 55.5 |
| A | 404, 399, 404, 406, 402 | 404 / 558 (**+138%** / +101%) | 516 / 894 | 28.1 to 60.9 |
| B | 201, 180, 194, 215, 203 | 201 / 405 (+18% / +46%) | 293 / 826 | 23.8 to 67.9 |

**Quiet window:** 3 rounds (main A B, B A main, A B main). Every pass
started at load 5.2-16.0.

| Arm | `took` p50 per pass (ms) | median `took` p50 / p95 | median wall p50 / p95 |
|---|---|---|---|
| main | 182, 176, 177 | 177 / 298 | 236 / 384 |
| A | 416, 434, 393 | 416 / 571 (**+135%** / +92%) | 470 / 628 |
| B | 210, 186, 180 | 186 / 410 (+5% / +38%) | 233 / 452 |

The A cost does not shrink on a quiet host. It is the engine's
case-insensitive `*word*` wildcard scan over the keyword `title` of 188k
records, up to 8 per query. B's p95 rises because a query whose words form
an identifier adds up to four `term` clauses on `name` and a `bool` wrapper.

### Decision

- **A fails rule 1:** +135% to +138% `took` p50, against a bar of 25%.
- **B passes rule 2:** +5% (quiet) / +18% (loaded) `took` p50. It keeps a
  significant held-out gain (`id` +0.230, p = 0.0001, no losses). It also
  keeps most of the `sep` gain, keeps the ID fixture, and loses no query on
  any set.
- **B does not keep the name lane's gain on prose questions** (`h` +0.023,
  `javadoc` +0.068). Recovering that needs a cheaper name match than a
  per-word wildcard scan, for example an analyzed sub-field of `title`
  split on case changes, at index time.

Question sets: `data/r7-{h,hm,id}-{queries,qrels}.tsv`. Raw per-query JSON: [`results/r7/`](results/r7/). Every pass with its load
is in [`r7/passes.log`](results/r7/passes.log).
