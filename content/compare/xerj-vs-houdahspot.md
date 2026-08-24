---
title: "HoudahSpot compared with XERJ for file search"
h1: "XERJ vs HoudahSpot — Spotlight GUI vs an agent index"
description: "HoudahSpot is a power-user front end for Spotlight, with hundreds of attribute filters. XERJ is a folder index an agent calls. No Mac run stands behind this page."
slug: "xerj-vs-houdahspot"
cluster: "Comparison: desktop search"
question: "HoudahSpot vs a local search engine I can script?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent that must query a folder on a Mac without a person driving a GUI. Read https://xerj.org/llms.txt, start a local XERJ node, run xerj autoindex on the folder, read the data map, then answer over HTTP or through xerj mcp and cite the ax_path of every file you used."
commands:
  - cmd: "xerj autoindex ./documents --url http://127.0.0.1:9200 --prefix docs --progress plain"
    note: "Index the folder. The command takes no configuration file and no mapping."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docs-*/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"must\":[{\"match_phrase\":{\"body\":\"termination for convenience\"}}],\"filter\":[{\"term\":{\"ax_format\":\"docx\"}}]}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Combine a phrase and a format filter in one scriptable request."
  - cmd: "xerj mcp --help"
    note: "Read the MCP stdio server's tools, which is how an agent calls the index."
links_out:
  - "compare/xerj-vs-spotlight"
  - "compare/xerj-vs-recoll"
  - "compare/xerj-vs-docfetcher"
  - "/answers/search-file-contents-in-a-folder"
  - "/answers/give-chatgpt-claude-local-file-access"
evidence:
  - claim: "HoudahSpot builds upon macOS Spotlight to deliver advanced file search for macOS and requires Spotlight indexing to be enabled."
    source: "https://www.houdah.com/houdahSpot/"
  - claim: "HoudahSpot searches by file name, text content and file extension, and by attributes including tags, date created, file size, image resolution and author, with boolean operators, regular expressions, multiple search folders and excluded folders."
    source: "https://www.houdah.com/houdahSpot/features.html"
  - claim: "HoudahSpot lets you add and sort by any of hundreds of available columns, preview results with Quick Look and highlighted text, and save recurring searches as templates."
    source: "https://www.houdah.com/houdahSpot/features.html"
  - claim: "HoudahSpot automates searches with AppleScript and integrates with launchers such as Alfred and LaunchBar."
    source: "https://www.houdah.com/houdahSpot/"
  - claim: "mdfind queries the central macOS metadata store that HoudahSpot builds on, with -onlyin, -name, -count, -literal, -interpret and -live."
    source: "https://keith.github.io/xcode-man-pages/mdfind.1.html"
  - claim: "XERJ streaming extractors cover JSON/JSONL, dialect-sniffed CSV, structured logs, SQL dumps, SQLite, PDF, DOCX, HTML, XML, YAML, plain text and gzip variants, with no configuration file and no mapping."
    source: "landing/llms.txt:231"
faq:
  - q: "HoudahSpot vs a local search engine I can script?"
    a: "HoudahSpot is a GUI over the Mac's metadata store, scriptable through AppleScript. XERJ is a node that answers HTTP and MCP directly, so a program calls it without a wrapper."
  - q: "What's better than HoudahSpot if I want an API for an agent?"
    a: "An engine with an API. XERJ answers the Elasticsearch REST API and serves 10 MCP tools from the same binary, so the agent sends a query rather than driving an interface."
  - q: "Advanced Mac file search that a coding agent can call?"
    a: "Index the folder with `xerj autoindex` and attach it with `xerj mcp`. The agent then gets cited passages with a file path on every hit."
  - q: "Did you compare XERJ and HoudahSpot head to head?"
    a: "No. Nothing on this page was run on macOS, HoudahSpot was not installed, and there is no timing, no recall figure and no win count here."
  - q: "Which one has the better metadata filters?"
    a: "HoudahSpot, and it is not close. Its own site advertises hundreds of sortable columns and attributes such as image resolution, author and tags, with boolean operators and regular expressions."
  - q: "Does HoudahSpot need Spotlight?"
    a: "Yes. It builds on macOS Spotlight and requires Spotlight indexing to be enabled, so what Spotlight has not indexed is not there for HoudahSpot to filter."
  - q: "Which one gives an agent a citable passage?"
    a: "XERJ. Every document carries `ax_path`, `ax_file` and `ax_format` plus four more provenance fields, so a hit names its source file and an agent can cite it."
  - q: "Can I use both?"
    a: "Yes. HoudahSpot for a person refining a search by hand, XERJ for the folder a program has to query on its own."
---

**TL;DR** — HoudahSpot is a power-user front end for the Mac's own Spotlight index, with hundreds of attribute filters and saved templates. XERJ is a folder index a program calls over HTTP or MCP. No head-to-head was run and nothing here was tested on macOS.

## No Mac run stands behind this page

HoudahSpot was not installed and nothing on this page was executed on macOS. There is no timing, no recall figure and no win count.

Every HoudahSpot statement below comes from Houdah Software's own site. The `mdfind` statements come from the macOS `mdfind(1)` man page. Every XERJ statement is a documented capability of the binary.

Build your own file set if you want numbers. Only that comparison describes your documents.

## What HoudahSpot is

HoudahSpot is an advanced file-search application for macOS. Its own description is that it **builds upon macOS Spotlight**, and it requires Spotlight indexing to be enabled.

That is the load-bearing fact for this comparison. HoudahSpot is a much better way to *ask* the Mac's metadata store a question. The store is still the Mac's. `mdfind` is the command-line client for the same store, with `-onlyin`, `-name`, `-count`, `-literal`, `-interpret` and `-live`.

What it adds is the interrogation surface. Search by file name, text content and file extension. Search by attributes including tags, creation date, file size, image resolution and author.

Combine criteria with boolean operators. Match with regular expressions. Search several folders at once while excluding others.

Add and sort by any of hundreds of available columns. Preview a result with Quick Look and highlighted text. Save a recurring search as a template.

Automate with AppleScript, and hook into launchers such as Alfred and LaunchBar.

For a person who searches for a living, that is a serious tool.

## What XERJ is

XERJ is a single Rust binary that runs one search node. `xerj autoindex <folder>` reads the folder, detects each file family by content, infers a dataset per file shape and writes the documents. It takes no configuration file and no mapping.

The streaming extractors cover JSON and JSONL, CSV with dialect sniffing, structured logs, `sqldump` files, SQLite, PDF, DOCX, HTML, XML, YAML, plain text and gzip variants of those. XERJ builds its own index. It does not read the operating system's.

The node answers the Elasticsearch REST API, and `xerj autoindex map` prints a data map of what the folder became. The `xerj mcp` subcommand is a stdio MCP server in the same binary, and it serves 10 tools against a running node.

XERJ is single-node, with no replication and no failover. The default embedder is lexical feature hashing rather than a neural model; neural retrieval is opt-in through `--embed-mode neural`. There is no OCR.

## Capabilities, side by side

Every row is a documented capability of each tool. **No row is a measured result.**

| capability | HoudahSpot | XERJ |
| --- | --- | --- |
| index it searches | the macOS Spotlight store | its own, built by `xerj autoindex` |
| requires Spotlight indexing | yes | no |
| power-user metadata filters | hundreds of columns and attributes | 7 `ax_*` provenance fields |
| boolean operators and regular expressions in the UI | yes | query DSL over HTTP instead |
| saved search templates | yes | a saved request body |
| preview with Quick Look and highlighting | yes | file path only, in `ax_path` |
| multiple search folders, excluded folders | yes | one folder per run, plus ignore rules |
| scripting surface | AppleScript, launcher integrations | Elasticsearch REST API and MCP |
| HTTP query API | not advertised | yes |
| MCP server for an agent | none | yes, `xerj mcp`, 10 tools |
| typed columns from a CSV, a `sqldump` file or SQLite | not advertised | yes, a dataset per file shape |
| per-file refusal reason | not advertised | `autoindex-catalog`, one reason per file |
| agent memory in the engine | none | yes, `/_memory/{namespace}` |
| runs on Linux and Windows | macOS only | yes |

## When to choose HoudahSpot instead

Choose HoudahSpot when a **person** is doing the searching and the search is hard. Refine by attribute, combine criteria with booleans, add a column and sort on it. That is what it is built for, and it is genuinely good at it.

Choose HoudahSpot for **metadata filters**. Image resolution, author, tags, dates, sizes, and hundreds of sortable columns. XERJ has seven provenance fields and no ambition to compete here. Conceded outright.

Choose HoudahSpot when you want a **preview pane**. Quick Look with the match highlighted, and folding to show the context around a hit, beats reading an HTTP response.

Choose HoudahSpot when the searches **repeat**. Templates turn a hard-won query into something a person reuses next week without rebuilding it.

Choose HoudahSpot when the Mac's index **already has** what you need. If Spotlight indexed it, HoudahSpot can filter it, and no second index has to exist.

## When XERJ is the better fit

Choose XERJ when the caller is a **program**. HoudahSpot automates through AppleScript, a scripting bridge into an application. XERJ answers HTTP and MCP directly, so an agent sends a query and reads JSON.

Choose XERJ when the folder must become **typed data**. A semicolon-delimited CSV, a `sqldump` file, a SQLite file: each becomes a dataset with fields. You filter and aggregate those fields, rather than filtering files by attribute.

Choose XERJ when a **missing file must be explainable**. Refused files land in `autoindex-catalog` with a reason. The ignore rules report what they dropped and which rule dropped it.

Choose XERJ when the hit must carry **provenance an agent can cite**. Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword fields.

Choose XERJ when the machine is **not a Mac**. HoudahSpot is macOS only.

## What XERJ does not have

No GUI, no preview pane, no highlighting in a result window, no saved templates.

No OCR, no email handler and no archive handler. No role-based access control and no single sign-on. XERJ never fetches a page from the web: the files must already be on disk.

No system-wide index. XERJ knows the folders you indexed and nothing else.

## How to read this page

This is a capability comparison drawn from each tool's own documentation. It is not a benchmark and it did not run on macOS.

The honest summary is short. HoudahSpot is the better tool for a person searching a Mac by hand. Its metadata filtering is in a different class from anything XERJ offers. XERJ is the index a program queries when it needs the contents of a chosen folder as structured, cited data.

The [Spotlight comparison](/compare/xerj-vs-spotlight) covers the store HoudahSpot sits on. The [Recoll comparison](/compare/xerj-vs-recoll) covers the cross-platform desktop search with email indexing and OCR hooks. The [DocFetcher comparison](/compare/xerj-vs-docfetcher) covers the portable Java GUI.
