---
title: "XERJ vs Khoj for searching your own documents"
h1: "What's a simpler alternative to Khoj if I only need search and memory?"
description: "Khoj is a whole self-hosted assistant: chat, custom agents, scheduled research and image generation. XERJ is the smaller piece, the search and memory engine."
slug: "xerj-vs-khoj"
cluster: "Comparison: personal search"
question: "What's a simpler alternative to Khoj if I only need search and memory?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local node with `xerj --insecure --data-dir ./xerj-data`, run `xerj autoindex ./my-docs --prefix docs --state-dir ./state-docs`, register `xerj mcp --url http://127.0.0.1:9200` as an MCP server in the agent host you already run, then answer the user question with one xerj_search call and cite the ax_path of every hit. Report how many services are running when you finish."
commands:
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "One process. No database server and no model download are part of this step."
  - cmd: "xerj autoindex ./my-docs --prefix docs --state-dir ./state-docs"
    note: "PDF, DOCX, markdown and plain text are read where they sit."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Attach the running node to the agent you already run. This stdio MCP server serves 10 tools and starts no node of its own."
links_out:
  - "local-embeddings-without-openai-api"
  - "give-chatgpt-claude-local-file-access"
  - "search-file-contents-in-a-folder"
  - "agent-memory-without-postgres-qdrant"
evidence:
  - claim: "Khoj is an open-source personal application whose documented surface includes chat with local or online models, custom agents, scheduled research delivered by email, voice chat, image generation and clients for browser, Obsidian, Emacs, desktop, phone and WhatsApp."
    source: "https://docs.khoj.dev/features/all-features"
  - claim: "Khoj states that it understands Word, PDF, org-mode, markdown and plaintext files as well as Notion pages."
    source: "https://docs.khoj.dev/features/all-features"
  - claim: "Khoj search uses a bi-encoder to create meaning vectors and a slower cross-encoder to re-rank the candidates, and the models are configurable."
    source: "https://docs.khoj.dev/features/search"
  - claim: "Self-hosting Khoj is documented through Docker or pip, and the project stores its embeddings in Postgres using the pgvector extension."
    source: "https://docs.khoj.dev/get-started/setup"
  - claim: "Khoj image generation runs through third-party providers such as Replicate rather than on the local node."
    source: "https://docs.khoj.dev/features/image_generation"
  - claim: "Khoj is licensed under the GNU Affero General Public License version 3."
    source: "https://github.com/khoj-ai/khoj/blob/master/LICENSE"
faq:
  - q: "What is Khoj?"
    a: "A self-hostable personal application with chat, custom agents, scheduled research, voice and image generation, plus clients for browser, Obsidian, Emacs, desktop, phone and WhatsApp."
  - q: "Khoj vs a local search engine for an agent?"
    a: "Khoj is the finished application. A local search engine is the layer underneath: XERJ has no chat window, no model of its own and no user interface."
  - q: "Self-hosted second brain vs a folder index?"
    a: "Pick the second brain if you want the finished application. Pick the folder index if you already run an agent and need it to search local files."
  - q: "What does Khoj need to run?"
    a: "Docker or pip, a chat model that is local or remote, and Postgres with the pgvector extension for its embeddings."
  - q: "What does XERJ need to run?"
    a: "One binary and a data directory. There is no database server, no Python environment and no model download on the default path."
  - q: "Does XERJ do meaning-based matching by default?"
    a: "No. The default embedder is lexical feature hashing, not neural. Neural embeddings are opt-in through --embed-mode neural and CPU-only."
  - q: "Is there a measured comparison on this page?"
    a: "No. No head-to-head was run. Every Khoj fact here comes from Khoj's own documentation or repository."
  - q: "Is Khoj free, and what license is it under?"
    a: "Khoj is AGPL-3.0, which obliges you to publish your source if you offer a modified version over a network. XERJ is Apache-2.0, which does not."
---

**TL;DR** — Khoj is a whole application: chat, custom agents, scheduled research, voice, image generation and clients from Obsidian to WhatsApp. XERJ is the search and memory engine that sits under an agent you already run. No head-to-head was run for this page.

## Concede the product surface

Khoj is not a search library with a demo page attached. Its own documentation describes chat with local or online models and custom agents with their own persona and tools. It also describes scheduled research delivered by email, voice chat, and image generation through third-party providers.

It also ships clients: browser, desktop, phone, Obsidian, Emacs and a WhatsApp path. That is a product, and building it is a different amount of work from building an index.

XERJ has none of that. There is no chat window, no user interface, no model of its own and no phone client. If you want the finished application, Khoj is the honest answer and this page will not argue.

## What each one actually is

Khoj answers a person. XERJ answers a program.

That difference decides most of the rest. Khoj holds the model, the conversation and the interface, and its retrieval sits inside. XERJ holds only the retrieval, and the agent that calls it is yours.

If you already run a coding agent through MCP, adding Khoj means adding a second application with its own model configuration. Adding XERJ means adding one binary and a tool list.

## What each one reads

Khoj states that it understands Word, PDF, org-mode, markdown and plaintext files, plus Notion pages, and its repository description also lists image files.

XERJ reads thirteen families. The list holds JSON and JSONL, CSV, structured logs, SQL exports and SQLite. It also holds PDF, DOCX, HTML, XML, YAML, plain text, code and gzip variants.

The two lists overlap on documents and part company on data. Khoj covers image files, which XERJ does not, because XERJ does no optical character recognition at all. XERJ covers a SQLite database, a hostile CSV and a multi-gigabyte SQL export, which Khoj does not document.

## What each one costs to start

Khoj documents Docker or pip, a chat model that is local or remote, and Postgres with the pgvector extension holding the embeddings. The shipped compose file pins a pgvector image and adds services beside it.

XERJ is one static Rust binary of about 36 MB with a sub-second start:

```sh
xerj --insecure --data-dir ./xerj-data
xerj autoindex ./my-docs --prefix docs --state-dir ./state-docs
```

No database server, no Python environment and no model download sit on that path. That is a smaller commitment, and it buys a smaller product.

## How retrieval differs

Khoj creates meaning vectors with a bi-encoder and re-ranks the candidates with a slower cross-encoder, and both models are configurable.

XERJ ships a zero-config embedder in the binary, and the default one is lexical feature hashing rather than neural. On the default path there is no meaning-based signal at all, so a question worded differently from the document can miss. Neural embeddings are opt-in through `--embed-mode neural`, and they are CPU-only.

Read that plainly: on the shipped default, Khoj's retrieval understands a paraphrase and XERJ's does not.

## Where the agent boundary sits

Khoj can connect to a Model Context Protocol server built by somebody else, which makes it a client of that ecosystem. Its documentation does not describe Khoj exposing an MCP server of its own, and it does expose an HTTP API with key or bearer authentication for third-party applications.

XERJ sits on the other side of that line. `xerj mcp` is a stdio MCP server in the same binary, serving 10 tools, and `/_memory/{ns}` gives an agent namespaced memory in the engine.

## The limits you inherit with XERJ

XERJ is single-node only. There is no data-plane replication and no failover, so one host is the whole deployment.

The server retains heap for every document it indexes, which is an open tracked defect. Corpora past a few million documents can exhaust memory on one node.

There is no user interface. Every answer on the XERJ side arrives as JSON to whatever called it.

## When to choose Khoj instead

Choose Khoj when you want an application rather than a component. Chat, agents and clients are the reason.

Choose Khoj when a person will use it directly, on a phone or in Obsidian, with no agent in between.

Choose Khoj when image files matter, or when you want retrieval that handles a paraphrase without you turning anything on.

## When to choose XERJ instead

Choose XERJ when you already run an agent and it needs local files. One binary and an MCP tool list is the whole integration.

Choose XERJ when the corpus is not only documents. SQLite, CSV dialects and SQL exports are the difference.

Choose XERJ when you want no database server and no model download in the setup path.

## What was not measured

No head-to-head was run for this page. No Khoj deployment was installed, so there is no shared question, no timing and no accuracy figure here.

Every Khoj fact above comes from Khoj's own documentation or repository. Read those sources before you decide.
