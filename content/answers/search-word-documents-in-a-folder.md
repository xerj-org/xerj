---
title: "How do I search a folder of Word contracts?"
h1: "How do I search through a folder of contracts in .docx?"
description: "Index a folder of DOCX contracts with one xerj autoindex command and search their paragraphs. A captured run also shows the decompression guard refusing a file."
slug: "search-word-documents-in-a-folder"
cluster: "Files and formats"
question: "How do I search through a folder of contracts in .docx?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./docx --url http://127.0.0.1:9200 --prefix docx --progress plain`, POST a match_phrase query to /docx-docs/_search for a phrase you expect inside a paragraph, and report the ax_path, the extracted title and the matching body text."
commands:
  - cmd: "xerj autoindex ./docx --url http://127.0.0.1:9200 --prefix docx --progress plain"
    note: "Index every DOCX file in the folder and print the terminal xerj-done line."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docx-docs/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"second reviewer\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_format\",\"title\",\"body\"]}'"
    note: "Search the paragraph text of every indexed Word document."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"doc_kind\":\"file\"}}]}},\"size\":100,\"_source\":[\"path\",\"format\",\"status\",\"reason\",\"records\"]}'"
    note: "Read the per-file verdict, including any DOCX that the guard refused."
links_out:
  - "search-file-contents-in-a-folder"
  - "search-all-pdfs-in-a-folder"
  - "search-obsidian-pdf-docx-attachments"
  - "/compare/xerj-vs-docfetcher"
faq:
  - q: "How do I search through a folder of contracts in .docx?"
    a: "Run `xerj autoindex` on the folder, then send a `match_phrase` query for the clause wording. XERJ reads the paragraph text of each DOCX file, so a clause is found wherever it sits."
  - q: "How do I search a folder of Word documents?"
    a: "Run `xerj autoindex` on the folder, then send a `match_phrase` query to the `_search` endpoint. XERJ reads the paragraph text of each DOCX file."
  - q: "How do I find a clause across a directory of Word contracts?"
    a: "Send the clause wording as a `match_phrase` against the index the run created. Each hit carries `ax_path` and the extracted `title`, so you know which contract it came from."
  - q: "Does XERJ extract the title of a Word document?"
    a: "Yes. In the captured run the `title` field held 'Change management policy', the first paragraph of the file, and the `body` field held the rest."
  - q: "Does XERJ index .doc files as well as .docx?"
    a: "The extractor reads the zipped OpenXML format that `.docx` uses. Convert a legacy `.doc` file to `.docx` before you index the folder."
  - q: "What stops a malicious Word document?"
    a: "A decompression guard set at 72 MiB and 20,000 paragraphs. A file past either limit produces 0 documents and lands in the catalog as junk."
  - q: "Why did a small DOCX produce no documents?"
    a: "A small DOCX can expand to hundreds of megabytes when unzipped. The captured 245,109-byte file expands to about 96 MiB and the guard refused it."
---

**TL;DR** — `xerj autoindex` indexes a folder of Word documents in one command, and a `match_phrase` query then searches their paragraphs. In a captured run, 1 DOCX file produced 2 documents, and a query for `second reviewer` returned the paragraph and the title. A second, deliberately expanded file produced 0 documents.

## Index the folder in one command

`xerj autoindex <folder>` unzips each DOCX file, reads its paragraph text, and writes the result to a XERJ node. The command needs no mapping and no conversion step.

```sh
xerj autoindex ./docx --url http://127.0.0.1:9200 --prefix docx --progress plain
```

The captured run over 1 Word document created 1 index, `docx-docs`, holding 2 documents. XERJ detects the DOCX family from file content, so a renamed file still parses correctly.

```text
xerj-done ok=true exit=0 reason=completed wall=0.1s files=1 records=2 datasets=1 junk_files=0
```

## A query returns the paragraph and the title

One `match_phrase` query for `second reviewer` returned 1 hit, with the extracted title, the source path and the paragraph text in `_source`. Word documents land in the document family, which carries `title`, `headings`, `section` and `body`.

```sh
curl -s -XPOST 'http://127.0.0.1:9200/docx-docs/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"second reviewer"}},"size":5,"_source":["ax_path","ax_format","title","body"]}'
```

The single hit below comes from `raw/docx-search.json`, with the body text unedited.

```json
{
  "ax_format": "docx",
  "ax_path": "policy-note.docx",
  "title": "Change management policy",
  "body": "Change management policy\n\nEvery change to the checkpoint journal needs a second reviewer.\n\nA quokka named Bandicoot signs off the weekly rota.\n\nRollback is always the first option considered."
}
```

The first paragraph became the `title`, and the whole paragraph sequence became the `body`. Query `body` for text inside the document, Use `title` when you want the name a human reader recognizes.

## The decompression guard fires on a small file

A DOCX file is a zip archive, so a 245,109-byte file on disk can hold a `word/document.xml` that expands to about 96 MiB. XERJ refuses such a file rather than allocating for it. The captured guard fixture produced 0 documents.

```text
{
  "format": "docx",
  "path": "expanded.docx",
  "reason": "no records extracted (docx candidate family, 1 junk lines)",
  "records": 0,
  "status": "junk"
}
```

The limits are 72 MiB of decompressed XML and 20,000 paragraphs. Before the guard existed, an 815 KB file measured 1.68 GB of resident memory, which is the failure the guard prevents.

## The reason string does not name the guard

The captured reason string is generic. The string reads `no records extracted (docx candidate family, 1 junk lines)` and never names the 72 MiB decompression limit. The catalog alone therefore does not tell an operator why XERJ refused the file.

Read the run summary next to the catalog entry. The guard run reported the line below. A run that reports both `files=0` and `junk_files=1` had a candidate DOCX file reach the extractor and produce nothing.

```text
xerj-done ok=true exit=3 reason=completed-with-junk wall=0.3s files=0 records=0 datasets=0 junk_files=1
```

We publish this as a finding, not as a feature. The guard does the correct thing, and the message it leaves behind is too generic to diagnose from.

## What the capture does not cover

The measurement is a single-node run of 2 small files on one host, so it shows behavior and not throughput. XERJ has no replication and no failover in this configuration.

The extractor reads the zipped OpenXML format that `.docx` uses. Convert a legacy `.doc` file to `.docx` first. XERJ has no extractor for `.xlsx`, `.pptx`, `.rtf` or `.odt`, and refuses a file in any of those formats rather than parsing part of it.

Ranking is BM25 over the extracted paragraph text. The default embedder in XERJ is lexical feature hashing and cannot connect a query to a synonym; neural embeddings are opt-in through `--embed-mode neural`.
