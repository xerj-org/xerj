---
title: "claude-mem vs MemPalace vs XERJ: memory for Claude Code"
h1: "claude-mem vs MemPalace vs XERJ: which memory should Claude Code use?"
description: "claude-mem summarizes sessions with a model. MemPalace keeps every word. XERJ stores what the agent chooses. A fair comparison and a picker to find your fit."
slug: "xerj-vs-claude-mem-and-mempalace"
cluster: "Comparison: agent memory"
question: "Should I use claude-mem, MemPalace or XERJ as memory for Claude Code?"
intent: "comparison"
published: "2026-10-30"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node, run claude mcp add xerj -- xerj mcp, then in a new Claude Code session store two facts about this repo with xerj_memory_store and recall them in a third session with xerj_memory_recall, and report what came back."
answer: "Should I use claude-mem, MemPalace or XERJ for Claude Code memory? Use claude-mem for automatic, model-written session summaries. Use MemPalace to keep every conversation word for word, saved by hooks. Use XERJ when you want the agent to choose what to save and one local engine for memory, logs, docs and code."
intent_group: "agent memory"
queries:
  - "claude-mem alternative"
  - "mempalace alternative"
  - "best mcp memory server for claude code"
  - "claude-mem vs mempalace"
links_out:
  - "what-is-an-mcp-memory-server"
  - "claude-code-memory"
  - "search-claude-code-past-sessions"
  - "/compare/xerj-vs-mem0"
  - "coding-agent-memory-across-sessions"
faq:
  - q: "What is the difference between claude-mem and MemPalace?"
    a: "claude-mem has a model compress each session into short observations and adds them at the next session start. MemPalace stores the conversation word for word and finds it with search, with no summary step."
  - q: "Does claude-mem need a paid service?"
    a: "Its installer offers a hosted observer with a 30 day free trial. You can instead pass a --provider flag to use your own OpenRouter or Gemini key, or your Anthropic plan."
  - q: "Is MemPalace's 96.6% score independent?"
    a: "No. MemPalace reports 96.6% R@5 on LongMemEval in raw mode in its own README. We did not rerun it."
  - q: "How is XERJ different from both?"
    a: "XERJ has no hooks. The agent saves and recalls through MCP tools when it decides to. The same local engine also searches logs, docs, code and past session files."
  - q: "Can I use more than one?"
    a: "Yes. They are separate MCP servers or plugins. Running two memory systems means two places to check, so start with one."
  - q: "Which is the best MCP memory server for Claude Code?"
    a: "It depends on what you want saved and who decides. Use the picker on this page, then try your choice for a week."
---

**TL;DR** All three give Claude Code memory that lasts across sessions. claude-mem saves on its own and has a model write short summaries. MemPalace saves every word on its own and searches it by meaning. XERJ saves only what the agent chooses, through MCP tools, in a local engine that also searches your other data.

```visual
claude-memory-designs
```

## Side by side

Read from each project's README and the XERJ source on 2026-10-30. We did not run a head-to-head benchmark.

| | claude-mem | MemPalace | XERJ |
| --- | --- | --- | --- |
| How it saves | Lifecycle hooks, automatic | Hooks, periodically and before compaction | The agent calls `xerj_memory_store` |
| What it keeps | Model-written observations and summaries | Original text, word for word | The text the agent sends |
| Model on write | Yes: hosted observer, your key, or your Anthropic plan | No, in raw mode | No |
| Search | SQLite FTS5 plus Chroma | ChromaDB by default; other backends | BM25, semantic or hybrid in one index |
| How it recalls | Context at session start, plus MCP search | MCP tools and a recall skill | MCP `xerj_memory_recall` or HTTP |
| Runtime | Bun worker service | Python package | One binary |
| Other agents | Codex, OpenCode and others | Codex CLI and Cursor hooks | Any MCP client or HTTP caller |
| License | Apache 2.0 | MIT | Apache 2.0 |

## Find your fit

```visual
memory-tool-picker
```

## claude-mem in short

claude-mem installs hooks for every stage of a Claude Code session. A model reads what happened and writes compact observations, stored in SQLite with full text search and in Chroma for vectors. At the next session start it adds relevant context by itself, so you do not have to ask. That model call is the trade: the installer offers a hosted observer with a 30 day free trial, or `--provider` to use your own key or Anthropic plan.

Pick it when you want memory that works without the agent thinking about it.

## MemPalace in short

MemPalace keeps the conversation as written, with no summarizing. It organizes text into wings, rooms and drawers so searches can be scoped. ChromaDB is the default backend and others can be plugged in. Its README reports 96.6% R@5 on LongMemEval in raw mode with no API calls; that is the project's own number.

Pick it when you never want a model to decide what to forget.

## XERJ in short

XERJ is a local search engine with a memory API. The agent stores a memory by calling a tool and recalls it the same way. Recall is BM25 by default, with semantic and hybrid modes. Because it is a full search engine, the same node can index your logs, docs, code and [past Claude Code sessions](/answers/search-claude-code-past-sessions).

Limits to know: it saves nothing unless the agent asks, it runs on one node with no replication, and namespaces isolate by name, not by credential.

See [what an MCP memory server is](/answers/what-is-an-mcp-memory-server) for the general idea, and [Claude Code memory](/answers/claude-code-memory) for the built-in options that come first.

## How to try it with XERJ

Use the Copy the prompt button and paste it into Claude Code. It installs XERJ, adds it as an MCP server and tests store and recall across sessions. To add it by hand:

```bash
curl -fsSL https://xerj.org/get | sh
claude mcp add xerj -- xerj mcp
```

## Sources checked

All checked on 2026-10-30.

- claude-mem README, [thedotmack/claude-mem](https://github.com/thedotmack/claude-mem): hooks, worker, SQLite FTS5, Chroma, observer and provider options. License Apache 2.0.
- MemPalace README, [MemPalace/mempalace](https://github.com/MemPalace/mempalace): verbatim storage, ChromaDB default, pluggable backends, hooks, LongMemEval R@5. License MIT.
- XERJ: `engine/crates/xerj-api/src/memory_api.rs` and `engine/crates/xerj-mcp/src/lib.rs`.
