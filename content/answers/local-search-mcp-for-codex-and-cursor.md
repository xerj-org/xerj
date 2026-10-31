---
title: "A local search tool for Codex and Cursor"
h1: "Search tool for a Codex agent, on the files you have"
description: "xerj init writes the MCP entry for Claude Code, Cursor and Codex when those folders exist. On main, xerj mcp serves 23 tools, or 20 with no hub key."
slug: "local-search-mcp-for-codex-and-cursor"
cluster: "Agent access and memory"
question: "Search tool for a Codex agent"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node. In a project that already has .codex and .cursor, run xerj init with the node URL. Restart the agent. Call xerj_search only after tools/list shows it. xerj mcp does not start a node."
answer: "A search tool for a Codex agent starts with `xerj init`. The command writes the MCP entry for Claude Code, and it writes Cursor and Codex only when those folders already exist. Codex reads the entry in trusted projects only. On main `xerj mcp` serves 23 tools from a running node."
key_fact: "On main xerj mcp serves 23 tools, or 20 in a build with no hub key."
key_fact_source: "landing/docs/agents/schemas/mcp-tools.json"
key_fact_date: "2026-10-28"
intent_group: "Agent access and memory"
queries:
  - "search tool for codex agent"
  - "plug search into cursor"
  - "codex mcp server local search"
  - "mcp servers for data processing"
  - "llms.txt search engine for agents"
commands:
  - cmd: "xerj --insecure --data-dir /tmp/xerj-aq-2026-10-20/node --bind 127.0.0.1 --port 9217"
    note: "Start the node first. xerj mcp does not start one."
  - cmd: "xerj init --url http://127.0.0.1:9217 --disable-feedback"
    note: "Run inside a project that already has .codex and .cursor. The capture wrote both configs."
  - cmd: "xerj mcp --url http://127.0.0.1:9217 --disable-feedback"
    note: "stdio MCP. On main it serves 23 tools, or 20 with no hub key."
links_out:
  - "give-chatgpt-claude-local-file-access"
  - "code-search-mcp-for-claude-code"
  - "mcp-api-search-or-vector-search"
evidence:
  - claim: "xerj init wrote .mcp.json and .cursor/mcp.json and appended .codex/config.toml."
    source: "benchmarks/agent-queries-2026-10-20/05-init-stdout.txt"
  - claim: "tools/list returned 17 tools from xerj mcp v1.0.0-rc.97."
    source: "benchmarks/agent-queries-2026-10-20/05-tool-names.txt"
  - claim: "On main xerj mcp serves 23 tools, or 20 in a build with no hub key."
    source: "landing/docs/agents/schemas/mcp-tools.json"
  - claim: "Codex config is read for trusted projects only."
    source: "engine/crates/xerj-autoindex/src/init.rs:221"
og_image: "og/answers-local-search-mcp-for-codex-and-cursor.png"
og_image_alt: "Cover: three outlined cards labelled mcp, cursor and codex, with the number 17 in ochre."
faq:
  - q: "Search tool for a Codex agent?"
    a: "Run `xerj init` in a project that already has `.codex/`. It appends `[mcp_servers.xerj]`. Codex reads that file in trusted projects only."
  - q: "Plug search into Cursor?"
    a: "If `.cursor/` exists, init writes `.cursor/mcp.json` with the same `mcpServers.xerj` entry. Restart Cursor."
  - q: "Does init configure Windsurf and Cline?"
    a: "It prints the entry and tells you to copy it. It does not write their config files."
  - q: "How many tools does xerj mcp serve?"
    a: "23 on main, or 20 in a build with no hub key. The count includes the 3 hub tools. The rc.97 capture on this page returned 17, so read `tools/list` from your own binary."
  - q: "Does xerj mcp start the node?"
    a: "No. Start the node, then run `xerj mcp`. The handshake can succeed and every tool call can still fail if nothing is listening."
---

**TL;DR**: A search tool for a Codex agent starts with `xerj init`. The command writes the MCP entry for Claude Code, and it writes Cursor and Codex only when those folders already exist. Codex reads the entry in trusted projects only. On main `xerj mcp` serves 23 tools from a running node.

XERJ is a new way for AI to search for data.

## Run init where the folders already are

The capture made an empty project with `.codex/` and `.cursor/`, put `model = "gpt-5"` in `.codex/config.toml`, and ran init against `http://127.0.0.1:9217`.

```text
wrote    .mcp.json
wrote    .cursor/mcp.json
wrote    .cursor/rules/xerj.mdc
wrote    .cursor/rules/xerj-hub.mdc
appended .codex/config.toml
wrote    AGENTS.md
```

`.mcp.json` and `.cursor/mcp.json` both got `mcpServers.xerj` with `command`, `args` `["mcp"]`, and `XERJ_URL`. Codex got `[mcp_servers.xerj]` appended under the model line. Init did not create `.codex/`. On a project without that directory, the Codex file is not written.

The printed lines for Windsurf, Claude Desktop and Cline say to copy the entry. They are instructions, not files written.

This binary also printed a hub line and wrote `xerj-hub.mdc`. A build without a hub signing key does not. Read what your binary printed.

## Start the node, then the MCP server

```sh
xerj --insecure --data-dir /tmp/xerj-node --bind 127.0.0.1 --port 9217
xerj mcp --url http://127.0.0.1:9217 --disable-feedback
```

`xerj mcp` talks to the node. It does not start one. Agents that read `https://xerj.org/llms.txt` before the first call get the field names and the caveats in that file.

The capture sent MCP `initialize` and `tools/list` to `xerj mcp`. The server named itself `xerj-mcp` version `1.0.0-rc.97` and returned 17 tools. The first is `xerj_search`. On main the server serves 23. Codex itself was not installed on the capture machine, so this is the tool list Codex would be handed, not a Codex session.

## Measured on 2026-10-20

| Check | Result |
|---|---|
| `xerj init` with `.cursor/` and `.codex/` | Wrote both MCP configs and appended Codex |
| Windsurf, Claude Desktop, Cline | Printed only. No config file written |
| `tools/list` | 17 tools, version 1.0.0-rc.97 (23 on main) |
| Codex process | Not run. No Codex install on the machine |

## When not to use XERJ

If the agent only needs one file it already has open, a search server is extra machinery. Init is for the agent that will call a tool instead of reading the tree.
