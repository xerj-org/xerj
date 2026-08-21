---
title: "How do I full-text search a SQLite database?"
h1: "How do I full-text search a SQLite database I just copied onto disk?"
description: "xerj autoindex reads a SQLite file read-only and turns each table into an index. A captured WAL-mode run indexed 400 of 450 committed rows and changed no bytes."
slug: "full-text-search-sqlite-database"
cluster: "Files and formats"
question: "How do I full-text search a SQLite database?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run sqlite3 on the target database with PRAGMA wal_checkpoint(TRUNCATE) so no committed row is left in the -wal file, record sha256sum of the .db, -wal and -shm files, start a local XERJ node, run `xerj autoindex ./sqlite --url http://127.0.0.1:9200 --prefix sq --progress plain`, compare the checksums again, then POST a match_phrase query to /sq-*/_search and report the hit total against the row count you expected."
commands:
  - cmd: "xerj autoindex ./sqlite --url http://127.0.0.1:9200 --prefix sq --progress plain"
    note: "Read the SQLite file read-only and create one index per table."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/_cat/indices/sq-*?format=json"
    note: "See the table-to-index map with a document count for each table."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/sq-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"checkpoint journal did not replay\"}},\"size\":3,\"_source\":[\"ax_dataset\",\"ax_locator\",\"subject\",\"status\"],\"track_total_hits\":true}'"
    note: "Full-text search a TEXT column and return the row locator per hit."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/sq-tickets/_count"
    note: "Count the documents one table produced, so you can compare it with the row count."
links_out:
  - "index-csv-with-many-columns"
  - "search-sql-dump-file"
  - "full-text-search-browser-history"
  - "/compare/xerj-vs-ripgrep-all"
faq:
  - q: "How do I full-text search a SQLite database?"
    a: "Run `xerj autoindex` on the folder holding the `.db` file, then send a `match_phrase` query. XERJ turns each table into an index and each row into a document."
  - q: "I have a .sqlite file. How do I search all the text in it without writing FTS5?"
    a: "Let the indexer do it. XERJ indexes the text columns itself and ranks with BM25, so the source database needs no FTS5 table and no triggers."
  - q: "Can I search inside a SQLite file the way I search a folder?"
    a: "Yes. A database file in a folder is one more format to `autoindex`: it is picked up in the same run as the other files, and each table lands in its own index."
  - q: "What's the easiest way to grep a SQLite database?"
    a: "Index it first, then query it. A byte scan reads the file container rather than the rows, while `autoindex` opens the database read-only and turns each row into a document."
  - q: "Does XERJ modify my SQLite file?"
    a: "No. XERJ opens the file read-only and immutable. In the captured run the SHA-256 of the `.db`, `-wal` and `-shm` files was identical before and after."
  - q: "Does XERJ read rows that are still in the WAL?"
    a: "No. The captured run indexed the 400 checkpointed rows and returned 0 hits for the 50 rows that lived only in `support.db-wal`."
  - q: "How do I avoid missing recent rows?"
    a: "Run `PRAGMA wal_checkpoint(TRUNCATE)` on the database before you index it, or index a copy taken after a checkpoint."
---

**TL;DR** — `xerj autoindex` reads a SQLite file read-only and turns each table into an index. In a captured WAL-mode run, XERJ created `sq-tickets` with 400 documents and `sq-agents` with 13, and left all 3 source files byte-identical. Rows still resident in the write-ahead log were absent.

## Index the database file in one command

`xerj autoindex <folder>` opens the SQLite file read-only and immutable, reads each table, and writes one document per row. XERJ never opens the database for writing, and it never touches the write-ahead log.

```sh
xerj autoindex ./sqlite --url http://127.0.0.1:9200 --prefix sq --progress plain
```

The captured run created 2 indices from 1 file. Each table became its own index, and each row became a document with the table columns as fields.

| index | source table | documents |
| --- | --- | --- |
| `sq-tickets` | `tickets` | 400 |
| `sq-agents` | `agents` | 13 |

## Uncheckpointed rows are silently missing

This is the most important finding in the capture, and it will cost you data if you point XERJ at a live application database. The fixture held 450 committed ticket rows: 400 checkpointed into `support.db`, and 50 committed but still resident only in `support.db-wal`.

- A `match_phrase` query for text in the checkpointed rows returned **400 hits**.
- A `match_phrase` query for text that exists only in the WAL-resident rows returned **0 hits**.
- `sq-tickets` holds exactly 400 documents against 450 committed rows.

XERJ raised no warning about the gap. The read is genuinely immutable, and the price of that immutability is that everything after the last checkpoint is invisible.

Run a checkpoint before you index a live database. `PRAGMA wal_checkpoint(TRUNCATE)` folds the WAL into the main file. A run after that checkpoint sees every committed row.

An earlier fixture that left all of its data in the WAL produced 0 documents with the generic reason `no records extracted`. A database that returns nothing is therefore not proof that the database is empty.

## The source files did not change

XERJ hashed all 3 source files before and after the run, and all 3 hashes match. Immutable access is a real property here, not a claim about intent.

```text
before and after, unchanged:
df782a5795a98f46eb1732a0b01e01283320031fe3daad29eb6e36ee11db28b1  support.db
eba5aedf41bb37c1d3660b9a0ab38abed21474154a129b17e99b27a3499fb861  support.db-wal
4b6dcc29db7f3852205f9e020cd6ffd55da5f4d3803fce0150e68f325a60c4fd  support.db-shm
```

## Column types become field types

XERJ infers an Elasticsearch type per column rather than copying the SQLite declaration. The `tickets` table produced the mapping below, plus the 7 `keyword` provenance fields that XERJ writes on every document.

| field | inferred type |
| --- | --- |
| `id` | `long` |
| `opened_at` | `date` |
| `status` | `keyword` |
| `subject` | `text` |
| `body` | `semantic_text` |

XERJ elects `semantic_text` for a long prose column such as `body`. The default embedder in XERJ is lexical feature hashing, so that field cannot connect a query to a synonym. The neural embedder is opt-in through `--embed-mode neural`.

## The query that searched a TEXT column

One `match_phrase` query against `/sq-*/_search` returned 400 hits, each carrying an `ax_locator` that names the source table and row.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/sq-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"checkpoint journal did not replay"}},"size":3,"_source":["ax_dataset","ax_locator","subject","status"],"track_total_hits":true}'
```

```json
{"total": {"value": 400, "relation": "eq"},
 "first_hit": {"ax_dataset": "tickets", "ax_locator": "ttickets:r1",
               "subject": "segment merge stall #1", "status": "closed"}}
```

The source database needs no FTS5 table, no triggers and no schema change. XERJ ranks with BM25 over the text it extracted from the column.

## Why the run exits 3

A WAL-mode SQLite database is 3 files on disk, and 2 of them are binary. XERJ refused `support.db-wal` and `support.db-shm` with `format=binary` and the reason `binary content (unknown)`, which made the run report `junk_files=2` and exit 3.

```text
xerj-done ok=true exit=3 reason=completed-with-junk wall=0.3s files=1 records=413 datasets=2 junk_files=2
```

Do not treat exit 3 here as a failure. The tables indexed correctly, and the 2 refusals are the sidecar files that XERJ is right to leave alone.

## What this capture does not show

This is a single-node run of 1 database on 1 host, so it measures correctness and boundaries rather than throughput. XERJ has no replication and no failover in this configuration.

Every number above comes from `RUN-A`, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host. We publish the WAL result as a failed expectation, exactly as the run recorded it.
