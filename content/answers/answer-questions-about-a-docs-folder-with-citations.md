---
title: "Answer questions about a docs folder"
h1: "Answer questions about a docs folder, with a file to cite"
description: "Index the docs folder with xerj autoindex. A phrase query returns the file to cite. xerj search on the same question ranked code lines instead."
slug: "answer-questions-about-a-docs-folder-with-citations"
cluster: "Folder indexing"
question: "Answer questions about a docs folder"
intent: "informational"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node, run xerj autoindex on the docs folder, then run xerj autoindex map and query the prose field it names. Cite ax_path for every passage you use. If xerj search returns code identifiers, say so and run a match_phrase on the prose field."
answer: "Answer questions about a docs folder by indexing it with `xerj autoindex`, then running a phrase query. Each hit carries `ax_path`, so the agent can cite the file. On 2026-10-20 a phrase query for \"what is in here\" returned `WHY_XERJ.md` and took 1 ms. `xerj search` on the same words did not."
key_fact: "A phrase query for \"what is in here\" returned WHY_XERJ.md and took 1 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/08-docs-phrase.json"
key_fact_date: "2026-10-20"
intent_group: "Folder indexing"
queries:
  - "answer questions about docs folder"
  - "local search for documentation with citations"
  - "search enterprise docs offline"
  - "search large roadmap and changelog files"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /workspace/docs --prefix docs --state-dir /tmp/xerj-aq-2026-10-20/state-docs"
    note: "Index the docs folder. The capture exited 3, which is completed-with-junk."
  - cmd: "xerj search --url http://127.0.0.1:9217 --prefix docs -k 3 what is in here"
    note: "Definition-first search. On this question it returned code lines, not the prose sentence."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/docs-docs/_search -H 'content-type: application/json' -d '{\"size\":3,\"_source\":[\"ax_path\",\"title\"],\"query\":{\"match_phrase\":{\"body\":\"what is in here\"}}}'"
    note: "Phrase query on the prose field. One hit, WHY_XERJ.md, took 1 ms."
links_out:
  - "how-xerj-autoindexes-a-folder"
  - "search-file-contents-in-a-folder"
  - "catalog-files-with-autoindex-map"
evidence:
  - claim: "autoindex of docs/ exited 3, wall 14.3 s, 9,940 records, 3 junk files."
    source: "benchmarks/agent-queries-2026-10-20/02-autoindex-docs.txt"
  - claim: "xerj search wrote 689 bytes and rg wrote 270 bytes for the same words."
    source: "benchmarks/agent-queries-2026-10-20/02-byte-compare.txt"
  - claim: "match_phrase on body took 1 ms and returned WHY_XERJ.md."
    source: "benchmarks/agent-queries-2026-10-20/08-docs-phrase.json"
og_image: "og/answers-answer-questions-about-a-docs-folder-with-citations.png"
og_image_alt: "Cover: three short lines of squares, the middle line in ochre, labelled WHY_XERJ.md from a 1 ms phrase query."
faq:
  - q: "Answer questions about a docs folder?"
    a: "Index the folder, then run a phrase query on the prose field. Cite `ax_path`. The agent writes the answer. XERJ returns the passage."
  - q: "Local search for documentation with citations?"
    a: "The 2026-10-20 phrase query returned `WHY_XERJ.md` and took 1 ms. Nothing in that query left the machine."
  - q: "Search large roadmap and changelog files?"
    a: "They are markdown in the same folder. A hit carries `ax_path`. This capture's prose hit did not include a line number."
  - q: "Does xerj search answer a prose question?"
    a: "Not this one. `xerj search` returned three code passages about a variable named `here`. `rg` found the sentence in `WHY_XERJ.md`."
  - q: "Can I run xerj ask on the folder?"
    a: "No. `xerj ask` answers hub corpora. A folder you autoindexed is `xerj search`, `xerj def`, or a `_search` body."
---

**TL;DR** — Answer questions about a docs folder by indexing it with `xerj autoindex`, then running a phrase query. Each hit carries `ax_path`, so the agent can cite the file. On 2026-10-20 a phrase query for "what is in here" returned `WHY_XERJ.md` and took 1 ms. `xerj search` on the same words did not.

XERJ is a new way for AI to search for data.

## Index the folder

The capture pointed `xerj autoindex` at this repo's `docs/` directory. It walked 294 files, skipped 10 by ignore rules, and committed 11 datasets and 9,940 records. Wall time was 14.3 s. Exit 3, `reason=completed-with-junk`: 3 files were junk, and the rest is searchable. Exit 3 is success.

```text
xerj-done ok=true exit=3 reason=completed-with-junk wall=14.3s files=290 records=9940 generation=1
```

`xerj autoindex map` names the real fields. On the markdown dataset the prose field was `body`.

## The code command missed the sentence

```sh
xerj search --url http://127.0.0.1:9217 --prefix docs -k 3 "what is in here"
```

The command printed three passages, all code, all a variable named `here`. It said to cite `ax_path:line`. The capture file is 689 bytes.

`rg -n` on `docs/**/*.md` printed one line, in `WHY_XERJ.md`, and that capture file is 270 bytes. For this question `rg` was shorter and it was the right line.

## Ask the prose field

```sh
curl -s -XPOST http://127.0.0.1:9217/docs-docs/_search -H 'content-type: application/json' -d '{"size":3,"_source":["ax_path","title"],"query":{"match_phrase":{"body":"what is in here"}}}'
```

One hit. `ax_path` was `WHY_XERJ.md`. `took` was 1. The title on the hit was the file's first heading.

The same response said the query scored `body` with BM25 and did not read the embedding. Ranking is lexical unless the node was started with `--embed-mode neural`.

XERJ does not write the answer. It returns the passage and the path. The agent quotes the path.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| `xerj autoindex` on `docs/` | Exit 3, 9,940 records, wall 14.3 s, 3 junk files |
| `xerj search` "what is in here" | 3 code passages. Capture file 689 bytes |
| `rg` the same words | 1 line in `WHY_XERJ.md`. Capture file 270 bytes |
| `match_phrase` on `body` | 1 hit, `WHY_XERJ.md`, `took` 1 ms |

One node, binary v1.0.0-rc.97. `rg` won this comparison on both bytes and the right line.

## When not to use XERJ

If the question is one exact string in a handful of files, `rg` already returned it. Use the index when the folder is large enough that you want a ranked passage and a path, or when the next step is an agent call rather than a scroll.
