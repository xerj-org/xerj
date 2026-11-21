# Agent search ranking: CJK runs, docs below code, dotted members, exact IDs, all-terms tier

**Run:** 2026-11-20, one Linux x86_64 host (32 cores) shared with other builds, so
every timing below has the 1/5/15-minute load average next to it. Server: one private
node per arm set, `--insecure`, throwaway data dir, default `--embed-mode lexical`.

Both arms run against the **same node and the same indices**. The query body for
`xerj search` and the `xerj_search` / `xerj_batch_search` MCP tools is built on the
client (`xerj_common::code_query::search_query_body_for_mapping`), so "before" and
"after" are two client binaries:

| Arm | Source | sha256 |
|---|---|---|
| base | main `4b90a220e` | `6ea9e6573170a36011d56e0624e8c5eddf178576183e98b63e16e2509676b80c` |
| new | this branch | `05484d6432db3f1b5f065b671dfb2ef8054114e02c3e30a97eb6f46b1ddfd049` |

The final commit differs from the `new` build in one line: a README is now a file named
`readme` or `readme.*`, so `src/readme_parser.rs` is no longer demoted (caught by the
unit test). No path in these runs starts with `readme` without being a README.

Issues: #2580 (CJK), #2513 (docs above code), #2426 (dotted member), #1788 (exact
IDs), #3007 (all-terms-first). Losses and non-wins are listed, not dropped.

## #2580: a CJK run is matched as a phrase

The `standard` analyzer gives one token per Han character (`_analyze` on this node:
`雷同` → `雷`, `同`), so `雷同` was the bag `{雷, 同}`. The body now gets one
`match_phrase` per run of 2+ CJK characters on each content field.

Fixture [`fixtures/cjk3`](./fixtures/cjk3): six Chinese notes, only `a-target.md`
contains `雷同`, only `d-target-dish.md` contains `无名小炒`; the others are full of the
single characters. Prose corpus, autoindexed, `body` mapped `text`.

| Query | Surface | base rank of target | new rank of target |
|---|---|---:|---:|
| `雷同` | `xerj search` | 3 | **1** |
| `雷同` | MCP `xerj_search` | 3 | **1** |
| `无名小炒` | `xerj search` | 2 | **1** |
| `无名小炒` | MCP `xerj_search` | 2 | **1** |

Raw: [`fixtures-cjk3-cli.txt`](./results/fixtures-cjk3-cli.txt),
[`fixtures-mcp.txt`](./results/fixtures-mcp.txt).

**Loss left in — punctuation.** The analyzer drops punctuation without a position gap,
so `雷，雷，雷。同` puts `雷` and `同` at adjacent positions and the phrase matches it.
Fixture [`fixtures/cjk2`](./fixtures/cjk2) has that sequence in a noise file: `雷同`
still ranks the target 2nd behind it (base: 3rd), and `无名小炒` likewise 2nd behind a
file with `无名，小炒`. ([`fixtures-cjk-cli.txt`](./results/fixtures-cjk-cli.txt))

**Not fixed here — zero hits on short notes.** The first fixture (six notes of 250–350
bytes) was mapped `body: keyword` by autoindex, so every query on it returned 0 hits
before and after ([`fixtures-cli.txt`](./results/fixtures-cli.txt)). That is type
inference counting whitespace tokens, filed as #3330.

## #2513: README / docs / demo / icon hits behind implementation for a question

On a code corpus, when the query reads as words (natural language, or 2+ plain words
such as `WAL rotation`) and does not name docs/readme/demo/example/icon itself, a hit
under `README*`, `docs/`, `*demo*/`, `sample(s)/`, `icons/`, an `.svg`, or an index
named `*-icons` keeps half its score (same factor as the #2122 test/example penalty),
then the page is stable-sorted. Nothing is removed. Lookups (one token) are unchanged.

| Fixture / query | base top hit | new top hit |
|---|---|---|
| [`fixtures/tick`](./fixtures/tick) `query live orders` (CLI and MCP) | `README.md` (src 4th) | **`src/orders/live.rs`** |
| xerj snapshot `WAL rotation` | `xerj-storage/src/wal.rs` | `xerj-storage/src/wal.rs` (demo notes at 2,3,4,6 → 3,4,6,7) |
| xerj snapshot `autoindex resume journal` | `docs/research/…/proposed-llms-install.md` | `user-feedback/…md` (docs/ pair gone from top 5; `state.rs` 5 → 3) |

The rc.100 report (README translations above the WAL code) no longer reproduces on
main: base already ranks `wal.rs` first, so that row is not a before/after win. The
snapshot is `git archive` of main (3,465 files); autoindex ended `aborted` in its final
refresh phase after 2,370 s on this loaded host (load ~100), so a few datasets may be
incomplete; both arms query the same indices. Markdown outside the listed
directories (`user-feedback/`, `tools/`) is not demoted: still a gap.
Raw: [`fixtures-cli.txt`](./results/fixtures-cli.txt),
[`xerj-snapshot-cli.txt`](./results/xerj-snapshot-cli.txt).

## #2426: `Type.member` resolves the owner

`_analyze` keeps `Position.open_price` as one token (`position.open_price`), so the
plain body matched nothing. The body now also searches `Position open_price`, adds
`term name = Position` (boost 10), `term name = open_price` (4), and a file-level
clause (defines `Position` and mentions `open_price`, boost 8).

Fixture [`fixtures/pos`](./fixtures/pos), query `Position.open_price`:

| Surface | base | new |
|---|---|---|
| `xerj search` | 0 hits | 1: `core/src/position.rs` `struct`, then the file |
| MCP `xerj_search` | 0 hits | 1: `core/src/position.rs` `struct` |

Duplicate snippets (#2402) are not collapsed — that stays a design decision.

## #1788: exact advisory and package IDs

`CVE-…`, `GHSA-…`, `RUSTSEC-…`, `PYSEC-…`, `GO-…` and `pkg@version` in a query add a
`constant_score` `term` on the keyword `id` / `aliases` fields (only when a read mapping
names them; autoindex indices are strict and never get one), on `name`, and a phrase
on the content fields. The constant is 3000, above a question's two coverage boosts.

Pinned rust-vulns pack (`pack-rust-vulns-2026-10-24`, sha256 `54f0419e…3e69e3d`, 2,051
records) and the pinned 50-item workload from `ccc159ffe`
(`tools/benchmarks/batch-retrieval/cases.json`), same mapping and scoring rule, through
MCP `xerj_batch_search`, size 3, 3 repeats. Re-implemented in
[`harness/rv_eval.py`](./harness/rv_eval.py) because the pinned `run.py` binds random
ports, which this run could not use.

| Workload | base | new |
|---|---:|---:|
| Pinned 50 items (canonical/alias via `identifier`+`field`, near-miss, text hit@3, duplicate, invalid) | 50/50 | 50/50 |
| 30 canonical+alias IDs as a plain `query`, labelled ID at rank 1 | **0/30** | **30/30** |
| Same IDs inside a sentence (`what does advisory <ID> affect`), rank 1 | **0/30** | **30/30** |
| Stable across 3 repeats | yes | yes |

The pinned workload does not move: its ID items use the adapter's exact `identifier`
path and its 8 text queries contain no ID. The gain is on the plain-query path an
agent uses when it does not know the field name. A first version with a plain boost-20
`term` got 30/30 bare but **2/30** in a sentence (coverage boosts outscored it); that
is why the clause is a `constant_score` above them.

Latency (MCP, persistent session, whole batch call; host shared, so not a speed claim):

| | base | new |
|---|---:|---:|
| pinned 50-item call, median of 3 | 313.0 ms (load 68.3/71.6/62.2) | 228.5 ms (load 40.2/41.4/50.4) |
| 60 ID queries (2 calls), median of 3 | 115.0 ms (load 68.3/71.6/62.2) | 140.4 ms (load 40.2/41.4/50.4) |

The ID run is +25 ms over 60 queries (~0.4 ms per query) at lower load; the pinned run
is faster at lower load. Within this host's noise; a quiet-host measurement is still
owed. Raw: [`rv-base.json`](./results/rv-base.json), [`rv-new.json`](./results/rv-new.json).

## #3007: all-terms-first tier — measured, not a win, default unchanged

`xerj search --all-terms-first` (opt-in) runs the usual body with a filter that every
content token matches some field, ranks that page, then fills the rest from the usual
any-term body. Measured on BEIR test splits with the release CLI (this branch, built before the
exact-ID constant changed; no BEIR query contains an ID, so the body is the same), k=10, nDCG@10 and
R@1 (relevant doc at rank 1):

| Arm | SciFact nDCG@10 | SciFact R@1 | NFCorpus nDCG@10 | NFCorpus R@1 |
|---|---:|---:|---:|---:|
| plain `multi_match` title+text (README baseline row) | **0.6572** | **0.5333** | **0.3016** | **0.4365** |
| `xerj search` default | 0.6229 | 0.5133 | 0.2980 | 0.4180 |
| `xerj search --all-terms-first` | 0.6228 | 0.5133 | 0.2963 | 0.4118 |

- The tier needed the any-term fill on 300/300 SciFact and 197/323 NFCorpus queries.
- It changes almost nothing because the default body already has the tier: a 3+-token
  question gets `COVERAGE_MATCH_BOOST` twice when a document matches every token.
- The `xerj search` default body loses to plain `multi_match` on both datasets
  (-0.0343 / -0.0036 nDCG@10). That is a new finding, not caused by this branch
  (no BEIR query contains an ID, CJK run or dotted member, so base and new build the
  same body); it is worth its own issue.
- CLI latency p50/p95 (process spawn + mapping read + search): SciFact default
  31.9/61.8 ms, tiered 32.6/62.0 ms (load 32.2/52.9/51.4); NFCorpus default 25.5/42.5,
  tiered 25.7/44.8 ms (load 28.1/50.6/50.6). Plain `_search`: 4.7 / 2.6 ms p50.
- **SearchTome not run:** `github.com/meghanadh/SearchTome` (rev 48d3b354) returns 404
  from this host; a third-party "subset rebuild" exists on Hugging Face but is not the
  pinned data, so the issue's 56.3 vs 64.1 baseline was not re-measured.

Per the issue's rule the default does not flip. Raw per-query TSVs:
[`beir-scifact.tsv`](./results/beir-scifact.tsv),
[`beir-nfcorpus.tsv`](./results/beir-nfcorpus.tsv).

## Reproduce

```sh
xerj --insecure -d "$(mktemp -d)" --port 22110 &
python3 harness/beir_load.py http://localhost:22110 <beir>/scifact/corpus.jsonl sf-scifact
python3 harness/beir_tiers.py http://localhost:22110 <beir>/scifact sf-scifact ./xerj out.tsv
xerj autoindex fixtures/cjk3 --url http://localhost:22110 --prefix cr --yes
harness/cli_rank.sh ./xerj cr "雷同"
python3 harness/mcp_rank.py ./xerj http://localhost:22110 cr "雷同"
python3 harness/rv_eval.py http://localhost:22110 rust-vulns-pack.zip cases.json ./xerj new --ingest
```

BEIR data: `BeIR/scifact`, `BeIR/nfcorpus` and their `-qrels` on Hugging Face (the UKP
zip host did not answer). `cli_rank.sh` uses port 22110; edit it for another node.
