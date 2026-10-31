---
title: "Search GitHub issues and PRs locally"
h1: "Search a repo's issues and pull requests on your machine"
description: "Export issues and pull requests to JSONL and index them locally. A captured gh search file was 36,550 bytes; xerj search's file was 1,865."
slug: "search-github-issues-and-prs-locally"
cluster: "GitHub issues"
question: "Search repo issues and pull requests locally"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node. Export issues and pull requests to JSONL, run xerj autoindex, then search with a _search body that lists _source fields. Report number, title and state. If a passage is cited as line 1 for two different issues, trust number and title, not the line."
answer: "Search a repo's issues and pull requests locally. Export them to JSONL, index the folder, and query that index instead of reading `gh` JSON. On 2026-10-20 the `gh` capture file was 36,550 bytes and the `xerj search` capture file was 1,865 bytes. Two of the three passages were cited as line 1."
key_fact: "The captured gh search file was 36,550 bytes and the xerj search file was 1,865 bytes."
key_fact_source: "benchmarks/agent-queries-2026-10-20/01-byte-compare.txt"
key_fact_date: "2026-10-20"
intent_group: "GitHub issues"
queries:
  - "search repo issues and prs"
  - "gh issue search returns too much json"
  - "mcp server search github repo issues without cloning"
  - "search closed issues for the same bug"
  - "link issues to code locations"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/issues --prefix gh --state-dir /tmp/xerj-aq-2026-10-20/state-gh"
    note: "Index the export. 1,532 records, wall 7.2 s."
  - cmd: "xerj search --url http://127.0.0.1:9217 --prefix gh -k 3 xerj feedback --open-pr commits and pushes"
    note: "Three passages. Two were labeled issues.jsonl:1."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-*/_search -H 'content-type: application/json' -d '{\"size\":5,\"_source\":[\"number\",\"title\",\"state\",\"url\"],\"query\":{\"multi_match\":{\"query\":\"xerj feedback --open-pr commits and pushes\",\"fields\":[\"title\",\"body\"]}}}'"
    note: "The field list is what an agent should read. Issue 960 ranked first."
links_out:
  - "search-json-and-jsonl-logs"
  - "code-search-mcp-for-claude-code"
  - "keep-index-current-as-files-change"
  - "find-duplicate-github-issues-before-filing"
evidence:
  - claim: "The issue export indexed in 7.2 s, 1,532 records, exit 0."
    source: "benchmarks/agent-queries-2026-10-20/01-autoindex-issues.txt"
  - claim: "xerj search's capture file was 1,865 bytes and the gh capture file was 36,550 bytes."
    source: "benchmarks/agent-queries-2026-10-20/01-byte-compare.txt"
  - claim: "The narrowed _search response reported _savings of 19,444 bytes, sampled."
    source: "benchmarks/agent-queries-2026-10-20/01-duplicate-search.json"
  - claim: "tools/list returned 17 tools."
    source: "benchmarks/agent-queries-2026-10-20/05-tool-names.txt"
og_image: "og/answers-search-github-issues-and-prs-locally.png"
og_image_alt: "Cover: a short ochre bar marked 1,865 bytes beside a long ink bar marked 36,550 bytes."
faq:
  - q: "Search repo issues and PRs locally?"
    a: "Export both to JSONL and index the folder. Search `gh-*`. The capture held 532 issues and 1,000 pull requests."
  - q: "gh issue search returns too much JSON?"
    a: "The captured `gh` file, with issue bodies, was 36,550 bytes. The captured `xerj search` file was 1,865 bytes. Narrow `_source` when you need fields, not a passage."
  - q: "Search GitHub issues without cloning the repo?"
    a: "You do not clone. `gh` writes the export, and the index stays on the machine. The agent then calls search or `xerj mcp`."
  - q: "Does this link an issue to the code?"
    a: "No. A passage quoted `feedback.rs` because the issue body contained those lines. There is no call graph."
  - q: "Why do two hits say line 1?"
    a: "In this capture `xerj search` labeled two different passages `issues.jsonl:1`. Read `number` and `title` from `_source`."
---

**TL;DR** — Search a repo's issues and pull requests locally. Export them to JSONL, index the folder, and query that index instead of reading `gh` JSON. On 2026-10-20 the `gh` capture file was 36,550 bytes and the `xerj search` capture file was 1,865 bytes. Two of the three passages were cited as line 1.

XERJ is a new way for AI to search for data.

## Export, then index

The capture saved 532 issues and 1,000 pull requests from `xerj-org/xerj` (`gh pr list --limit 1000` returned 1,000). One JSON object per line. `xerj autoindex` committed 1,532 records in 7.2 s, exit 0. The datasets were `gh-jsonl` and `gh-json`, not a name you guess.

```text
xerj-done ok=true exit=0 reason=completed wall=7.2s files=2 records=1532 generation=1
```

## What the agent should call

```sh
xerj search --url http://127.0.0.1:9217 --prefix gh -k 3 "xerj feedback --open-pr commits and pushes"
```

Three passages came back. The command said they are lexical and that it does not consult the embedding. The default embedder is lexical feature hashing. `--embed-mode neural` was not on. Two passages were labeled `issues.jsonl:1`. The third had no line. The scores printed were 42.22, then 35.68, then 44.36, so the print order was not score order.

The lines from `feedback.rs` were inside the issue text. That is a quote of the issue, not a link from the issue to the code.

## Return the fields, not the body

```sh
curl -s -XPOST http://127.0.0.1:9217/gh-*/_search -H 'content-type: application/json' -d '{"size":5,"_source":["number","title","state","url"],"query":{"multi_match":{"query":"xerj feedback --open-pr commits and pushes","fields":["title","body"]}}}'
```

`took` was 7. Issue 960 was first. `_savings.bytes` was 19,444, `measured` `sampled`. The same question through `gh issue list --json number,title,body,state,url` produced a capture file of 36,550 bytes, because each hit included `body`.

`xerj mcp` against this node returned 17 tools on rc.97, including `xerj_search`. On main it serves 23, or 20 in a build with no hub key. The server does not start a node.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| Index | 1,532 records, wall 7.2 s, exit 0 |
| `xerj search` capture file | 1,865 bytes, 3 passages, two labeled line 1 |
| `gh issue list --json` capture file | 36,550 bytes, bodies included |
| `_source` narrowed to 4 fields | `_savings` 19,444 bytes, sampled. `took` 7 |

The byte win sits next to the line-label loss. Use `number` and `title` when the line is 1.

## When not to use XERJ

If you need live issue state for one number, `gh issue view` is the current source. The index is the export you last wrote. Re-index when you refresh the file.
