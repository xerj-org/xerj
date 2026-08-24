---
title: "Search a .csv.gz or .json.gz without unzipping it"
h1: "How do I search a .csv.gz or .json.gz without unzipping it first?"
description: "Gzip is a wrapper, not a family. autoindex unwraps it and sniffs the inner CSV or JSON, so a compressed export becomes typed fields rather than one opaque blob."
slug: "search-gzipped-csv-or-json"
cluster: "Files and formats"
question: "How do I search gzipped CSV exports?"
intent: "how-to"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent with a folder holding .csv.gz and .json.gz exports. Read https://xerj.org/llms.txt, run xerj autoindex on the folder, then run xerj autoindex map --json and confirm from the map that the compressed files produced typed columns before you write a query against them."
commands:
  - cmd: "xerj autoindex ./exports --url http://127.0.0.1:9200 --prefix gzx --state-dir ./state-gzx --progress plain --dry-run"
    note: "Read the plan first. The plan names the family each compressed file was sniffed as."
  - cmd: "xerj autoindex ./exports --url http://127.0.0.1:9200 --prefix gzx --state-dir ./state-gzx --progress plain"
    note: "Index the folder. The .gz files are decompressed once, during this run."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map so the next query names a real index and a real field."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/gzx-*/_mapping"
    note: "Confirm the inner columns are typed, rather than one text blob per file."
links_out:
  - "search-gzip-logs-without-zgrep"
  - "index-multiple-csv-files"
  - "search-json-and-jsonl-logs"
  - "catalog-files-with-autoindex-map"
  - "/compare/xerj-vs-ripgrep-all"
evidence:
  - claim: "Streaming extractors cover JSON/JSONL, CSV (dialect-sniffed: semicolon, decimal comma, BOM, quoted multiline), structured logs, SQL dumps, SQLite, PDF, DOCX, HTML, XML, YAML, plain text, and gzip variants."
    source: "landing/llms.txt:231"
  - claim: "In the captured gzip-log run the plain copy and the gzip copy of one log both returned 5001 from _count and both returned 1250 hits for level=ERROR, and the gzip index recorded ax_format as logs(gzip)."
    source: "content/answers/search-gzip-logs-without-zgrep.md"
  - claim: "ripgrep-all wraps ripgrep with adapters for PDF, Office documents, zip, tar, compressed files and SQLite, and needs no index."
    source: "https://github.com/phiresky/ripgrep-all"
faq:
  - q: "How do I search gzipped CSV exports?"
    a: "Point `xerj autoindex` at the folder. Gzip is treated as a wrapper, so the inner CSV is sniffed and typed and you query columns rather than a blob."
  - q: "Can I index .json.gz files as structured data?"
    a: "Yes. JSON and JSONL are parsed families and gzip is a wrapper over them, so a `.json.gz` file lands as records with fields, not as one compressed document."
  - q: "How do I grep a compressed CSV?"
    a: "You can, with `zgrep` or `ripgrep-all`, and for a single one-off question that is the smaller tool. Indexing wins when you are going to ask the folder more than one question."
  - q: "How do I search a .csv.gz or .json.gz without unzipping it first?"
    a: "You never unzip it yourself. `xerj autoindex` decompresses each `.gz` once during the run and indexes what it reads, so no query ever expands the archive again."
  - q: "Does the CSV dialect survive the gzip wrapper?"
    a: "That is the design: the wrapper is stripped and the inner bytes go through the same CSV dialect sniff that handles semicolons, decimal commas, a BOM and quoted multiline fields. No capture on this page measures it."
  - q: "Can XERJ read a .tar.gz?"
    a: "No. XERJ has no archive handler, so a tar inside a gzip is not unpacked. `ripgrep-all` does walk nested archives, and it is the right tool for that folder."
  - q: "How do I tell a gzip-sourced document from a plain one?"
    a: "Read `ax_format`. The captured gzip log index recorded `logs(gzip)` where the plain copy recorded `logs`, and `ax_path` keeps the `.gz` file name."
  - q: "Did you measure a .csv.gz run for this page?"
    a: "No. The measured gzip capture on this site is the log family. The CSV and JSON wrapper behaviour here is documented capability, and it is written as such."
---

**TL;DR** — Gzip is a wrapper, not a family. `xerj autoindex` strips it during the run and sniffs the bytes underneath. A `.csv.gz` becomes typed columns, and a `.json.gz` becomes documents with fields. You never decompress anything yourself, and no query re-reads the archive.

## What this page measured, and what it did not

The measured gzip capture on this site is the **log** family. A plain log and its gzip copy both returned `5001` from `_count`, and both returned 1250 hits for `level=ERROR`. The gzip index stored `ax_format` as `logs(gzip)`. That run is published on the [gzip logs page](/answers/search-gzip-logs-without-zgrep).

**No `.csv.gz` or `.json.gz` capture was run for this page.** The wrapper-plus-inner-dialect behaviour below is a documented capability of the extractors, not a measurement. This page publishes no counts, no field lists and no timings for those two shapes.

If your export matters, run the dry run on your own folder first and read the plan. That takes one command and it answers the question for your files rather than for someone else's.

## Gzip is a wrapper, not a family

XERJ detects file families by content, not by file extension. Gzip sits one layer above that. The streaming extractors cover JSON and JSONL, CSV with dialect sniffing, structured logs, `sqldump` files, SQLite, PDF, DOCX, HTML, XML, YAML and plain text — **and gzip variants of them**.

So the question "which family is this file?" is answered after the wrapper comes off, not before. A `sales.csv.gz` is a CSV. An `events.json.gz` is JSON.

The practical consequence is that the inner dialect still gets sniffed. CSV detection covers semicolon delimiters, decimal commas, a byte-order mark and quoted multiline fields, and none of that is skipped because the bytes arrived compressed.

## Why this is different from grepping the archive

`zgrep` and `ripgrep-all` both give you the text inside a `.gz`. What they cannot give you is the **shape**.

A compressed CSV read as text is a stream of lines. A compressed CSV read as a CSV is a dataset with named columns. A numeric column is then a number you can range over, and a keyword column is a term you can filter and aggregate.

```sh
curl -s -XGET 'http://127.0.0.1:9200/gzx-*/_mapping'
```

That request is the check that matters. If the inner columns appear in the mapping with their own types, the dialect survived the wrapper. If the whole file is one text field, it did not, and you should say so rather than write around it.

## Read the plan before the run

```sh
xerj autoindex ./exports --url http://127.0.0.1:9200 --prefix gzx --state-dir ./state-gzx --progress plain --dry-run
```

A dry run walks, sniffs and infers, prints the plan, and indexes nothing. It is the cheapest way to see which family each compressed file was recognised as, and it costs one command.

Then run it for real and read the data map, which names the datasets and the fields your next query can use.

```sh
xerj autoindex map --url http://127.0.0.1:9200
```

## Decompression happens once

XERJ pays for decompression during the `autoindex` run, not during a query. The compressed file is not re-read to answer a search.

What you pay instead is index size and memory during the run. XERJ is single-node and the server currently retains heap per indexed document, so keep a compressed-export corpus to a modest volume rather than pointing it at an archive directory of unknown size.

Provenance still names the compressed source. `ax_path` keeps the `.gz` file name, and `ax_format` names the wrapper alongside the family. A hit from a compressed export is therefore never mistaken for a hit from a plain one.

## When to choose ripgrep-all instead

Choose `ripgrep-all` when the archive is **nested**. It wraps ripgrep with adapters for PDF, Office documents, zip, tar, compressed files and SQLite. It walks into them without building an index.

XERJ has no archive handler. A `.tar.gz`, a `.zip` and a mail archive are not unpacked, so their contents are not searchable in XERJ at all. That is a real rga win and it is not worked around on this page.

Choose `ripgrep-all` for one question asked once, too. There is no node to start and no index to build. For a single regex over a single compressed file, that is less work by every measure.

Choose XERJ when the folder will be asked more than one question. Choose it when the answer needs typed columns rather than matching lines. Choose it when the caller is an agent that wants HTTP or MCP. The [ripgrep-all comparison](/compare/xerj-vs-ripgrep-all) covers that trade in full.

## Related shapes

Uncompressed CSV exports in a folder are the [multiple CSV files page](/answers/index-multiple-csv-files), which carries a real capture of how headers split into datasets.

JSON and JSONL logs sitting next to gzip logs are the [JSON logs page](/answers/search-json-and-jsonl-logs). Both of those pages measured what they publish; this one names its documented mechanism and stops there.
