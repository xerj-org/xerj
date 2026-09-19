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
scripts/xc-index.sh  xerj-storage                                     # xerj autoindex; re-run to update in place
scripts/xc-index.sh  xerj-storage --fresh                             # rebuild beside the old index, verify, then swap
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

## Tests

```sh
tools/xerj-code/tests/test_xc_corpus.sh         # offline; local git fixtures over file://
tools/xerj-code/tests/test_xc_index_fresh.py    # offline; fake node + fake xerj: the --fresh build-verify-swap contract (#930)
tools/xerj-code/tests/test_state_ledger.py      # offline; xc.py's "in state/ but not loaded here" guard
```
