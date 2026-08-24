---
title: "How do I find out what is in a messy data folder?"
h1: "How should an agent figure out what's in a messy data folder before searching?"
description: "Point xerj autoindex at the folder, then read xerj autoindex map: every dataset it inferred, the fields and types, the time range, and every file it refused."
slug: "catalog-files-with-autoindex-map"
cluster: "Files and formats"
question: "I dropped a messy data folder on an agent. How should it figure out what's inside before searching?"
intent: "tool-selection"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex on the messy folder you were handed, then run xerj autoindex map --json and report every dataset, its index name, its formats, its field names with types, its time field and time range, and finally query autoindex-catalog for doc_kind file to list every file the run refused and the reason it gave."
commands:
  - cmd: "xerj autoindex map --url http://127.0.0.1:9410 --disable-feedback"
    note: "Print the data map for everything the node has indexed."
  - cmd: "xerj autoindex map --json --url http://127.0.0.1:9410 --disable-feedback"
    note: "Print the same map as machine-readable catalog documents."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/autoindex-catalog/_search' -H 'content-type: application/json' -d '{\"query\":{\"term\":{\"doc_kind\":\"file\"}},\"size\":500,\"_source\":[\"path\",\"format\",\"status\",\"reason\",\"records\"],\"sort\":[{\"path\":\"asc\"}],\"track_total_hits\":true}'"
    note: "List every catalogued file with its format, status and refusal reason."
links_out:
  - "search-file-contents-in-a-folder"
  - "index-multiple-csv-files"
  - "read-autoindex-progress"
  - "/docs/recipes/zero-config-autoindex"
faq:
  - q: "I dropped a messy data folder on an agent. How should it figure out what's inside before searching?"
    a: "Run `xerj autoindex` on the folder, then `xerj autoindex map`. The map names every dataset it inferred, the fields and their types, and the files it refused, before any query is sent."
  - q: "Is there a tool that can look at a folder and tell me what's actually in it?"
    a: "`xerj autoindex map` prints the catalog XERJ inferred: one row per dataset with its index name, formats, document count, time field and time range, then a field table per dataset."
  - q: "How do I inventory a folder of mixed files without opening each one?"
    a: "Query `autoindex-catalog` for `doc_kind` `file`. Our run listed 43 files, 39 indexed and 4 refused, and each refusal carried its reason string."
  - q: "Can an agent read the map without parsing Markdown?"
    a: "Yes. Run `xerj autoindex map --json` and the same catalog arrives as documents, with a `fields_json` string and ready-to-send queries per dataset."
  - q: "Why does the map list fewer datasets than _cat/indices?"
    a: "Because the catalog stores 1 dataset document per slug, with the id `ds:<slug>`. A later folder using the slug `docs` replaces the earlier entry."
  - q: "Can I sort a search by file modification date?"
    a: "No. A text dataset maps 9 fields and none of them is a date, so no indexed file-date field exists to sort on."
  - q: "What are the .xerj-memory indices in _cat/indices?"
    a: "`autoindex` creates 1 `.xerj-memory-<brain>-edges` index per indexed folder by default. Our run finished with 18 of them."
---

**TL;DR** — `xerj autoindex map` prints the catalog that the last run inferred. In our capture the map described 2 datasets from a 6-file folder. Each dataset carried every field with its type, its cardinality, its null share and a ready-to-send query.

## Index the folder, then print the map

`xerj autoindex` writes a catalog while it indexes, and `xerj autoindex map` prints that catalog. The Markdown form is for a person and the `--json` form is for an agent.

```sh
xerj autoindex map --url http://127.0.0.1:9410 --disable-feedback
```

```sh
xerj autoindex map --json --url http://127.0.0.1:9410 --disable-feedback
```

## What the map lists for a folder

The map opens with the run header, then lists one row per dataset, then one field table per dataset. Our 6-file folder produced 2 datasets in 0.2 seconds.

| index | documents | files | formats | time field | time range |
| --- | --- | --- | --- | --- | --- |
| `mx-data` | 41 | 1 | `["csv"]` | — | — |
| `mx-docs` | 10 | 5 | `["code","html","json","txt-prose","yaml"]` | — | — |

The document counts in that table come from a capture taken before XERJ began writing one document per code declaration. A source file now contributes one document for every declaration it holds on top of its whole-file document, so the dataset carrying the `code` file produces more documents than the row shows. The CSV row is unaffected. Read the count from your own map.

Each field row carries the inferred type, an estimated cardinality, a null share and 3 example values. The table below is the `mx-data` table from the capture.

| field | type | cardinality | null share | examples |
| --- | --- | --- | --- | --- |
| `amount` | `long` | 40 | 0% | `107`, `114`, `121` |
| `customer` | `keyword` | 40 | 0% | `cust-001`, `cust-002` |
| `order_id` | `long` | 40 | 0% | `1`, `2`, `3` |
| `region` | `keyword` | 3 | 0% | `amer`, `apac`, `emea` |

## Time ranges appear only with a date field

The map prints a time field and a time range when schema inference typed a field as a date. Our 6-file folder had none, so both columns printed as a dash.

Other datasets in the same run did have one, and the map printed the real bounds.

| index | time field | earliest | latest |
| --- | --- | --- | --- |
| `jlog-json` | `ts` | `2026-02-01T00:00:00.000Z` | `2026-02-28T23:47:00.000Z` |
| `jlog-jsonl` | `timestamp` | `2026-01-01T00:00:00.000Z` | `2026-01-28T23:59:53.000Z` |
| `gzz-logs` | `ts` | `2026-03-01T00:00:00.000Z` | `2026-03-28T23:59:57.000Z` |
| `sq-tickets` | `opened_at` | `2026-01-01T00:00:00.000Z` | `2026-01-28T23:00:00.000Z` |

There is no date-typed field for a file modification time on a text document. A text dataset such as `mt-docs` mapped 9 fields, and 0 of them was a date. A content query therefore cannot order by file date.

## No correlation section appeared

The map printed no correlation between datasets for this folder. What it printed instead was a `graph` line naming 5 live edges in `.xerj-memory-mixed6-edges`. A notes block explained why `body` became the elected text field.

State that limit before you promise a reader cross-dataset correlations. The capture is the source, and the capture has none.

## Every file and every refusal

The catalog holds 1 document per file with its detected format, its status and the reason for a refusal. Query the catalog directly for the full picture.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/autoindex-catalog/_search' -H 'content-type: application/json' -d '{"query":{"term":{"doc_kind":"file"}},"size":500,"_source":["path","format","status","reason","records"],"sort":[{"path":"asc"}],"track_total_hits":true}'
```

The whole capture cataloged 43 files: 39 indexed and 4 refused. The 4 refusals were an expanded DOCX, an image-only PDF, and 2 SQLite sidecar files. Each refusal carried its own reason string.

## One catalog entry per dataset slug

The catalog stores 1 dataset document per slug, with the document id `ds:<slug>`. The capture indexed 18 folders into 1 node, and the catalog ended with 11 dataset documents against 23 non-hidden indices. A later folder with the slug `docs` had replaced the earlier one.

Give each folder its own `--prefix`, and print the map straight after each run if you want a per-folder view. Read `_cat/indices` when you want the full index list.

## The edges index per folder

`autoindex` creates 1 `.xerj-memory-<brain>-edges` index per indexed folder by default, named after the folder. The capture finished with 18 of them. A first read of `_cat/indices` is therefore longer than a reader expects.

That index also holds a brain metadata document with no `src` field. Add `{"exists": {"field": "src"}}` to any query over it, or the first hit is meaningless.

## Scope of this capture

The capture ran on 1 single-node process on 2026-08-21, over deterministic fixtures covering 43 cataloged files. This run measured no whole disk. Read the map as a description of the output and not as a capacity claim.
