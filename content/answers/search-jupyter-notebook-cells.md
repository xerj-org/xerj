---
title: "Search .ipynb notebooks and the .py files beside them"
h1: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files and the .py next to them?"
description: "A captured run indexed 2 notebooks by heading section, with the heading path and starting cell, and the .py beside them, all in one index."
slug: "search-jupyter-notebook-cells"
cluster: "Files and formats"
question: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-10-05"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, run `xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain`, then POST a match on body for a function name, a match_phrase on body for text printed only by an executed cell, and report ax_path, ax_locator, heading, heading_path and cell for every hit."
commands:
  - cmd: "xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain"
    note: "Index a folder of .ipynb files, and the .py files beside them, from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jp-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"okapi_transform\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_locator\",\"heading\",\"heading_path\",\"cell\"],\"track_total_hits\":true}'"
    note: "Find a function in notebook sections and in the .py that defines it, and get the section and cell back."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/jp-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"okapi accuracy 0.9137\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_locator\",\"heading_path\",\"cell\"],\"track_total_hits\":true}'"
    note: "Find text that exists only in an executed cell's stored output."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/jp-docs/_mapping"
    note: "Read the fields a notebook section produces, with the type of each."
links_out:
  - "search-json-and-jsonl-logs"
  - "local-embeddings-without-openai-api"
  - "give-chatgpt-claude-local-file-access"
faq:
  - q: "I need to find a function I wrote in a notebook months ago. How do I search all .ipynb files?"
    a: "Index the folder and query `body` for the function name. XERJ writes 1 document per Markdown-heading section, so a hit returns the `heading`, the `heading_path` and the `cell` where the section starts."
  - q: "How do I search text inside Jupyter notebooks and jump to the cell?"
    a: "Query `body` and read `cell` from the hit. In the captured run, the printed accuracy came back from the section `Okapi retrieval sweep > Run the sweep`, which starts at cell 3."
  - q: "How do I search notebooks and scripts as one project?"
    a: "Point one `autoindex` run at the folder that holds both. In the captured run, the 2 notebooks and the `.py` beside them landed in 1 index, `jp-docs`, and one query for `okapi_transform` returned both notebooks and the `.py` file."
  - q: "Does XERJ search executed cell output as well as code?"
    a: "Yes, for text output. Printed text, `text/plain` results and errors are written into the section body after their cell. Image outputs and HTML-only outputs are not indexed."
  - q: "Is XERJ notebook-aware?"
    a: "Yes, from the release after v1.0.0-rc.82. XERJ recognizes a notebook by its content, not its extension, and groups cells by their Markdown headings. It reads any kernel and records the kernel language. It never executes a notebook."
  - q: "Which notebook fields can I filter on?"
    a: "`heading` and `language` are `keyword`, `cell` is `long`, and `heading_path` and `title` are `text`. `body` holds the Markdown, the code and the text outputs."
  - q: "Do markdown cells and code cells end up in the same document?"
    a: "Yes. A section holds every cell from its heading to the next heading, Markdown and code together, with the code fenced in the kernel language and each text output after its cell."
---

**TL;DR** — XERJ writes 1 document per Markdown-heading section of a notebook. In a captured run, the printed accuracy `okapi accuracy 0.9137` came back from the section `Okapi retrieval sweep > Run the sweep`, starting at cell 3, and a query for `okapi_transform` found both notebooks and the `.py` that defines it.

## Index the notebook folder

`xerj autoindex <folder>` recognizes each `.ipynb` file by its content. The captured run read 2 notebooks and 1 `.py` file into 1 dataset, `jp-docs`, with 11 documents and 0 junk files.

```sh
xerj autoindex ./notebooks --url http://127.0.0.1:9200 --prefix jp --progress plain
```

The 2 notebooks came from a fixture generator, written to nbformat 4.5 with Markdown cells, code cells, a printed output, an image output and an error. No notebook server ran on the host.

## The unit of extraction is the heading section

A section starts at each Markdown heading and holds every cell up to the next heading. A hit therefore names the notebook, the section and the cell where the section starts.

| field | example value | type |
| --- | --- | --- |
| `ax_path` | `okapi-retrieval-sweep.ipynb` | `keyword` |
| `ax_locator` | `c3-s0` | `keyword` |
| `heading` | `Run the sweep` | `keyword` |
| `heading_path` | `Okapi retrieval sweep > Run the sweep` | `text` |
| `cell` | `3` | `long` |
| `language` | `python` | `keyword` |

`title` comes from the notebook's first top-level heading. `language` comes from the kernel, so an R or Julia notebook says so.

A heading in the middle of a cell starts a section too. The second notebook opens with `# Evaluation notes` and has `## Findings` inside the same cell, so the captured run gave `Findings` the locator `c1.2-s0`.

## Markdown, code and output are in one body

The `Run the sweep` section holds 2 cells. Its `body` keeps the Markdown as written, then the code inside a `python` code fence, then the printed output in a plain code fence after the cell:

```text
okapi accuracy 0.9137
corpus labelled-20
```

Four queries ran against the same index:

| query | hits | what matched |
| --- | --- | --- |
| `match` on `body` for `okapi_transform` | 4 | 3 notebook sections, and the code document of `okapi_utils.py` |
| `match_phrase` on `body` for `okapi experiment notes` | 2 | the opening section of each notebook |
| `match_phrase` on `body` for `okapi accuracy 0.9137` | 1 | `Run the sweep`, cell 3, from the stored output only |
| `match_phrase` on `body` for `KeyError q-404` | 1 | `Okapi retrieval sweep > Plot the curve > A failing run`, cell 7 |

```sh
curl -s -XPOST 'http://127.0.0.1:9200/jp-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"okapi accuracy 0.9137"}},"size":5,"_source":["ax_path","ax_locator","heading_path","cell"],"track_total_hits":true}'
```

The output hit matters most. A printed accuracy number lives only in the stored output of an executed cell, and the hit names the section and the cell that produced it.

## What is not indexed

Image outputs are skipped, and so is the `text/plain` stand-in beside them, such as `<Figure size 640x480 with 1 Axes>`. HTML-only outputs are skipped too. In the captured run, queries for the figure stand-in, the image data and the HTML table each returned 0 hits.

Each text output is capped at 40 lines, so a long training log does not become the notebook. Terminal colors are stripped, and a progress bar keeps only its final state.

## What this capture does not show

Only 2 notebooks were indexed, so this run demonstrates the extraction unit and the locator rather than notebook-scale behavior. XERJ does not run, render or convert a notebook, and it fetched nothing over the network. R Markdown (`.Rmd`), Quarto (`.qmd`) and Jupytext or marimo `.py` notebooks are not read as notebooks.

XERJ runs single-node here, with no replication and no failover. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-C, captured on 2026-10-05 on a build of `main` after v1.0.0-rc.82 (commit `06185966`); notebook extraction is not in the v1.0.0-rc.82 release itself. The binary was a `quick` profile build, so no wall-clock figure from this run is published as a performance number.
