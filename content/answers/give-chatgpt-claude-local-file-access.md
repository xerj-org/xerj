---
title: "MCP server to search local files from ChatGPT or Claude"
h1: "Can ChatGPT search a folder on my laptop, or do I need something else?"
description: "xerj mcp is a stdio MCP server that proxies 23 tools to a running XERJ node, so ChatGPT, Claude or Cursor search an indexed local folder instead of opening files."
slug: "give-chatgpt-claude-local-file-access"
cluster: "Agent access and memory"
question: "Can ChatGPT search a folder on my laptop, or do I need something else?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, index a local folder with xerj autoindex --prefix mcpdocs, add xerj mcp to your MCP host config with the absolute binary path and XERJ_URL, then call tools/list and one xerj_search over mcpdocs-* and report the ax_path of every hit instead of opening a file."
answer: "Can ChatGPT or Claude search a folder on my laptop, or do I need something else? You need a local MCP server. Install XERJ, start a node, index the folder with xerj autoindex, and point the agent host at xerj mcp with an absolute path. One xerj_search call then reads the indexed local files."
key_fact: "On main xerj mcp serves 23 tools, each with an inputSchema in the published mcp-tools.json."
key_fact_source: "landing/docs/agents/schemas/mcp-tools.json"
key_fact_date: "2026-10-28"
intent_group: "Find the right tool"
queries:
  - "mcp server search local files"
  - "claude code plugin local search"
  - "cursor mcp full text search"
commands:
  - cmd: "xerj --insecure --data-dir ./data --disable-feedback"
    note: "Start the one node the MCP server proxies to. `xerj mcp` never starts a node itself."
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9500 --prefix mcpdocs --state-dir ./state-mcpdocs --progress plain --disable-feedback"
    note: "Index the local folder the agent is meant to read."
  - cmd: "xerj mcp --url http://127.0.0.1:9500 --disable-feedback"
    note: "Run the MCP stdio server by hand to see the JSON-RPC stream before you wire an agent host to it."
links_out:
  - "code-search-mcp-for-claude-code"
  - "search-file-contents-in-a-folder"
  - "/docs/agents/quickstart"
  - "/compare/xerj-vs-web-agent-search"
  - "/compare/xerj-vs-localsynapse"
faq:
  - q: "Can ChatGPT search a folder on my laptop, or do I need something else?"
    a: "Not on its own: the hosted chat has no path to your disk. Give it a local MCP server over an indexed folder, and the agent searches the index instead of reading files."
  - q: "How do I give Claude or ChatGPT access to files on my machine?"
    a: "Index the folder with `xerj autoindex`, then add `xerj mcp` to the agent host as an MCP server. The agent searches the index instead of reading files."
  - q: "How do I let an agent search files on my machine offline?"
    a: "Run the node and the MCP server on that machine. The MCP server speaks stdio to the agent host and HTTP to your own node, so the index and the documents stay local."
  - q: "Does xerj mcp start a XERJ node?"
    a: "No. `xerj mcp` proxies to a node that is already running. Start the node first with `xerj --data-dir ./data`, then point the MCP server at its URL."
  - q: "How many tools does the XERJ MCP server expose?"
    a: "23 on main, or 20 in a build with no hub key, and every one carries an `inputSchema`: `xerj_search`, 3 more retrieval tools, `xerj_def`, `xerj_batch_search`, 2 memory tools, 4 brain tools, `xerj_status`, `xerj_map`, `xerj_index_budget`, `xerj_autoindex_remove`, `xerj_code_search`, `xerj_plan`, `xerj_field_report`, `xerj_corpus_feedback` and 3 Corpus Hub tools. A live `tools/list` from v1.0.0-rc.97 on 2026-10-19 returned 17, and the first capture, in August 2026, returned 10."
  - q: "Why does my MCP host fail to launch xerj?"
    a: "Use an absolute path in the `command` field. An MCP host launched from a desktop icon does not inherit your shell PATH, so a bare `xerj` fails there."
  - q: "Can I restrict which indices the agent can read?"
    a: "Only with a scoped API key. XERJ has no RBAC and no SSO, and `GET /_security/roles` reports `\"enforced\": false` for the seeded roles."
---

**TL;DR**: XERJ ships an MCP server as `xerj mcp`. Install the latest XERJ, start a node, index a folder with `xerj autoindex`, then point the agent host at the binary with an absolute path. On main it serves 23 tools, and in a v1.0.0-rc.97 capture one `xerj_search` call read an indexed local file.

## What `xerj mcp` actually is

`xerj mcp` is a Model Context Protocol server that speaks newline-delimited JSON-RPC 2.0 over stdio and proxies its tools to a XERJ node over HTTP. The agent host launches the binary; the binary talks to the node you started.

The command does not start a node. Start one first, otherwise every tool call fails at the HTTP hop.

```text
Speaks MCP over stdio (newline-delimited JSON-RPC 2.0 on stdin/stdout) and
proxies 23 tools — xerj_def, xerj_batch_search, ... xerj_hub_ask — to a
XERJ node that
is ALREADY RUNNING. This command does not start a node; start one first with
`xerj --insecure --data-dir ~/xerj-data`.
```

## Index the folder before you wire the agent

An agent can only read what the node holds. Index the folder first, and give the run a prefix so the index name is predictable.

```sh
xerj autoindex ./notes --url http://127.0.0.1:9500 --prefix mcpdocs --state-dir ./state-mcpdocs --progress plain --disable-feedback
```

The prefix decides the index pattern the agent queries. A `--prefix mcpdocs` run answers to `mcpdocs-*` in every later tool call.

## The agent-host configuration we used

Every `mcpServers`-shaped configuration takes a `command`, an `args` list and an `env` map. Use the absolute path of the binary. An agent host launched from a desktop icon does not inherit your shell PATH.

```json
{
  "mcpServers": {
    "xerj": {
      "command": "/home/you/.local/bin/xerj",
      "args": ["mcp", "--url", "http://127.0.0.1:9500"],
      "env": { "XERJ_DISABLE_FEEDBACK": "true" }
    }
  }
}
```

If the node did not start with `--insecure`, add `"XERJ_AUTH": "ApiKey <key>"` beside the URL. The key lives at `<data-dir>/admin.key`.

## The handshake, exactly as captured

The agent host opens with `initialize`. XERJ echoes the protocol version, names itself and declares one capability.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocolVersion":"2025-06-18",
  "capabilities":{"tools":{}},
  "clientInfo":{"name":"xerj-evidence-harness","version":"2.0"}}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocolVersion":"2025-06-18",
  "capabilities":{"tools":{"listChanged":false}},
  "serverInfo":{"name":"xerj-mcp","version":"<your build>"}}}
```

The server also returns an `instructions` string that tells the agent which tool to reach for. Diagnostics go to stderr, so stdout carries only the JSON-RPC stream.

## The tools, with their required arguments

A live `tools/list` from v1.0.0-rc.97 (2026-10-19) returned 17 tools, and all 17 carried an `inputSchema`. The first capture for this page, in August 2026, returned 10. On main the list has 23 (20 in a build with no hub key), and `mcp-tools.json` carries every schema.

| tool | required arguments |
| --- | --- |
| `xerj_search` | none (`index` defaults to every index) |
| `xerj_semantic_search` | `index`, `field`, `query` |
| `xerj_vector_search` | `index`, `field`, `query_vector` |
| `xerj_hybrid_search` | `index`, `queries` |
| `xerj_memory_store` | `namespace`, `text` |
| `xerj_memory_recall` | `namespace` |
| `xerj_brain_ego` | `brain`, `node` |
| `xerj_brain_link` | `brain`, `src`, `dst`, `type` |
| `xerj_brain_unlink` | `brain`, `edge_id` |
| `xerj_brain_overview` | `brain` |
| `xerj_map` | none |
| `xerj_code_search` | `corpus`, `query` |
| `xerj_plan` | `prompt` |
| `xerj_field_report` | `agent`, `used_for`, `verdict` |
| `xerj_hub_find` | `question` |
| `xerj_hub_def` | `slug`, `symbol` |
| `xerj_hub_ask` | `question` |

## One real call against an indexed local file

The agent sends a `tools/call` with an Elasticsearch query object. The response arrives as a text content block holding the node's own `_search` body.

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
  "name":"xerj_search",
  "arguments":{"index":"mcpdocs-*","size":3,
    "_source":["ax_path","ax_format","title"],
    "query":{"match_phrase":{"body":"checkpoint journal"}}}}}
```

```json
{"hits":{"total":{"value":1,"relation":"eq"},"hits":[
  {"_index":"mcpdocs-docs","_score":1.4359609,
   "_source":{"title":"# Runbook","ax_path":"01-runbook.md","ax_format":"txt-prose"}}]}}
```

The hit names `01-runbook.md`, a file on the local disk. The agent now has a path and a title without opening a single file itself.

## Read the `_xerj.hints` block

That same response carried a hint object about the `body` field. XERJ scored it with BM25 and did not consult the embedding written at index time. XERJ's default embedder is lexical feature hashing, and the neural embedder is opt-in through `--embed-mode neural`.

The hint names the query to run instead. An agent that reads `_xerj.hints` corrects itself without a second round trip through you.

## Failures come back as results, not as errors

Two failure shapes matter when you write the agent's error handling. Both arrive as a JSON-RPC **success** carrying `isError: true`, not as a JSON-RPC error object.

```json
{"jsonrpc":"2.0","id":9,"result":{
  "content":[{"type":"text","text":"unknown tool: xerj_not_a_tool"}],"isError":true}}
```

A call against a missing index passes the node's own 404 `index_not_found_exception` body through verbatim. The hint beside it lists index names close to the one you asked for. A `ping` returns `{}`.

## The published tool schemas are stale

On main the binary serves 23 tools. The published `openai-tools.json` and `anthropic-tools.json` still list 6 (checked 2026-10-28), so 17 are missing: `xerj_status`, `xerj_def`, `xerj_batch_search`, the 4 `xerj_brain_*` tools, `xerj_map`, `xerj_index_budget`, `xerj_autoindex_remove`, `xerj_code_search`, `xerj_plan`, `xerj_field_report`, `xerj_corpus_feedback` and the 3 `xerj_hub_*` tools. The MCP schema at `/docs/agents/schemas/mcp-tools.json` lists all 23, and a test keeps it in step with the server source.

That is a documentation defect, confirmed against the source that generates the tool list. If you wire either vendor schema into an agent by hand, add the missing tools yourself or read `tools/list` from a live server instead.

## What this setup does not give you

XERJ is single-node. The MCP server has no replication, no failover and no multi-region mode behind it. The agent depends on the one node you started.

Access control is coarse. XERJ has no RBAC and no SSO. `GET /_security/roles` reports `"enforced": false`, so every authenticated caller reaches every index. A scoped API key is the only way to narrow what the agent can read.

## What the capture was

One node, started with `--insecure` on port 9500, and one MCP session of 12 requests recorded verbatim. The binary was a `ci-test` build, so no timing from this run is a performance figure.
