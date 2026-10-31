---
title: "Full-text search over JSONL and CSV files from the CLI"
h1: "Full-text search over JSONL files from the CLI: index a folder and query it with BM25"
description: "xerj autoindex reads JSONL and CSV with no schema, then answers BM25 per field from the CLI or MCP. 2,902 rows indexed in 12.4 s; queries took 1 ms."
slug: "full-text-search-over-jsonl-files-cli"
cluster: "Answers for coding agents"
question: "How do I run full-text search over JSONL files from the CLI and query a folder with BM25?"
intent: "how-to"
published: "2026-10-19"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
og_image: "og/answers-full-text-search-over-jsonl-files-cli.png"
og_image_alt: "Cover: a field of 1,000 dots for the CSV rows, 79 ink BM25 hits for 'bm25 jsonl folder' in 1 ms and row id 480 ringed in yellow; an exact rg phrase search found 0"
queries:
  - "full-text search over JSONL files from the CLI"
  - "index a folder of JSONL files and query it with BM25"
commands:
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start a single-user node on localhost:9200 in its own terminal."
  - cmd: "xerj autoindex ./scraped --prefix scr"
    note: "Sniff each file by content and index it; one index per inferred dataset."
  - cmd: "curl -s 'localhost:9200/scr-csv/_search' -H 'content-type: application/json' -d '{\"size\":3,\"query\":{\"match\":{\"query\":\"bm25 jsonl folder\"}}}'"
    note: "BM25 on one column of the CSV, ranked, with the row's other fields in _source."
evidence:
  - claim: "autoindex: done in 12.4s (12.4 s), 2902 documents, 3 datasets"
    source: "benchmarks/agent-intents-2026-10-19/03-autoindex-jsonl-csv.txt"
  - claim: "CSV match: took 1 ms, 79 hits, id 480 first"
    source: "benchmarks/agent-intents-2026-10-19/03-csv-match.json"
  - claim: "JSONL multi_match: took 1 ms, 57 hits, #1801 first"
    source: "benchmarks/agent-intents-2026-10-19/03-jsonl-multi-match.json"
  - claim: "xerj search cites two documents as xerj-prs.jsonl:1"
    source: "benchmarks/agent-intents-2026-10-19/03-xerj-search-cli.txt"
  - claim: "Folder size 6,444,108 bytes (6.4 MB); rg counts 0 and 41"
    source: "benchmarks/agent-intents-2026-10-19/README.md"
links_out:
  - "search-json-and-jsonl-logs"
  - "index-multiple-csv-files"
  - "index-csv-with-many-columns"
  - "batch-lookup-cve-ghsa-ids-offline"
  - "/compare/xerj-vs-elasticsearch-local"
  - "/compare/xerj-vs-vector-database"
  - "/compare/best-local-full-text-search-jsonl-csv-2026"
faq:
  - q: "How do I run full-text search over JSONL files from the CLI?"
    a: "Run `xerj autoindex` on the folder, then query the node with curl or `xerj search`. No mapping is needed: each record becomes a document with its own fields."
  - q: "Can I index a folder of JSONL files and query it with BM25?"
    a: "Yes. Our folder held 517 and 1,385 JSONL records plus a 1,000-row CSV; autoindex built three indices in 12.4 s, and the field queries returned ranked hits in 1 ms."
  - q: "Is there an MCP server to search local CSV files?"
    a: "Yes. `xerj mcp` exposes `xerj_search`, which takes the same index and query body as the curl call on this page. Register it with `xerj init`."
  - q: "Why use this instead of grep or jq?"
    a: "grep finds exact strings and does not rank. `rg -c -i \"bm25 jsonl folder\"` found 0 lines in the CSV; BM25 on the `query` column returned 79 ranked rows with the best one first."
  - q: "Should I use the CLI search or the HTTP query for structured files?"
    a: "Use the field-scoped HTTP query, or `xerj_search` over MCP. In our run `xerj search` ranked long PR bodies above the CSV rows and cited two different documents as `xerj-prs.jsonl:1`."
  - q: "Does it work on a JSON file that is one big array?"
    a: "Yes, but check the label. Our 1,385-record file was detected as format `json` rather than `jsonl`; the records still indexed one per document."
---

**TL;DR** — For full-text search over JSONL files from the CLI, run `xerj autoindex` on the folder and query it with BM25. No schema or mapping is needed; CSV works the same way. On 2026-10-19 a folder of 2,902 JSONL and CSV rows indexed in 12.4 s, and the field queries returned ranked hits in 1 ms.

## Run it

The folder in this run held three files. Two are GitHub issues and pull requests as JSON lines. The third is a 1,000-row CSV of agent search queries.

```sh
xerj --insecure --data-dir ./xerj-data &
xerj autoindex ./scraped --prefix scr
```

```text
done in 12.4s — 3 datasets, 2902 records live, 0 duplicate aliases, 0 junk records, 0 junk/skipped files (233.8 docs/sec)
indexed: 2902 documents live; this run submitted 2902 source records from 3 files
  scr-json                                       1385 docs
  scr-csv                                        1000 docs
  scr-jsonl                                       517 docs
```

Each CSV column became a field. Query one column with BM25:

```sh
curl -s 'localhost:9200/scr-csv/_search' -H 'content-type: application/json' \
  -d '{"size":3,"_source":["id","agent","query"],"query":{"match":{"query":"bm25 jsonl folder"}}}'
```

Output, reduced with jq to the fields that matter:

```json
{"took":1,"total":79,"hits":[
 {"score":9.459448,"id":480,"query":"index folder of jsonl files and query with bm25"},
 {"score":7.1556997,"id":374,"query":"bm25 search local folder python library"},
 {"score":6.7786074,"id":370,"query":"search csv and jsonl folder one command"}]}
```

The JSONL issues take a `multi_match` with a boost on the title:

```sh
curl -s 'localhost:9200/scr-jsonl/_search' -H 'content-type: application/json' \
  -d '{"size":3,"_source":["number","title"],"query":{"multi_match":{"query":"batch CVE GHSA lookup","fields":["title^2","body"]}}}'
```

```json
{"took":1,"total":57,"hits":[
 {"score":19.358475,"number":1801,"title":"[feature]: Batch queries (many IDs or questions per call, per-item answer or abstain)"},
 {"score":10.939819,"number":1788,"title":"[feature]: Field-aware BM25 plus exact ID matching in `xerj code`"},
 {"score":10.761112,"number":1244,"title":"xerj code over JSON corpora: one shard's synthesized empty text defeats the own-field fallback (cve-records G7 0/7)"}]}
```

## Measured on 2026-10-19

| Measure | Value |
| --- | --- |
| Input | 3 files, 6.4 MB: 517 + 1,385 JSON lines, 1,000 CSV rows |
| Index time | 12.4 s, 2,902 documents |
| CSV query "bm25 jsonl folder" | 79 hits, took 1 ms, row 480 first |
| JSONL query "batch CVE GHSA lookup" | 57 hits, took 1 ms, issue #1801 first |
| `rg -c -i "bm25 jsonl folder"` on the CSV | 0 lines |
| `rg -c -i bm25` on the CSV | 41 lines, unranked |

## How it compares

| Tool | Ranked | Field-aware | No schema | Query from | Index step |
| --- | --- | --- | --- | --- | --- |
| xerj | yes, BM25 | yes | yes | CLI, curl, MCP | yes |
| grep / ripgrep | no | no | yes | CLI | no |
| jq | no | yes, exact | yes | CLI | no |
| SQLite FTS5 | yes, BM25 | yes | no, create the table | SQL | yes |
| Elasticsearch | yes, BM25 | yes | mapping usually needed | HTTP | yes, and a JVM |

## When not to use xerj

- You need an exact value from one field. `jq 'select(.id == 480)'` is enough.
- The file is small and read once. Loading it into the model or a spreadsheet costs less.
- You need SQL joins or aggregates across files. Use SQLite or DuckDB.
- You need exact line numbers from `xerj search` on JSONL. In our run it cited two different documents as `xerj-prs.jsonl:1`.
