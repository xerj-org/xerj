# XERJ.code — the reference-coding toolkit

An agent that does not know an API guesses, runs, fails, and guesses again. Each
lap costs **output** tokens, the expensive kind. This toolkit replaces laps with
a lookup: clone the repositories that already contain a correct implementation,
index them with XERJ, and retrieve the passage before writing.

Since [#977](https://github.com/xerj-org/xerj/issues/977) the whole loop ships
**inside the `xerj` binary** as the `xerj corpus` and `xerj code` subcommands —
no scripts, no `python3`. (Before that they were wrapper scripts in this
directory; before [#319](https://github.com/xerj-org/xerj/issues/319) they were
not shipped at all.)

```sh
xerj corpus add xerj-storage https://github.com/spacejam/sled …  # clone the peers, once
xerj corpus index xerj-storage                                   # xerj autoindex; re-run to update in place
xerj corpus index xerj-storage --fresh                           # rebuild beside the old index, verify, then swap
xerj corpus index xerj-storage --include-data                    # also index CSV/HDF/simulation output (skipped by default)
xerj code xerj-storage "how does sled fsync its WAL segment on rotation?"
```

A reference corpus is code and docs. Test outputs and datasets (CSV, HDF5,
Parquet, simulation dumps) are skipped by default because they can out-record
the whole corpus — one test folder in the
[field report](https://github.com/xerj-org/xerj/issues/1979) produced 636,724
records. The run prints how many files and bytes it left out; `--include-data`
indexes them anyway.

Or rebuild a corpus someone else already defined and vetted:

```sh
xerj corpus add --from hub/xerj-storage.json && xerj corpus index xerj-storage
```

## What you need

- The `xerj` binary — a release build, or `curl -fsSL https://xerj.org/get | sh`.
- A running XERJ instance (`--url` or `$XERJ_URL`, default
  `http://localhost:9200`): `xerj --data-dir ~/.xerj-code/data --insecure`.
- `git` (for `xerj corpus add`).

Everything lives under `~/.xerj-code/` — `corpora/` (clones + `corpus.json`),
`state/` (the ledger), `autoindex-state/` (per-build journals), `data/` (the
server dir) — overridable with `XERJ_CODE_HOME`. All of it **persists**: never
put it under `/tmp`, or the corpora silently retrieve nothing after a reboot.

## Sharing a corpus: definitions, not indexes

`corpus.json` is a few hundred bytes of URLs, commit SHAs and licences. It
contains no source, so there is no redistribution question, and it rebuilds the
corpus anywhere:

```json
{"corpus":"xerj-storage","cloned_at":"2026-08-06T20:24:00Z","repos":[
  {"repo":"sled","url":"https://github.com/spacejam/sled","licence":"Apache-2.0/MIT",
   "sha":"e449d17111f4a097e1c66b6db241962ccb6a4136","files":70,"bytes":1322994}
]}
```

`xerj corpus add` writes one after every build, and `--from` reads one back.
The SHA is the full 40 characters because that is what a remote will serve:
`git fetch --depth 1 origin e449d17` fails with *couldn't find remote ref*,
while the same fetch with the full SHA succeeds — so a pinned rebuild is one
cheap shallow fetch per repo. A manifest carrying short SHAs (anything written
before this change) is rejected rather than silently rebuilt at the tip.

`--from` is exact: an existing clone is **moved** to the recorded SHA, and local
edits or untracked files inside it are discarded. Same manifest in, same bytes
out — which is what makes a shared prompt or a shared measurement comparable
across two machines. A `review` block in the input manifest (see
[`hub/README.md`](hub/README.md)) is preserved for every repo that lands at its
recorded SHA.

New clones are prepared in a private temporary directory and published only
when checkout succeeds (#2453). An ordinary failure cleans that attempt's
stage; a hard kill can leave a hidden `.xerj-clone-*` directory, but a retry
uses a new stage instead of treating it as a completed checkout. One
`corpus add` per corpus runs at a time; a second is refused.

An existing checkout with an unreadable commit, a missing index, or an
`index.lock` is preserved and refused. These states do not prove it is safe
to delete the directory or overwrite local edits. Wait for active Git
operations; for an incomplete checkout, move the directory to an unused
backup location and rerun the original command. The backup retains local
files for review. Completed linked worktrees remain reusable.

A local path (`/path/to/checkout` or `file:///path/to/checkout`) is cloned the
same way. Git checks out committed files only, so a root `.xerjignore` that is
untracked or has uncommitted edits would never reach the clone, and the index
would apply built-in ignore rules alone. `corpus add` copies that root file
into the clone when its bytes differ from the checkout, and says so. Running
`corpus add` again refreshes the copy from the source working tree. A pinned
`--from` rebuild does not copy it — the recorded SHA stays exact — and says so
when the working tree's root `.xerjignore` differs. Nested `.xerjignore` files
are not carried. Removing the source file does not delete a copy already in
the clone.

Pre-built *indexes* are a different question (on-disk format version, embedder
identity, redistribution of the indexed source) and are not offered here.

## Following upstream

A corpus does not track its git remotes: nothing fetches or polls them. A
`xerj corpus index` run reads the checkouts as they are, so an upstream commit
is not searchable until you pull it. To refresh a corpus added from git URLs:

```sh
git -C ~/.xerj-code/corpora/<name>/<repo> pull --ff-only   # each checkout
xerj corpus add <name> <git-url>...   # same URLs; records the new commit SHAs
xerj corpus index <name>              # updates the build in place
```

`corpus add` keeps a checkout it finds (`[skip] … already cloned`) and records
its current commit. `corpus index` alone would index the pulled files too, but
`corpus.json` would keep naming the old commit, and a `--from` rebuild of it
would come back without them. A corpus built `--from` a manifest stays at the
manifest's pins; change the SHAs there to move it.

For a folder indexed with `xerj autoindex <dir>`, pull and re-run the same
command; `--watch` reindexes the files a pull changes, but never fetches.

## Licences are recorded, and reviewed

`xerj corpus add` classifies each repo from the licence file it actually ships,
and warns when the result is copyleft or source-available. The detection is
text-matching heuristics and **has been wrong**, in both directions:

- Elasticsearch's triple licence (AGPL-3.0 / SSPL-1.0 / Elastic-2.0) was
  recorded as `Apache-2.0`, because its text contains the phrase *an "Apache
  License 2.0" compatible license*. Fixed by checking restrictive phrases first.
- sonic's plain MPL-2.0 was recorded as `GPL`, because MPL-2.0 defines
  "Secondary License" in terms of the GNU GPL inside its own text. Fixed by
  reading the title before the body.

So the manifests in [`hub/`](hub/) carry a `review` block a human filled in —
SPDX expression, and whether the repo is safe to adapt from or approach-only.
See [`hub/README.md`](hub/README.md). `xerj corpus list` surfaces each corpus's
`review.use` values.

A corpus of **your own** code is a different case. A private repo often has
no LICENSE file, so the detector records `NONE-FOUND` or `UNKNOWN` and every
passage otherwise carries `!! …: adapt the APPROACH, do not copy the code`.
That banner is wrong for first-party code. Mark the corpus explicitly:

```sh
xerj corpus add --own <name> <local-git-path>
xerj corpus own <name>          # an existing corpus
xerj corpus own <name> --off    # clear it
```

That writes `"own": true` in the corpus's `corpus.json`. The hit header still
names the unidentified licence. `xerj code` and MCP `xerj_code_search` omit
the do-not-copy banner for that unidentified licence only. A licence the
detector actually named (GPL, AGPL, MPL, and the rest of the restricted
tuple) still warns. A local path is not marked own unless you pass the flag,
and a `--from` manifest cannot set it — that file is untrusted input.
Re-adding the corpus keeps a marker already recorded. Hub pack licence
classes are a separate record and are not changed by this marker.

`xerj corpus index` ends with a short per-file-class summary: extensions that
were indexed, data-file extensions left out (CSV/HDF/simulation output,
unless `--include-data`), and ignore-rule skips.

**Retrieved code is evidence, not authority.** It shows how *that* project
solved it under *its* constraints. Cite `file:line`, and say plainly when you
are adapting rather than copying.

## Honest scope

Retrieval wins decisively on code the model has **not** memorised — private,
internal, niche, or post-cutoff. On popular public library code the model
already knows, it is overhead. `SKILL.md` and
[`docs/case-studies/reference-coding/`](../../docs/case-studies/reference-coding/)
carry the measured numbers, including the cases where this loses.

## Passage windows

When no matching symbol is available, or with `--no-symbol`, `xerj code`
selects a window of source around query terms, capped at 800 characters by
default. It aligns the window to whole lines only when doing so preserves
the selected window's term score; a partial line is preferable to dropping the
match. Window offsets refer to the original source, including when Unicode
lowercasing changes its length. `--full N` changes the character limit, and
`--full 0` keeps the explicit file-head mode.

A matching definition longer than the limit is windowed the same way: the
passage shows the part of that definition around the query terms, with a
marker giving the window's offset from the definition's start, and the
`kind name @ line N` label still names the definition's first line. When the
head of the definition is already the best window, it is shown with a
`truncated at N chars` marker.

## Hybrid retrieval, fused server-side

With `--mode hybrid`, `xerj code` sends **one native top-level `hybrid` query**
(`{"fusion":{"method":"rrf","k":60}}`) — the same fusion the engine serves over
the wire, no client-side rank merging. Before it fires, a BM25 size-1 preflight
decides honesty: no lexical match is reported as a miss (exit 1), because with
the lexical embedder vector nearest-neighbours are not evidence of a match.
The mapping is read first and the vector arm is aimed only at indices whose
`body` is `semantic_text` — a semantic leg against plain-text indices 400s the
whole request — and the fused query targets that capable set when it is smaller
than the corpus. Every hybrid run opens with an arms-ran note naming what
actually ran and which lexical-only indices were excluded; per-hit per-arm
ranks are gone (the server-side fuser does not expose them). If no capable
mapping exists, or the fused request fails, the answer is plain BM25 labelled
`BM25 only — …`, never a silently-BM25 "hybrid". In standalone
`--mode semantic`, mapping and search HTTP/transport failures exit `2`, rather
than reporting no match; there are no BM25 results to fall back to.

## Weak matches

Nearest neighbours used to render like hits: hybrid RRF scores a rank-1
miss the same as a rank-1 hit (`2/(k+1)` with k=60). A page is now labelled

```text
weak match: the corpus may not cover this
```

and every passage on it is marked `weak`, when either of these holds:

- the query has at least three content terms (English function words such as
  `the` and `with` dropped) and the best passage's text contains fewer than
  half of them — a nearest neighbour, or a collision on one stem such as
  `recall` inside `recall@k`; or
- the scores are hybrid RRF and the top score is strictly below `1/(k+1)`,
  half of a typical two-leg rank-1 score.

A passage whose score is strictly below half the page's top score is marked
`weak` on its own header. The stronger passages on that page, and any page
that clears both checks, render exactly as before. A weak page still exits
0 (the passages are there; they are not evidence the corpus covers the
need). Fall back to grep or another corpus. `--json` is the raw response
and does not carry the label. `--meatl` prints `@weak why=corpus-may-not-cover`
and appends ` weak` to those `@ok` lines. `xerj_code_search` returns the
same text.

## Machine-readable retrieval

Pass `--json` to return the search result as JSON, including when no passages
match. Check both stdout and the exit status: matching results exit `0`, while
an empty `hits.hits` array exits `1`. A corpus with no live indices still exits
`3` with a diagnostic on stderr; it is not an empty search result. Exit `2` is
usage, an unreachable node, or the 30-day staleness refusal.

`--meatl` prints one line per hit instead of prose:
`@ok f=<file>:<line> score=<12.34>[ why=<licence>][ weak]` (hybrid scores
render as `rrf=<0.1234>`), `@no q="<query>" why=no-match-in-corpus` for a
miss, `@weak why=corpus-may-not-cover` when the page is a weak match, and
`@mode <arms-ran note>` for the hybrid/semantic note line.

Agents without a shell get the same pipeline as an MCP tool,
`xerj_code_search` (`xerj mcp`) — byte-identical text, with refusals mapped to
`isError: true`, a no-match mapped to `isError: false`, and an optional
`licence_policy: "strict"` that strips passage text from restricted-licence
hits.

## Tests

The contracts live where the code lives, gated by scoped cargo tests (run from
`engine/`, with `--profile ci-test`):

```sh
cargo test --profile ci-test -p xerj-common xccode    # staleness, licences, exit triangle, hybrid shape, passages, ledger
cargo test --profile ci-test -p xerj-autoindex xc::   # corpus lifecycle, path gate, usage
cargo test --profile ci-test -p xerj-mcp              # tool schema presence + isError mapping
cargo test --profile ci-test -p xerj-common xccode::manifest   # hub manifests pass the untrusted-input gate
```
