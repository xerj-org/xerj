---
title: "Find every place a config key is set in a repo"
h1: "How do I find every place a config key is set across YAML and XML in a repo?"
description: "XERJ autoindex indexes YAML and XML config files as full-text documents, maps 9 fields including body and title, and answers one bool query across both."
slug: "search-yaml-xml-config-repository"
cluster: "Files and formats"
question: "How do I find every place a config key is set across YAML and XML in a repo?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure on a scratch data directory, run xerj autoindex ./config --prefix cfg, read GET /cfg-*/_mapping to learn the real field names, then send one bool query over cfg-*/_search that matches both the YAML file and the XML file, and report ax_path and ax_format for every hit."
commands:
  - cmd: "xerj autoindex ./config --url http://127.0.0.1:9410 --prefix cfg --state-dir ./state-cfg --progress plain --disable-feedback"
    note: "Index a folder of YAML and XML config files."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9410/cfg-*/_mapping'"
    note: "Read every field XERJ mapped for the config files."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/cfg-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"should\":[{\"match\":{\"body\":\"Bandicoot\"}},{\"query_string\":{\"query\":\"tls_enabled OR tlsEnabled\"}}],\"minimum_should_match\":1}},\"size\":10,\"_source\":[\"ax_dataset\",\"ax_format\",\"ax_path\"],\"track_total_hits\":true}'"
    note: "Match the YAML file and the XML file with one bool query."
links_out:
  - "index-markdown-into-elasticsearch-api"
  - "syntax-aware-code-search-refactoring"
  - "search-openapi-spec-for-agent"
faq:
  - q: "How do I find every place a config key is set across YAML and XML in a repo?"
    a: "Index the tree, then search the key name inside `body`. Every hit carries `ax_path` and `ax_format`, so the answer is the list of files that mention the key."
  - q: "How do I search YAML and XML config across a big repo?"
    a: "Run `xerj autoindex` on the repo and send one bool query over the index pattern. In our capture that returned 2 hits, 1 `xml` document and 1 `yaml` document."
  - q: "How do I find a Helm value that is overridden in three files?"
    a: "Search the key name and read `ax_path` on each hit; the hits name every file that carries it. XERJ does not merge or resolve overrides, so ranking the files by precedence is your job."
  - q: "Does XERJ create one field per YAML key?"
    a: "No. The captured `cfg-docs` mapping held `body`, `title` and 7 provenance fields, and no key-derived field. Search the key name inside `body` instead."
  - q: "Which fields can I query on a config index?"
    a: "Query `body` for the file text, `title` for its name, and the 7 `ax_*` provenance fields for path, format, dataset and run. The captured `cfg-docs` mapping held exactly those 9 fields."
  - q: "Does XERJ read TOML and INI files?"
    a: "TOML and INI have no dedicated extractor. Both fall through to the text path, so the text stays searchable but no key becomes a field."
  - q: "How large a config file can XERJ index?"
    a: "The whole-file cap is 64 MiB and the longest line is 16 MiB. One document holds at most 512 fields, and one dataset holds at most 512 fields."
---

**TL;DR** — XERJ `autoindex` indexes a folder of YAML and XML config files with no schema file. In our capture 1 `cluster.yaml` and 1 `service.xml` became 4 documents in one `cfg-docs` index. One bool query returned 2 hits, one from each format.

## One command for both formats

`xerj autoindex` detects YAML and XML by content, not by file extension, and indexes both in one pass. XERJ grouped both files into a single index, `cfg-docs`, whose dataset slug is `docs`. The run needs no mapping file and no schema.

The command below indexes a folder that holds `cluster.yaml` and `service.xml`.

```sh
xerj autoindex ./config --url http://127.0.0.1:9410 --prefix cfg --state-dir ./state-cfg --progress plain --disable-feedback
```

## The fields XERJ mapped

XERJ mapped 9 fields on `cfg-docs`: 7 provenance fields, `body` as `semantic_text`, and `title` as `keyword`. The 7 `ax_*` fields are on every XERJ document, and each one is a `keyword`.

| field | type | what it holds |
| --- | --- | --- |
| `body` | `semantic_text` | the text of the config file |
| `title` | `keyword` | the document title XERJ extracted |
| `ax_path` | `keyword` | the path relative to the indexed folder |
| `ax_paths` | `keyword` | every current path for the same content |
| `ax_file` | `keyword` | the content key for the source file |
| `ax_format` | `keyword` | `yaml` or `xml` |
| `ax_dataset` | `keyword` | the inferred dataset slug, `docs` here |
| `ax_locator` | `keyword` | which part of the file the document came from |
| `ax_run` | `keyword` | the `autoindex` run that wrote the document |

Read the mapping from your own node with one request.

```sh
curl -s -XGET 'http://127.0.0.1:9410/cfg-*/_mapping'
```

## No field per config key

XERJ mapped no field per YAML key and no field per XML attribute in this capture. Keys such as `tls_enabled`, `tlsEnabled`, `retention_days` and `maxConnections` stayed searchable inside `body` only. Write queries against `body` and `title`, not against a flattened key path.

XERJ types a field from tabular input such as a CSV column or a JSON array. The 2 config files produced 4 documents in total, and no key from either file became a field.

## One bool query across both formats

One bool query with 2 `should` clauses returned 2 hits: 1 `xml` document and 1 `yaml` document, both scored `0.30869728`. The `query_string` clause carried both spellings of the same setting, `tls_enabled OR tlsEnabled`, because the YAML file and the XML file name it differently.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/cfg-*/_search' -H 'content-type: application/json' -d '{"query":{"bool":{"should":[{"match":{"body":"Bandicoot"}},{"query_string":{"query":"tls_enabled OR tlsEnabled"}}],"minimum_should_match":1}},"size":10,"_source":["ax_dataset","ax_format","ax_path"],"track_total_hits":true}'
```

The captured response held these 2 hits.

```json
{"hits":{"total":{"value":2,"relation":"eq"},"hits":[
 {"_index":"cfg-docs","_score":0.30869728,"_source":{"ax_dataset":"docs","ax_format":"xml","ax_path":"service.xml"}},
 {"_index":"cfg-docs","_score":0.30869728,"_source":{"ax_dataset":"docs","ax_format":"yaml","ax_path":"cluster.yaml"}}]}}
```

## The embedder behind the body field

XERJ elects `body` as a `semantic_text` field on document datasets. The default embedder is lexical feature hashing, and the neural embedder is opt-in through `--embed-mode neural`. A `match` query on a `semantic_text` field runs BM25, not kNN, so the bool query above is full-text search.

## What the capture does not show

The capture indexed 2 config files on 1 single-node XERJ process. This run measured no large config repository. Treat the field table as the shape of the result, not as a size claim or a timing claim.
