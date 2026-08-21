---
title: "Search .ipynb notebooks and the .py files beside them"
h1: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files and the .py next to them?"
description: "A captured run indexed one document per notebook cell, with cell_type, execution_count and the nbformat cell id, so a hit names the exact cell."
slug: "search-jupyter-notebook-cells"
cluster: "Files and formats"
question: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain`, then POST a match_phrase on source for text that exists only in a markdown cell, a match on source for a symbol defined only in a code cell, and a match on outputs for text printed only by an executed cell, and report ax_path, ax_locator, cell_type, id and execution_count for every hit."
commands:
  - cmd: "xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain"
    note: "Index a folder of .ipynb files from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jp-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"source\":\"okapi_transform\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_locator\",\"cell_type\",\"id\",\"execution_count\"],\"track_total_hits\":true}'"
    note: "Find a symbol that exists only in a code cell, and get the cell locator back."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jp-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"outputs\":\"okapi accuracy 0.9137\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_locator\",\"cell_type\",\"id\",\"execution_count\",\"outputs\"],\"track_total_hits\":true}'"
    note: "Find text that exists only in an executed cell's stored output."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/jp-json/_mapping"
    note: "Read the 25 fields a notebook cell produces, with the type of each."
links_out:
  - "search-json-and-jsonl-logs"
  - "local-embeddings-without-openai-api"
  - "give-chatgpt-claude-local-file-access"
faq:
  - q: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files?"
    a: "Index the folder and query `source` for the function name. XERJ writes 1 document per cell, so a hit returns `ax_locator` such as `cells:e3` and the nbformat cell `id`."
  - q: "How do I search text inside Jupyter notebooks and jump to the cell?"
    a: "Query `source` for the text and read the returned `id`. The nbformat cell id, for example `cell-abcd0001`, is stable inside the file and identifies the cell without a line number."
  - q: "How do I search notebooks and scripts as one project?"
    a: "Point one `autoindex` run at the folder that holds both. Notebook cells are parsed as JSON and `.py` files are parsed as source code, so one index pattern covers both, with cells matched on `source` and scripts on `defs`."
  - q: "Does XERJ search executed cell output as well as code?"
    a: "Yes. Text printed only by an executed cell returned 1 hit from the `outputs` field, at `ax_locator` `cells:e3` with `execution_count` 2."
  - q: "Is XERJ notebook-aware?"
    a: "No. XERJ gives an `.ipynb` file the `json` family. The per-cell result follows from nbformat storing cells as a JSON array, not from a notebook extractor."
  - q: "Which notebook fields can I filter on?"
    a: "A cell produces 25 fields, including `cell_type` as `keyword`, `execution_count` as `long`, `id` as `keyword`, and `source` and `outputs` as `text`."
  - q: "Do markdown cells and code cells end up in the same index?"
    a: "Yes. Both live in the same index and are separated by the `cell_type` keyword. The captured run put 11 documents from 2 notebooks into `jp-json`."
---

**TL;DR** — XERJ writes 1 document per notebook cell. In a captured run, a hit returned `ax_locator` `cells:e3`, the nbformat cell `id` `cell-abcd0004`, `cell_type` `code` and `execution_count` 2. Markdown text, code symbols and executed cell output are each queryable on their own field.

## Index the notebook folder

`xerj autoindex <folder>` reads each `.ipynb` file as JSON. The captured run read 2 notebooks into 1 dataset and 11 documents live in `jp-json`, with 0 junk files.

```sh
xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain
```

The 2 notebooks came from the fixture generator, written to nbformat 4.5 with markdown cells, code cells and executed output cells. No notebook server ran on the host.

## The unit of extraction is the cell

Each cell became its own document. A hit therefore names the notebook, the cell position and the cell identity. That is enough to open the right place.

| field | example value | type |
| --- | --- | --- |
| `ax_path` | `okapi-retrieval-sweep.ipynb` | `keyword` |
| `ax_locator` | `cells:e3` | `keyword` |
| `id` | `cell-abcd0004` | `keyword` |
| `cell_type` | `code` | `keyword` |
| `execution_count` | `2` | `long` |

The `id` value is the nbformat cell id from the file itself. Use it to jump to the cell, because it does not move when a neighboring cell grows.

## Markdown, code and output are separately queryable

Three queries ran against the same index, and each one matched a different kind of cell content.

| query | hits | what matched |
| --- | --- | --- |
| `match_phrase` on `source` for `okapi experiment notes` | 2 | markdown cells `cells:e0` and `cells:e6` |
| `match` on `source` for `okapi_transform` | 4 | code cells with `execution_count` 2, 3 and 4, plus 1 markdown cell |
| `match` on `outputs` for `okapi accuracy 0.9137` | 1 | code cell `cells:e3`, from stored output only |

```sh
curl -s -XPOST 'http://127.0.0.1:9200/jp-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"outputs":"okapi accuracy 0.9137"}},"size":5,"_source":["ax_path","ax_locator","cell_type","id","execution_count","outputs"],"track_total_hits":true}'
```

The output hit matters most. A printed accuracy number lives only in the stored output of an executed cell. A plain file search finds that string inside the raw JSON, but it cannot name the cell that produced it.

## XERJ is not notebook-aware

The `autoindex-catalog` gave both `.ipynb` files the `json` family. There is no notebook extractor, no kernel, and no execution.

The per-cell result follows from the file format. The nbformat schema stores cells as a JSON array. The JSON family splits on that array and carries each cell's own keys through as fields.

Notebook metadata comes through the same way. The captured mapping holds `nbformat` and `nbformat_minor`. It also holds 10 flattened `metadata_kernelspec_*` and `metadata_language_info_*` keyword fields.

## The `outputs` field holds serialized JSON

`outputs` is a `text` field that carries the whole output list as a string. The captured value was `[{"name":"stdout","output_type":"stream","text":["okapi accuracy 0.9137\n","corpus labelled-20\n"]}]`.

Full-text queries work on that string. A structured filter, for example on `output_type`, needs a client-side parse of the field.

## What this capture does not show

Only 2 notebooks were indexed, so this run demonstrates the extraction unit and the locator rather than notebook-scale behavior. XERJ does not run, render or convert a notebook, and it fetched nothing over the network.

XERJ runs single-node here, with no replication and no failover. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-B, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from this run is published as a performance number.
