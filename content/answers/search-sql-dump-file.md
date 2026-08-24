---
title: "Search a SQL dump without restoring it"
h1: "I have a 2GB SQL dump. How do I find rows mentioning a customer without loading it into Postgres?"
description: "xerj autoindex parses a MySQL-style .sql file directly, with no database server, and makes every INSERT value searchable. A captured run indexed 321 documents."
slug: "search-sql-dump-file"
cluster: "Files and formats"
question: "I have a 2GB SQL dump. How do I find rows mentioning a customer without loading it into Postgres?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./sqldump --url http://127.0.0.1:9200 --prefix dump --progress plain`, GET /_cat/indices/dump-*?format=json to see one index per table, then POST a match_phrase query to /dump-*/_search for a string you expect inside an INSERT value and report the hit with its ax_locator."
commands:
  - cmd: "xerj autoindex ./sqldump --url http://127.0.0.1:9200 --prefix dump --progress plain"
    note: "Index a folder holding a MySQL-style .sql file; no database server is involved."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/_cat/indices/dump-*?format=json"
    note: "List one index per table, with a document count for each."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/dump-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"Bandicoot verified run 42\"}},\"size\":3,\"_source\":[\"ax_dataset\",\"ax_locator\",\"title\",\"author\",\"body\"],\"track_total_hits\":true}'"
    note: "Find text that lives inside an INSERT value and return its locator."
links_out:
  - "full-text-search-sqlite-database"
  - "index-csv-with-many-columns"
  - "catalog-files-with-autoindex-map"
evidence:
  - claim: "GNU grep is line-oriented. It selects and prints the input lines that match a pattern, so a multi-row INSERT that sits on one physical line is returned whole."
    source: "https://www.gnu.org/software/grep/manual/grep.html"
faq:
  - q: "I have a 2GB SQL dump. How do I find rows mentioning a customer without loading it into Postgres?"
    a: "Point `xerj autoindex` at the folder holding the dump, then query the index it builds; no database server is started. Our capture used a small MySQL-style file of 321 documents, so it shows the shape of the job and not a multi-gigabyte wall time."
  - q: "How do I search inside a SQL dump file?"
    a: "Run `xerj autoindex` on the folder holding the `.sql` file, then send a `match_phrase` query. XERJ reads the statements and indexes the values inside them."
  - q: "Can I grep a mysqldump without restoring it?"
    a: "You can search it without restoring it. XERJ parses the `.sql` text directly and returns the matched row with its `ax_locator`, rather than the physical line the statement sits on."
  - q: "What's a sane way to search a multi-gigabyte .sql file?"
    a: "Index it once and query the index. Each table becomes its own index, so a filter separates `articles` from `authors`. This page publishes no measurement above the captured file, so time the run on your own hardware."
  - q: "Does each table become its own index?"
    a: "Yes. The captured file held 2 tables and produced `dump-articles` with 301 documents and `dump-authors` with 20."
  - q: "Which SQL dialects does XERJ read?"
    a: "The extractor targets `mysqldump`-style output. It was captured against a MySQL 8.0.36 file with backtick-quoted identifiers and multi-row `INSERT` statements."
  - q: "How do I trace a hit back to the source file?"
    a: "Read `ax_locator` and `ax_path`. In the captured run `ax_locator` held `tarticles:s0:t41`, which names the table and the position of the row inside the file."
---

**TL;DR** — `xerj autoindex` parses a MySQL-style `.sql` file directly, with no database server involved. In a captured run one file produced 2 indices, `dump-articles` with 301 documents and `dump-authors` with 20. A `match_phrase` query then found a phrase inside an `INSERT` value.

## One command, no database server

`xerj autoindex <folder>` reads the `.sql` text, follows the `CREATE TABLE` statements to get the column names, and turns each row inside an `INSERT` statement into a document. No MySQL process and no restore step are involved.

```sh
xerj autoindex ./sqldump --url http://127.0.0.1:9200 --prefix dump --progress plain
```

XERJ detects the family from file content, not from the file extension. The captured file opened with the header below, and `sqldump` is one of the families the detector recognises.

```text
-- MySQL dump 10.13  Distrib 8.0.36, for Linux (x86_64)
--
-- Host: localhost    Database: support
```

## One index per table

The captured file held 2 tables and produced 2 indices. The run reported 321 documents live from 1 file, with 0 junk files, and exited 0.

| index | source table | documents |
| --- | --- | --- |
| `dump-articles` | `articles` | 301 |
| `dump-authors` | `authors` | 20 |

```text
xerj-done ok=true exit=0 reason=completed wall=0.3s files=1 records=321 datasets=2 junk_files=0
```

The column names come from the `CREATE TABLE` statement, so the fields keep the names a reader already knows. A query names `title`, `body` and `author` rather than a positional column number.

## Find text inside an INSERT value

One `match_phrase` query against `/dump-*/_search` matched a phrase that exists only inside an `INSERT` value, and returned the whole row.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/dump-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"Bandicoot verified run 42"}},"size":3,"_source":["ax_dataset","ax_locator","title","author","body"],"track_total_hits":true}'
```

The single hit below comes from `raw/sqldump-query.json`.

```json
{"total": {"value": 1, "relation": "eq"},
 "hit": {"ax_dataset": "articles",
         "ax_locator": "tarticles:s0:t41",
         "title": "Article 42",
         "author": "author-02",
         "body": "The checkpoint journal replays cleanly after restart. A quokka named Bandicoot verified run 42."}}
```

`ax_locator` is the trace back to the source. The value `tarticles:s0:t41` names the table and the position of the row inside the file. An agent can quote a hit and still point a human at the original text.

## Why this beats grep on the same file

A multi-row `INSERT` statement puts many rows on one physical line. A line-oriented text search therefore returns the whole statement rather than the row you wanted. XERJ splits the statement into rows first, so the captured query returned 1 document and not 1 line of 50 tuples.

Typed fields follow from the same parse. XERJ has the column names, so you can filter on `author`, aggregate on it exactly, or sort by an inferred date column.

## When to choose grep instead

Choose grep when the question is one literal string. grep needs no node and no index. It answers from a cold start.

Choose grep for a regular expression over raw bytes. Choose grep for a file that changed 1 second ago. XERJ answers from the last `autoindex` run, and grep reads the bytes on disk.

## What the extractor targets

The extractor targets `mysqldump`-style output. The capture used a MySQL 8.0.36 file with backtick-quoted identifiers, `LOCK TABLES` blocks, and `INSERT INTO ... VALUES` statements carrying 50 tuples each.

XERJ reads the `.sql` file as text, so a compressed copy works too. Gzip is transparent on every parsed family.

## What this capture does not show

This is a single-node run of 1 file on 1 host, so it shows parsing and correctness rather than throughput. XERJ has no replication and no failover in this configuration.

Ranking is BM25 over the extracted values. The default embedder in XERJ is lexical feature hashing, so a query matches terms rather than meanings; neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from `RUN-A`, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host.
