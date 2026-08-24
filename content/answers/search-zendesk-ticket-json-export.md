---
title: "Search a JSON export of support tickets"
h1: "I have a JSON export of Jira issues (or Zendesk tickets). How do I search it like a help-center search?"
description: "Index the exported ticket JSON on disk. A captured run returned 36 description hits with per-ticket locators and an exact status count of 30 per status."
slug: "search-zendesk-ticket-json-export"
cluster: "Files and formats"
question: "I have a JSON export of support tickets. How do I search it like a real help-center search?"
intent: "tool-selection"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, save a Zendesk incremental ticket export to a local folder as tickets.json, ticket_comments.json and users.json, start a local XERJ node, run `xerj autoindex ./zendesk-export --url http://127.0.0.1:9200 --prefix zd --progress plain`, POST a match_phrase on description, POST a terms aggregation on status with size 0, and report the hit locators, the bucket counts and the difference between the bucket total and track_total_hits."
commands:
  - cmd: "xerj autoindex ./zendesk-export --url http://127.0.0.1:9200 --prefix zd --progress plain"
    note: "Index a saved Zendesk export folder from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/zd-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"description\":\"aardvark billing dispute\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_locator\",\"id\",\"subject\",\"status\",\"priority\"],\"track_total_hits\":true}'"
    note: "Full-text search the ticket description and get a per-ticket locator back."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/zd-json-tickets/_search -H 'content-type: application/json' -d '{\"size\":0,\"aggs\":{\"by_status\":{\"terms\":{\"field\":\"status\",\"size\":20}}},\"track_total_hits\":true}'"
    note: "Count tickets per status, exactly, and read the total beside the buckets."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/zd-json-tickets/_search -H 'content-type: application/json' -d '{\"query\":{\"terms\":{\"tags\":[\"escalated\"]}},\"size\":10,\"_source\":[\"id\",\"status\",\"tags\"],\"track_total_hits\":true}'"
    note: "Filter the multi-valued tags field for one tag value."
links_out:
  - "search-json-and-jsonl-logs"
  - "catalog-files-with-autoindex-map"
  - "give-chatgpt-claude-local-file-access"
faq:
  - q: "I have a JSON export of support tickets. How do I search it like a real help-center search?"
    a: "Save the export to disk, index the folder, then send a `match_phrase` on the description field. The captured run returned 36 hits, each carrying the ticket `id`, `subject`, `status` and `priority`."
  - q: "How do I search through an export of Zendesk tickets?"
    a: "Save the export to local disk and run `xerj autoindex` on the folder. The captured node observed 0 non-loopback peers over its whole life."
  - q: "How do I search a Jira export without Jira?"
    a: "It is the same job: the export is JSON on disk, so the fields are typed and an issue key, a status or a comment token becomes a filter. This capture used a Zendesk export, so the Jira field names were not verified here."
  - q: "Are the status counts exact or estimated?"
    a: "Exact. The captured aggregation returned 6 buckets of 30 with `doc_count_error_upper_bound` 0 and `sum_other_doc_count` 0."
  - q: "Why do the status buckets total 180 when the search says 181?"
    a: "The extra document is the whole-file document for `tickets.json`, which carries no `status`. Compare the bucket total with `track_total_hits` before you report a count."
  - q: "How do I filter on ticket tags?"
    a: "XERJ typed `tags` as a multi-valued `keyword`, so a `terms` filter with the tag value returns the matching tickets. `custom_fields` is `keyword` too."
  - q: "Do ticket comments become searchable too?"
    a: "Yes. The captured run put `ticket_comments.json` into its own index, `zd-json-ticket-comments`, with 61 documents."
---

**TL;DR** — XERJ answers a Zendesk ticket search from the exported JSON on local disk, with 1 `xerj autoindex` run. In a captured run, a phrase from a ticket description returned 36 hits with per-ticket locators such as `tickets:e4`. A `status` aggregation returned 6 buckets of exactly 30.

## Index the saved export

Zendesk's incremental export gives you JSON with a `tickets` array, a `count` and an `end_of_stream` flag. XERJ reads that file as ordinary JSON and gives each exported file its own index.

```sh
xerj autoindex ./zendesk-export --url http://127.0.0.1:9200 --prefix zd --progress plain
```

The captured run read 3 files into 3 datasets and 246 documents live: `zd-json-tickets` with 181 documents, `zd-json-ticket-comments` with 61 and `zd-json-users` with 4. There were 0 junk files.

The fixture generator wrote that payload to Zendesk's incremental-export shape. Zendesk never ran on the host, and no account was connected, so this page tests the exported JSON and not a live integration.

## A description phrase finds the exact ticket

A `match_phrase` on `description` returned 36 hits. Each hit named the file, the position inside the `tickets` array, and the ticket's own fields.

| field | example value |
| --- | --- |
| `ax_path` | `tickets.json` |
| `ax_locator` | `tickets:e4` |
| `id` | `5` |
| `subject` | `Aardvark billing dispute on invoice #5` |
| `status` | `closed` |
| `priority` | `normal` |

```sh
curl -s -XPOST 'http://127.0.0.1:9200/zd-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"description":"aardvark billing dispute"}},"size":5,"_source":["ax_path","ax_locator","id","subject","status","priority"],"track_total_hits":true}'
```

The locator is what makes this useful to an agent. The value `tickets:e4` names element 4 of the `tickets` array. A follow-up read therefore opens 1 ticket instead of the whole export.

## Status counts are exact

A terms aggregation on `status` returned 6 buckets of exactly 30 documents each. The keys were `closed`, `hold`, `new`, `open`, `pending` and `solved`. XERJ reported `doc_count_error_upper_bound` 0 and `sum_other_doc_count` 0, because every aggregation in XERJ is exact.

```json
{"by_status": {"buckets": [{"key": "closed",  "doc_count": 30},
                           {"key": "hold",    "doc_count": 30},
                           {"key": "new",     "doc_count": 30},
                           {"key": "open",    "doc_count": 30},
                           {"key": "pending", "doc_count": 30},
                           {"key": "solved",  "doc_count": 30}],
               "doc_count_error_upper_bound": 0, "sum_other_doc_count": 0}}
```

The buckets total 180 while the same request reported 181 hits. The extra document is the whole-file document for `tickets.json`, which carries no `status`. Compare the bucket total with `track_total_hits` whenever the number is the deliverable.

## The fields a ticket produces

XERJ inferred the types from the values, with no mapping written by hand.

| field | inferred type |
| --- | --- |
| `description` | `semantic_text` |
| `subject`, `raw_subject` | `text` |
| `status`, `priority`, `type`, `tags`, `via_channel`, `custom_fields` | `keyword` |
| `created_at`, `updated_at`, `generated_timestamp` | `date` |
| `id`, `requester_id`, `assignee_id`, `organization_id`, `group_id` | `long` |
| `is_public`, `has_incidents`, `allow_channelback` | `boolean` |

The `date` typing is what makes a range query over `created_at` work straight after the run. The default embedder in XERJ is lexical feature hashing, so a `semantic_text` field written by that embedder cannot connect a query to a synonym. Neural embeddings are opt-in through `--embed-mode neural`.

## What this capture does not show

XERJ has no Zendesk connector, no API token flow and no sync job. The node observed 0 distinct non-loopback peers across 185 samples over its whole life. Request the export from Zendesk yourself, save it to disk, then index the folder.

If you flatten the export to JSONL, keep every line under 4,096 characters. A captured reproduction indexed 30 documents at 4,095 characters per line and 0 documents at 4,096, with the family flipping from `jsonl` to `json`.

XERJ runs single-node here, with no replication and no failover. Every number above comes from RUN-B, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
