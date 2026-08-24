---
title: "How do I search a folder of CSV exports?"
h1: "I have a directory of CSV exports. How do I query them without opening each one in Excel?"
description: "Point xerj autoindex at the folder once. A captured run split 3 CSV files across 2 schemas into 2 indices and answered one query across both of them."
slug: "index-multiple-csv-files"
cluster: "Files and formats"
question: "I have a directory of CSV exports. How do I query them without opening each one in Excel?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./csv-exports --url http://127.0.0.1:9200 --prefix multi --progress plain`, GET /_cat/indices/multi-*?format=json to see how many indices the different headers produced, then POST one query_string search across multi-* with a terms aggregation on ax_dataset and report the per-dataset counts."
commands:
  - cmd: "xerj autoindex ./csv-multi --url http://127.0.0.1:9200 --prefix multi --progress plain"
    note: "Index every CSV file in the folder in one pass."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/_cat/indices/multi-*?format=json"
    note: "List the indices the run created, with a document count for each."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/multi-*/_search -H 'content-type: application/json' -d '{\"query\":{\"query_string\":{\"query\":\"emea OR wh-1\"}},\"size\":5,\"_source\":[\"ax_dataset\",\"ax_file\",\"region\",\"warehouse\"],\"aggs\":{\"by_dataset\":{\"terms\":{\"field\":\"ax_dataset\",\"size\":10}}}}'"
    note: "Search both schemas in one request and group the hits by dataset."
links_out:
  - "index-csv-with-many-columns"
  - "catalog-files-with-autoindex-map"
  - "search-file-contents-in-a-folder"
  - "/compare/xerj-vs-typesense"
faq:
  - q: "I have a directory of CSV exports. How do I query them without opening each one in Excel?"
    a: "Give `xerj autoindex` the folder, not each file. It reads every CSV in one run, groups the files by header, and each group becomes an index you query over HTTP."
  - q: "What's a good way to read and search across a bunch of CSV files?"
    a: "Index the folder once, then query the index pattern. XERJ reads every CSV file in the folder in one run and groups the files by their header."
  - q: "How do I search 20 CSV exports like they were one table?"
    a: "Files that share a header join one dataset, so one query pattern such as `/multi-*/_search` reads them all. The captured cross-index query returned 171 hits from 2 indices in one request."
  - q: "How do I get counts and totals from a folder of CSV exports?"
    a: "Run a `terms` aggregation on a keyword field such as `ax_dataset` or `ax_file`; both are exact bucket counts. This capture measured counts only, so it does not show sums over numeric columns."
  - q: "Do all my CSV files land in one index?"
    a: "Only if they share a header. Files with the same columns join one dataset, and a file with different columns gets its own index."
  - q: "How do I tell which file a hit came from?"
    a: "Read the `ax_file` and `ax_path` fields on the hit. XERJ writes 7 keyword provenance fields on every document it creates."
  - q: "What happens if I add a fourth CSV file later?"
    a: "Run `xerj autoindex` on the folder again. Document ids are content-derived and the run journal converges, so re-runs do not duplicate the earlier files."
---

**TL;DR** — Give `xerj autoindex` the folder rather than each file. In a captured run, 3 CSV files across 2 different headers became 2 indices holding 402 and 151 documents. One `query_string` request across `multi-*` then returned 171 hits and split them by dataset.

## Index the folder, not each file

`xerj autoindex <folder>` reads every CSV file under the folder in one pass. XERJ groups the files by their header, so files that share columns share an index.

```sh
xerj autoindex ./csv-multi --url http://127.0.0.1:9200 --prefix multi --progress plain
```

The captured folder held 3 files. Two of them shared the header `invoice_id,customer,region,amount`, and the third had the unrelated header `sku,warehouse,on_hand,reorder_level`.

## Two schemas produced two indices

XERJ created 2 indices from the 3 files, not 1 and not 3. Each index is named `<prefix>-<dataset>`, and the captured `ax_dataset` values were `csv` and `csv-inventory-snapshot`.

| index | documents | source files |
| --- | --- | --- |
| `multi-csv` | 402 | `sales_q1.csv`, `sales_q2.csv` |
| `multi-csv-inventory-snapshot` | 151 | `inventory_snapshot.csv` |

```sh
curl -s -XGET 'http://127.0.0.1:9200/_cat/indices/multi-*?format=json'
```

This split is the point of the design. Two files with the same columns stay comparable in one index, and a file with different columns never pollutes that mapping.

## One query reads both indices

A single `query_string` request against `/multi-*/_search` returned 171 hits drawn from both indices. The same request carried a `terms` aggregation on `ax_dataset`, so the response also reports where the hits came from.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/multi-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"query_string":{"query":"emea OR wh-1"}},"size":5,"_source":["ax_dataset","ax_file","region","warehouse"],"aggs":{"by_dataset":{"terms":{"field":"ax_dataset","size":10}}}}'
```

```json
{"hits": {"total": {"value": 171, "relation": "eq"}},
 "aggregations": {"by_dataset": {"buckets": [{"key": "csv", "doc_count": 133},
                                             {"key": "csv-inventory-snapshot", "doc_count": 38}],
                                 "doc_count_error_upper_bound": 0,
                                 "sum_other_doc_count": 0}}}
```

## Provenance tells you the source file

Every document carries 7 `keyword` provenance fields: `ax_path`, `ax_paths`, `ax_file`, `ax_locator`, `ax_dataset`, `ax_run` and `ax_format`. All 7 are exact-match fields, so you can filter or aggregate on any of them.

Filter on `ax_file` to restrict a search to one CSV file. Aggregate on `ax_dataset` to report a count per schema. Read `ax_path` to give a human reader the file to open.

## Adding files later

Run `xerj autoindex` on the folder again after you add a file. XERJ derives every document id from content, and the resume journal converges. A second run over the same folder therefore does not duplicate the files from the first run.

A new header creates a new index rather than changing an existing mapping. The earlier files stay queryable, and the new file becomes searchable under the same index pattern.

## What this capture does not show

This measurement is a single-node run of 3 small files on 1 host. The capture reports how the dataset split behaves, not how fast XERJ indexes a large folder. XERJ has no replication and no failover in this configuration.

Ranking is BM25 over the indexed columns. The default embedder in XERJ is lexical feature hashing, so a query matches terms rather than meanings; neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from `RUN-A`, captured on 2026-08-21.
