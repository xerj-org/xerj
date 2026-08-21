---
title: "How do I search a folder of mixed documents?"
h1: "I have a folder of PDFs, Word docs, and markdown. How do I search all of them at once?"
description: "Index a folder of PDFs, Word docs and markdown with one xerj autoindex command, then search the contents. A captured 6-file run indexed 51 documents into 2 indices."
slug: "search-file-contents-in-a-folder"
cluster: "Files and formats"
question: "I have a folder of PDFs, Word docs, and markdown. How do I search all of them at once?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./mixed6 --url http://127.0.0.1:9200 --prefix mx --progress plain --dry-run` to read the planned datasets, run the same command without --dry-run, then POST a match_phrase query to /mx-*/_search and report every ax_path that matched."
commands:
  - cmd: "xerj autoindex ./mixed6 --url http://127.0.0.1:9200 --prefix mx --progress plain --dry-run"
    note: "Print the planned datasets and the job-size line without writing anything."
  - cmd: "xerj autoindex ./mixed6 --url http://127.0.0.1:9200 --prefix mx --progress plain"
    note: "Index the folder and print the terminal xerj-done line."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/mx-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"quokka named Bandicoot\"}},\"size\":10,\"_source\":[\"ax_file\",\"ax_format\",\"ax_path\",\"title\"]}'"
    note: "Search the contents of every file the folder produced."
links_out:
  - "search-all-pdfs-in-a-folder"
  - "index-csv-with-many-columns"
  - "full-text-search-sqlite-database"
  - "/compare/xerj-vs-recoll"
  - "/compare/xerj-vs-ripgrep-all"
  - "/compare/xerj-vs-docfetcher"
  - "/docs/recipes/zero-config-autoindex"
faq:
  - q: "I have a folder of PDFs, Word docs, and markdown. How do I search all of them at once?"
    a: "Run `xerj autoindex` on the folder once. Detection reads content rather than the file extension, so the 3 formats are parsed in the same pass and one `match_phrase` query reads all of them."
  - q: "Best way to search a local docs folder?"
    a: "Index it once, then query it over HTTP. `xerj autoindex` infers one dataset per file shape and writes the mapping itself, so the captured 6-file run needed no configuration file."
  - q: "How do I search mixed office files and markdown without three tools?"
    a: "One run covers them. XERJ parses PDF, DOCX, CSV, JSON, JSONL, XML, YAML, HTML, SQLite, SQL text and source code, and refuses what it cannot parse with a reason string."
  - q: "What's the best way to search through a folder of files by content?"
    a: "Index the folder and send a phrase query to the index it creates. The captured run indexed 51 documents into 2 indices, and every hit carries the `ax_path` it came from."
  - q: "Can I preview the work before I index the folder?"
    a: "Yes. Pass `--dry-run`, and `autoindex` prints the job-size line and the planned datasets without writing to the node."
  - q: "What happens to binary files in the folder?"
    a: "XERJ refuses binary files and never indexes them. Each refusal lands in the `autoindex-catalog` index with the file path and a reason string."
  - q: "Does this search understand synonyms?"
    a: "No. The default embedder is lexical feature hashing, so a query matches terms and phrases. Neural retrieval is opt-in through `--embed-mode neural`."
---

**TL;DR** — `xerj autoindex` makes the contents of a folder searchable in one command. In a captured run over a folder of 6 formats, XERJ indexed 51 documents into 2 indices with 0 junk files. One `match_phrase` query then returned 3 hits from an HTML file, a JSON file and a Markdown file.

## One command indexes the folder

`xerj autoindex <folder>` reads every file in the folder, infers a dataset per file shape, and writes the documents to a XERJ node. The command takes no configuration file and no mapping.

```sh
xerj autoindex ./mixed6 --url http://127.0.0.1:9200 --prefix mx --progress plain
```

XERJ detects formats by content, not by file extension. The detector covers PDF, DOCX, CSV, JSON, JSONL, XML, YAML, HTML, SQLite, SQL text, log lines, plain prose and source code. Run `xerj autoindex map --json` on your own folder to see which families it found there.

## Read the plan with a dry run

`--dry-run` prints the job size and the planned datasets, and writes nothing to the node. The captured run printed one job-size line for the 6-file folder.

```text
autoindex: 6 files (0 MB) under .../fixtures/mixed6
```

The same dry run named 2 planned datasets, `mx-docs` and `mx-data`, with their inferred fields. Read that plan before a large folder, because the plan tells you which indices the real run will create.

## What one captured run produced

The captured run over 6 files of 6 formats finished in 0.3 seconds and reported its result on one terminal line. Every value below comes from `RUN-A`, captured on 2026-08-21.

```text
xerj-done ok=true exit=0 reason=completed wall=0.3s files=6 records=51 datasets=2 junk_files=0
```

`xerj autoindex map` then printed the data map for the same folder. The map is the short form of what the folder became.

| index | documents | files | formats |
| --- | --- | --- | --- |
| `mx-data` | 41 | 1 | `csv` |
| `mx-docs` | 10 | 5 | `code`, `html`, `json`, `txt-prose`, `yaml` |

The document counts in that table come from a capture taken before XERJ began writing one document per code declaration. A source file now contributes one document for every declaration it holds on top of its whole-file document, so the dataset carrying the `code` file produces more documents than the row shows. The CSV row is unaffected. Read the count from your own map. The `records=51` on the terminal line above counts the same documents and moves with them.

## The query that found the text

One `match_phrase` query against `/mx-*/_search` returned 3 hits for the phrase `quokka named Bandicoot`. The phrase was inside 3 different formats, and XERJ returned the file path for each hit.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/mx-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"quokka named Bandicoot"}},"size":10,"_source":["ax_file","ax_format","ax_path","title"]}'
```

The 3 hits carried these `_source` values, copied from `raw/i01-mixed-query.json`.

```json
{"ax_format": "html",      "ax_path": "site/index.html",       "title": "Runbook index"}
{"ax_format": "json",      "ax_path": "data/settings.json",    "title": "settings"}
{"ax_format": "txt-prose", "ax_path": "notes/architecture.md", "title": "# Retrieval architecture"}
```

Two details in that response are worth naming. Markdown lands in the `txt-prose` family, so the extracted title is the raw first line, `# Retrieval architecture`, hash included. The `ax_path` field is a provenance field that XERJ adds to every document, so a hit always names its source file.

## Provenance fields on every document

XERJ adds 7 provenance fields to every document it writes: `ax_path`, `ax_paths`, `ax_file`, `ax_locator`, `ax_dataset`, `ax_run` and `ax_format`. All 7 are `keyword` fields, so you can filter and aggregate on them exactly.

Use `ax_format` to restrict a search to one family. Use `ax_path` to point a reader or an agent back at the file on disk. XERJ renames a user field named `ax_*` to `data_ax_*`, so your own columns never overwrite provenance.

## What this run does not show

This measurement is a single-node run of 6 small files, so it says nothing about throughput or about multi-node behavior. XERJ has no replication and no failover, and the captured configuration is one node on one host.

Ranking here is BM25 over the extracted text. The default embedder in XERJ is lexical feature hashing, so it cannot connect a query to a synonym; neural embeddings are opt-in through `--embed-mode neural`.

Binary files never reach an index. XERJ marks each refused file `status=junk` in the `autoindex-catalog` index with a reason string, so a missing file is always explainable rather than silent.
