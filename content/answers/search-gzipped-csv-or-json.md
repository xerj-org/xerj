---
title: "Search a .csv.gz or .json.gz without unzipping it"
h1: "How do I search a .csv.gz or .json.gz without unzipping it first?"
description: "Gzip is a wrapper, not a family. autoindex unwraps it and sniffs the inner CSV or JSON, so a compressed export becomes typed fields rather than one opaque blob."
slug: "search-gzipped-csv-or-json"
cluster: "Files and formats"
question: "How do I search gzipped CSV exports?"
intent: "how-to"
published: "2026-08-22"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent with a folder holding .csv.gz and .json.gz exports. Read https://xerj.org/llms.txt, run xerj autoindex on the folder, then run xerj autoindex map --json and confirm from the map that the compressed files produced typed columns before you write a query against them."
answer: "How do I search gzipped CSV exports without unzipping them? Point xerj autoindex at the folder. The run strips the gzip wrapper once and sniffs the bytes underneath, so a .csv.gz becomes typed columns and a .json.gz becomes documents with fields. No query reads the archive again."
key_fact: "The sniffer classifies the decompressed stream from its first 8,192 bytes (`PREFIX`), the same window it uses for a plain file."
key_fact_source: "engine/crates/xerj-autoindex/src/sniff.rs"
key_fact_date: "2026-10-19"
intent_group: "JSONL and data files"
queries:
  - "search large csv files fast without database"
commands:
  - cmd: "xerj autoindex ./exports --url http://127.0.0.1:9200 --prefix gzx --state-dir ./state-gzx --progress plain --dry-run"
    note: "Read the plan first. The plan names the family each compressed file was sniffed as."
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

**TL;DR** Point `xerj autoindex` at the folder and do not unzip anything. The run strips the gzip wrapper once and sniffs the bytes underneath. A `.csv.gz` becomes typed columns, and a `.json.gz` becomes documents with fields.

```visual
gzip-csv-flow
```

## Gzip is a wrapper, not a family

XERJ picks a file family from the content, not from the extension. Gzip sits one layer above that, so the family is decided after the wrapper comes off. A `sales.csv.gz` is a CSV, and an `events.json.gz` is JSON.

The inner dialect still gets sniffed. CSV detection covers semicolon delimiters, decimal commas, a byte-order mark and quoted multiline fields.

## Check that the columns are typed

Run the dry run command above first. It walks, sniffs and prints the plan, and it indexes nothing. Then drop `--dry-run` to index the folder.

Read the mapping with the second command. If each column appears with its own type, the dialect survived the wrapper. If the whole file is one text field, it did not.

`xerj autoindex map --url http://127.0.0.1:9200` prints the data map, so the next query names a real index and a real field.

## What was measured

The measured gzip capture on this site is the log family. A plain log and its gzip copy returned the same counts.

```visual
gzip-plain-vs-gzip
```

No `.csv.gz` or `.json.gz` capture was run for this page. The CSV and JSON behaviour here is a documented capability of the extractors, so run the dry run on your own folder to see it for your files.

## Decompression happens once

XERJ decompresses during the `autoindex` run, not during a query. You pay in index size and in memory during the run instead. XERJ is single-node and the server retains heap per indexed document, so keep a compressed-export corpus to a modest volume.

Provenance names the compressed source. `ax_path` keeps the `.gz` file name, and `ax_format` names the wrapper next to the family.

## XERJ or ripgrep-all

| Job | XERJ | ripgrep-all |
| --- | --- | --- |
| Typed columns from a `.csv.gz` | Yes | No, matching lines |
| An agent asking over HTTP or MCP | Yes | No |
| A `.tar.gz` or a `.zip` | No archive handler | Yes |
| One regex, asked once | Needs a node and an index | Yes, no index |

XERJ does not unpack a `.tar.gz` or a `.zip`, so their contents are not searchable in XERJ. An `mbox` file needs no unpacking: since v1.0.0-rc.75 XERJ detects it by content and streams one message at a time ([#949](https://github.com/xerj-org/xerj/pull/949)). PST, OST and Maildir are unhandled. The [ripgrep-all comparison](/compare/xerj-vs-ripgrep-all) covers the trade in full.

## Related shapes

Plain CSV exports are on the [multiple CSV files page](/answers/index-multiple-csv-files), with a capture of how headers split into datasets. JSON and JSONL logs are on the [JSON logs page](/answers/search-json-and-jsonl-logs).
