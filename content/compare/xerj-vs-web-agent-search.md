---
title: "Local folder search vs a web agent search API"
h1: "How do I let an agent search a folder on my laptop instead of crawling the web?"
description: "Exa, Tavily and Firecrawl take a query or a URL and answer from the public web. XERJ takes a folder already on your disk and never opens a network connection."
slug: "xerj-vs-web-agent-search"
cluster: "Comparison: agent retrieval"
question: "How do I let an agent search a folder on my laptop instead of crawling the web?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a node with `xerj --insecure --data-dir ./xerj-data`, run `xerj autoindex ./private-folder --prefix priv --state-dir ./state-priv`, attach it with `xerj mcp --url http://127.0.0.1:9200`, then answer the user question from the indexed files with one xerj_search call, cite the ax_path of every hit, and send no file content to a web search API."
commands:
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start one local node. Nothing here reaches the public internet."
  - cmd: "xerj autoindex ./private-folder --prefix priv --state-dir ./state-priv"
    note: "Read files that are already on this disk. There is no URL input."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Give the agent 10 tools over that folder. Every tool reads the local node, and none of them takes a URL."
links_out:
  - "give-chatgpt-claude-local-file-access"
  - "how-xerj-autoindexes-a-folder"
  - "search-html-export"
  - "search-file-contents-in-a-folder"
evidence:
  - claim: "Exa describes itself as a search engine built for AI systems and says it builds its own index of pages on the web, refreshed every hour."
    source: "https://exa.ai/docs/reference/faqs"
  - claim: "The Exa contents endpoint takes a list of URLs as its input and returns page text, summaries and metadata."
    source: "https://exa.ai/docs/reference/get-contents"
  - claim: "Tavily describes its REST API as a search engine for LLM agents plus an extraction product, and its extract endpoint takes a list of URLs."
    source: "https://docs.tavily.com/documentation/api-reference/endpoint/extract"
  - claim: "The Tavily free plan includes 1,000 API credits every month with no credit card."
    source: "https://docs.tavily.com/documentation/api-credits"
  - claim: "The Firecrawl scrape endpoint requires a url body parameter documented as 'The URL to scrape', and the site-traversal endpoint requires a base URL."
    source: "https://docs.firecrawl.dev/api-reference/endpoint/scrape"
  - claim: "Mastra is a TypeScript framework for agents that provides a common interface over third-party vector databases rather than its own index, and its web search tools wrap the Tavily API."
    source: "https://mastra.ai/docs/rag/vector-databases"
faq:
  - q: "Does XERJ search the web?"
    a: "No. XERJ opens no network connection to a website and fetches nothing over the internet. It reads files that are already on the disk you point it at."
  - q: "Can ChatGPT search a folder on my laptop, or do I need something else?"
    a: "You need something else. The documented input of Exa and Firecrawl is a query string or a URL, and neither API accepts a filesystem path, so the folder needs a local index the agent can call."
  - q: "When should an agent use a web search API vs a local file index?"
    a: "Call a web API when the answer lives on a public page. Call a local index when the answer lives in a file you own."
  - q: "Can I use both in the same agent?"
    a: "Yes. They answer different questions, and an agent can hold one tool for each."
  - q: "What if I already saved the web pages to disk?"
    a: "Then the job is local. XERJ indexes a folder of saved HTML files, and no fetch happens."
  - q: "Is there a measured comparison on this page?"
    a: "No. No head-to-head was run. The two categories take different inputs, so a shared task does not exist."
  - q: "What does Mastra provide?"
    a: "A TypeScript agent framework. Its retrieval paths are adapters over other vector stores, and its web search tools wrap a search vendor."
  - q: "Is my file content sent anywhere?"
    a: "Not by XERJ. A single-node XERJ process reads local files and answers on localhost."
---

**TL;DR** — Exa, Tavily and Firecrawl answer questions about the public web. Their documented input is a query string or a URL. XERJ answers questions about files that are already on your disk, and it opens no network connection to a website. This page is a category boundary, not a bakeoff.

## The two categories take different inputs

The clearest way to separate these tools is to read what each one accepts.

| tool | documented input | what comes back |
| --- | --- | --- |
| Exa `/search` | a query string | ranked public web results, optionally with page text |
| Exa `/contents` | a list of URLs | page text, summaries and metadata |
| Tavily `/search` | a query string | web results shaped for an agent |
| Tavily `/extract` | a list of URLs | the raw content of those pages |
| Firecrawl `/scrape` | one URL | that page as markdown or HTML |
| XERJ `xerj autoindex` | a folder path on this machine | typed indices you query over the Elasticsearch REST API |

No web API in that table accepts a filesystem path. XERJ accepts nothing else.

## What the web APIs are good at

Exa maintains its own index of pages on the web and refreshes it every hour, and it matches with transformer models rather than terms alone. That is a real capability, and nothing local reproduces it.

Tavily returns web results already shaped for a language model, and its free plan carries 1,000 API credits per month with no credit card. Firecrawl turns a page into markdown a model can read, and its `/scrape` endpoint documents its `url` parameter as "The URL to scrape".

If the question is "what does the current release note say on the vendor's site", that is their job. XERJ cannot answer it at all.

## What XERJ is good at

XERJ reads a folder that is already on disk and makes it queryable with one command:

```sh
xerj autoindex ./private-folder --prefix priv --state-dir ./state-priv
```

Formats are read from a content signature rather than trusted by extension. The families covered are JSON and JSONL, CSV, structured logs, SQL exports, SQLite, PDF, DOCX, HTML, XML, YAML, plain text, code and gzip variants. An agent then reads the result through `xerj mcp`, a stdio MCP server in the same binary that serves 10 tools.

The default embedder is lexical feature hashing, not neural, so a match on a differently worded question needs `--embed-mode neural`, which is opt-in and CPU-only.

## The boundary, stated plainly

XERJ fetches nothing over the network. There is no fetcher, no URL input and no site traversal anywhere in the product.

`xerj autoindex` is CLI-only. It must run on a machine that can see the files, and no HTTP endpoint makes a remote node walk a folder.

The reverse is equally firm. A web API cannot see your disk, and sending a private file to one is a decision about your data, not a retrieval strategy.

Saved pages are the one place the two categories touch. Once a site is on disk as HTML files, the job is local, and XERJ indexes those files like any other family.

## Mastra is a third thing again

Mastra is a TypeScript framework for building agents, not a search index. Its retrieval documentation describes a common interface over third-party vector databases, and its web search tools wrap a search vendor's API.

If you are choosing a framework, that question is separate from which index answers your question. Mastra can hold both tools described here.

## When to choose a web search API instead

Choose Exa, Tavily or Firecrawl when the answer is on a public page and you do not have it locally. That is most current-events, vendor-documentation and competitive-research work.

Choose them when you want a page converted to markdown for a model. Choose them when you want a site turned into a list of URLs. XERJ has no equivalent.

Choose them when you have no machine to run a process on. XERJ ships self-hosted only, as a single binary you start yourself.

## When to choose the local index instead

Choose a local index when the files are private and large. Choose it when one folder answers many questions in a session. Nothing leaves the host.

Choose it when the corpus has no web shape at all. A SQLite file, a hostile CSV or a DOCX contract has no URL. No web API has an input for it.

## What was not measured

No head-to-head was run for this page, and none is meaningful. The two categories take different inputs, so there is no shared task to score.

Every claim about a web vendor above comes from that vendor's own API reference. Their absence of a filesystem input is an absence in their documentation, not a statement they published.
