---
title: "Find duplicate GitHub issues before filing"
h1: "Find duplicate GitHub issues before you file"
description: "Export issues to JSONL, index them with xerj autoindex, and search the draft title. On 2026-10-20 issue 960 ranked first; a paraphrase did not."
slug: "find-duplicate-github-issues-before-filing"
cluster: "GitHub issues"
question: "Find duplicate GitHub issues before you file"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node. Export the repo issues to JSONL, run xerj autoindex on that folder, and search the draft title before you file. Report the top hit's number, state and title, and say when a reworded query does not rank the same issue first."
answer: "Find duplicate GitHub issues before you file. Export the issues to JSONL, index the folder with `xerj autoindex`, and search the draft title. On 2026-10-20 that search over 1,532 records ranked issue 960 first and took 7 ms. A paraphrase of the same bug did not rank it first."
key_fact: "A search over 1,532 indexed records ranked issue 960 first and took 7 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/01-duplicate-search.json"
key_fact_date: "2026-10-20"
intent_group: "GitHub issues"
queries:
  - "find duplicate github issues"
  - "check if feature request already exists"
  - "check if github issue is duplicate tool"
  - "dedupe github issues before filing"
  - "search closed issues for same bug"
  - "cluster similar bug reports"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/issues --prefix gh --state-dir /tmp/xerj-aq-2026-10-20/state-gh"
    note: "Index the JSONL export. The 2026-10-20 run committed 1,532 records in 7.2 s."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-*/_search -H 'content-type: application/json' -d '{\"size\":5,\"_source\":[\"number\",\"title\",\"state\",\"url\"],\"query\":{\"multi_match\":{\"query\":\"xerj feedback --open-pr commits and pushes\",\"fields\":[\"title\",\"body\"]}}}'"
    note: "Search the draft title words. Issue 960 ranked first."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-*/_search -H 'content-type: application/json' -d '{\"size\":5,\"_source\":[\"number\",\"title\",\"state\"],\"query\":{\"more_like_this\":{\"fields\":[\"title\",\"body\"],\"like\":[\"xerj feedback --open-pr commits and pushes into whatever repository it is run from\"],\"min_term_freq\":1,\"min_doc_freq\":1}}}'"
    note: "Ask for similar text. Read the top 5, not the hit count."
links_out:
  - "search-json-and-jsonl-logs"
  - "how-xerj-autoindexes-a-folder"
  - "keep-index-current-as-files-change"
evidence:
  - claim: "autoindex committed 1,532 records in 7.2 s, exit 0."
    source: "benchmarks/agent-queries-2026-10-20/01-autoindex-issues.txt"
  - claim: "The title-and-body query took 7 ms and ranked issue 960 first."
    source: "benchmarks/agent-queries-2026-10-20/01-duplicate-search.json"
  - claim: "The paraphrase query took 7 ms and ranked an unrelated pull request first."
    source: "benchmarks/agent-queries-2026-10-20/01-paraphrase-search.json"
  - claim: "more_like_this took 4 ms and still returned 1,501 hits."
    source: "benchmarks/agent-queries-2026-10-20/01-more-like-this.json"
og_image: "og/answers-find-duplicate-github-issues-before-filing.png"
og_image_alt: "Cover: a row of small squares with one ochre ring on the square for issue 960, the issue a 7 ms search ranked first."
faq:
  - q: "Find duplicate GitHub issues before filing?"
    a: "Export the issues to JSONL, index the folder, and search the draft title. In the 2026-10-20 capture, issue 960 ranked first."
  - q: "Check if a feature request already exists?"
    a: "Search the closed issues as well as the open ones. Issues 960, 488 and 484 were all closed, and they were the matching reports."
  - q: "Will a reworded title find the same issue?"
    a: "Not by default. The paraphrase ranked an unrelated pull request first. The default ranking matches words."
  - q: "Does more_like_this cluster bug reports?"
    a: "It put the three related issues in the top 5, and it also returned 1,501 hits. Read the top 5. The hit count is not a cluster size."
  - q: "Is there a GitHub Action that does this?"
    a: "No. This is a local check you run before you file. One-off lookups can stay on GitHub's own search."
---

**TL;DR** — Find duplicate GitHub issues before you file. Export the issues to JSONL, index the folder with `xerj autoindex`, and search the draft title. On 2026-10-20 that search over 1,532 records ranked issue 960 first and took 7 ms. A paraphrase of the same bug did not rank it first.

XERJ is a new way for AI to search for data.

## Export once, then search the draft

`gh issue list --json` writes one JSON array. Split it to one object per line, and do the same for pull requests. The capture used `number`, `title`, `body`, `state`, `url` and `createdAt` from `xerj-org/xerj`: 532 issues and 1,000 pull requests (`--limit 1000` returned 1,000).

```sh
xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/issues --prefix gh --state-dir /tmp/xerj-aq-2026-10-20/state-gh
```

The run committed generation 1. Two datasets, `gh-jsonl` and `gh-json`, held 1,532 records. Wall time was 7.2 s. Exit 0.

```text
xerj-done ok=true exit=0 reason=completed wall=7.2s files=2 records=1532 generation=1
```

## Search the words you are about to file

```sh
curl -s -XPOST http://127.0.0.1:9217/gh-*/_search -H 'content-type: application/json' -d '{"size":5,"_source":["number","title","state","url"],"query":{"multi_match":{"query":"xerj feedback --open-pr commits and pushes","fields":["title","body"]}}}'
```

`took` was 7. The first hit was closed issue 960, then closed issue 488, merged pull request 1771, closed issue 484, and merged pull request 981. Those three issues are the same bug, filed more than once.

The response also said the query scored `body` with BM25 and did not consult the embedding. The default embedder is lexical feature hashing. `--embed-mode neural` is off unless you start the node with it.

## A paraphrase is a different result

The same index, asked "does the feedback command commit files I did not mean to publish", took 7 ms and ranked merged pull request 968 first. That pull request is about object-storage watch, not this bug. Issue 484 was second.

`more_like_this` on the title of issue 960 took 4 ms. The top 5 included issues 960, 488 and 484. It also returned 1,501 hits, so the hit count is not a group of duplicates. Read the top 5.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| `multi_match` on `title` and `body` | Issue 960 first. `took` 7. 1,480 hits |
| Paraphrase with few shared words | Unrelated pull request 968 first. `took` 7 ms |
| `more_like_this` | Issues 960, 488, 484 in the top 5. 1,501 hits. `took` 4 ms |
| Index | 1,532 records, wall 7.2 s, exit 0 |

One node, `XERJ_MAX_PROCESS_MEMORY_MB=4096`, binary v1.0.0-rc.97. There is no duplicate probability and no confidence score on a hit.

## When not to use XERJ

A single lookup on a small repo does not need an index. GitHub's own search is enough, and you can stop there.

Refreshing the export is a new file on disk. Re-run `xerj autoindex` on that folder when the issues change.
