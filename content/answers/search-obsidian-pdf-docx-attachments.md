---
title: "Search markdown notes and PDFs in one vault"
h1: "What's the best way to search markdown notes and PDFs in the same vault?"
description: "A captured XERJ run indexed a vault of markdown notes and returned text from inside a PDF attachment and a DOCX attachment, plus wikilink edges between the notes."
slug: "search-obsidian-pdf-docx-attachments"
cluster: "Files and formats"
question: "What's the best way to search markdown notes and PDFs in the same vault?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./vault --url http://127.0.0.1:9200 --prefix ob --progress plain`, then POST a match_phrase on body for a string that exists only inside a PDF attachment and a second one for a string only inside a DOCX attachment, read the autoindex-catalog for any file whose status is junk, and report the hit paths and the junk reasons."
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
    a: "A note whose non-blank lines mostly read as YAML — `key: value` lines or `- ` bullets — can be classified as `yaml` and refused with 0 documents. Front matter on its own no longer does it. Read the `autoindex-catalog` for the reason string."
  - q: "Does XERJ connect to Obsidian itself?"
    a: "No. XERJ reads the vault folder from local disk. The captured node opened 0 non-loopback connections over its whole life, so no application or service was contacted."
---

**TL;DR** — XERJ `autoindex` reads a vault folder from local disk and extracts text from its attachments. In a captured run, a phrase that exists only inside `pangolin-escalation-matrix.pdf` returned 1 hit, and a phrase only inside `capybara-change-window.docx` returned 1 hit. Wikilinks between the notes became `wikilink` edges, each carrying the line that created it.

## What the captured run actually indexed

The captured run indexed a 7-file folder laid out as an Obsidian vault: Markdown notes, `[[wikilinks]]`, a `.obsidian/` directory, 1 PDF and 1 DOCX. The fixture generator `gen_fixtures.py` wrote that folder to the real on-disk vault layout. Obsidian never ran on the host, so this page tests the file layout and not the Obsidian application.

`xerj autoindex` walked 7 files, read 6 of them and pruned the `.obsidian/` directory as a hidden dotfile. Dataset names are inferred from the folder rather than fixed, so read the index list the run prints instead of guessing one; a query against `ob-*` covers whatever it created.

```sh
xerj autoindex ./vault --url http://127.0.0.1:9200 --prefix ob --progress plain
```

## Attachment text is searchable

Both attachments returned their own text, at their own path. Each phrase below exists in exactly one binary file and nowhere in the Markdown notes.

| query | hits | path returned | `ax_format` |
| --- | --- | --- | --- |
| `match_phrase` on `body` for `pangolin escalation matrix` | 1 | `attachments/pangolin-escalation-matrix.pdf` | `pdf` |
| `match_phrase` on `body` for `capybara change window` | 1 | `attachments/capybara-change-window.docx` | `docx` |

The PDF hit carries `ax_locator` `p1-s0`, which names page 1, passage 0. A locator of that shape lets an agent open the source file at the right place instead of reading the whole attachment.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/ob-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"pangolin escalation matrix"}},"size":5,"_source":["ax_path","ax_format","ax_locator","title"],"track_total_hits":true}'
```

## Wikilinks become edges that carry their own quote

The vault produced edges in `.xerj-memory-amber-obsidian-edges`, typed `same_dir`, `sequence`, `wikilink` and `pathcite`. Every `wikilink` edge carries an `evidence.quote`, so a reader can see the line that created each link.

Two of the captured quotes are `Owns [[Runbooks/Restart procedure]].` and `Escalate per ![[attachments/pangolin-escalation-matrix.pdf]] if the`. The second quote shows an embedded attachment link resolving to the PDF.

`xerj brain` holds a graph-shaped index over the indexed documents. The detectors are structural: they match wikilinks, Markdown links, shared directories and path citations. No detector reads meaning from the note text.

## A note that reads as YAML can be refused

The family sniffer decides on content and never on the file extension. A note that opens with a `---` front-matter block is now classified by the body under it. Ordinary Markdown carrying `title:` and `tags:` front matter is therefore indexed as the prose it is.

What still lands in the `yaml` family is a note whose lines mostly read as YAML. The sniffer counts a non-blank line as YAML-like when it matches `key: value` or starts with `- `. Once that share reaches 60 percent, the file goes to YAML. A Markdown bullet list counts towards the share; `- [ ]` and `- [x]` task items do not. YAML parsing then fails, the file is dropped with 0 documents, and no fallback to prose happens. Read the `autoindex-catalog` after every run.

In this vault, `Index.md` is one heading over 3 link bullets, so 3 of its 4 non-blank lines read as a YAML sequence. It was refused with the reason `no records extracted (yaml candidate family, 1 junk lines)`. That is 1 of the 7 files, and the only signal at the terminal was `junk_files=1`.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/autoindex-catalog/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"term":{"doc_kind":"file"}},"size":500,"_source":["path","format","status","records","reason"],"sort":[{"path":"asc"}]}'
```

## What this capture does not show

No Obsidian application, plugin or sync service was involved. XERJ has no connector and fetches nothing over the network, and the node opened 0 non-loopback connections across the whole run. Copy or mount the vault to local disk first, then index the folder.

XERJ runs single-node here, with no replication and no failover. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-B, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
