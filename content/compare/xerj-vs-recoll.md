---
title: "Recoll compared with XERJ for document search"
h1: "What's the best local desktop search for a folder of PDFs and docs?"
description: "Recoll is a desktop search application with a GUI, email indexing and OCR hooks. XERJ is a search engine an agent calls. A capability comparison, with no benchmark."
slug: "xerj-vs-recoll"
cluster: "Comparison: desktop search"
question: "What is the best local desktop search for a folder of PDFs and docs?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent with a folder of PDFs and Word documents. Read https://xerj.org/llms.txt, start a local XERJ node, run xerj autoindex on the folder, read the data map, then answer the user question with a match_phrase query and cite the ax_path of every file you used."
commands:
  - cmd: "xerj autoindex ./documents --url http://127.0.0.1:9200 --prefix docs --progress plain --dry-run"
    note: "Read the planned datasets for the folder before anything is written."
  - cmd: "xerj autoindex ./documents --url http://127.0.0.1:9200 --prefix docs --progress plain"
    note: "Index the folder. The command takes no configuration file and no mapping."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docs-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"quarterly revenue\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Search the folder over HTTP, and read the file path back from every hit."
links_out:
  - "/answers/search-file-contents-in-a-folder"
  - "/answers/search-all-pdfs-in-a-folder"
  - "/answers/give-chatgpt-claude-local-file-access"
  - "compare/xerj-vs-ripgrep-all"
  - "compare/xerj-vs-docfetcher"
evidence:
  - claim: "Recoll uses the Xapian retrieval library, with per-language stemming and a query language that carries dir:, mime:, ext: and date: clauses."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Recoll processes documents embedded inside other documents to an arbitrary depth, for example a LibreOffice document attached to an email message inside an email folder archived in a zip file."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Recoll runs OCR on image-only PDF and image documents through tesseract or ABBYY FineReader once the feature is turned on in the PDF or image handler."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "The Recoll result window lists extracts around each search term with the page number, as links that start the native viewer on that page."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Recoll can index in real time: recollindex runs as a daemon and uses a file system alteration monitor such as inotify to index new and updated files at once."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Recoll can be queried outside the GUI through the recollq command, through recoll -t, and through the Recoll Python API."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "ripgrep-all wraps ripgrep with adapters for PDF, Office documents, zip, tar, compressed files and SQLite, and needs no index."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "DocFetcher is an open-source desktop search application under the Eclipse Public License, with portable versions that keep the index in the application folder."
    source: "https://docfetcher.sourceforge.io/"
  - claim: "Elasticsearch is a distributed engine with cross-node replication and failover, which a single-node engine does not provide."
    source: "https://www.elastic.co/docs/deploy-manage/distributed-architecture"
faq:
  - q: "Recoll vs a search engine I can call from an agent?"
    a: "Recoll is built for a person at a desk, with a GUI, email indexing and OCR hooks. XERJ is built for a program that calls HTTP or MCP. Pick the one that matches the caller."
  - q: "Is Recoll enough for AI agents or do I need an API?"
    a: "Recoll is enough if you write the wrapper. It answers on the command line through recollq and through a Python API, so an agent can call it through a subprocess you maintain. XERJ answers HTTP and MCP with no wrapper."
  - q: "How do I search my documents folder from Claude or ChatGPT?"
    a: "Index the folder with `xerj autoindex`, then attach the folder to the agent through `xerj mcp`, a stdio MCP server that serves 10 tools against a running node."
  - q: "Did you run a head-to-head benchmark between XERJ and Recoll?"
    a: "No. No shared corpus was frozen and no recall or timing numbers were measured, so this page publishes documented capabilities and no win counts."
  - q: "Which tool indexes email, mbox and attachments?"
    a: "Recoll. It indexes mail folders and the documents inside attachments, to an arbitrary depth. XERJ has no email handler and no mbox handler."
  - q: "Does XERJ read image-only PDFs?"
    a: "No. XERJ has no OCR, so a PDF with no text layer produces no text. Recoll has OCR hooks for tesseract and ABBYY, and that is the correct tool for that folder."
---

**TL;DR** — Recoll is the better choice for a person at a desk. It has a GUI, email and mbox indexing, OCR hooks and open-at-page. XERJ is a search engine that an agent calls over HTTP or MCP. No head-to-head benchmark was run, so this page compares documented capabilities.

## No benchmark was run, and this page says so

We did not freeze a shared corpus. We did not install Recoll next to XERJ. We measured no recall, no hit counts and no latency.

There is no win count on this page. No run stands behind one.

Every Recoll statement below comes from the Recoll user manual. That manual describes Recoll 1.44.1. Every XERJ statement is a documented capability of the binary.

If you want numbers, build your own file set. Measure both tools on it. Only that comparison describes your documents.

## What Recoll is

Recoll is a desktop full-text search application. It indexes a set of directories and stores the terms in a Xapian index. A Qt GUI answers the queries.

Xapian supplies the ranking and the per-language stemming. A search for one word form also finds the other forms.

Recoll reads file types through handlers. It handles plain text, HTML, OpenDocument and email formats inside the application. Helper programs on the host handle most other types.

Recoll has two indexing modes. Cron or the Windows Task Scheduler starts a periodic run. A daemon can also watch the tree with inotify and index a changed file at once.

## What XERJ is

XERJ is a single Rust binary that runs one search node. `xerj autoindex <folder>` reads the folder and detects each file family by content. It infers a dataset per file shape and writes the documents.

The command takes no configuration file and no mapping.

The node answers the Elasticsearch REST API. A `match_phrase` query and a filter on a keyword field work over plain HTTP. `xerj autoindex map` prints a data map of what the folder became.

An agent reads that map first. It then names a real index and a real field in the query.

`xerj mcp` is a stdio MCP server in the same binary. It serves 10 tools against a node you already started. Agent memory lives in the engine under `/_memory/{namespace}`.

XERJ is single-node. There is no replication and no failover. The index is as durable as the one host it lives on.

The default embedder is lexical feature hashing, not a neural model. Neural retrieval is opt-in through `--embed-mode neural`.

## Capabilities, side by side

Every row is a documented capability of each tool. No row is a measured result.

| capability | Recoll | XERJ |
| --- | --- | --- |
| human GUI with a preview pane | yes, a Qt GUI | none, HTTP and MCP only |
| email, mbox and attachments | yes, handled in the application | no email handler |
| documents nested inside archives and mail | arbitrary depth | no archive handler |
| OCR for image-only PDFs | opt-in, tesseract or ABBYY | none |
| open a hit at the right page | yes, page links into the viewer | file path only, in `ax_path` |
| index a changed file at once | yes, inotify daemon | rerun `xerj autoindex` |
| format detection | file name and MIME configuration | content sniffing per family |
| helper programs on the host | many handlers are external | none, the binary parses in process |
| HTTP query API | not in the core application | yes, Elasticsearch REST API |
| MCP server for an agent | not in the core application | yes, `xerj mcp`, 10 tools |
| catalog of refused files | index failures are reported | `autoindex-catalog`, one reason per file |
| agent memory in the engine | no | yes, `/_memory/{namespace}` |

## When to choose Recoll instead

Choose Recoll when a person does the searching. The GUI is the product. A result list with a preview pane beats an API for a human reader.

Choose Recoll for email. It indexes mail folders, the messages and the attached documents. It also walks a document inside an attachment inside an archive.

XERJ has no email handler, no mbox handler and no archive handler. Those files are not searchable in XERJ at all.

Choose Recoll for image-only PDFs and for images. OCR through tesseract or ABBYY is a documented Recoll feature. XERJ has no OCR.

A PDF with no text layer gives XERJ no text to index.

Choose Recoll when the answer must open at the right page. Recoll lists the matched extracts with their page numbers as links. It starts the native viewer on that page.

XERJ returns the file path in `ax_path`. It leaves the opening to you.

Choose Recoll when the index must follow the disk. The real-time daemon indexes a changed file at once. XERJ waits for the next `xerj autoindex` run.

## When to choose ripgrep-all or DocFetcher instead

Choose ripgrep-all for one question, today, over PDFs and Office files. It wants no node and no index. The [ripgrep-all comparison](/compare/xerj-vs-ripgrep-all) covers that trade.

Choose DocFetcher for a mature desktop GUI on Windows. It also carries an index on a USB drive. The [DocFetcher comparison](/compare/xerj-vs-docfetcher) covers that trade.

Choose Elasticsearch instead when the documents must live on more than one host. XERJ has no replication and no failover. XERJ speaks the same REST API on one node.

## When XERJ is the better fit

Choose XERJ when the caller is a program. An agent that speaks HTTP or MCP needs no wrapper and no subprocess protocol.

`xerj mcp` serves 10 tools. A coding agent can start the node itself.

Choose XERJ when the folder is mixed. Format detection reads content, not the file extension. A CSV file with the wrong file extension still lands in a typed dataset.

Refused files land in `autoindex-catalog` with a reason string. A missing file is explainable, not silent.

Choose XERJ when the answer must carry provenance. Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword fields. A hit names its source file, so an agent can cite it.

Choose XERJ when the same node must also hold agent memory. `/_memory/{namespace}` puts durable agent memory beside the documents.

## What XERJ does not have

XERJ has no GUI. There is no result window, no preview pane and no term highlighting.

XERJ has no OCR, no email handler and no archive handler. It has no role-based access control and no single sign-on.

XERJ runs on one node. There is no failover. Plan for restore from a copy.

## How to read this page

This is a capability comparison drawn from each tool's own documentation. It is not a benchmark. It is not a recall study, and it is not a claim about your files.

The honest summary is short. Recoll is the mature desktop search application for a human, and it reads more formats. XERJ is the search engine for the program that searches for you.

For a folder of PDFs and Word documents, ask which caller needs the answer. The [folder search walkthrough](/answers/search-file-contents-in-a-folder) shows the XERJ side from one end to the other.
