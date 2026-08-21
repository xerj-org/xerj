---
title: "Search a help-center you saved as HTML"
h1: "I saved a help-center as HTML. How do I search it like the real help-center search?"
description: "XERJ autoindex indexes a static HTML export from local disk, extracts title and a headings array per page, and fetched 0 non-loopback peers in our capture."
slug: "search-html-export"
cluster: "Files and formats"
question: "I saved a bunch of docs pages as HTML. How do I search that?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex ./html-export --prefix web on a folder of saved HTML pages, then POST a match query on body to /web-*/_search and report ax_path, title and the headings array for every hit without fetching any URL."
commands:
  - cmd: "xerj autoindex ./html-export --url http://127.0.0.1:9410 --prefix web --state-dir ./state-web --progress plain --disable-feedback"
    note: "Index a folder of saved HTML pages from local disk."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/web-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"checkpoint journal\"}},\"size\":10,\"_source\":[\"ax_path\",\"title\",\"headings\",\"section\"],\"track_total_hits\":true}'"
    note: "Search the page text and return the title and headings of each hit."
links_out:
  - "search-file-contents-in-a-folder"
  - "search-confluence-html-export"
  - "give-chatgpt-claude-local-file-access"
  - "/compare/xerj-vs-pagefind"
faq:
  - q: "I saved a bunch of docs pages as HTML. How do I search that?"
    a: "Index the folder from disk and send a `match` query on `body`. The HTML family produces `title`, `headings` and `body`, so a hit names the page and its section headings."
  - q: "How do I search my Confluence HTML export?"
    a: "The same way. XERJ reads the saved files from disk and fetches no URL, and the Confluence export article covers that space layout in detail."
  - q: "How do I search a wget of vendor docs?"
    a: "Point `autoindex` at the download directory. Every page becomes a document with `title`, `headings` and `body`, and one `match` query on `body` covers the whole mirror."
  - q: "Does site navigation drown the real hits?"
    a: "It can. Every saved page repeats its nav and footer text in `body`, so a common word matches many pages. Query a distinctive phrase, or read `title` and `headings` to keep the answer inside the article text."
  - q: "Does XERJ download the pages itself?"
    a: "No. XERJ indexes HTML files that are already on disk and fetches no URL. Our network watch observed 0 non-loopback peers during the run."
  - q: "Can I see which headings a page has?"
    a: "Yes. Ask for `headings` in `_source`. In our capture `recovery.html` returned `[\"Recovery procedure\", \"Overview\"]` in document order."
  - q: "Is the zero-network result a packet capture?"
    a: "No. The capture is a sampler at a 0.05 second interval, so a connection opening and closing inside one gap would be missed. The capture states that limit."
---

**TL;DR** — XERJ `autoindex` indexes a static HTML export straight from local disk. In our 3-page capture XERJ extracted a `title` and a `headings` array from each page, and one `match` query on `body` returned 3 hits. A network watch observed 0 non-loopback peers during the run.

## Index the export from disk

`xerj autoindex` indexes the HTML files that are already on disk and fetches no URL. XERJ has no fetcher and follows no `href`, so a saved site export is the input, not a live site.

The command below indexes a folder of 3 static pages.

```sh
xerj autoindex ./html-export --url http://127.0.0.1:9410 --prefix web --state-dir ./state-web --progress plain --disable-feedback
```

## Fields the HTML family produced

The HTML family produced `title`, `headings` and `body`, on top of the 7 `ax_*` provenance fields. The `headings` field is an array of the heading text in document order, so `recovery.html` carried `["Recovery procedure", "Overview"]`.

That field set distinguishes HTML from plain text in XERJ. A Markdown file lands in the `txt-prose` family and gets no `headings` array, as [the Markdown answer](/answers/index-markdown-into-elasticsearch-api) shows.

## The query and its 3 hits

One `match` query on `body` for `checkpoint journal` returned 3 hits, one per page, ranked by BM25. The captured `_source` carried `ax_path`, `title` and `headings` for every hit.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/web-*/_search' -H 'content-type: application/json' -d '{"query":{"match":{"body":"checkpoint journal"}},"size":10,"_source":["ax_path","title","headings","section"],"track_total_hits":true}'
```

| `ax_path` | `title` | `headings` | `_score` |
| --- | --- | --- | --- |
| `recovery.html` | Recovery procedure | `["Recovery procedure", "Overview"]` | `0.61400104` |
| `index.html` | Export index | `["Runbook export", "Overview"]` | `0.57417387` |
| `glossary.html` | Glossary | `["Glossary", "Overview"]` | `0.14597225` |

## Proof that no URL was fetched

A network watch over the XERJ node and the whole harness process tree observed 0 non-loopback peers for the whole run. The watch polled `/proc/net/tcp` and `/proc/net/tcp6` and cross-referenced the socket inodes of up to 4 process ids in the watched tree.

The method has one honest limit, and the capture states it. The watch is a sampler, not a packet capture, and it took 4 samples at a 0.05 second interval across 0.32 seconds. A connection that opens and closes inside one gap can escape the sampler.

## Limits worth knowing before you start

XERJ indexes files from a filesystem walk on 1 single-node process, so the export must exist on disk first. A hidden dotfile and a dotted directory are always skipped, and `--no-ignore` does not turn that off.

XERJ elects `body` as a `semantic_text` field on document datasets. The default embedder is lexical feature hashing, and the neural embedder is opt-in through `--embed-mode neural`. A `match` query on a `semantic_text` field runs BM25, not kNN.
