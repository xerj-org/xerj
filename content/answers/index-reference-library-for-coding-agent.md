---
title: "Index a reference library for a coding agent"
h1: "How do I set up reference coding so the agent retrieves a mechanism before writing code?"
description: "Clone the library you are about to write against, index it with xerj autoindex, and retrieve the passage first. The published study, with the tracks where it loses."
slug: "index-reference-library-for-coding-agent"
cluster: "Coding agents: reference retrieval"
question: "How do I set up reference coding so the agent retrieves a mechanism before writing code?"
intent: "how-to"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent about to write against an unfamiliar library. Read https://xerj.org/llms.txt, clone the library into ./ref, start a XERJ node with --insecure, run xerj autoindex ./ref --prefix ref, read xerj autoindex map, then answer with a defs query and quote the retrieved passage with its ax_path before you write any code."
commands:
  - cmd: "git clone --depth 1 https://github.com/serde-rs/serde ./ref/serde"
    note: "Put the reference source on local disk. A shallow clone is enough to read from."
  - cmd: "xerj autoindex ./ref --url http://127.0.0.1:9200 --prefix ref --state-dir ./state-ref --progress plain --dry-run"
    note: "Print the plan and the ignore accounting before anything is written."
  - cmd: "xerj autoindex ./ref --url http://127.0.0.1:9200 --prefix ref --state-dir ./state-ref --progress plain"
    note: "Index the reference tree. Definitions are extracted through tree-sitter."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/ref-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"defs\":\"deserialize_seed\"}},\"size\":5,\"_source\":[\"ax_path\",\"language\",\"symbols\"]}'"
    note: "Retrieve the declaration site and its symbols instead of opening the file."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Register this line as an MCP server in the agent host, so the agent queries the index itself. It attaches to the node you already started."
links_out:
  - "index-monorepo-for-agent"
  - "code-search-mcp-for-claude-code"
  - "syntax-aware-code-search-refactoring"
  - "compare/xerj-vs-ripgrep-for-code-agents"
evidence:
  - claim: "Across five languages, 16 runs: the xerj arm solved 16/16 with 9,982 output tokens at $1.58, the native arm solved 16/16 with 26,477 output tokens at $3.27, and the bare arm solved 11/16 at $11.18."
    source: "docs/case-studies/reference-coding/data/results-multilang-final.json"
  - claim: "On the seven domains with an unrecallable runtime contract, 21 runs: the bare arm solved 1/21 at $21.90, the native arm 21/21 at $4.26, and the xerj arm 21/21 at $3.38."
    source: "docs/case-studies/reference-coding/CASE_STUDY.md:569"
  - claim: "On memorized logic, 6 runs: the bare arm solved 6/6 at $1.49, the native arm 6/6 at $9.14, and the xerj arm 5/6 at $4.40. Retrieval lost that track."
    source: "docs/case-studies/reference-coding/data/results-realism.json"
  - claim: "On memorized bulk data, 3 runs: every arm solved 3/3, the native arm at $0.60 and the xerj arm at $1.43. Retrieval was overhead."
    source: "docs/case-studies/reference-coding/data/results-search.json"
  - claim: "The 13 reference libraries were written for the study, so they cannot be in any training set, and a real private codebase is the untested end state."
    source: "docs/case-studies/reference-coding/SUMMARY.md"
  - claim: "A follow-up experiment on richer symbols and identifier tokenization measured no improvement on realistic queries on the same corpus."
    source: "docs/case-studies/reference-coding/SERVER_UPLIFT_SCORECARD.md"
faq:
  - q: "What is reference coding?"
    a: "Retrieving how a library actually implements a mechanism, and putting that passage in front of the agent, before the agent writes the code that calls it. The retrieval is the whole intervention."
  - q: "My agent keeps inventing methods that do not exist in the library. What do I do?"
    a: "Clone the library, run `xerj autoindex` on it, and have the agent query the `defs` field first. A hit names the declaration site with its file and line, so the agent quotes real symbols instead of plausible ones."
  - q: "Does handing the agent a retrieved reference actually change the result?"
    a: "On code the model cannot recall, yes. In the study's unrecallable-contract track, 21 runs, the memory-only arm solved 1/21 at $21.90 and the retrieval arm solved 21/21 at $3.38. Those libraries were written for the study."
  - q: "When is this not worth doing?"
    a: "When the reference is public code the model already memorized. On the memorized-logic track the retrieval arm solved 5/6 at $4.40 while the memory-only arm solved 6/6 at $1.49. That is a XERJ loss and it is the honest answer for popular libraries."
  - q: "How is this different from indexing my own monorepo?"
    a: "The monorepo case is about stopping an agent loading your own tree into context. This one is about a library you did not write, cloned on purpose so the agent can read how it works. See the monorepo page for the ignore accounting."
  - q: "Do I need embeddings turned on for this?"
    a: "No. The default embedder is lexical feature hashing, and the study ran with it. Neural embeddings are opt-in through `--embed-mode neural` and were not the measured configuration."
  - q: "Was this measured on a real private codebase?"
    a: "No. The 13 reference libraries were written for the study so they could not be in a training set. A real private codebase is the untested end state, and the page says so rather than assuming it transfers."
---

**TL;DR** — Clone the library you are about to write against. Index it with `xerj autoindex`. Let the agent retrieve the passage before it writes code. A published study measured a gain on code the model cannot recall, and a loss on code it knows.

## The four steps

Reference coding is a retrieval step placed before the writing step. Put the reference source on disk, index it, query it, and quote the hit into the prompt.

```sh
git clone --depth 1 https://github.com/serde-rs/serde ./ref/serde
xerj autoindex ./ref --url http://127.0.0.1:9200 --prefix ref --state-dir ./state-ref --progress plain --dry-run
xerj autoindex ./ref --url http://127.0.0.1:9200 --prefix ref --state-dir ./state-ref --progress plain
```

Run the dry run first. It prints the job size and every ignore rule that fired, and it writes nothing.

Then query the `defs` field rather than opening files. A hit carries `ax_path`, `language` and a `symbols` array. Each symbol carries a kind, a name and a line number.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/ref-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"defs":"deserialize_seed"}},"size":5,"_source":["ax_path","language","symbols"]}'
```

Register the same node with the agent host through `xerj mcp`, a stdio MCP server in the same binary. The agent then performs the retrieval itself, instead of waiting for you to paste it.

## What was measured

The study ran the same Claude Code three ways against 13 reference libraries in Rust, Python, JavaScript, C and Java. Only the retrieval differed between the second and third arms.

| arm | what it had |
| --- | --- |
| bare | tools disabled, answering from memory |
| native | the agent's own source-searching tools |
| xerj | the same agent, with the reference retrieved by XERJ and injected |

The verdict was objective: the generated code had to compile and pass a hidden test the model never saw.

**Across five languages, 16 runs.** The native and xerj arms both solved 16/16. The xerj arm spent 9,982 output tokens at $1.58; the native arm spent 26,477 output tokens at $3.27. The bare arm solved 11/16 at $11.18.

**On seven domains carrying a runtime contract the compiler cannot warn about, 21 runs.** The bare arm solved 1/21 while burning $21.90. The native arm solved 21/21 at $4.26 and the xerj arm solved 21/21 at $3.38.

Both tracks carry the same caveat: **the reference libraries were written for the study**, so no model could have memorized them. That is what makes the contrast measurable, and it is also the reason the result does not automatically transfer to your repository.

## Where retrieval loses

The value is gated by memorization, and the line is sharp. On public code the model already knows, injecting a reference is neutral at best.

| memorized track | bare | native | xerj |
| --- | --- | --- | --- |
| memorized logic, 6 runs | 6/6 at $1.49 | 6/6 at $9.14 | **5/6 at $4.40** |
| memorized bulk data, 3 runs | 3/3 at $0.98 | 3/3 at $0.60 | 3/3 at $1.43 |

On the memorized-logic track the retrieval arm lost a task the memory-only arm solved, and cost three times as much. On the memorized bulk-data track the model reproduced a 256-value quantization table from memory, and the injected reference only gave it more to re-emit.

Do not run this workflow against a popular open-source library the model can already quote. Run it against code the model has never seen.

## How this differs from indexing your own monorepo

Both workflows start with `xerj autoindex`, and they answer different questions.

Indexing your own tree is about context budget: the agent stops opening every file and queries the index instead. That walkthrough, with the captured ignore accounting, is on [how to stop an agent reading the whole repo](/answers/index-monorepo-for-agent).

Reference coding is about knowledge the agent does not have. The tree is somebody else's library, cloned for the purpose, and the point is the passage that describes the mechanism, not the size of the context.

## The retrieval boundary

Extraction is definitions only. There is no call graph, no import graph and no cross-file reference resolution, so a `defs` hit names the declaration site and never the call site.

The default embedder is lexical feature hashing, not a neural model. A query and a paraphrase that share no words do not match, and neural retrieval is opt-in through `--embed-mode neural`. The study ran on the default.

XERJ is single-node. There is no replication and no failover.

## What this page did not re-run

The numbers above come from the committed case study captured on 2026-08-05, not from a fresh run on the library named in the commands. The commands are the documented workflow; the measurement is the study.

A follow-up experiment tried to improve the retrieval itself, with richer symbols and identifier tokenization. It measured no improvement on realistic queries on that corpus. The effect that held is retrieval against no retrieval, not a better retriever.

One more gap worth naming: the gain was measured on small reference libraries, where searching for a distinctive name is already precise. Whether it grows on a large private corpus is untested, and this page does not claim it.
