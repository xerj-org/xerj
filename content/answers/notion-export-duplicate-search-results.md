---
title: "Why does my Notion export duplicate titles?"
h1: "Why does my Notion export duplicate titles?"
description: "A captured run indexed a Notion Markdown export twice and got the same 8 documents with byte-identical ids. The duplicates come from the export layout."
slug: "notion-export-duplicate-search-results"
cluster: "Files and formats"
question: "API sync/export duplicates Notion titles in pages, databases, search results"
intent: "troubleshooting"
published: "2026-08-21"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, unzip a Notion Markdown and CSV workspace export, run `xerj autoindex ./notion-export --url http://127.0.0.1:9200 --prefix nt --progress plain` twice, POST a match_all sorted by ax_path and ax_locator after each run, compare the returned _id lists byte for byte, and report every file the autoindex-catalog marked junk with its reason."
answer: "Why does my Notion export duplicate titles in search results? The duplicates come from the export layout and from the 2 documents XERJ writes per Markdown file, not from re-indexing. A captured run indexed the same Notion export twice and returned 8 documents both times, with byte-identical ids, so the index never doubled."
key_fact: "Re-indexing the same Notion export a second time returned the same 8 documents, with byte-identical ids."
key_fact_source: "capture"
key_fact_date: "2026-08-21"
intent_group: "logs and docs"
queries:
  - "search markdown notes MCP server"
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

**TL;DR** The duplicates come from the export layout and from the 2 documents XERJ writes per Markdown file. Re-indexing is not the cause: the same export indexed twice returned 8 documents both times, with byte-identical ids.

```visual
notion-export-docs
```

## The export layout repeats the title

Notion writes a page as `<Title> <32-hex>.md` and its children into a sibling folder named `<Title> <32-hex>/`. One title then shows up in a file name and in a folder name.

In the run, a phrase that Notion copied into a parent page and a child page returned 2 hits, one in each file. Both hits are correct, because the text is in 2 exported files. The third command above is a search of that kind.

## Each Markdown page is 2 documents

XERJ writes 1 whole-file document plus 1 document per passage, so a short page is a pair with the same `ax_path`. Group on `ax_path` in your client for 1 result per page:

```visual
notion-group-hits
```

The second command above is the listing query, sorted on `ax_path` and then `ax_locator`.

## Re-indexing is idempotent

The run indexed the export, then indexed the same folder again with the same command. Both runs returned 8 documents, every `_id` matched byte for byte, and the second run added nothing. Its terminal line read `files=0 records=8`: every file was unchanged, so no new source documents went in.

## Database row pages can be refused as YAML

A Notion database row page is a title over `Status:`, `Owner:` and `Decided:` lines. When most non-blank lines of a file read as YAML, the sniffer gives it the `yaml` family. The YAML extractor got nothing from these pages, so each was dropped with 0 documents and no fallback to prose. Front matter alone does not do this, because a file that opens with `---` is classified by the body under it.

All 3 row pages were refused with the reason `no records extracted (yaml candidate family, 1 junk lines)`, and the terminal only showed `junk_files=3`. A 3-file reproduction outside Notion did the same: `prose-no-colons.txt` was indexed as `txt-prose`, and `speaker-colons.txt` and `markdown-bullets.md` were dropped as `yaml`.

This run predates a sniffer fix closed on 2026-10-04 ([issue #1142](https://github.com/xerj-org/xerj/issues/1142)). A file now also has to parse as a YAML stream before it gets the `yaml` family, so labeled prose that does not parse is indexed as prose. Re-run and read `autoindex-catalog` to see how your own row pages land.

## The database CSV keeps the rows

Notion also writes each database to `<Name> <id>_all.csv`. XERJ indexed it as `csv` with 4 documents: 1 whole-file document and 3 rows at `r0`, `r1` and `r2`. Read `autoindex-catalog` after every run with the last command above, and compare the file count with the document count before you report either.

## What this capture does not show

The export folder came from the fixture generator, written to Notion's own file naming. Notion never ran on the host and no workspace was connected. XERJ has no Notion connector and fetches nothing over the network.

XERJ ran single-node, with no replication and no failover. The default embedder is lexical feature hashing, so a paraphrase that shares no words with the query does not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number here comes from RUN-B, captured on 2026-08-21 on a 16-core AMD EPYC 9645 host with a `ci-test` profile build, so no wall-clock figure is published as a performance number.
