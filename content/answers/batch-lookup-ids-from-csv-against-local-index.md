---
title: "Look up many IDs in one local query"
h1: "Look up 1,000 IDs against a local index"
description: "Put the IDs in one terms query. On 2026-10-20, 1,000 IDs against 532 issues returned took 0: 532 found, 468 absent."
slug: "batch-lookup-ids-from-csv-against-local-index"
cluster: "Batch lookup"
question: "Look up 1,000 IDs against a local dataset"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node, index the dataset, and read the id field type from xerj autoindex map. Send one terms query with every id from the CSV. Print found or not found for each id from the hits. A wildcard index name on _msearch is not a pattern."
answer: "Look up 1,000 IDs against a local index with one `terms` query, then mark each ID found or not found. On 2026-10-20 the query sent 1,000 numbers over 532 issues. The response `took` was 0. 532 IDs were in the index and 468 were not."
key_fact: "A terms query of 1,000 IDs against 532 issues returned 532 hits and took 0 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/08-batch-terms.json"
key_fact_date: "2026-10-20"
intent_group: "Batch lookup"
queries:
  - "lookup 1000 ids against local dataset fast"
  - "batch query local index from csv of ids"
  - "msearch elasticsearch multiple queries"
  - "batch search many queries one request search engine"
commands:
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{\"size\":0,\"track_total_hits\":true,\"query\":{\"terms\":{\"number\":[960,488,484]}}}'"
    note: "The shape of the query. The capture sent 1,000 numbers, not these three."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/_msearch -H 'content-type: application/x-ndjson' --data-binary @benchmarks/agent-queries-2026-10-20/fixtures/msearch-named.ndjson"
    note: "Named index gh-jsonl. The capture's item status was 200."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{\"size\":3,\"_source\":[\"number\",\"title\",\"state\"],\"query\":{\"match\":{\"title\":{\"query\":\"feedbak open-pr\",\"fuzziness\":\"AUTO\"}}}}'"
    note: "One misspelling. Issue 960 ranked first, took 1 ms."
links_out:
  - "index-multiple-csv-files"
  - "search-json-and-jsonl-logs"
  - "find-duplicate-github-issues-before-filing"
evidence:
  - claim: "A fuzziness AUTO query took 1 ms and ranked issue 960 first."
    source: "benchmarks/agent-queries-2026-10-20/08-fuzzy.json"
  - claim: "The terms query over 1,000 ids returned took 0 and 532 hits."
    source: "benchmarks/agent-queries-2026-10-20/08-batch-terms.json"
og_image: "og/answers-batch-lookup-ids-from-csv-against-local-index.png"
og_image_alt: "Cover: a bar of squares, 532 filled ochre and the rest hollow, for 532 found IDs out of 1,000."
faq:
  - q: "Look up 1,000 IDs against a local dataset?"
    a: "One `terms` query on the id field. The capture sent 1,000 numbers to `gh-jsonl`. `took` was 0. 532 were issues in the index and 468 were not."
  - q: "Batch query a local index from a CSV of IDs?"
    a: "Turn the column into a JSON array and put it in `terms`. Print found or not found yourself. There is no batch subcommand."
  - q: "msearch several queries in one request?"
    a: "Name the index. `{\"index\":\"gh-*\"}` came back as item status 404, `no such index [gh-*]`. `{\"index\":\"gh-jsonl\"}` came back status 200."
  - q: "Join a CSV with a fuzzy match?"
    a: "One observed misspelling, `feedbak open-pr`, ranked issue 960 first in 1 ms, with 13 hits. That is one query, not a measured join accuracy."
  - q: "What if the index name is wrong?"
    a: "Guessing `gh-issues` returned 404. The real names were `gh-jsonl` and `gh-json`. Read `xerj autoindex map`."
---

**TL;DR** — Look up 1,000 IDs against a local index with one `terms` query, then mark each ID found or not found. On 2026-10-20 the query sent 1,000 numbers over 532 issues. The response `took` was 0. 532 IDs were in the index and 468 were not.

XERJ is a new way for AI to search for data.

## One terms query

The id field on the issue export was `number`, type `long`. The capture built 1,000 integers: every issue number in the export, then unused integers up to 1,000. The list is `04-ids.txt`.

```sh
curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{"size":0,"track_total_hits":true,"query":{"terms":{"number":[960,488,484]}}}'
```

The command above shows the shape with three ids. The captured body sent all 1,000. `size` 0 asks for the count. The response was `took` 0 and `hits.total.value` 532. The other 468 ids were absent. Found or not found is that comparison. It is not a per-id abstain feature.

Guessing the index name `gh-issues` returned HTTP 404. The issues lived in `gh-jsonl`.

## _msearch needs the real name

A wildcard header did not expand:

```text
{"index":"gh-*"}
```

The HTTP status of the call was 200. The item inside was status 404, `no such index [gh-*]`.

The same call with `{"index":"gh-jsonl"}` and 6 issue numbers returned item status 200, `took` 0, and 6 hits.

## One fuzzy query, nothing more

```sh
curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{"size":3,"_source":["number","title","state"],"query":{"match":{"title":{"query":"feedbak open-pr","fuzziness":"AUTO"}}}}'
```

`took` was 1. 13 hits. Issue 960 was first, then 488, then 1837. That is the whole observation.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| 1,000 ids, `terms` on `gh-jsonl` | `took` 0. 532 found, 468 absent |
| `_msearch` index `gh-*` | Item status 404, `no such index [gh-*]` |
| `_msearch` index `gh-jsonl`, 6 ids | Item status 200, 6 found, `took` 0 |
| `fuzziness` `AUTO` on `feedbak open-pr` | Issue 960 first, 13 hits, `took` 1 ms |
| Index name `gh-issues` | HTTP 404 |

## When not to use XERJ

If the ids are exact and they already live in one table, a SQL join is the smaller tool. Use the terms query when the ids are a file and the records are an index you already built.
