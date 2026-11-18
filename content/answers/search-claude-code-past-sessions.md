---
title: "Search Claude Code past sessions"
h1: "Search Claude Code past sessions on disk"
description: "Index Claude Code session JSONL and search the message field. On a 2026-10-20 fixture, zstd returned 2 hits in message_content; xerj search returned none."
slug: "search-claude-code-past-sessions"
cluster: "Agent memory: setup"
question: "Search Claude Code past sessions"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node and run xerj autoindex on the session JSONL folder, then read xerj autoindex map for the message field. Search that field with a match query and cite ax_path. If xerj search returns no match, say so. This is BM25 over files you point at, not a session-memory feature."
answer: "Search Claude Code past sessions by indexing the session JSONL and querying the message field. On 2026-10-20 a three-file fixture, not a live chat folder, indexed 8 records in 0.6 s. Searching zstd on message_content returned 2 hits. xerj search on the same words returned no match."
key_fact: "A match on message_content for zstd returned 2 hits and took 0 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/08-sessions-message.json"
key_fact_date: "2026-10-20"
intent_group: "Agent memory: setup"
queries:
  - "search claude code past sessions"
  - "index agent session jsonl and search it"
  - "why did we decide this search past chats"
  - "search local session jsonl for a decision"
  - "find a past claude code session by keyword"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/sessions --prefix sessions --state-dir /tmp/xerj-aq-2026-10-20/state-sessions"
    note: "Index the fixture folder. 3 files, 8 records, wall 0.6 s, exit 0."
  - cmd: "xerj search --url http://127.0.0.1:9217 --prefix sessions -k 3 why did we decide zstd"
    note: "Definition-first search. This run printed no match."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/sessions-jsonl/_search -H 'content-type: application/json' -d '{\"size\":2,\"_source\":[\"type\",\"timestamp\",\"message_role\",\"message_content\",\"ax_path\"],\"query\":{\"match\":{\"message_content\":\"zstd\"}}}'"
    note: "Match on the flattened message field. 2 hits, both session-short.jsonl, took 0."
links_out:
  - "coding-agent-memory-across-sessions"
  - "search-json-and-jsonl-logs"
  - "keep-index-current-as-files-change"
evidence:
  - claim: "The session fixture indexed 8 records in 0.6 s, exit 0."
    source: "benchmarks/agent-queries-2026-10-20/07-autoindex-sessions.txt"
  - claim: "xerj search printed no match in sessions-*."
    source: "benchmarks/agent-queries-2026-10-20/07-xerj-search.txt"
  - claim: "A match on message_content for zstd returned 2 hits and took 0."
    source: "benchmarks/agent-queries-2026-10-20/08-sessions-message.json"
  - claim: "The tool-output line was 10769 bytes."
    source: "benchmarks/agent-queries-2026-10-20/07-long-line-bytes.txt"
  - claim: "A phrase search for TOOL_OUTPUT_MARKER returned 1 hit and took 0."
    source: "benchmarks/agent-queries-2026-10-20/08-sessions-marker.json"
  - claim: "MAX_LINE is commented as a 16 MB line cap."
    source: "engine/crates/xerj-autoindex/src/extract/mod.rs:96"
  - claim: "Issue 1793, Index past agent sessions as searchable memory, is open."
    source: "benchmarks/agent-queries-2026-10-20/09-issue-1793.json"
og_image: "og/answers-search-claude-code-past-sessions.png"
og_image_alt: "Cover: two rows, one ochre row marked 2 hits for zstd, and one hollow row marked no match."
faq:
  - q: "Search Claude Code past sessions?"
    a: "Point `xerj autoindex` at the session JSONL folder and query the message field the map names. On this fixture that field was `message_content`, and `zstd` returned 2 hits."
  - q: "Does xerj search find the decision?"
    a: "Not this time. `xerj search --prefix sessions` printed `no match` for `why did we decide zstd`. The hits were on `message_content`, not in that command."
  - q: "What about a long tool-output line?"
    a: "`session-tool.jsonl` is 10,769 bytes on one line. A phrase search for `TOOL_OUTPUT_MARKER` returned 1 hit in `sessions-docs`. The extractor line cap is 16 MB."
  - q: "Is session memory a shipped feature?"
    a: "No. Issue 1793, index past agent sessions as searchable memory, was open on 2026-10-20. This page is BM25 over files you point at."
  - q: "Were these someone's real chats?"
    a: "No. This machine had no `~/.claude` directory. The three files are a fixture in the Claude Code JSONL shape, committed under the benchmark folder."
---

**TL;DR** — Search Claude Code past sessions by indexing the session JSONL and querying the message field. On 2026-10-20 a three-file fixture, not a live chat folder, indexed 8 records in 0.6 s. Searching zstd on message_content returned 2 hits. xerj search on the same words returned no match.

XERJ is a new way for AI to search for data.

## Point autoindex at the session folder

This machine had no `~/.claude` directory. The capture indexed three fixture files in the Claude Code JSONL shape: `type`, `message`, `sessionId`, `uuid`, `timestamp`. Chat transcripts have no verified speaker schema, so this page reports that fixture and nothing else. The files are in `benchmarks/agent-queries-2026-10-20/fixtures/sessions/`.

```sh
xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/sessions --prefix sessions --state-dir /tmp/xerj-aq-2026-10-20/state-sessions
```

```text
xerj-done ok=true exit=0 reason=completed wall=0.6s files=3 records=8 generation=1
```

Two datasets. `sessions-jsonl` held 2 records, the two lines of `session-short.jsonl`, with `message_content`, `message_role` and `timestamp`. The other two files were sniffed as JSON, not as one record per line, and landed in `sessions-docs` (6 records). Read `xerj autoindex map` before you pick a field.

## xerj search missed; the message field hit

```sh
xerj search --url http://127.0.0.1:9217 --prefix sessions -k 3 why did we decide zstd
```

```text
xerj search: these hits are lexical; this command does not consult the embedding.
no match in sessions-* for: why did we decide zstd
```

`xerj search` is definition-first code search. These fixtures have no code symbols. The miss is the loss.

```sh
curl -s -XPOST http://127.0.0.1:9217/sessions-jsonl/_search -H 'content-type: application/json' -d '{"size":2,"_source":["type","timestamp","message_role","message_content","ax_path"],"query":{"match":{"message_content":"zstd"}}}'
```

`took` was 0. Both hits are `session-short.jsonl`. The user line is `why did we decide to compress segments with zstd`, timestamp `2026-10-01T15:04:05.000Z`. The assistant line stores the content array as a JSON string, including `We picked zstd`. The same word on `body` returned 0 hits. The decision text is on `message_content`.

The response scored that field with BM25. The default embedder is lexical feature hashing. `--embed-mode neural` was not on.

## A long line did index

`session-tool.jsonl` is one line of 10,769 bytes. A phrase search for `TOOL_OUTPUT_MARKER` returned 1 hit, `ax_path` `session-tool.jsonl`, on `sessions-docs`, and `took` was 0. The extractor's line cap, `MAX_LINE` in `extract/mod.rs`, is 16 MB. This line is under that cap, and it indexed.

Session memory as a product is issue 1793, and it was open on 2026-10-20. This page does not ship that feature.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| `xerj autoindex` on the 3-file fixture | 8 records, wall 0.6 s, exit 0 |
| `xerj search` for `why did we decide zstd` | `no match` |
| `match` `message_content` `zstd` | 2 hits, both `session-short.jsonl`, `took` 0 |
| `match` `body` `zstd` | 0 hits, `took` 0 |
| Line of 10,769 bytes, phrase `TOOL_OUTPUT_MARKER` | 1 hit, `session-tool.jsonl`, `took` 0 |

One node, binary v1.0.0-rc.97. The index stayed on that machine.

## When not to use XERJ

If you need a memory product that loads past sessions on its own, this is not it. Issue 1793 is the request. If the folder is empty, there is nothing to search.
