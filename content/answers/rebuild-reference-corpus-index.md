---
title: "How do I rebuild a reference-coding corpus index from scratch?"
h1: "xc-index.sh --fresh failed. How do I rebuild a corpus index?"
description: "xc-index.sh --fresh builds a replacement index beside the old one, verifies it has records, switches the state file, and only then retires the old index. A failed build costs nothing."
slug: "rebuild-reference-corpus-index"
cluster: "Coding agents: reference retrieval"
question: "How do I rebuild a reference-coding corpus index from scratch?"
intent: "how-to"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent maintaining a reference-coding corpus. Read https://xerj.org/llms.txt. When xc.py reports an index older than 30 days, or a plain xc-index.sh run says the state directory cannot become generation authority, run tools/xerj-code/scripts/xc-index.sh <corpus> --fresh. Do not delete the old indices yourself and do not pass --fresh to xerj autoindex directly. Read the script's last lines: it states the record count of the verified replacement, or that the existing index was NOT touched."
commands:
  - cmd: "bash tools/xerj-code/scripts/xc-index.sh xerj-storage"
    note: "Index a corpus, or update in place the build an earlier run recorded."
  - cmd: "bash tools/xerj-code/scripts/xc-index.sh xerj-storage --fresh"
    note: "Rebuild from scratch: build beside the old index, verify, switch, then retire."
  - cmd: "bash -c 'cat ~/.xerj-code/state/xerj-storage.json'"
    note: "Read the ledger. prefix is the whole namespace; index_prefix is the one verified build."
  - cmd: "curl -s 'http://127.0.0.1:9200/xc-xerj-storage*/_count'"
    note: "Count the records yourself. The script already did, and refused to switch on zero."
  - cmd: "env XERJ_URL=http://127.0.0.1:9200 tools/xerj-code/scripts/xc.py xerj-storage 'merge policy segment selection'"
    note: "Query the rebuilt corpus. xc.py addresses the verified build, not the whole namespace."
links_out:
  - "index-reference-library-for-coding-agent"
  - "resume-interrupted-autoindex-run"
  - "autoindex-exit-codes"
  - "check-codebase-index-is-complete"
evidence:
  - claim: "On v1.0.0-rc.74 a state directory written before the generation-v1 format aborted with exit 1: this state directory contains a legacy nonempty plan that cannot become generation authority."
    source: "benchmarks/autoindex-resilience/fresh-before-rc74.legacy-state.txt"
  - claim: "On v1.0.0-rc.74 xerj autoindex --fresh over a committed generation aborted with exit 1: --fresh cannot discard committed corpus generation 1 under the same destination."
    source: "benchmarks/autoindex-resilience/fresh-before-rc74.committed-generation.txt"
  - claim: "The --fresh contract is pinned by 46 offline checks: a new state directory and prefix, --fresh never forwarded to autoindex, nothing deleted before the replacement's count was read, deletes by exact name, a failed or empty build leaves the old index and state file untouched, and a sibling corpus is never touched."
    source: "tools/xerj-code/tests/test_xc_index_fresh.py"
  - claim: "xc.py queries index_prefix, the one verified build, and never the bare namespace glob once a corpus has been rebuilt."
    source: "tools/xerj-code/tests/test_state_ledger.py"
faq:
  - q: "How do I rebuild a reference-coding corpus index from scratch?"
    a: "Run `xc-index.sh <corpus> --fresh`. It builds a replacement beside the existing index, verifies that it holds records, switches the state file, and only then deletes the old indices by exact name."
  - q: "xc-index.sh --fresh failed. How do I rebuild a corpus index?"
    a: "Update the script. Older versions forwarded `--fresh` to `xerj autoindex`, which refuses it once a corpus generation has committed. The current script never forwards it and rebuilds under a new prefix instead."
  - q: "Does --fresh delete my working index first?"
    a: "No. The old index stays live until the replacement exited 0 or 3 and its record count is above zero. A build that fails or comes back empty is removed, and the old index and state file are left exactly as they were."
  - q: "Why does the state file have both prefix and index_prefix?"
    a: "`prefix` is always `xc-<corpus>`, the whole namespace, so anything that globs on it keeps working. `index_prefix` is the one verified build, and it is what `xc.py` queries."
  - q: "Is xc-index.sh --fresh the same as xerj autoindex --fresh?"
    a: "No. `xerj autoindex --fresh` only discards the resume journal, never removes records, and is refused once a durable generation exists. The script's `--fresh` is a full rebuild under a new prefix and state directory."
  - q: "How much disk does a rebuild need?"
    a: "Room for the corpus twice. Both builds exist on the node from the start of the rebuild until the old one is retired. This was not measured; plan for double."
---

**TL;DR** — Run `xc-index.sh <corpus> --fresh`. It builds a replacement beside the existing index, checks that it holds records, switches the state file, and only then deletes the old indices by exact name. A build that fails costs you nothing. Older versions of the script failed on every corpus that had been indexed before.

## Why the old --fresh failed

The script used to forward `--fresh` to `xerj autoindex`. That flag means something narrower than its name suggests. It discards the resume journal. It never removes records from the node. And it is refused once a durable corpus generation exists.

So the rebuild failed in both states a previously indexed corpus can be in.

A state directory written before the `generation-v1` format cannot be adopted:

```text
error: this state directory contains a legacy nonempty plan that cannot become generation authority
```

A state directory written since refuses the flag outright:

```text
error: this attempt made no remote mutations. `--fresh` cannot discard committed corpus generation 1 under the same destination
```

Both messages give the same advice: build into a new `--state-dir` and a new `--prefix`, validate, then switch readers. A wrapper that owns the whole `xc-<corpus>` namespace can do exactly that. Now it does.

## What --fresh does now

| Step | What happens | What can go wrong |
| --- | --- | --- |
| Build | `xerj autoindex` runs with prefix `xc-<corpus>-b<stamp>` and a state directory of its own. `--fresh` is not forwarded. | Nothing an earlier run left behind can be adopted, or can refuse. |
| Verify | autoindex exited 0 or 3, and `_count` for the new prefix is above zero. | A zero count fails the build, even on exit 0. |
| Switch | `state/<corpus>.json` is rewritten by atomic rename. | A reader sees the old build or the new one, never half. |
| Retire | The old indices are deleted by exact name, then their catalog documents and state directory. | A crash between switch and retire leaves a duplicate, never a gap. |

The old indices are listed before the build starts. The retire step can therefore never include what the run itself created.

## A failed build costs nothing

If autoindex aborts, or exits 0 with zero records, the script removes only what that build created. The old indices, the old state file and the old state directory stay exactly as they were, and the script says so:

```text
xc-index: the existing index was NOT touched and is still what xc.py serves.
```

There is one case where a failed build is kept. autoindex can abort in finalisation after every document was written. If that happens and there is no working index to fall back to, the script records the build as indexed, stores the real exit code in `autoindex_exit`, and prints a warning that coverage is not guaranteed. It never swaps a working index out for a build that failed.

## Two prefixes in the state file

```text
{"corpus":"xerj-storage","prefix":"xc-xerj-storage","index_prefix":"xc-xerj-storage-b20260918101500", ...}
```

`prefix` is always `xc-<corpus>`. It matches every build's indices, so anything that globs `xc-<corpus>*` keeps working across a rebuild.

`index_prefix` is the one build that was verified. `xc.py` queries it. During a rebuild the namespace holds two builds, and a query against the whole namespace would return passages twice, or from a half-built index.

A state file written before builds existed has no `index_prefix`. `xc.py` falls back to `prefix` for it.

## Siblings are safe

`xc-battle-*` also matches the indices of a corpus named `battle-terse`. The script knows which corpus names extend this one, from `corpora/` and from `state/`, and leaves their indices out of every list. Deletes are by exact index name. There is no wildcard delete anywhere in the script.

## A plain re-run updates in place

`xc-index.sh <corpus>` without `--fresh` re-runs autoindex against the recorded build's prefix and state directory. Additions, edits, deletions and renames reconcile incrementally, and the index prefix does not change.

If that run fails with the `generation authority` message, the corpus predates the generation format. Run `--fresh` once.

## What to plan for

A rebuild holds the corpus on the node twice until the old build is retired. Plan disk for double. This was not measured.

The old state directory of a corpus indexed before builds existed lives under `~/.xerj/autoindex/`, in a folder named by a hash. The script cannot compute that name, so it leaves the folder behind. It is harmless, and you can delete it by hand.

## How this page was checked

The two error messages are captures from v1.0.0-rc.74, committed under `benchmarks/autoindex-resilience/`. Local paths in them were shortened; nothing else was changed.

The new behaviour is pinned by `tools/xerj-code/tests/test_xc_index_fresh.py`: 46 offline checks against a fake node and a fake `xerj` binary that refuses what the real one refuses. The same test fails against the old script with the first error above.

Writing that test found three defects in the first draft of the new script. An index listing that never reached its filter, a record count that read as zero when the node put a space after the colon, and two rebuilds inside one second retiring the build they had just verified. All three are fixed and covered.
