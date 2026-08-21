---
title: "How do I search a Slack JSON export?"
h1: "How do I search a Slack JSON export?"
description: "Point xerj autoindex at an unzipped Slack export. A captured run found a threaded reply, kept ts, thread_ts and user, and round-tripped Japanese text."
slug: "search-slack-json-export"
cluster: "Files and formats"
question: "How do I search a Slack or Discord JSON export?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, unzip a Slack export so that channels.json sits beside one folder per channel, run `xerj autoindex ./slack-export --url http://127.0.0.1:9200 --prefix sl --progress plain`, then POST a match_phrase on text for a string written inside a threaded reply, GET the mapping for one channel index, and report the hit ax_path, ax_locator, ts, thread_ts and user for every hit."
commands:
  - cmd: "xerj autoindex ./slack-export --url http://127.0.0.1:9200 --prefix sl --progress plain"
    note: "Index an unzipped Slack export folder from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/sl-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"text\":\"narwhal deployment freeze\"}},\"size\":5,\"track_total_hits\":true}'"
    note: "Find a phrase written inside a threaded reply, across every channel index."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/sl-incidents/_mapping"
    note: "Read the fields one channel produced, with the type XERJ inferred for each."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/sl-incidents/_search -H 'content-type: application/json' -d '{\"query\":{\"terms\":{\"reply_users\":[\"U02BBBB\"]}},\"size\":10,\"_source\":[\"ax_path\",\"ax_locator\",\"user\",\"ts\",\"reply_users\"],\"track_total_hits\":true}'"
    note: "Filter the multi-valued reply_users field for one user id."
links_out:
  - "search-json-and-jsonl-logs"
  - "give-chatgpt-claude-local-file-access"
  - "catalog-files-with-autoindex-map"
faq:
  - q: "How do I search a Slack export without a Slack connection?"
    a: "Unzip the export to local disk and run `xerj autoindex` on the folder. The captured node observed 0 non-loopback peers over its whole life."
  - q: "Are messages inside a thread searchable?"
    a: "Yes. A phrase written only inside a threaded reply returned 2 hits. The hit carried `ax_path` `incidents/2026-01-15.json` and `ax_locator` `e1`."
  - q: "Does XERJ keep the Slack timestamps and the author?"
    a: "Yes. The captured hit carried `ts` 1768478460.0002, `thread_ts` 1768478400.0001 and `user` U02BBBB, so a reply can be traced to its parent."
  - q: "Do Japanese and emoji messages survive indexing?"
    a: "Yes. A Japanese message returned 1 hit and came back byte-identical, and French accented text and emoji appear unchanged in the captured hits."
  - q: "How do I filter on reactions or on reply users?"
    a: "`reply_users` is a keyword array, so a `terms` filter returns the thread. `reactions` holds the whole array as one string, so it needs a `wildcard`."
  - q: "Is ts indexed as a date field?"
    a: "No. XERJ typed `ts` and `thread_ts` as `double` in the captured run, because Slack writes an epoch value with a fractional part."
  - q: "Can I convert the export to JSONL first?"
    a: "Keep every line under 4,096 characters. A captured reproduction indexed 30 documents at 4,095 characters per line and 0 at 4,096."
  - q: "I downloaded a Discord server export. How do I search it?"
    a: "Unzip it to local disk and run the same `xerj autoindex` command on the folder. A Discord export is JSON, and XERJ picks the family by reading the content rather than the file name. No Discord export was captured for this page, so read `xerj autoindex map` and the mapping for your own export before you write a filter."
---

**TL;DR** — Unzip the Slack export to local disk, then run `xerj autoindex` on the folder. In a captured run, a phrase written inside a threaded reply returned 2 hits at `incidents/2026-01-15.json` with `ax_locator` `e1`. The `ts`, `thread_ts` and `user` values all survived.

## Index the unzipped export folder

Slack gives you `channels.json`, `users.json` and 1 folder per channel with 1 JSON file per day. Run `xerj autoindex` on that folder. XERJ reads the layout as ordinary JSON and gives each channel its own index.

```sh
xerj autoindex ./slack-export --url http://127.0.0.1:9200 --prefix sl --progress plain
```

The captured run read 6 files into 6 datasets and 23 documents live, with 0 junk files. The indices were `sl-incidents` with 6 documents, `sl-json-users` with 5, `sl-json-channels` with 4, `sl-general` with 3, `sl-releases` with 3 and `sl-docs` with 2.

The fixture generator wrote that export to Slack's on-disk format. Slack never ran on the host, and no workspace was connected, so this page tests the exported files and not a live integration.

## A threaded reply is findable, with its locator

A phrase that exists only inside a threaded reply returned 2 hits. The first hit named the exact source file and the message position inside it.

| field | value returned |
| --- | --- |
| `ax_path` | `incidents/2026-01-15.json` |
| `ax_locator` | `e1` |
| `user` | `U02BBBB` |
| `ts` | `1768478460.0002` |
| `thread_ts` | `1768478400.0001` |

`thread_ts` points at the parent message, so an agent can rebuild the thread from the same file. The message text in that hit mixes Japanese, French and emoji, and it came back unchanged.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/sl-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"text":"narwhal deployment freeze"}},"size":5,"track_total_hits":true}'
```

## Non-ASCII text round-trips

A message written in Japanese returned 1 hit for a Japanese phrase query. The returned `text` was `新しいランブックを共有します。 The runbook is in the vault.`, byte for byte.

Full-text ranking on `text` uses BM25. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

## The fields a Slack message produces

One channel index produced 24 fields. XERJ inferred the types from the values, without configuration.

| field | inferred type | note |
| --- | --- | --- |
| `text` | `text` | the message body, ranked with BM25 |
| `user`, `parent_user_id`, `team`, `client_msg_id` | `keyword` | exact-match identifiers |
| `ts`, `thread_ts`, `latest_reply` | `double` | epoch seconds with a fractional part, not a `date` |
| `reply_count`, `reply_users_count` | `long` | thread counters |
| `reply_users` | `keyword` | a real multi-valued array |
| `reactions` | `keyword` | 1 value holding the serialized JSON array |
| `blocks` | `semantic_text` | the rich-text block payload |

The `reactions` field is worth naming. XERJ did not expand it into 1 keyword per reaction name. The field holds the whole array as a single string such as `[{"name":"+1","users":["U01AAAA","U03CCCC"],"count":2}]`, so a reaction-name filter needs a `wildcard` or a client-side parse.

## If you flatten the export to JSONL, cap the line length

A long Slack message can push a JSONL line past a hard boundary. Keep every line under 4,096 characters, or the whole file is dropped.

A captured reproduction used 2 files of 30 valid JSON lines each, identical apart from length. At 4,095 characters the sniffer chose `jsonl` and indexed 30 documents. At 4,096 characters it chose `json`, failed to parse, and indexed 0.

## Discord and other chat exports

A Discord export is JSON on disk too. `xerj autoindex` picks the family for each file by reading the content, not the file name. The command above is the one to start with. That is where the verified part stops: no Discord export was captured for this page.

Nothing on this page is a Discord field name. The channel layout, the field names and any per-channel or per-guild filter all depend on what your export contains. Run `xerj autoindex map` and read the mapping for one index before you write a filter. The same applies to any other chat archive exported to JSON.

## What this capture does not show

XERJ has no Slack connector, no token flow and no message shipper. The node observed 0 distinct non-loopback peers across 185 samples during the whole run. Request the export from Slack yourself, unzip it, then index the folder.

XERJ runs single-node here, with no replication and no failover. Every number above comes from run RUN-B, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
