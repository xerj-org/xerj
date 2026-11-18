---
title: "Count keyword mentions by month"
h1: "Count mentions of a word over time in a posts file"
description: "Index a JSONL posts file and send one date histogram on the date field. On 2026-10-20, autoindex in issue titles returned 88 hits and took 123 ms."
slug: "count-keyword-mentions-over-time-jsonl"
cluster: "Files and formats"
question: "Count mentions of a word over time in a posts file"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node, run xerj autoindex on the JSONL folder, and read xerj autoindex map before you aggregate. Send one _search with size 0, a match_phrase, and a date_histogram on the field the map types as date. If that field is double, say the histogram is empty and do not invent months."
answer: "Count mentions of a word over time in a posts dataset. Index the JSONL, check the date field, and send one date histogram. On 2026-10-20 the phrase autoindex in issue titles returned 88 hits and took 123 ms: 1 in July, 50 in August, 11 in September, 26 in October. A fractional epoch field returned no months."
key_fact: "A date histogram for the phrase autoindex returned 88 hits and took 123 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/08-histogram-phrase.json"
key_fact_date: "2026-10-20"
intent_group: "Files and formats"
queries:
  - "count mentions of X over time in posts dataset"
  - "keyword frequency over time across posts"
  - "topic counts in jsonl posts by month"
  - "aggregate search hits by month cli"
  - "search posts by keyword and date"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/issues --prefix gh --state-dir /tmp/xerj-aq-2026-10-20/state-gh"
    note: "Index the posts file. This capture used a GitHub issue export, 1,532 records, wall 7.2 s."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{\"size\":0,\"query\":{\"match_phrase\":{\"title\":\"autoindex\"}},\"aggs\":{\"per_month\":{\"date_histogram\":{\"field\":\"createdAt\",\"calendar_interval\":\"month\"}}}}'"
    note: "Phrase plus one bucket per month. The first run took 123 ms and returned 88 hits."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/epoch-jsonl/_search -H 'content-type: application/json' -d '{\"size\":0,\"query\":{\"match_all\":{}},\"aggs\":{\"per_month\":{\"date_histogram\":{\"field\":\"ts\",\"calendar_interval\":\"month\"}}}}'"
    note: "Same aggregation on a double field. 40 docs, zero buckets."
links_out:
  - "search-json-and-jsonl-logs"
  - "search-slack-json-export"
  - "how-xerj-autoindexes-a-folder"
evidence:
  - claim: "The issue export autoindex finished in 7.2 s with 1,532 records."
    source: "benchmarks/agent-queries-2026-10-20/01-autoindex-issues.txt"
  - claim: "The match_phrase date histogram took 123 ms and returned 88 hits."
    source: "benchmarks/agent-queries-2026-10-20/08-histogram-phrase.json"
  - claim: "The match date histogram took 630 ms and returned the same four months."
    source: "benchmarks/agent-queries-2026-10-20/08-histogram.json"
  - claim: "A string field becomes date when at least 95% of sampled values parse."
    source: "engine/crates/xerj-autoindex/src/infer/mod.rs:470"
  - claim: "The fractional-epoch fixture indexed 40 records in 0.6 s and the histogram returned no buckets."
    source: "benchmarks/agent-queries-2026-10-20/06-autoindex-epoch.txt"
  - claim: "date_histogram on the double field ts returned buckets empty and took 0."
    source: "benchmarks/agent-queries-2026-10-20/08-epoch-histogram-all.json"
og_image: "og/answers-count-keyword-mentions-over-time-jsonl.png"
og_image_alt: "Cover: four bars for July 1, August 50, September 11 and October 26, with 123 ms beside them."
faq:
  - q: "Count mentions of a word over time in a posts file?"
    a: "Index the file, confirm the date field in `xerj autoindex map`, and send one `date_histogram` with `calendar_interval` `month`. The capture's phrase query returned 88 hits in 123 ms."
  - q: "What if the time field is a fractional epoch?"
    a: "On the fixture, `ts` was typed `double`. `date_histogram` on it returned `buckets: []` and `took` 0, over 40 documents. Check the type before you read the buckets."
  - q: "Is a date histogram a trend model?"
    a: "No. The buckets are counts of documents whose date falls in that month. There is no model and no forecast."
  - q: "Phrase or a single word?"
    a: "Both returned the same four months and 88 hits. `match_phrase` on `title` took 123 ms. `match` on `title` took 630 ms. 123 ms is not a floor."
  - q: "Did this run use a Reddit archive?"
    a: "No. The posts file was a GitHub issue export whose `createdAt` values are ISO strings. Do not treat these month counts as a Reddit corpus."
---

**TL;DR** — Count mentions of a word over time in a posts dataset. Index the JSONL, check the date field, and send one date histogram. On 2026-10-20 the phrase autoindex in issue titles returned 88 hits and took 123 ms: 1 in July, 50 in August, 11 in September, 26 in October. A fractional epoch field returned no months.

XERJ is a new way for AI to search for data.

## Index the file, then read the field type

The measured posts file is a GitHub issue export, not a Reddit archive. `createdAt` on `gh-jsonl` is `date`. The run committed 1,532 records in 7.2 s, exit 0.

```sh
xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /tmp/xerj-aq-2026-10-20/issues --prefix gh --state-dir /tmp/xerj-aq-2026-10-20/state-gh
```

```text
xerj-done ok=true exit=0 reason=completed wall=7.2s files=2 records=1532 generation=1
```

A string field becomes `date` when at least 95% of the sampled values parse as dates. That rule is in `infer/mod.rs`. Read `xerj autoindex map` on your own file. Do not assume the column you call `created` is `date`.

## One query, one count per month

```sh
curl -s -XPOST http://127.0.0.1:9217/gh-jsonl/_search -H 'content-type: application/json' -d '{"size":0,"query":{"match_phrase":{"title":"autoindex"}},"aggs":{"per_month":{"date_histogram":{"field":"createdAt","calendar_interval":"month"}}}}'
```

```text
{"took":123,…,"hits":{"total":{"value":88,"relation":"eq"},…},"aggregations":{"per_month":{"buckets":[{"key_as_string":"2026-07-01T00:00:00.000Z","doc_count":1},{"key_as_string":"2026-08-01T00:00:00.000Z","doc_count":50},{"key_as_string":"2026-09-01T00:00:00.000Z","doc_count":11},{"key_as_string":"2026-10-01T00:00:00.000Z","doc_count":26}]}}}
```

`size` 0 asks for the count, not the posts. The four buckets sum to 88.

The same months with `match` instead of `match_phrase` also returned 88 hits. That run took 630 ms. The phrase time is not a floor.

## When the field is not a date

A 40-line fixture stored fractional epoch strings such as `1700000000.000100` in `ts`. Autoindex typed `ts` as `double` and indexed the file in 0.6 s. The 20-year integer-epoch rule did not apply: the mapping shows `double`, not `long`.

```sh
curl -s -XPOST http://127.0.0.1:9217/epoch-jsonl/_search -H 'content-type: application/json' -d '{"size":0,"query":{"match_all":{}},"aggs":{"per_month":{"date_histogram":{"field":"ts","calendar_interval":"month"}}}}'
```

```text
{"took":0,…,"hits":{"total":{"value":40,"relation":"eq"},…},"aggregations":{"per_month":{"buckets":[]}}}
```

The call succeeded. The buckets were empty. An empty list here means the field is not a date, not that the word never occurs.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| `match_phrase` `title` `autoindex`, `date_histogram` month | 88 hits. Jul 1, Aug 50, Sep 11, Oct 26. `took` 123 ms |
| `match` `title` `autoindex`, same histogram | Same four months, 88 hits. `took` 630 ms |
| Issue export index | 1,532 records, wall 7.2 s, exit 0. `createdAt` is `date` |
| Fractional epoch fixture, `ts` typed `double` | 40 records, wall 0.6 s. Histogram buckets empty, `took` 0 |

One node, `XERJ_MAX_PROCESS_MEMORY_MB=4096`, binary v1.0.0-rc.97. A warm replay of the two issue histograms returned the same buckets with `took` 0. The times above are the first runs.

## When not to use XERJ

If the posts already live in one table and the date column is a real date, a SQL `GROUP BY` month is the smaller tool. Use the histogram when the file is JSONL and you already have the index.
