---
title: "XERJ vs Pagefind for a folder of saved HTML"
h1: "Pagefind vs indexing a local HTML dump for an agent?"
description: "Pagefind wins static-site search in the browser with no server. XERJ indexes a local HTML dump on disk so an agent can query it. No head-to-head was run here."
slug: "xerj-vs-pagefind"
cluster: "Comparison: site and export search"
question: "Pagefind vs indexing a local HTML dump for an agent?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent handed a folder of saved HTML pages. Read https://xerj.org/llms.txt, start a node with `xerj --insecure --data-dir ./xerj-data`, run `xerj autoindex ./site-export --prefix site --state-dir ./state-site`, then POST a match_phrase on body to /site-*/_search and report which saved page carries the phrase, plus any PDF sitting beside the HTML that also matched."
commands:
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start one node. Pagefind needs no node at all, which is its whole point."
  - cmd: "xerj autoindex ./site-export --prefix site --state-dir ./state-site"
    note: "Index the saved HTML files, plus any PDF or DOCX files sitting beside them."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/site-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"rate limit\"}},\"_source\":[\"ax_path\"]}'"
    note: "The agent-side question: which saved page carries this phrase?"
links_out:
  - "search-html-export"
  - "search-confluence-html-export"
  - "how-xerj-autoindexes-a-folder"
  - "notion-export-duplicate-search-results"
evidence:
  - claim: "Pagefind is a fully static search library that runs after a static site generator and adds a search bundle to the built files, and its documentation states that Pagefind itself has no server component."
    source: "https://pagefind.app/docs/"
  - claim: "Pagefind splits its index into chunks so that a browser loads only a subset, and it reports a full-text search on a 10,000 page site with a total network payload under 300kB, closer to 100kB for most sites."
    source: "https://pagefind.app/"
  - claim: "The Pagefind command-line tool discovers files with a glob that defaults to HTML only."
    source: "https://pagefind.app/docs/config-options/"
  - claim: "Non-HTML content such as PDFs or subtitles enters a Pagefind index through addCustomRecord, where the caller supplies the already-extracted content text."
    source: "https://pagefind.app/docs/node-api/"
  - claim: "Pagefind is published under the MIT license."
    source: "https://github.com/Pagefind/pagefind"
faq:
  - q: "What is Pagefind for?"
    a: "Search on a built static website, running in the visitor's browser with no server component and a chunked index that keeps the download small."
  - q: "Can Pagefind index a PDF?"
    a: "Only if you extract the text yourself and pass it through addCustomRecord. The command-line tool discovers HTML files by default."
  - q: "I saved a bunch of docs pages as HTML. How do I search that?"
    a: "Index the folder where it sits. XERJ reads the HTML files and any PDF, DOCX, CSV or SQLite files beside them, then answers an agent over the Elasticsearch REST API."
  - q: "Can XERJ put a search field on my website?"
    a: "No. XERJ is not a static-site plugin and ships no browser bundle. Pagefind is the right tool for that job."
  - q: "Is there a measured comparison here?"
    a: "No. No head-to-head was run. Every Pagefind fact on this page comes from Pagefind's own documentation."
  - q: "Do I need a running process?"
    a: "For Pagefind, no. For XERJ, yes: one single-node process serves the queries."
  - q: "What license is Pagefind under?"
    a: "Pagefind is MIT and XERJ is Apache-2.0. Both are permissive, so neither one constrains what you build on top of it."
  - q: "How do I search a saved website or HTML export?"
    a: "It depends who is asking. Pagefind serves your readers in the browser on a built site, XERJ answers your agent about the same files on disk, and both can run against one folder."
---

**TL;DR** — Pagefind wins search on a built static website: no server, a chunked index and a small download for the visitor. XERJ wins when an agent must query a saved HTML export on disk alongside the PDFs next to it. No head-to-head was run for this page.

## Pagefind is very good at its job

Pagefind runs after your static site generator and writes a search bundle into the built output. Its documentation is explicit that Pagefind itself has no server component, and the search integration is baked into the site.

The index is split into chunks so a browser loads only a subset. Pagefind reports a full-text search on a 10,000 page site with a total network payload under 300kB, and closer to 100kB for most sites.

Nothing on this page argues with any of that. If you are shipping a documentation site and you want the reader to search it, use Pagefind.

## The two tools answer different questions

Pagefind answers a reader typing in a browser on a site you built. XERJ answers an agent asking about a folder you saved.

That second folder is usually not a built site. It is a Confluence export, a Notion export, a saved help center, or a folder somebody handed you. The HTML sits next to PDFs, spreadsheets exported as CSV, and sometimes a SQLite file.

Pagefind discovers files with a glob that defaults to HTML only. Content in any other format enters through `addCustomRecord`, where you supply text you already extracted. Pagefind ships no PDF or DOCX parser, and its documentation does not claim one.

## What the local index does with the same folder

One command indexes the saved HTML and every other file sitting beside it, with no glob to configure and no extraction step to write yourself.

```sh
xerj autoindex ./site-export --prefix site --state-dir ./state-site
```

The command reads a content signature rather than the file extension. It infers field types, writes explicit mappings, and files what it learned in a catalog index.

Thirteen families are covered. The list holds HTML, plain text, PDF, DOCX and CSV. It also holds JSON and JSONL, structured logs, SQL exports, SQLite, XML, YAML, code and gzip variants.

The result is queryable over the Elasticsearch REST API, and `xerj mcp` serves 10 tools to an agent over stdio. A `match_phrase` query returns the file path and the matching passage rather than the whole page.

## What XERJ is not

XERJ is not a static-site plugin. It ships no browser bundle, no client-side index and no ready-made search field for a website.

Putting XERJ behind a public site would mean running a single-node process and exposing it, which is the opposite of what Pagefind was built to avoid. Do not read this page as a suggestion to do that.

XERJ also does no optical character recognition. A page image with no text layer stays junk until a separate tool gives it a text layer.

## The limits you inherit with XERJ

XERJ is single-node only. There is no data-plane replication and no failover, so one host is the whole deployment.

The server retains heap for every document it indexes, which is an open tracked defect. Corpora past a few million documents can exhaust memory on one node.

The default embedder is lexical feature hashing, not neural. Matching a differently worded question needs `--embed-mode neural`, which is opt-in and CPU-only.

## When to choose Pagefind instead

Choose Pagefind whenever a person will search a site you build. That is its job, and it needs no infrastructure.

Choose Pagefind when the search must work from static hosting. A chunked index in the browser saves the bandwidth and the process you would otherwise run.

Choose Pagefind when your content is already HTML. No other format is in its path.

## When to choose the local index instead

Choose XERJ when the folder is an export rather than a built site. The files beside the HTML are the reason.

Choose XERJ when the caller is an agent. The answer comes back as a cited passage with a file path.

Choose XERJ when the same folder holds a CSV, a SQL export or a SQLite file. Pagefind was never meant to read those.

## What was not measured

No head-to-head was run for this page. There is no timing here and no recall figure, because no Pagefind index was built to produce one.

Every Pagefind fact above comes from Pagefind's own documentation. Read it before you decide.
