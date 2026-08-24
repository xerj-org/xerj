---
title: "XERJ vs Meilisearch for local and agent search"
h1: "What's a local alternative to Meilisearch that indexes a folder by itself?"
description: "A local alternative to Meilisearch that reads a folder of mixed files by itself. Meilisearch wins typo tolerance by default. No head-to-head was run here."
slug: "xerj-vs-meilisearch"
cluster: "Comparison: search engines"
question: "What's a local alternative to Meilisearch that indexes a folder by itself?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local node with `xerj --insecure --data-dir ./xerj-data`, run `xerj autoindex ./docs-folder --prefix docs --state-dir ./state-docs` against the raw folder, then POST an explicit fuzzy query on body to /docs-*/_search and report the hits. Also report how many documents you had to build and POST by hand before the first query."
commands:
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start one node. No document array is needed yet."
  - cmd: "xerj autoindex ./docs-folder --prefix docs --state-dir ./state-docs"
    note: "The folder becomes queryable indices with no schema work."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/docs-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"fuzzy\":{\"body\":{\"value\":\"reciept\",\"fuzziness\":\"AUTO\"}}}}'"
    note: "The nearest thing XERJ has to typo tolerance is an explicit fuzzy query, not a default."
links_out:
  - "search-engine-without-docker"
  - "how-xerj-autoindexes-a-folder"
  - "search-file-contents-in-a-folder"
  - "do-search-embeddings-help"
evidence:
  - claim: "Meilisearch accepts one typo for query terms of five or more characters and up to two typos when the term is at least nine characters long, and typo tolerance is on by default."
    source: "https://www.meilisearch.com/docs/capabilities/full_text_search/relevancy/typo_tolerance_settings"
  - claim: "Documents enter Meilisearch through POST /indexes/{index_uid}/documents, and the endpoint accepts application/json, application/x-ndjson and text/csv."
    source: "https://www.meilisearch.com/docs/reference/api/documents/add-or-update-documents"
  - claim: "Meilisearch hybrid search merges the two result sets with a semanticRatio that defaults to 0.5 and gives equal weight to each side."
    source: "https://www.meilisearch.com/docs/capabilities/hybrid_search/advanced/custom_hybrid_ranking"
  - claim: "Meilisearch publishes a first-party Model Context Protocol server so an assistant can create indices, add documents and search over them."
    source: "https://www.meilisearch.com/docs/getting_started/integrations/mcp"
  - claim: "Meilisearch's Enterprise Edition is licensed under the Business Source License 1.1 and cannot be freely used in production, and sharding is the only feature exclusive to it."
    source: "https://www.meilisearch.com/docs/resources/self_hosting/enterprise_edition"
  - claim: "Meilisearch Cloud is a paid managed plan that starts at 20 dollars per month with usage-based or resource-based billing."
    source: "https://www.meilisearch.com/pricing"
faq:
  - q: "Which tool handles a typo better?"
    a: "Meilisearch. Typo tolerance is on by default there: one typo at five characters, two at nine. XERJ needs an explicit fuzzy query."
  - q: "Is Meilisearch the right thing to point Claude at a folder?"
    a: "Not on its own. Its document endpoint accepts JSON, NDJSON and CSV, so you extract the text and build the documents yourself before an agent can query them."
  - q: "What does XERJ do that Meilisearch does not?"
    a: "It reads a folder of mixed files directly, including PDF, DOCX, SQLite and SQL exports, and writes typed mappings without a schema step."
  - q: "Does Meilisearch have an MCP server?"
    a: "Yes. Meilisearch publishes a first-party MCP server, so this is not a point where XERJ stands alone."
  - q: "Is there a measured comparison on this page?"
    a: "No. No head-to-head was run. Every vendor fact here comes from Meilisearch's own documentation."
  - q: "How does each side do hybrid search?"
    a: "Meilisearch interpolates the two result sets with a semanticRatio that defaults to 0.5. XERJ fuses with Reciprocal Rank Fusion or a weighted linear sum."
  - q: "Which license applies?"
    a: "XERJ is Apache-2.0. Meilisearch is MIT with an Enterprise Edition under the Business Source License 1.1, which holds sharding."
  - q: "Meilisearch vs a local search engine for an agent?"
    a: "Call Meilisearch when a person types into a search field. Call XERJ when an agent must query files that are already on disk."
---

**TL;DR** — Meilisearch wins typo-tolerant instant search, and its own defaults document the win. XERJ wins the step before the query: it reads a folder of mixed files with one command, where Meilisearch expects you to build and POST the documents. No head-to-head was run for this page.

## Concede the typo first

Meilisearch turns typo tolerance on by default. Its documentation states the thresholds: one typo for terms of five or more characters, and up to two typos once a term reaches nine characters.

That is the behavior a person expects in a search-as-you-type field, and Meilisearch ships it with no work from you. XERJ does not. The nearest equivalent is an explicit `fuzzy` clause in the query DSL, chosen per query.

If your users type `reciept` and must land on `receipt` without you thinking about it, stop reading and use Meilisearch. No measurement on this page changes that answer, and none was run.

## The real difference is the step before the query

Meilisearch takes documents you give it. The documented route is `POST /indexes/{index_uid}/documents`, and the endpoint accepts three content types: JSON, NDJSON and CSV.

Nothing in that path reads a PDF, a DOCX contract, a SQLite file or a SQL export. You extract the text, shape it into a document array, and send it.

For a docs site built from markdown, that pipeline is short. For a folder of mixed office files, it is a project.

XERJ starts one step earlier:

```sh
xerj autoindex ./docs-folder --prefix docs --state-dir ./state-docs
```

The command reads a content signature rather than the file extension. It infers field types from bounded samples, writes explicit mappings, and files what it learned in a catalog index.

Thirteen families are covered. The list holds JSON and JSONL, CSV, structured logs, SQL exports and SQLite. It also holds PDF, DOCX, HTML, XML, YAML, plain text, code and gzip variants.

## Two engines, two query languages

An agent that already writes Elasticsearch query DSL sends it to XERJ unchanged, because `GET /` advertises version 8.13.0 and the wire protocol is the transport. That compatibility is an adoption bridge and not a fork.

Meilisearch has its own search API, which is smaller and quicker to learn for a person. Neither is better in the abstract. The question is which dialect the caller already speaks.

## Hybrid retrieval works differently on each side

Meilisearch merges its two result sets with a `semanticRatio` that defaults to 0.5, giving each side equal weight. XERJ fuses with Reciprocal Rank Fusion or with a weighted linear sum inside one query.

The disclosure that matters more than either mechanism: the XERJ default embedder is lexical feature hashing, not neural. On the default path there is no meaning-based signal to fuse, and connecting synonyms needs `--embed-mode neural`, which is opt-in and CPU-only.

## Where Meilisearch has a lead nobody should hide

Meilisearch publishes a first-party MCP server, so an assistant can create indices, add documents and search through it. Agent access is not a place where XERJ stands alone.

Meilisearch Cloud is a managed plan that starts at 20 dollars per month. XERJ ships self-hosted only and offers no managed product, so if you want somebody else to run it, XERJ has no answer.

## Where the license lines fall

XERJ is Apache-2.0. Meilisearch is MIT, with an Enterprise Edition under the Business Source License 1.1 that its documentation says cannot be freely used in production.

Sharding is the one feature held in that Enterprise Edition. XERJ is single-node only and has no sharding at all, so neither side gives you growth across machines for free.

## The limits you inherit with XERJ

XERJ is single-node only. There is no data-plane replication, no failover and no multi-region mode, so one host is the whole deployment and a snapshot plan is your durability story.

The server retains heap for every document it indexes, which is an open tracked defect. Corpora past a few million documents can exhaust memory on one node.

XERJ does no optical character recognition. A page image with no text layer is junk on both engines until a separate tool gives it a text layer.

## When to choose Meilisearch instead

Choose Meilisearch for a search field a person types into. Typo tolerance, instant results and a small API are its job, and it does that job well.

Choose Meilisearch when your documents already exist as JSON in a database or a build step. The push model costs nothing when the extraction problem is already solved.

Choose Meilisearch when you want a managed plan. XERJ has none.

## When to choose XERJ instead

Choose XERJ when the corpus is a folder on a disk and nobody has built a pipeline for it. One command turns mixed formats into typed indices.

Choose XERJ when the caller is an agent rather than a person. The same binary also holds namespaced agent memory and an MCP server that serves 10 tools.

Choose XERJ when the files hold shapes Meilisearch never accepts. A SQLite database, a hostile CSV with a decimal comma and a folder of DOCX contracts all qualify.

## What was not measured

No head-to-head was run for this page. There is no timing here, no recall figure and no typo-query score, because no Meilisearch node was installed to produce one.

Every Meilisearch fact above comes from Meilisearch's own documentation, and the links are in the sources for this article. Read them before you decide, because vendor defaults and license terms move.
