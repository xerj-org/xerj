---
name: xerj-code
description: Reference-coding with XERJ. Clone the libraries that already solved your problem, index them locally, and retrieve the exact implementation before writing code — so the agent reads passages instead of re-deriving algorithms across retry loops. Use when starting a task in an unfamiliar API, porting an algorithm, or when an agent has already looped twice on the same error.
---

# XERJ.code — reference coding

An agent that does not know an API guesses, runs, fails, and guesses again. Each
lap costs **output** tokens, which are the expensive kind. This skill replaces
laps with a lookup: clone the repositories that already contain a correct
implementation, index them with XERJ, and retrieve the passage before writing.

The trade is deliberate. Retrieval spends **input** tokens, which are cheaper per
token and cacheable; loops spend **output** tokens. See [`COSTS.md`](../../docs/case-studies/reference-coding/COSTS.md) for
the arithmetic and for the conditions under which this trade **loses**.

> **Status, stated plainly: measured, and the value is real but narrow.**
> (Full case study with real tokens and dollars: [`docs/case-studies/reference-coding/CASE_STUDY.md`](../../docs/case-studies/reference-coding/CASE_STUDY.md), 2026-08-05.)
>
> The comparison that matters is against **native Claude Code** — the same agent
> with tools that would grep the source itself — not a bare model. Measured across
> three regimes, same agent, one objective verdict (compiles + passes hidden tests):
>
> - **Unfamiliar code with a contract the model can't recall** (a seal, a
>   generational handle, a lazy refill, a specific hash scheme — measured across
>   **seven purpose-built libraries, 21 runs**): native and xerj both solve
>   **21/21**; **xerj uses 1.5× fewer output tokens and costs 1.3× less than
>   native**, because retrieval replaces the grep. Pure memory **fails 1/21** and
>   burns **6.5× the cost** flailing. This is the win, and it is decisive. The
>   compiler can leak an API *name* across a retry loop, but never a *contract*.
> - **Memorised code** (any popular public crate — even a 256-value table, or a
>   protobuf-style varint): retrieval is **overhead**. Pure memory is cheapest;
>   injecting a big reference can be the *worst* arm, because the model re-emits
>   what it was handed.
>
> So: **use this when the reference is code the model has not memorised** — your
> own private/proprietary code, an internal API, a post-cutoff or niche library.
> On public library references the model already knows, it costs more than it
> saves. The native-vs-xerj gap grows with corpus size (grep pulls the whole tree
> into context; retrieval pulls one passage).

## Standing corpora for this repository (MANDATORY since 2026-08-06)

In the xerj repo, retrieval-before-writing is **required** for non-trivial
engine work, not optional. Each corpus below has a pinned, licence-reviewed
definition in [`hub/`](hub/) — `xc-corpus.sh --from hub/<name>.json` rebuilds it
at the same commits anyone else is using:

| corpus | projects | use it for |
|---|---|---|
| `xerj-search` | lucene, tantivy, meilisearch, quickwit, sonic, elasticsearch | FTS, BM25, postings, merge policy, typo tolerance, segment layout, ES wire semantics |
| `xerj-vector` | qdrant, usearch, instant-distance, hnswlib | HNSW build, neighbour heuristics, quantisation, filtered kNN |
| `xerj-storage` | sled, fjall, redb | WAL, flush epochs, crash recovery, compaction, page allocation |
| `xerj-columnar` | clickhouse | columnar storage, aggregation execution, compression codecs, vectorised scans |

**Approach-only sources:** `elasticsearch` (AGPL-3.0 / SSPL-1.0 / Elastic-2.0)
and `sonic` (MPL-2.0). Never copy their code into Apache-2.0 XERJ; for ES,
pasting source would also falsify the project's public "shares no code with
Elasticsearch" claim. `meilisearch` is MIT at the core but **BUSL-1.1 for its
Enterprise Edition** parts — check the header/path first. Read the design,
write our own implementation. Each manifest's `review.use` field says which
bucket a repo is in; `hub/README.md` explains the three.

### Operational traps, both hit in practice

- **Nothing under `/tmp`.** Clones live in `~/.xerj-code/corpora/`, the index in
  `~/.xerj-code/data`. A `/tmp` data dir is lost on reboot and the whole corpus
  silently retrieves nothing.
- **`xc-index.sh <corpus> --fresh` is the rebuild, and it never costs you a
  working index.** It builds a replacement *beside* the existing one — under
  `xc-<corpus>-b<stamp>-*`, with an autoindex `--state-dir` of its own in
  `~/.xerj-code/autoindex-state/<corpus>/` — verifies it (autoindex exited `0`
  or `3` **and** `_count > 0`), switches `state/<corpus>.json` to it by atomic
  rename, and only then deletes the old indices, by exact name. A build that
  fails or comes back empty is removed and the old index, the old state file
  and the old state directory stay exactly as they were. Use it after the data
  dir was wiped or moved, when `xc.py` says the index is older than 30 days, or
  when a plain re-run says the state directory "cannot become generation
  authority". It is **not** `xerj autoindex --fresh`, which the script never
  forwards: that flag only discards autoindex's resume journal and is refused
  outright once a corpus generation has committed — which is why `--fresh` used
  to fail on every corpus that had been indexed before
  ([#930](https://github.com/xerj-org/xerj/issues/930)).
- **A record count the node does not answer is never read as zero.** The
  script asks up to 6 times (`XC_COUNT_TRIES`, `XC_COUNT_PAUSE`). If the node
  still cannot count the existing index, it is presumed to be a working one and
  no failed build is kept over it; if it cannot count the new build, nothing is
  deleted and nothing is switched. Only a number — or a 404, no index matches —
  authorises a delete or a swap.
- **An interrupted FIRST build is kept, and `xc.py` says so.** When a build
  fails after writing records and there is no working index to fall back to,
  the script keeps it rather than leave no corpus at all, and records
  `salvaged: true` plus the real `autoindex_exit` in the state file. Such a
  build can be partial, so `xc.py` warns on stderr with every query that
  coverage is INCOMPLETE — **a miss is then not evidence that the code is
  absent** — and `xc.py --list` marks the corpus. A plain `xc-index.sh <corpus>`
  resumes it (same prefix, same state directory) and clears the mark when it
  finishes.
- **A plain `xc-index.sh <corpus>` updates in place.** It re-runs autoindex
  against the recorded build's prefix and state directory, so additions,
  edits, deletions and renames reconcile incrementally.
- **The state file has two prefixes on purpose.** `prefix` is always
  `xc-<corpus>` — the whole namespace, so anything that globs `xc-<corpus>*`
  keeps working across rebuilds. `index_prefix` is the one verified build, and
  it is what `xc.py` queries, so a half-built replacement never leaks into
  answers during a rebuild.
- **During a rebuild both builds exist**, so the node briefly holds the corpus
  twice. Budget disk for it on a large corpus.
- Always verify: `curl -s "$URL/xc-<corpus>*/_count"` must be > 0. The script
  does this itself and says so; do it again if you are scripting around it.

## When this is worth it

Use it when the task touches code someone else has already written correctly:

- an unfamiliar library, framework, or protocol
- porting an algorithm whose reference implementation exists
- matching an established convention across a large codebase
- **any time you have already looped twice on the same error** — that is the
  signal that guessing is not converging

Do not use it for code that only exists in this repository (ordinary file reads
are cheaper), for one-line edits, or when you already know the API. Indexing has
a fixed setup cost; a task you would finish in two tool calls will not repay it.

## The loop

```
xc-corpus.sh <name> <git-url>...   # clone reference repos (once per domain)
xc-corpus.sh --from <manifest>     # ...or rebuild a corpus someone else defined
xc-index.sh  <name>                # index them with xerj autoindex (once per corpus)
xc.py <name> "<what you need>"     # retrieve before writing (every task)
```

### 1. Build a corpus

Group repositories by *problem domain*, not by language. A corpus for "async
Rust" holds tokio, hyper, and tower; a corpus for "parsers" holds nom, pest, and
tree-sitter. Keep corpora small and sharp — a corpus that contains everything
retrieves like a search engine with no query.

```sh
scripts/xc-corpus.sh async-rust \
  https://github.com/tokio-rs/tokio \
  https://github.com/hyperium/hyper
```

Clones are shallow (`--depth 1`) and land in `~/.xerj-code/corpora/<name>/`.
Every build writes a `corpus.json` beside them — URLs, full commit SHAs and
licences, a few hundred bytes, no source. That file *is* the shareable corpus
definition: hand it to someone else and

```sh
scripts/xc-corpus.sh --from their-corpus.json
```

rebuilds the same commits on their machine (an existing clone is moved to the
recorded SHA, so both trees match). Vetted definitions for the corpora this
project uses live in [`hub/`](hub/):

```sh
scripts/xc-corpus.sh --from hub/xerj-storage.json && scripts/xc-index.sh xerj-storage
```

### 2. Index it

```sh
scripts/xc-index.sh async-rust
```

This runs `xerj autoindex` against the local XERJ instance. **Exit code 3 means
"completed with junk files" and is not a failure** — some files in any real
repository are unparseable. Treat 0 and 3 as success; anything else is real.

### 3. Retrieve before writing

```sh
scripts/xc.py async-rust "graceful shutdown with a broadcast channel"
```

`xc.py` returns the **matching definition**, not a byte window:

```
─── valkey/src/networking.c  (score 12.68, BSD)
    [function addBulkStringToReplyIOV @ line 2635 — 366 of 272,498 chars]
static void addBulkStringToReplyIOV(char *buf, size_t buf_len, ...) {
```

That line number is the whole point. A record is one whole file, so ranking can
only tell you *which* file matched; the definition is located from the `symbols[]`
the index already carries. Before this, an 8 KB slice of a 272 KB file was a coin
flip on whether the answer was inside it — a query about null replies correctly
ranked `networking.c` and then returned its licence banner, because `addReplyNull`
lives at line 1460. Read the named definition, cite it, and if the top hits are
irrelevant say so and fall back to normal work rather than forcing them in.

`--full N` caps each passage at N chars. `--no-symbol` falls back to a raw window
(only useful for data files with no symbols at all).

### Retrieval modes

The default is **`bm25`**. Measured across two corpora and twelve hand-labelled
queries:

|          | rust-text |      | kv-oss (C) |      | combined |       |
|----------|----------:|-----:|-----------:|-----:|---------:|------:|
|          | top-1     | top-3| top-1      | top-3| top-1    | top-3 |
| **bm25** | 3/6       | 6/6  | **6/6**    | 6/6  | **9/12** |**12/12**|
| hybrid   | **5/6**   | 6/6  | 2/6        | 4/6  | 7/12     | 10/12 |
| semantic | 4/6       | 4/6  | 2/6        | 3/6  | 6/12     | 7/12  |

top-3 is the operative metric — the agent reads *k* passages, not one — and BM25
is perfect on it while never being the worst arm on either corpus. It is also
~5× faster, with no vector round trip and no mapping lookup.

Hybrid was previously the default, chosen on `rust-text` alone where it wins
top-1. That was a corpus-specific result: on the C corpus the vector arm reaches
only **5 of 407** indices (issue #173), so fusion mixes a good ranking with one
that cannot see 98.8% of the material. Use `--mode hybrid` when a corpus has
broad `semantic_text` coverage *and* you specifically care about top-1 — and
measure it before trusting it.

```sh
scripts/xc.py rust-text "lazy DFA cache eviction"                 # bm25 (default)
scripts/xc.py rust-text "lazy DFA cache eviction" --mode hybrid   # + vector, RRF-fused
scripts/xc.py rust-text "lazy DFA cache eviction" --mode semantic # vector only
```

A hybrid answer opens with a line naming the arms that actually ran. **The vector
arm only works where `body` is mapped as `semantic_text`**, and a `semantic` query
against an index where it is not does not degrade — it fails the whole search with
a 400 and takes every other index in the wildcard with it. `xc.py` reads the
mapping first and aims the vector arm only at capable indices, so a corpus with no
`semantic_text` says `BM25 only — ...` rather than pretending.

**`xc.py` never requests `highlight`.** On this engine a highlight block changes
`_score` and reorders hits (issue #177): identical query, top-1 **6/6** without it
and **1/6** with it. Since passages now come from `symbols[]`, the highlighter
bought nothing and cost ranking.

## Rules that keep this honest

**Retrieved code is evidence, not authority.** A passage proves how *that*
project solved the problem under *its* constraints. Cite `file:line` when you
rely on it, and say plainly when you are adapting rather than copying.

**Check the licence before copying.** `xc-corpus.sh` records each repository's
licence in `corpus.json`. Copying a GPL implementation into a permissively
licensed project is a real problem, not a technicality. Adapt the approach and
write your own code when the licences are incompatible.

**A stale index is worse than none.** It returns code that no longer exists and
sends the agent down a dead path with false confidence. `xc.py` refuses to
answer from an index older than 30 days unless you pass `--stale-ok`; re-run
`xc-index.sh` instead.

**Never index the working repository into a shared corpus.** Reference corpora
are for other people's code. Mixing your own in makes retrieval return your own
mistakes as precedent.

## MEATL

For agent-to-agent and agent-to-tool exchanges, use the notation in `MEATL.md` —
a compact, checkable format that cuts output tokens on the messages no human
reads. It is explicitly **not** for user-facing text: prose to a person stays
prose. A summary compressed into arrow chains costs the reader more than it
saves the budget.

## Measuring it

The published numbers, the per-run raw data and the price model live in
[`docs/case-studies/reference-coding/`](../../docs/case-studies/reference-coding/):
`CASE_STUDY.md` for the write-up, `COSTS.md` for why output tokens are the
expensive kind, `data/results-*.json` for the runs behind each figure, and
`generated/` for code the two arms actually produced.

The measurement harness itself (`csrun.py`, the purpose-built reference
libraries, the hidden-test task specs) is **not** shipped in this repository —
only the retrieval tooling in this directory is. What that harness does, if you
want to build the equivalent: run three arms of the *same* agent — `bare`
(memory only), `native` (it greps the corpus tree itself), `xerj` (the retrieved
passage is injected) — over tasks whose verdict is objective, i.e. the generated
code compiles and passes a hidden test the model never sees. Report medians, not
means: one flailing trial can be 5× another.

Two rules that decide whether a result means anything:

- **Validate the tasks before spending a token.** Inject the reference-correct
  solution (must pass) and a plausible wrong one — correct but quadratic, or
  missing the contract — which must **fail**. A benchmark whose tests pass wrong
  code measures nothing.
- **Do not measure on code the model has memorised.** It reproduces even a
  256-value table from recall, and retrieval then shows as pure overhead. The
  honest test bed is code that was never in a training set — your own private
  code is the realistic case.

## After the session: file the field report

Retrieval is only half the loop. The other half is telling the maintainers what
using XERJ was actually like — the baseline contribution asked of every agent
that uses it (`user-feedback/16-agent-field-reports/README.md`,
https://xerj.org/llms.txt). One command drafts it and fills in the facts
(version, OS, what was indexed) for you:

```sh
# opens the PR (branch + commit of ONLY the report + gh pr create):
xerj feedback --open-pr --used-for "reference coding" \
  --verdict "…what worked, what did not…"

# sandboxed and cannot push? draft it and print the exact git+gh commands:
xerj feedback --dry-run --used-for "reference coding" --verdict "…"
```

It only ever auto-fills facts it can observe and leaves every opinion as a
placeholder for you to fill — never an invented verdict. A pull request that adds
only a field report is exempt from the CLA gate, so this costs a signature
nothing. `xerj feedback --help` has the full contract.
