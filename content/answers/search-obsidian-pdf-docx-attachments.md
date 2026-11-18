---
title: "Search markdown notes and PDFs in one vault"
h1: "What's the best way to search markdown notes and PDFs in the same vault?"
description: "A captured XERJ run indexed a vault of markdown notes and returned text from inside a PDF attachment and a DOCX attachment, plus wikilink edges between the notes."
slug: "search-obsidian-pdf-docx-attachments"
cluster: "Files and formats"
question: "What's the best way to search markdown notes and PDFs in the same vault?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./vault --url http://127.0.0.1:9200 --prefix ob --progress plain`, then POST a match_phrase on body for a string that exists only inside a PDF attachment and a second one for a string only inside a DOCX attachment, read the autoindex-catalog for any file whose status is junk, and report the hit paths and the junk reasons."
answer: "What's the best way to search Markdown notes and PDFs in the same vault? Run xerj autoindex on the vault folder. It reads attachments too. In a captured run a phrase found only inside a PDF attachment returned 1 hit, a phrase only inside a DOCX returned 1 hit, and wikilinks became edges carrying the line that made them."
key_fact: "Omnisearch, the in-app alternative, has 2,189 GitHub stars and released 1.32.0 on 2026-10-24."
key_fact_source: "https://github.com/scambier/obsidian-omnisearch"
key_fact_date: "2026-10-30"
intent_group: "logs and docs"
queries:
  - "search markdown notes MCP server"
commands:
  - cmd: "xerj autoindex ./vault --url http://127.0.0.1:9200 --prefix ob --progress plain"
    note: "Index a vault folder, including the attachments folder beside the notes."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/ob-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"pangolin escalation matrix\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_format\",\"ax_locator\",\"title\"],\"track_total_hits\":true}'"
    note: "Find a phrase that exists only inside the PDF attachment."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/.xerj-memory-amber-obsidian-edges/_search -H 'content-type: application/json' -d '{\"query\":{\"exists\":{\"field\":\"src\"}},\"size\":50,\"track_total_hits\":true}'"
    note: "Read every edge the vault produced, with the detector and the evidence quote."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{\"query\":{\"term\":{\"doc_kind\":\"file\"}},\"size\":500,\"_source\":[\"path\",\"format\",\"status\",\"records\",\"reason\"],\"sort\":[{\"path\":\"asc\"}]}'"
    note: "List every file the run read, with the family it was given and why any file was refused."
links_out:
  - "search-all-pdfs-in-a-folder"
  - "search-word-documents-in-a-folder"
  - "index-markdown-into-elasticsearch-api"
  - "give-chatgpt-claude-local-file-access"
  - "/compare/xerj-vs-obsidian-omnisearch"
evidence:
  - claim: "A file gets the yaml family when 60% or more of its non-blank lines read as YAML (yaml_line_ratio >= 0.6) and the text parses as a YAML stream; the 3 of 4 example is 75%."
    source: "engine/crates/xerj-autoindex/src/sniff.rs"
faq:
  - q: "What's the best way to search markdown notes and PDFs in the same vault?"
    a: "Index the vault folder itself, notes and attachments together. In the captured run a phrase that exists only inside `pangolin-escalation-matrix.pdf` returned 1 hit at that path."
  - q: "Can I search an Obsidian vault including PDFs and Word attachments, not just notes?"
    a: "Yes. A phrase that exists only inside `capybara-change-window.docx` returned 1 hit at that path, and the DOCX extractor produced 1 document from the file."
  - q: "I have a pile of markdown notes. How do I search them properly?"
    a: "Point `xerj autoindex` at the folder and query the index it creates. Wikilinks between notes also become edges, and each `wikilink` edge carries the line of text that created it."
  - q: "How do I search notes and attached PDFs as one library?"
    a: "One run over the vault root covers both. Every hit carries `ax_path` and `ax_locator`, so a match inside an attachment names the file and the position, such as `p1-s0`."
  - q: "Does this replace Omnisearch inside Obsidian?"
    a: "No. Omnisearch is a plugin for a person working inside the app, and it keeps that job. XERJ indexes the vault folder from outside so an agent can query it without the app open."
  - q: "Why did one of my notes return no results?"
    a: "A note whose non-blank lines mostly read as YAML, such as `key: value` lines or `- ` bullets, can be classified as `yaml` and refused with 0 documents. Front matter on its own no longer does it. Read the `autoindex-catalog` for the reason string."
  - q: "Does XERJ connect to Obsidian itself?"
    a: "No. XERJ reads the vault folder from local disk. The captured node opened 0 non-loopback connections over its whole life, so no application or service was contacted."
---

**TL;DR** Run `xerj autoindex` on the vault folder. It reads the attachments beside the notes. A phrase that exists only inside a PDF or a DOCX comes back as a hit at that file. Wikilinks become edges that carry the line that made them.

```visual
obsidian-vault-run
```

## Index the vault folder

The first command above indexes the whole vault, notes and attachments together. Dataset names are inferred from the folder, so read the index list the run prints. A query against `ob-*` covers whatever it created.

The captured vault was written by the fixture generator `gen_fixtures.py` in the real on-disk layout. Obsidian never ran on the host, so this tests the file layout, not the app.

## Attachment text comes back with a locator

| Query | Hits | Path returned | `ax_format` |
| --- | --- | --- | --- |
| `match_phrase` on `body` for `pangolin escalation matrix` | 1 | `attachments/pangolin-escalation-matrix.pdf` | `pdf` |
| `match_phrase` on `body` for `capybara change window` | 1 | `attachments/capybara-change-window.docx` | `docx` |

Each phrase exists in exactly one binary file and nowhere in the notes. The PDF hit carries `ax_locator` `p1-s0`, page 1, passage 0, so an agent can open the file at the right place instead of reading all of it. The second command above is the PDF query.

## Wikilinks become edges

The edges land in `.xerj-memory-amber-obsidian-edges`, and the third command above reads them. Every `wikilink` edge carries an `evidence.quote`. The detectors are structural: they match wikilinks, Markdown links, shared folders and path citations, and none reads meaning from the text.

## A note that reads as YAML can be refused

The sniffer picks a family from content, never from the extension. A note that opens with `---` front matter is classified by the body under it, so ordinary Markdown with `title:` and `tags:` is indexed as prose.

A note whose lines mostly read as YAML is different. When 60 percent or more of its non-blank lines read as YAML and the text also parses as YAML, it lands in the `yaml` family. The YAML extractor got nothing from such a note, and it was dropped with 0 documents. Try a note of your own:

```visual
obsidian-yaml-check
```

In this vault, `Index.md` was refused with the reason `no records extracted (yaml candidate family, 1 junk lines)`. The only signal at the terminal was `junk_files=1`, so read `autoindex-catalog` after every run. The last command above lists every file with its family and any refusal reason.

## XERJ or Omnisearch

Omnisearch is a plugin for a person searching inside Obsidian, and it keeps that job. XERJ indexes the folder from outside, so an agent can query it over HTTP or MCP with the app closed. The [Omnisearch comparison](/compare/xerj-vs-obsidian-omnisearch) covers the trade.

## What this capture does not show

No Obsidian app, plugin or sync service was involved. XERJ has no connector and fetches nothing over the network, so copy or mount the vault to local disk first.

XERJ ran single-node, with no replication and no failover. The default embedder is lexical feature hashing, so a paraphrase that shares no words with the query does not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number here comes from RUN-B, captured on 2026-08-21 with a `ci-test` profile build, so no wall-clock figure is published as a performance number.
