# XERJ.code — the reference-coding toolkit

An agent that does not know an API guesses, runs, fails, and guesses again. Each
lap costs **output** tokens, the expensive kind. This toolkit replaces laps with
a lookup: clone the repositories that already contain a correct implementation,
index them with XERJ, and retrieve the passage before writing.

These are the three commands the case study and the website print. Until now
they lived only in a working copy and were not shipped, so nobody outside the
project could run them ([#319](https://github.com/xerj-org/xerj/issues/319)).

```sh
scripts/xc-corpus.sh xerj-storage https://github.com/spacejam/sled …  # clone the peers, once
scripts/xc-index.sh  xerj-storage                                     # xerj autoindex, once per corpus
scripts/xc.py        xerj-storage "how does sled fsync its WAL segment on rotation?"
```

Or rebuild a corpus someone else already defined and vetted:

```sh
scripts/xc-corpus.sh --from hub/xerj-storage.json && scripts/xc-index.sh xerj-storage
```

## What you need

- The `xerj` binary — a release build, or `curl -fsSL https://xerj.org/get | sh`.
  `xc-index.sh` finds it via `XERJ_BIN`, defaulting to `xerj` on `PATH`.
- A running XERJ instance (`XERJ_URL`, default `http://localhost:9200`):
  `xerj --data-dir ~/.xerj-code/data --insecure`.
- `git`, `python3`, `curl`.

Clones land in `~/.xerj-code/corpora/`, overridable with `XERJ_CODE_HOME`.

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

`xc-corpus.sh` writes one after every build, and `--from` reads one back.
The SHA is the full 40 characters because that is what a remote will serve:
`git fetch --depth 1 origin e449d17` fails with *couldn't find remote ref*,
while the same fetch with the full SHA succeeds — so a pinned rebuild is one
cheap shallow fetch per repo. A manifest carrying short SHAs (anything written
before this change) is rejected rather than silently rebuilt at the tip.

`--from` is exact: an existing clone is **moved** to the recorded SHA, and local
edits or untracked files inside it are discarded. Same manifest in, same bytes
out — which is what makes a shared prompt or a shared measurement comparable
across two machines.

Pre-built *indexes* are a different question (on-disk format version, embedder
identity, redistribution of the indexed source) and are not offered here.

## Licences are recorded, and reviewed

`xc-corpus.sh` classifies each repo from the licence file it actually ships, and
warns when the result is copyleft or source-available. The detection is
text-matching heuristics and **has been wrong**, in both directions:

- Elasticsearch's triple licence (AGPL-3.0 / SSPL-1.0 / Elastic-2.0) was
  recorded as `Apache-2.0`, because its text contains the phrase *an "Apache
  License 2.0" compatible license*. Fixed by checking restrictive phrases first.
- sonic's plain MPL-2.0 was recorded as `GPL`, because MPL-2.0 defines
  "Secondary License" in terms of the GNU GPL inside its own text. Fixed by
  reading the title before the body.

So the manifests in [`hub/`](hub/) carry a `review` block a human filled in —
SPDX expression, and whether the repo is safe to adapt from or approach-only.
See [`hub/README.md`](hub/README.md).

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

When no matching symbol is available, or with `--no-symbol`, `xc.py` selects a
window of source around query terms, capped at 800 characters by default.
It aligns the window to whole lines only when doing so preserves
the selected window's term score; a partial line is preferable to dropping the
match. Window offsets refer to the original source, including when Unicode
lowercasing changes its length. `--full N` changes the character limit, and
`--full 0` keeps the explicit file-head mode.

## Hybrid retrieval fallback

With `--mode hybrid`, the optional vector arm falls back to the already
retrieved BM25 passages if its mapping lookup or search has a transport error:
connection failures, read timeouts, and interrupted HTTP responses. Prose and
MEATL output label the fallback as `BM25 only`; `--json` keeps its existing hits
format. A failed primary BM25 search still exits with an error. In standalone
`--mode semantic`, mapping and search HTTP/transport failures exit `2`, rather
than reporting no match; there are no BM25 results to fall back to.

## Machine-readable retrieval

Pass `--json` to return the search result as JSON, including when no passages
match. Check both stdout and the exit status: matching results exit `0`, while
an empty `hits.hits` array exits `1`. A corpus with no live indices still exits
`3` with a diagnostic on stderr; it is not an empty search result.

## Tests

```sh
tools/xerj-code/tests/test_xc_corpus.sh      # offline; local git fixtures over file://
python3 tools/xerj-code/tests/test_passage_window.py  # offline passage extraction regressions

python3 tools/xerj-code/tests/test_hybrid_fallback.py  # offline; simulated HTTP failures

python3 tools/xerj-code/tests/test_state_ledger.py  # offline; mocked HTTP responses
```
