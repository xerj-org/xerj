# Vetted corpus definitions

Each file here defines a reference corpus by **domain**, pinned to exact
commits. Rebuild one anywhere:

```sh
xerj corpus add --from tools/xerj-code/hub/xerj-storage.json
xerj corpus index  xerj-storage
```

| corpus | projects | use it for | approx. checkout |
|---|---|---|---|
| [`xerj-search`](xerj-search.json) | lucene, tantivy, quickwit, meilisearch, sonic, elasticsearch | FTS, BM25, postings, merge policy, typo tolerance, segment layout, ES wire semantics | ~880 MB |
| [`xerj-vector`](xerj-vector.json) | qdrant, usearch, instant-distance, hnswlib | HNSW construction, neighbour heuristics, quantisation, filtered kNN | ~44 MB |
| [`xerj-storage`](xerj-storage.json) | sled, fjall, redb | WAL, flush epochs, crash recovery, compaction, page allocation | ~28 MB |
| [`xerj-columnar`](xerj-columnar.json) | clickhouse | columnar storage, aggregation execution, compression codecs, vectorised scans | ~1.2 GB |

Group by problem domain, not by language. A corpus that contains everything
retrieves like a search engine with no query.

## Harvesting a subdirectory

A repo entry may set `"paths"` to a list of directories or files inside
that repository, for example `"paths": ["data/osv"]`. `xerj corpus add --from`
sparse-checks out only those paths, so a pin of a repository that also
contains reports, generated data, or a module tree does not become the
corpus. Omit `paths` to harvest the whole pin. The field is written back
into `corpus.json`, and a later `--from` of that file rebuilds the same
limited tree. Each entry is a relative directory (`data/osv`) or a regular
file (`pandas/core/resample.py`, which harvests that one file); absolute
paths, `..`, globs, and symlinks are refused.

## The `review` block

The `licence` field is what the detector read from the checkout. The `review`
block is what a human concluded, and it is why these manifests are contributed
by PR:

```json
"review": {"spdx": "AGPL-3.0-only OR SSPL-1.0 OR Elastic-2.0",
           "use": "approach-only", "by": "xerj-org", "at": "2026-08-12",
           "note": "Triple-licensed; the detector reports only the first match."}
```

`use` is one of:

- **`adapt-with-attribution`** — permissive (Apache-2.0 / MIT / BSD). Adapt
  freely; still cite `file:line` when you do.
- **`approach-only`** — copyleft or source-available (AGPL, SSPL, Elastic, BUSL,
  GPL, LGPL, MPL). Read the design, write your own code. Never paste.
- **`mixed`** — permissive core with restricted parts (Meilisearch: MIT core,
  BUSL-1.1 Enterprise Edition). Check the file's header and path before copying
  anything.

XERJ is Apache-2.0. Copying an incompatibly licensed implementation in is a real
problem, not a technicality. Elasticsearch is in `xerj-search` to answer *"what
does ES actually do here?"* for wire-protocol and semantics questions — XERJ's
public position is that it shares no code and no architecture with it, and
pasting ES source would make that claim false.

## Contributing a corpus

1. Build it locally: `xerj corpus add <name> <git-url>...`
2. Copy the generated `~/.xerj-code/corpora/<name>/corpus.json` to
   `hub/<name>.json` — the filename must match the `corpus` field.
3. Open each repo's licence file yourself and fill in a `review` block per
   entry. Do not copy the detector's answer without looking; it has been wrong
   in both directions (see [`../README.md`](../README.md)).
4. Check it: `cd engine && cargo test --profile ci-test -p xerj-common xccode::manifest`
   (the same untrusted-input gate `xerj corpus add --from` enforces; CI runs
   it over every file in `hub/`)
5. Open a PR. Say what the corpus is *for* — a domain a reviewer can judge, not
   a pile of repositories.

## Refreshing a pin

The SHAs are deliberately frozen: a shared pin is what makes two people's
retrieval results comparable. To move a corpus forward, rebuild without `--from`
(`xerj corpus add <name> <url>...` in a clean `XERJ_CODE_HOME`), re-review any
licence that changed, and open a PR with the new manifest.
