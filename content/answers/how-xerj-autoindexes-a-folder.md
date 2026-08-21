---
title: "Point a tool at a folder and ask about the files"
h1: "I want to point something at a folder and then ask questions about what's in it. What should I use?"
description: "Index the folder with one command, read the map of what XERJ found, then let an agent query it over MCP instead of opening the files one by one."
slug: "how-xerj-autoindexes-a-folder"
cluster: "Folder indexing"
question: "I want to point something at a folder and then ask questions about what's in it. What should I use?"
intent: "informational"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a node with `xerj --insecure --data-dir ./.xerj-data`, run `xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --progress plain --dry-run` to read the plan, run the same command without --dry-run, then run `xerj autoindex map --url http://127.0.0.1:9200` and answer the user question with one match_phrase query against a real index and a real field, citing the ax_path of every hit."
commands:
  - cmd: "xerj --insecure --data-dir ./.xerj-data"
    note: "Start a local node. Nothing is indexed yet and nothing leaves the machine."
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --progress plain --dry-run"
    note: "Print the plan and the ignore accounting before a single document is written."
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --progress plain"
    note: "Index the folder for real. No configuration file and no mapping are involved."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/notes-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"who owns the release checklist\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Ask the folder a question and read the answering file path out of ax_path."
links_out:
  - "what-is-xerj"
  - "how-xerj-combines-search"
  - "/docs/recipes/zero-config-autoindex"
  - "/docs/recipes/document-folder-index"
  - "give-chatgpt-claude-local-file-access"
  - "catalog-files-with-autoindex-map"
  - "/compare/xerj-vs-web-agent-search"
faq:
  - q: "I want to point something at a folder and then ask questions about what's in it. What should I use?"
    a: "Index the folder with `xerj autoindex`, read `xerj autoindex map` to see what it found, then let the agent query the index over MCP. No configuration file is involved."
  - q: "Can ChatGPT search a folder on my laptop, or do I need something else?"
    a: "A hosted chat cannot reach your disk on its own. It needs a local server it can call, such as an MCP server over an indexed folder."
  - q: "How do I give Claude or ChatGPT access to files on my machine?"
    a: "Index the folder, then register `xerj mcp` with the agent host. The agent calls a search tool and gets hits with file paths, and no file is uploaded."
  - q: "Is a web search API the right tool for a folder on disk?"
    a: "No. A web search API reads pages that are on the public web, and a private folder is not. Index the folder locally and keep the web API for web pages."
  - q: "Do I have to tell it what kind of files are in there?"
    a: "No. `xerj autoindex` detects each file family from the content rather than the file extension, then infers a dataset and its field types per file shape."
  - q: "How do I see what it did before I trust it?"
    a: "Run the command with `--dry-run` first: it walks, sniffs and infers, prints the plan and the ignore accounting, and writes nothing."
  - q: "Some of my files did not show up. Why?"
    a: "Every run prints what it dropped and by which ignore rule, and files it opened but refused land in `autoindex-catalog` with a reason string. Hidden files are always skipped."
---

**TL;DR** — Run `xerj autoindex <folder>` against a local node. It detects the file types itself, builds typed indices, and needs no configuration file. Then run `xerj autoindex map` to see which index and field to query. Ask the folder a question over HTTP or through `xerj mcp`.

## Index the folder with one command

`xerj autoindex <folder>` walks the folder, detects each file family by content rather than by file extension, infers a dataset per file shape, and writes the documents. There is no configuration file, no mapping and no schema to declare first.

```sh
xerj --insecure --data-dir ./.xerj-data
xerj autoindex ./notes --url http://127.0.0.1:9200 --prefix notes --progress plain
```

Add `--dry-run` to the same command to walk, sniff and infer without writing anything. The dry run prints the plan it would execute and counts the files each ignore rule dropped. That is the cheapest way to learn that half the folder sits inside `node_modules/`.

## Read the map before you write a query

`xerj autoindex map` prints what the folder became: every dataset, its index name, its formats, its field names with their inferred types, and a ready-to-send query. An agent that reads the map first names a real index and a real field instead of guessing one.

Guessing costs more than reading. A query against a field that does not exist returns an HTTP 400 or an empty result, and an agent cannot tell an empty result from a wrong field name.

## Ask the folder a question

A `match_phrase` query against the indexed folder returns the passages and the file each one came from. Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword provenance fields, so a hit names its source file.

```sh
curl -s -XPOST http://127.0.0.1:9200/notes-*/_search \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"who owns the release checklist"}},"size":5,"_source":["ax_path","ax_format","title"]}'
```

An agent answers from the returned passage and cites `ax_path`, rather than reading whole files into its context to find the paragraph.

## Hand the indexed folder to an agent

`xerj mcp` exposes the indexed folder to an agent host as a stdio tool server, serving 10 tools against a node that is already running. The agent sends a query and reads hits with file paths, instead of opening files one by one.

The node and the MCP server both run on your machine. No file content is uploaded, because the agent host calls a local process rather than a hosted API.

## Why a web search API cannot do this

A web search API reads pages that are on the public web, and its documented input is a query string or a URL. A folder on your own disk has no URL, so no web API can reach it.

Index the folder locally and keep the web API for web pages. The [web agent search comparison](/compare/xerj-vs-web-agent-search) sets out that boundary in full.

## What the run will not do for you

`xerj autoindex` reads no image-only PDF, because XERJ has no OCR and a PDF with no text layer produces no text. It has no email handler, no mbox handler and no archive handler.

Hidden files such as `.env`, `.git/` and `.ssh` are skipped whatever the ignore configuration says, which is what keeps secrets out of the index. Everything else it refused is recorded in `autoindex-catalog` with a reason, so a missing file is explainable rather than silent.
