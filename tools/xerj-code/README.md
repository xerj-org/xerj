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
xerj code xerj-storage "how does sled fsync its WAL segment on rotation?"
```

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

Pre-built *indexes* are a different question (on-disk format version, embedder
identity, redistribution of the indexed source) and are not offered here.

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

## Machine-readable retrieval

Pass `--json` to return the search result as JSON, including when no passages
match. Check both stdout and the exit status: matching results exit `0`, while
an empty `hits.hits` array exits `1`. A corpus with no live indices still exits
`3` with a diagnostic on stderr; it is not an empty search result. Exit `2` is
usage, an unreachable node, or the 30-day staleness refusal.

`--meatl` prints one line per hit instead of prose:
`@ok f=<file>:<line> score=<12.34>[ why=<licence>]` (hybrid scores render as
`rrf=<0.1234>`), `@no q="<query>" why=no-match-in-corpus` for a miss, and
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
cargo test --profile ci-test -p xerj-autoindex xc_    # corpus lifecycle, path gate, usage
cargo test --profile ci-test -p xerj-mcp              # tool schema presence + isError mapping
cargo test --profile ci-test -p xerj-common xccode::manifest   # hub manifests pass the untrusted-input gate
```
