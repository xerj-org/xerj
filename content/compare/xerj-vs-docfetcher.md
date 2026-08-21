---
title: "DocFetcher compared with XERJ for document search"
h1: "What's a free DocFetcher alternative that an agent can call?"
description: "DocFetcher is a desktop search GUI with a portable index and Outlook PST support. XERJ is a search engine an agent calls. A capability comparison, with no benchmark."
slug: "xerj-vs-docfetcher"
cluster: "Comparison: desktop search"
question: "What is a free DocFetcher alternative that an agent can call?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent on a Windows or Linux workstation with a documents folder. Read https://xerj.org/llms.txt, start a local XERJ node, run xerj autoindex on the folder, read the data map, then answer the user question with a match_phrase query that cites the ax_path of every hit."
commands:
  - cmd: "xerj autoindex ./Documents --url http://127.0.0.1:9200 --prefix docs --progress plain"
    note: "Index the documents folder. No configuration file and no mapping are needed."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docs-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"purchase order\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Ask the phrase question over HTTP and read the file path from every hit."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Serve the same node to an agent as a stdio MCP server."
links_out:
  - "/answers/search-file-contents-in-a-folder"
  - "/answers/search-word-documents-in-a-folder"
  - "/answers/give-chatgpt-claude-local-file-access"
  - "compare/xerj-vs-recoll"
  - "compare/xerj-vs-ripgrep-all"
evidence:
  - claim: "DocFetcher is an open-source desktop search application under the Eclipse Public License, and it runs on Windows, Linux and macOS."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "DocFetcher portable versions keep the index in the application folder, which makes a fully indexed document repository that can travel on a USB drive or a cloud drive."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "DocFetcher indexes Microsoft Office, Outlook PST, OpenOffice, PDF, EPUB, HTML, RTF, CHM, AbiWord, Visio, SVG and customizable plain-text formats, plus zip, 7z, rar and tar archives with unlimited nesting."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "The DocFetcher query syntax supports OR, AND and NOT with wildcards, phrase search, fuzzy search, proximity search and boosting, and the preview pane highlights every match."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "DocFetcher 1.1.27 requires Windows 7 SP1 or later, Linux with GTK3, or macOS 11 or later, and since 1.1.26 no separate Java runtime install is needed."
    source: "https://docfetcher.sourceforge.io/download/"
  - claim: "A multi-user web interface is sold separately as DocFetcher Server; the free desktop application has no such interface."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "ripgrep-all wraps ripgrep with adapters for PDF, Office documents, archives and SQLite, and needs no index."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "Recoll indexes email and can run OCR on image-only PDF documents through tesseract or ABBYY FineReader."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Elasticsearch is a distributed engine with cross-node replication and failover, which a single-node engine does not provide."
    source: "https://www.elastic.co/docs/deploy-manage/distributed-architecture"
faq:
  - q: "How do I search a documents folder on Windows without Spotlight?"
    a: "Index the folder with a tool of your own. DocFetcher indexes it for a person at the keyboard. XERJ indexes it for a program, and answers over HTTP or MCP."
  - q: "DocFetcher vs a local search API?"
    a: "DocFetcher is a GUI application with no API in the free version. A local search API answers a program instead of a person, which is the difference that matters for an agent."
  - q: "How do I search a folder of PDFs, Word docs, and markdown all at once?"
    a: "Both tools index the folder in one pass. DocFetcher shows the hits in a result window. `xerj autoindex` writes typed documents that a `match_phrase` query reads over HTTP."
  - q: "Did you run a head-to-head benchmark between XERJ and DocFetcher?"
    a: "No. No shared corpus was frozen and no recall or timing numbers were measured, so this page publishes documented capabilities and no win counts."
  - q: "Which tool carries its index on a USB drive?"
    a: "DocFetcher. The portable versions keep the index in the application folder, so the indexed repository travels with the drive. XERJ keeps its data in the node data directory."
  - q: "Can an agent call DocFetcher directly?"
    a: "Not the free desktop application, which has a GUI and no API. A multi-user web interface is sold separately as DocFetcher Server."
---

**TL;DR** — DocFetcher is a mature desktop search GUI. It reads Outlook PST files and nested archives, and its portable version carries the index on a USB drive. XERJ is a search engine that a program calls over HTTP or MCP. No head-to-head benchmark was run.

## No benchmark was run, and this page says so

We did not freeze a shared corpus. We did not install DocFetcher next to XERJ. We measured no recall, no hit counts and no latency.

There is no win count on this page. No run stands behind one.

Every DocFetcher statement below comes from the DocFetcher website. Every XERJ statement is a documented capability of the binary.

If you want numbers, index your own folder in both tools. Compare the two result lists yourself. That is the only comparison that describes your documents.

## What DocFetcher is

DocFetcher is an open-source desktop search application under the Eclipse Public License. It runs on Windows, Linux and macOS. The current release is 1.1.27, and since 1.1.26 it installs no separate Java runtime.

You create an index per folder. The GUI then answers from that index, with a result pane, a preview pane and highlighted matches. Filters narrow the result list by file size, file type and location.

The query syntax is the familiar boolean set. It has wildcards, phrase search, fuzzy search, proximity search and boosting.

DocFetcher indexes nothing by default. That is a design decision on the project's own page: the user chooses the folders rather than the whole disk.

## What XERJ is

XERJ is a single Rust binary that runs one search node. `xerj autoindex <folder>` reads the folder and detects each file family by content. It infers a dataset per file shape and writes the documents.

The command takes no configuration file and no mapping.

The node answers the Elasticsearch REST API. `xerj autoindex map` prints a data map of what the folder became. An agent reads that map, then names a real index and a real field.

`xerj mcp` is a stdio MCP server in the same binary. It serves 10 tools against a node you already started. Agent memory lives in the engine under `/_memory/{namespace}`.

XERJ is single-node. There is no replication and no failover. The default embedder is lexical feature hashing, not a neural model, and neural retrieval is opt-in through `--embed-mode neural`.

## Capabilities, side by side

Every row is a documented capability of each tool. No row is a measured result.

| capability | DocFetcher | XERJ |
| --- | --- | --- |
| human GUI with a preview pane | yes, with highlighted matches | none, HTTP and MCP only |
| Windows desktop application | yes, Windows 7 SP1 or later | a server process, no window |
| portable index on a USB drive | yes, the portable versions | data lives in the node data directory |
| Outlook PST email | yes | no email handler |
| zip, 7z, rar and tar archives | yes, unlimited nesting | no archive handler |
| OCR for image-only PDFs | none | none |
| fuzzy and proximity operators | yes, in the query syntax | terms and phrases |
| automatic index updates | yes, while the application runs | rerun `xerj autoindex` |
| format detection | file extension, with a regex option | content sniffing per family |
| HTTP query API | not in the free application | yes, Elasticsearch REST API |
| MCP server for an agent | none | yes, `xerj mcp`, 10 tools |
| catalog of refused files | index report in the GUI | `autoindex-catalog`, one reason per file |
| agent memory in the engine | none | yes, `/_memory/{namespace}` |
| license | Eclipse Public License | Apache-2.0 |

## When to choose DocFetcher instead

Choose DocFetcher when a person does the searching. The GUI has been built and repaired for years. A result window with a preview pane beats an API for a human reader.

Choose DocFetcher on a Windows workstation. It installs as a desktop application and wants no terminal, no node and no HTTP call.

Choose DocFetcher when the index must travel. The portable version keeps the index in the application folder. The folder can then live on a USB drive, in an encrypted volume or on a cloud drive.

Choose DocFetcher for Outlook PST files. XERJ has no email handler, so those messages never reach a XERJ index.

Choose DocFetcher for archives. It walks zip, 7z, rar and tar files with unlimited nesting. XERJ has no archive handler.

Choose DocFetcher when the query needs fuzzy or proximity operators. XERJ matches terms and phrases.

## When to choose Recoll or ripgrep-all instead

Choose Recoll instead when you want a desktop GUI with email indexing, OCR hooks and open-at-page. The [Recoll comparison](/compare/xerj-vs-recoll) covers that trade.

Choose ripgrep-all instead for one question, today, with no index at all. The [ripgrep-all comparison](/compare/xerj-vs-ripgrep-all) covers that trade.

Choose Elasticsearch instead when the documents must live on more than one host. XERJ has no replication and no failover. XERJ speaks the same REST API on one node.

## When XERJ is the better fit

Choose XERJ when the caller is a program. The free DocFetcher application has a window and no API. An agent that speaks HTTP or MCP calls XERJ with no wrapper.

Choose XERJ when the search must run with no desktop. The binary is a server process, so it runs on a headless host and answers over the network.

Choose XERJ when the folder is mixed. Format detection reads content, not the file extension. A CSV file with the wrong file extension still lands in a typed dataset.

Choose XERJ when the answer must carry provenance. Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword fields. An agent can cite the source file.

Choose XERJ when the same node must also hold agent memory. `/_memory/{namespace}` puts durable agent memory beside the documents.

## What XERJ does not have

XERJ has no GUI. There is no result window, no preview pane and no term highlighting.

XERJ has no OCR, no email handler and no archive handler. It has no role-based access control and no single sign-on.

XERJ runs on one node. There is no failover. Plan for restore from a copy.

## How to read this page

This is a capability comparison drawn from each tool's own documentation. It is not a benchmark, and it is not a claim about your files.

The honest summary is short. DocFetcher is the better desktop application for a person on Windows. XERJ is the search engine for the program that searches for you.

The [folder search walkthrough](/answers/search-file-contents-in-a-folder) shows the XERJ side in full.
