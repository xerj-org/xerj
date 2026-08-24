---
title: "What is XERJ?"
h1: "What is XERJ?"
description: "XERJ is an Apache-2.0 single Rust binary that indexes a local folder with one command and answers search, agent memory and MCP calls from the same process."
slug: "what-is-xerj"
cluster: "Product identity"
question: "What is XERJ?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent meeting this tool for the first time. Read https://xerj.org/llms.txt, start a node with `xerj --insecure --data-dir ./.xerj-data`, run `xerj autoindex ./docs --url http://127.0.0.1:9200 --prefix docs --progress plain` on the folder you were handed, run `xerj autoindex map --url http://127.0.0.1:9200` to learn the real index and field names, then POST one match_phrase query to /docs-*/_search and answer the user question citing the ax_path of every hit."
commands:
  - cmd: "xerj --insecure --data-dir ./.xerj-data"
    note: "Start a local development node. One process, no container and no JVM."
  - cmd: "xerj autoindex ./docs --url http://127.0.0.1:9200 --prefix docs --progress plain"
    note: "Make a folder searchable. The command takes no configuration file and no mapping."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docs-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"rotate the signing key\"}},\"size\":5,\"_source\":[\"ax_path\",\"ax_format\",\"title\"]}'"
    note: "Search the folder over the Elasticsearch REST API and read the file path back from every hit."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Attach the running node to an agent host over stdio MCP. This serves 10 tools and starts no node of its own."
links_out:
  - "how-xerj-autoindexes-a-folder"
  - "how-xerj-combines-search"
  - "search-engine-without-docker"
  - "/docs/recipes/agentic-memory"
  - "/docs/agents/endpoints"
evidence:
  - claim: "XERJ uses the Apache-2.0 license."
    source: "https://www.apache.org/licenses/LICENSE-2.0"
faq:
  - q: "What is XERJ?"
    a: "A single Rust binary that indexes local files and answers search queries over the Elasticsearch REST API. The same process also serves agent memory and an MCP tool surface."
  - q: "What do I actually type to get started?"
    a: "Two commands. Start a node with `xerj --insecure --data-dir ./.xerj-data`, then run `xerj autoindex ./docs` on the folder you want to search."
  - q: "Do I need Docker, a JVM or a config file to run it?"
    a: "No. XERJ is one native executable, and `xerj autoindex` takes no configuration file and no mapping."
  - q: "Can I point my existing Elasticsearch client at it?"
    a: "Yes. XERJ answers the Elasticsearch-8 wire protocol, so an existing client changes its base URL and nothing else."
  - q: "Is it free for commercial use?"
    a: "Yes. XERJ uses the Apache-2.0 license, which permits commercial use, modification and redistribution without a reciprocal source obligation."
  - q: "Does XERJ do semantic search out of the box?"
    a: "No. The default embedder is lexical feature hashing, which matches wording rather than meaning. A neural embedder is opt-in through `--embed-mode neural`."
  - q: "Can XERJ run across more than one machine?"
    a: "No. XERJ is single-node, with no replication, no sharding and no failover. One host is the whole deployment."
  - q: "Does XERJ crawl websites?"
    a: "No. XERJ reads files that are already on local disk. There is no URL input and no crawler."
---

**TL;DR** — XERJ is one Apache-2.0 Rust binary that indexes a local folder and answers search queries over the Elasticsearch REST API. `xerj autoindex` needs no schema and no configuration file. The same process serves agent memory at `/_memory/{namespace}` and 10 MCP tools through `xerj mcp`.

## Point it at a folder and query the folder

Two commands take a folder from nothing to searchable. Start the node, then hand `xerj autoindex` the directory.

```sh
xerj --insecure --data-dir ./.xerj-data
xerj autoindex ./docs --url http://127.0.0.1:9200 --prefix docs --progress plain
```

`xerj autoindex` detects each file family by content rather than by file extension, infers a dataset per file shape, and writes the documents. It takes no configuration file, no mapping and no schema.

`xerj autoindex map` then prints what the folder became: every dataset, its index name, its fields with their inferred types, and a ready-to-send query. An agent reads that map before it names an index in a query.

## Query it with an Elasticsearch client you already have

XERJ answers the Elasticsearch-8 wire protocol, so an existing Elasticsearch client changes its base URL and nothing else. A `match_phrase` query, a `term` filter on a keyword field and a `terms` aggregation all work over plain HTTP.

```sh
curl -s -XPOST http://127.0.0.1:9200/docs-*/_search \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"rotate the signing key"}},"size":5,"_source":["ax_path","ax_format","title"]}'
```

Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword provenance fields. A hit names the file it came from, so an agent can cite its source instead of guessing.

## Hand the same node to an agent

`xerj mcp` is a stdio MCP server inside the same binary. It serves 10 tools against a node that is already running: `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`, `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall` and four `xerj_brain_*` operations. The semantic tool runs whichever embedder the node loaded, and the default embedder is lexical feature hashing rather than a neural model.

The command does not start a node. Start one first, then register `xerj mcp --url http://127.0.0.1:9200` in the agent host with an absolute path to the binary.

## Store agent memory in the same process

Agent memory is an endpoint on the node, not a second deployment. An agent stores a fact with `POST /_memory/{namespace}` and reads it back with `_recall`, on the same port that answers `_search`.

Each namespace is an ordinary index named `.xerj-memory-{namespace}` under the node data directory, so memory outlives a restart. Two agents on one host keep separate memory by using separate namespaces.

## Which embedder actually runs

The default embedder is lexical feature hashing over word unigrams and character trigrams. It loads no model file and makes no network call, and it matches wording rather than meaning, so it cannot connect a query to a synonym.

The flag `--embed-mode neural` swaps in a MiniLM-class model that runs on the local CPU. That mode is opt-in, and a node reports the backend it loaded at `GET /v1/embedding/identity`.

## The license and the shape of the binary

XERJ uses the Apache-2.0 license and ships as one native executable. There is no container to pull, no JVM to install and no package manager step between the download and a running node.

| property | value |
| --- | --- |
| license | Apache-2.0 |
| distribution | one native binary |
| wire protocol | Elasticsearch-8 compatible |
| default embedder | lexical feature hashing |
| topology | single node |

## What XERJ is not

XERJ is single-node. There is no replication, no sharding and no failover, so one host is the whole deployment and a restore from a copy is the recovery plan.

XERJ has no OCR, so a PDF with no text layer produces no text. It has no email handler, no mbox handler and no archive handler. It reads no URL and downloads no web page. The files must already be on local disk.

XERJ has no graphical interface. There is no result window and no preview pane, because the intended caller is a program rather than a person at a desk.
