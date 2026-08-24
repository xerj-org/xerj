---
title: "How do I index Markdown into Elasticsearch?"
h1: "How do I index Markdown into the Elasticsearch API?"
description: "XERJ autoindex indexes Markdown as the txt-prose family and answers the Elasticsearch API, but it maps no headings array and keeps the hash in the title."
slug: "index-markdown-into-elasticsearch-api"
cluster: "Files and formats"
question: "What is the best way to index Markdown documents into Elasticsearch?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex ./markdown --prefix md, read GET /md-*/_mapping, then POST a match query with a highlight to /md-docs/_search and report that the mapping has no headings field and that title keeps its leading hash character."
commands:
  - cmd: "xerj autoindex ./markdown --url http://127.0.0.1:9410 --prefix md --state-dir ./state-md --progress plain --disable-feedback"
    note: "Index a Markdown folder into the prefix md."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9410/md-*/_mapping'"
    note: "Read the 9 fields XERJ mapped for Markdown."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/md-docs/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"replay the checkpoint journal\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_format\",\"title\"],\"highlight\":{\"fields\":{\"body\":{}}},\"track_total_hits\":true}'"
    note: "Run one Elasticsearch-shaped search with a highlight."
links_out:
  - "search-file-contents-in-a-folder"
  - "search-obsidian-pdf-docx-attachments"
  - "give-chatgpt-claude-local-file-access"
  - "/docs/api-es-compat"
faq:
  - q: "Does XERJ have a Markdown format?"
    a: "No. Markdown lands in the `txt-prose` family. Our capture detected all 6 Markdown documents as `txt-prose` and created no Markdown-specific field."
  - q: "Does XERJ extract Markdown headings into a field?"
    a: "No. The captured `md-docs` mapping has no `headings` field, and the run recorded that as a failed expectation. The HTML family does produce one."
  - q: "Why does the title start with a hash character?"
    a: "XERJ takes the raw first line of the file as the title. In our capture `01-runbook.md` returned the title `# Runbook`, hash included."
  - q: "Which fields does a Markdown index have?"
    a: "Nine: `body` as `semantic_text`, `title` as `keyword`, and the 7 `ax_*` provenance fields as `keyword`. Nothing else is mapped."
  - q: "Can I query a Markdown index with an Elasticsearch client?"
    a: "Yes. XERJ answers `_mapping` and `_search` on the Elasticsearch-compatible port, and the captured response carried `hits.total`, `_score`, `_source` and `highlight`."
  - q: "How do I find text under one Markdown heading?"
    a: "Send a `match_phrase` query on `body` for the heading text. No `headings` field exists on a Markdown index, so the heading is body text."
---

**TL;DR** — XERJ indexes a Markdown folder with `xerj autoindex` and serves it on the Elasticsearch REST API. Markdown lands in the `txt-prose` family, not in a Markdown family. In our capture XERJ mapped 9 fields, extracted the title as the literal `# Runbook`, and produced no `headings` array.

## One command, an Elasticsearch-shaped index

`xerj autoindex` indexes a Markdown folder and creates the index, the mapping and the documents in one pass. XERJ then answers `_mapping` and `_search` on the Elasticsearch-compatible port, so the same client code works against the result.

The command below indexes 3 Markdown files into the prefix `md`.

```sh
xerj autoindex ./markdown --url http://127.0.0.1:9410 --prefix md --state-dir ./state-md --progress plain --disable-feedback
```

## What XERJ detects Markdown as

XERJ detected all 6 documents from the 3 Markdown files as the `txt-prose` family. Markdown is not a separate format in XERJ; the sniffer routes it through the plain-text path using heading and sentence heuristics.

Ask the node which family it chose with a terms aggregation on `ax_format`.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/md-*/_search' -H 'content-type: application/json' -d '{"size":0,"aggs":{"by_format":{"terms":{"field":"ax_format","size":10}}}}'
```

## The mapping XERJ produced

XERJ mapped 9 fields on `md-docs`: 7 `ax_*` provenance fields as `keyword`, `body` as `semantic_text`, and `title` as `keyword`. That mapping is the whole contract for a Markdown index.

| field | type |
| --- | --- |
| `body` | `semantic_text` |
| `title` | `keyword` |
| `ax_dataset`, `ax_file`, `ax_format`, `ax_locator`, `ax_path`, `ax_paths`, `ax_run` | `keyword` |

## The captured failure: no headings array

XERJ produced no `headings` field for Markdown, and the run recorded that as a failed expectation. The HTML family does produce one, so an equivalent heading becomes a field in HTML and stays inside `body` in Markdown.

The second half of the same finding is the title. XERJ took the raw first line, so `01-runbook.md` carries the title `# Runbook` with the hash character included.

Plan for both facts. Search headings with a phrase query on `body`, and strip a leading `#` yourself if you display `title`.

## One `_search` response

One `match` query on `body` returned 3 hits with BM25 scores and a highlight per hit. The response is Elasticsearch-shaped, so `hits.total.value`, `_index`, `_score`, `_source` and `highlight` are all where a client expects them.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/md-docs/_search' -H 'content-type: application/json' -d '{"query":{"match":{"body":"replay the checkpoint journal"}},"size":5,"_source":["ax_path","ax_format","title","headings","section"],"highlight":{"fields":{"body":{}}},"track_total_hits":true}'
```

The top hit in the capture looked like this.

```json
{"_index":"md-docs","_score":2.5890436,
 "_source":{"ax_format":"txt-prose","ax_path":"01-runbook.md","title":"# Runbook"},
 "highlight":{"body":["# Runbook\n\n## Restart procedure\n\nStop <em>the</em> node, replay the checkpoint journal, then start it again."]}}
```

## The embedder behind the body field

XERJ elects `body` as a `semantic_text` field, and the default embedder is lexical feature hashing. The neural embedder is opt-in through `--embed-mode neural`. A `match` query on a `semantic_text` field runs BM25, not kNN, so the query above is full-text search.

## What the capture covers

The capture indexed 3 Markdown files into 6 documents on 1 single-node XERJ process. This run covered no vault-scale folder and no wikilink graph extraction. Treat the field list and the failure as the result, not as a size claim.
