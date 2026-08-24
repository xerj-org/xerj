---
title: "Spotlight compared with XERJ for folder content"
h1: "XERJ vs macOS Spotlight for folder content an agent can query"
description: "Spotlight is the Mac's system-wide index with zero setup and wide metadata. XERJ is a folder index an agent queries. A capability comparison, run on no Mac."
slug: "xerj-vs-spotlight"
cluster: "Comparison: desktop search"
question: "Spotlight can't find text inside my project files. What else is there?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent that needs the text inside a folder of project files. Read https://xerj.org/llms.txt, start a local XERJ node, run xerj autoindex on the folder, read the data map, then answer with a match_phrase query and cite the ax_path of every file you used - and do not assume the operating system's own index covers those files."
commands:
  - cmd: "xerj autoindex ./project --url http://127.0.0.1:9200 --prefix proj --progress plain --dry-run"
    note: "Read the plan, including which files the ignore rules would drop, before anything is written."
  - cmd: "xerj autoindex ./project --url http://127.0.0.1:9200 --prefix proj --progress plain --no-ignore"
    note: "Index the folder including files git ignores. Hidden files stay skipped either way."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/proj-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"retry budget\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Search the folder over HTTP and read the file path back from every hit."
  - cmd: "xerj mcp --help"
    note: "Read the MCP stdio server's tools, which is how an agent calls the index."
links_out:
  - "compare/xerj-vs-houdahspot"
  - "compare/xerj-vs-recoll"
  - "compare/xerj-vs-ripgrep-all"
  - "/answers/search-file-contents-in-a-folder"
  - "/answers/why-autoindex-skipped-files"
  - "/answers/give-chatgpt-claude-local-file-access"
evidence:
  - claim: "Spotlight helps you quickly find things on your computer and shows suggestions from apps, files, actions, the internet, and the Clipboard."
    source: "https://support.apple.com/guide/mac-help/search-with-spotlight-mchlp1008/mac"
  - claim: "Spotlight metadata importers are plug-ins that extract metadata and text content from a file so the system metadata store can answer queries about it, and an application ships an importer for its own document types."
    source: "https://developer.apple.com/documentation/coreservices/file_metadata/mdimporter"
  - claim: "mdfind queries the central metadata store and returns matching files, with -onlyin to restrict the search to a directory, -name to match on file name, -count to return a total, -literal and -interpret to control query parsing, and -live to keep the count updated."
    source: "https://keith.github.io/xcode-man-pages/mdfind.1.html"
  - claim: "XERJ streaming extractors cover JSON/JSONL, dialect-sniffed CSV, structured logs, SQL dumps, SQLite, PDF, DOCX, HTML, XML, YAML, plain text and gzip variants, with no configuration file and no mapping."
    source: "landing/llms.txt:231"
faq:
  - q: "Spotlight can't find text inside my project files. What else is there?"
    a: "Index the folder yourself. `xerj autoindex` sniffs each file by content and indexes the text, and it does not depend on an importer existing for that file type on your machine."
  - q: "How do I full-text search a folder on a Mac from an agent?"
    a: "Give the agent an index it can call. `xerj autoindex` builds one from the folder and `xerj mcp` serves 10 tools against the running node over stdio, so no subprocess wrapper is needed."
  - q: "Is Spotlight enough for searching PDFs and code?"
    a: "For a person at a keyboard, often yes, and it is already running. For an agent that needs typed fields, a per-file refusal reason and an HTTP API, it is a different job."
  - q: "Did you benchmark XERJ against Spotlight?"
    a: "No. Nothing on this page was tested on macOS, no `mdfind` query was timed, and there is no recall figure and no win count here."
  - q: "Which one indexes my whole disk?"
    a: "Spotlight. It is the system-wide index and it is already built. XERJ indexes the folder you name, in the run you start, and nothing else."
  - q: "Which one an agent can call over an API?"
    a: "XERJ answers the Elasticsearch REST API and serves MCP from the same binary. `mdfind` is a command-line client, so an agent reaches Spotlight through a subprocess wrapper you maintain."
  - q: "Does XERJ index files that git ignores?"
    a: "Not by default. It honours `.xerjignore`, `.gitignore` and `.git/info/exclude` plus a built-in list, and `--no-ignore` turns all of that off. Hidden files such as `.env` and `.ssh` stay skipped either way."
  - q: "Can I use both?"
    a: "Yes, and on a Mac that is the sensible answer. Spotlight for finding the file, XERJ for letting an agent query what is inside a folder you chose."
---

**TL;DR** — Spotlight is the Mac's system-wide index: zero setup, whole-disk reach and wide file metadata. XERJ is a folder index an agent calls over HTTP or MCP. No head-to-head was run for this page and nothing here was tested on macOS.

## No Mac run stands behind this page

We did not run this comparison on a Mac. No `mdfind` query was executed and none was timed.

There is no latency number here, no recall figure and no win count. Every Spotlight and `mdfind` statement below comes from Apple's own documentation and the macOS `mdfind(1)` man page. Every XERJ statement is a documented capability of the binary.

If you want numbers for your machine, run both against your own folder. Only that comparison describes your files.

## What Spotlight is

Spotlight is the search built into macOS. Apple describes it as helping you "quickly find things on your computer", with suggestions drawn from apps, files, actions, the internet and the Clipboard.

Its content comes from **metadata importers**. An importer is a plug-in that extracts metadata and text content from a file into the system metadata store, and an application ships an importer for its own document types. That architecture is the reason Spotlight knows a photograph's dimensions and a track's composer without you configuring anything.

`mdfind` is the command-line client for the same store. It takes `-onlyin` to restrict a search to a directory, and `-name` to match on file name only. It takes `-count` to return a total instead of paths. `-literal` and `-interpret` control how the query string is parsed, and `-live` keeps a count updated as files change.

## What XERJ is

XERJ is a single Rust binary that runs one search node. `xerj autoindex <folder>` reads the folder, detects each file family by content, infers a dataset per file shape and writes the documents. It takes no configuration file and no mapping.

The streaming extractors cover JSON and JSONL, CSV with dialect sniffing, structured logs, `sqldump` files, SQLite, PDF, DOCX, HTML, XML, YAML, plain text and gzip variants of those.

The node answers the Elasticsearch REST API. A `match_phrase` query and a filter on a keyword field work over plain HTTP. The `xerj autoindex map` command prints a data map of what the folder became, and an agent reads it before naming an index and a field.

`xerj mcp` is a stdio MCP server in the same binary and serves 10 tools against a node you already started. Agent memory lives in the engine under `/_memory/{namespace}`.

XERJ is single-node. There is no replication and no failover. The default embedder is lexical feature hashing rather than a neural model, and neural retrieval is opt-in through `--embed-mode neural`. There is no OCR.

## Capabilities, side by side

Every row is a documented capability of each tool. **No row is a measured result**, and no row is a claim about your Mac.

| capability | macOS Spotlight | XERJ |
| --- | --- | --- |
| already running, no setup | yes, it ships with the OS | no, you start a node and run a command |
| whole-disk reach | yes, the system-wide store | the folder you name, per run |
| file-name search across the disk | yes, `mdfind -name` | within an indexed folder, via provenance fields |
| breadth of file metadata | wide, importer-supplied | 7 `ax_*` provenance fields per document |
| coverage of a file type | whichever importer is installed | content sniffing across the extractor families |
| GUI a person uses | yes, the system search UI | none, HTTP and MCP only |
| live updates as files change | yes, and `mdfind -live` reports them | rerun `xerj autoindex` |
| query language | metadata query expressions | Elasticsearch query DSL over HTTP |
| HTTP query API | not documented for the system store | yes, Elasticsearch REST API |
| MCP server for an agent | none | yes, `xerj mcp`, 10 tools |
| typed columns from a CSV or a `sqldump` file | not described in the documentation | yes, a dataset per file shape |
| per-file refusal reason | not described in the documentation | `autoindex-catalog`, one reason per file |
| agent memory in the engine | none | yes, `/_memory/{namespace}` |
| OCR for image-only PDFs | not described in the documentation | none |

## When to choose Spotlight instead

Choose Spotlight when the searcher is a **person on a Mac**. It is already indexed, already running, and costs nothing to start. For finding a file you half-remember, it is hard to beat with anything you must install.

Choose Spotlight when the question is about the **whole disk**. XERJ indexes the folder you point it at, in the run you start. It has no system-wide view and no ambition to have one.

Choose Spotlight when the question is about **file metadata** rather than file contents. Dimensions, durations and authorship arrive through importers with no work from you. XERJ's provenance fields say where a document came from, not what the file's attributes are.

Choose Spotlight when you want **zero setup**. That is a real advantage and this page does not argue with it.

Mac UX is conceded here in full. This page is not an argument that you should stop using Spotlight.

## When XERJ is the better fit

Choose XERJ when the caller is a **program**. An agent that speaks HTTP or MCP needs no wrapper and no subprocess protocol. The `mdfind` tool runs on a command line. Reaching Spotlight from an agent therefore means a wrapper you write and maintain.

Choose XERJ when you need the folder to become **typed data**. A semicolon-delimited CSV, a `sqldump` file, a SQLite file: each becomes a dataset with fields. You can filter, range over and aggregate them. A file-metadata index is a different kind of answer.

Choose XERJ when a **missing file has to be explainable**. Refused files land in `autoindex-catalog` with a reason string. The ignore rules print what they dropped and which rule dropped it. The [skipped-files page](/answers/why-autoindex-skipped-files) walks that reconciliation.

Choose XERJ when the index must include code that **git ignores**. The indexer honours `.xerjignore`, `.gitignore` and `.git/info/exclude` by default, and `--no-ignore` turns the lot off. Hidden files such as `.env` and `.ssh` stay skipped either way. That is what keeps secrets out.

Choose XERJ when the same node must also hold **agent memory**, next to the documents it searches.

## What XERJ does not have

No GUI. No result window, no preview pane, no Quick Look.

No OCR, no email handler and no archive handler. No role-based access control and no single sign-on. XERJ never fetches a page from the web: the files must already be on disk.

One node, no failover. Plan for restore from a copy.

## How to read this page

This is a capability comparison drawn from each tool's own documentation. It is not a benchmark, it is not a recall study, and no part of it ran on macOS.

The honest summary is short. Spotlight is the operating system's own index and the right default for a person at a Mac. XERJ is the index a program queries when it needs the contents of a folder as structured, cited data.

For the power-user Spotlight front end, see the [HoudahSpot comparison](/compare/xerj-vs-houdahspot). For the cross-platform desktop search that indexes email and runs OCR, see the [Recoll comparison](/compare/xerj-vs-recoll).
