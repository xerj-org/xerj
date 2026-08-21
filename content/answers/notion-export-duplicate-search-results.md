---
title: "Why does my Notion export duplicate titles?"
h1: "Why does my Notion export duplicate titles?"
description: "A captured run indexed a Notion Markdown export twice and got the same 8 documents with byte-identical ids. The duplicates come from the export layout."
slug: "notion-export-duplicate-search-results"
cluster: "Files and formats"
question: "API sync/export duplicates Notion titles in pages, databases, search results"
intent: "troubleshooting"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, unzip a Notion Markdown and CSV workspace export, run `xerj autoindex ./notion-export --url http://127.0.0.1:9200 --prefix nt --progress plain` twice, POST a match_all sorted by ax_path and ax_locator after each run, compare the returned _id lists byte for byte, and report every file the autoindex-catalog marked junk with its reason."
commands:
  - cmd: "xerj autoindex ./notion-export --url http://127.0.0.1:9200 --prefix nt --progress plain"
    note: "Index the unzipped Notion export. Run the same line twice to test for duplicates."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/nt-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_all\":{}},\"size\":100,\"_source\":[\"ax_path\",\"ax_format\",\"ax_locator\"],\"sort\":[{\"ax_path\":\"asc\"},{\"ax_locator\":\"asc\"}],\"track_total_hits\":true}'"
    note: "List every document with its path and locator, in a stable order, after each run."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/nt-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"meerkat quarterly roadmap\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\"],\"track_total_hits\":true}'"
    note: "Find a phrase that Notion exported into 2 different pages."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{\"query\":{\"term\":{\"doc_kind\":\"file\"}},\"size\":500,\"_source\":[\"path\",\"format\",\"status\",\"records\",\"reason\"],\"sort\":[{\"path\":\"asc\"}]}'"
    note: "List every exported file with its family, status and refusal reason."
links_out:
  - "index-markdown-into-elasticsearch-api"
  - "catalog-files-with-autoindex-map"
  - "resume-interrupted-autoindex-run"
faq:
  - q: "Does indexing a Notion export twice create duplicates?"
    a: "No. The captured run indexed the same export twice and got 8 documents both times, with byte-identical ids and nothing added."
  - q: "Why do I see 2 results for the same Markdown page?"
    a: "Each Markdown file produced 2 documents: one whole-file document with `ax_locator` `file`, and one passage document with `ax_locator` `s0`."
  - q: "Why does one title appear in 2 different paths?"
    a: "Notion writes a page as `<Title> <32-hex>.md` and its children into a sibling `<Title> <32-hex>/` directory. Text copied into both pages therefore matches twice."
  - q: "Why did my Notion database rows return nothing?"
    a: "All 3 database row pages were classified as `yaml` and refused with 0 documents. A prose file whose non-blank lines mostly read as `key: value` can be dropped this way."
  - q: "How do I collapse duplicate hits into 1 page?"
    a: "Group the returned hits on `ax_path` in your client. The captured listing query sorts on `ax_path` and then `ax_locator`, which makes the pairs visible."
  - q: "Does the CSV that Notion writes for a database work?"
    a: "Yes. The captured run indexed `Decisions <id>_all.csv` as `csv` with 4 documents: 1 whole-file document and 3 row documents."
  - q: "Where do these results come from?"
    a: "All results come from run RUN-B, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host."
---

**TL;DR** — The duplicate titles come from the export layout and from the 2 documents XERJ writes per Markdown file. Re-indexing is not the cause. A captured run indexed the same export twice and returned 8 documents both times, with byte-identical ids.

## The export layout repeats the title

Notion writes a page as `<Title> <32-hex>.md` and puts its children into a sibling directory named `<Title> <32-hex>/`. The same title text therefore appears in a file name and in a directory name for one page.

A phrase that Notion copied into both the parent page and a child page returned 2 hits, at `Engineering Handbook a1b2c3d4e5f60718293a4b5c6d7e8f90.md` and at `Engineering Handbook .../Runbooks a1b2c3d4e5f60718293a4b5c6d7e8f90.md`. Both hits are correct. The text is genuinely in 2 exported files.

## Each Markdown file produces 2 documents

XERJ writes 1 whole-file document plus 1 document per passage. For a short Notion page that is a pair, and both documents carry the same `ax_path` and the same title text.

| `ax_path` | `ax_locator` | `ax_format` |
| --- | --- | --- |
| `Engineering Handbook a1b2c3d4e5f60718293a4b5c6d7e8f90.md` | `file` | `txt-prose` |
| `Engineering Handbook a1b2c3d4e5f60718293a4b5c6d7e8f90.md` | `s0` | `txt-prose` |
| `Engineering Handbook .../Runbooks a1b2c3d4e5f60718293a4b5c6d7e8f90.md` | `file` | `txt-prose` |
| `Engineering Handbook .../Runbooks a1b2c3d4e5f60718293a4b5c6d7e8f90.md` | `s0` | `txt-prose` |

Group the hits on `ax_path` in your client if you want 1 result per page. Sort on `ax_path` and then `ax_locator` to make the pairs visible.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/nt-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_all":{}},"size":100,"_source":["ax_path","ax_format","ax_locator"],"sort":[{"ax_path":"asc"},{"ax_locator":"asc"}],"track_total_hits":true}'
```

## Re-indexing is idempotent

The captured run indexed one Notion Markdown and CSV export, then indexed the same folder again with the same command. Both runs returned 8 documents. Every `_id` matched byte for byte, and the second run added nothing.

The second run reported `files=0 records=8` at the terminal. XERJ recognized every file as unchanged and submitted no new source documents.

```sh
xerj autoindex ./notion-export --url http://127.0.0.1:9200 --prefix nt --progress plain
```

## Watch for database pages refused as YAML

If most of an exported page's non-blank lines read as YAML — a `key: value` line, or a line opening with `- ` — the family sniffer classifies the page as `yaml`. Notion writes a database row page as a title over `Status:`, `Owner:` and `Decided:` lines, which is exactly that shape. YAML parsing then fails and XERJ drops the file with 0 documents. There is no fallback to prose, and the only terminal signal is a junk file count. A `---` front-matter block at the top of a Markdown file does not trigger this on its own: a file that opens with front matter is classified by the body under it.

All 3 database row pages under `Decisions 0f1e2d3c4b5a69788796a5b4c3d2e1f0/` were refused this way, with the reason `no records extracted (yaml candidate family, 1 junk lines)`. The run reported `junk_files=3`.

A minimal 3-file reproduction shows the same behavior outside Notion. XERJ indexed `prose-no-colons.txt` as `txt-prose`. It gave `speaker-colons.txt` and `markdown-bullets.md` the `yaml` family and dropped both.

## The database CSV still works

Notion also writes each database to `<Name> <id>_all.csv`. XERJ gave that file the `csv` family and produced 4 documents: 1 whole-file document and 3 row documents with locators `r0`, `r1` and `r2`.

The CSV therefore carries the rows that the refused Markdown pages lost. Read `autoindex-catalog` after every run and compare the file count with the document count before you report either.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/autoindex-catalog/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"term":{"doc_kind":"file"}},"size":500,"_source":["path","format","status","records","reason"],"sort":[{"path":"asc"}]}'
```

## What this capture does not show

The export folder in this run came from the fixture generator, written to Notion's own file naming. Notion never ran on the host, and no workspace was connected. XERJ has no Notion connector and fetches nothing over the network.

XERJ runs single-node here, with no replication and no failover. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-B, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
