---
title: "Claude Code memory: CLAUDE.md, auto memory and MCP servers"
h1: "How does Claude Code memory work, and when do you need more?"
description: "Claude Code remembers through CLAUDE.md files and auto memory. See what loads at session start, where the limits are, and how a memory server adds recall."
slug: "claude-code-memory"
cluster: "Agent memory: setup"
question: "How does Claude Code memory work, and how do I give a coding agent persistent memory?"
intent: "explainer"
published: "2026-10-30"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node, run claude mcp add --scope user xerj -- xerj mcp, store one fact about this repo with xerj_memory_store, start a new session, recall it with xerj_memory_recall, and report the result."
answer: "How does Claude Code memory work? Claude Code loads CLAUDE.md files you write and an auto memory index Claude writes at the start of every session. For more history than fits in context, or memory shared across agents, add an MCP memory server such as XERJ that the agent searches on demand."
key_fact: "Claude Code loads the first 200 lines or 25KB of MEMORY.md at the start of each session; topic files are read on demand."
key_fact_source: "https://code.claude.com/docs/en/memory"
key_fact_date: "2026-10-30"
intent_group: "agent memory"
queries:
  - "claude code memory"
  - "memory for coding agents"
  - "claude code persistent memory"
  - "claude code auto memory"
links_out:
  - "coding-agent-memory-across-sessions"
  - "what-is-an-mcp-memory-server"
  - "search-claude-code-past-sessions"
  - "/compare/xerj-vs-claude-mem-and-mempalace"
  - "code-search-mcp-for-claude-code"
faq:
  - q: "Where does Claude Code store auto memory?"
    a: "In ~/.claude/projects/<project>/memory/ on your machine. A MEMORY.md file is the index and topic files hold the details. All worktrees of the same repo share it."
  - q: "How long should CLAUDE.md be?"
    a: "The docs suggest under 200 lines per file. Longer files use more context and Claude may follow them less closely."
  - q: "How do I turn auto memory off?"
    a: "Use the /memory command to toggle it, or set CLAUDE_CODE_DISABLE_AUTO_MEMORY=1."
  - q: "Does Claude Code read AGENTS.md?"
    a: "Yes, when a directory has no CLAUDE.md, recent versions read AGENTS.md instead."
  - q: "When do I need a memory server?"
    a: "When notes outgrow what should load every session, when you want search over old sessions, or when several agents should share one memory."
  - q: "Does XERJ replace CLAUDE.md?"
    a: "No. Keep rules that apply every time in CLAUDE.md. Use XERJ for facts the agent looks up only when needed."
---

**TL;DR** Claude Code has two built-in memory systems. CLAUDE.md files hold rules you write, and auto memory holds notes Claude writes. Both load at the start of each session. When you need search over more than fits in context, add an MCP memory server that the agent calls only when it needs a fact.

```visual
claude-code-memory-layers
```

## What loads at session start

Step through it below. Each step is one source Claude Code reads.

```visual
claude-code-memory-steps
```

## The three options compared

| | CLAUDE.md | Auto memory | MCP memory server |
| --- | --- | --- | --- |
| Who writes it | You | Claude | The agent, through a tool |
| When it loads | Every session, in full | MEMORY.md at start; topics on demand | Only when called |
| Size guide | Under 200 lines per file | First 200 lines or 25KB of MEMORY.md | Limited by disk, not context |
| Shared | Through git, for project files | One machine, all worktrees of a repo | Any agent that can reach the server |
| Best for | Rules for every task | Lessons Claude picks up | Facts you look up later |

## When the built-ins are enough

For one person, one repo and one agent, start with CLAUDE.md and auto memory. Put build commands and code style in the project CLAUDE.md. Let auto memory collect corrections like "always use pnpm, not npm". Check what it saved with `/memory` now and then.

## When to add a memory server

Add one when any of these come up:

- Your notes are too long to load every time, but you still want to find them.
- You want to search past sessions, not only short notes.
- Claude Code, Codex and other agents should share the same facts.

A memory server stores facts outside the context window. The agent calls it with a query and gets the top matches. See [what an MCP memory server is](/answers/what-is-an-mcp-memory-server) and [memory across sessions](/answers/coding-agent-memory-across-sessions) for more.

## A real recall example with XERJ

We stored three memories in a local XERJ node and queried them with default settings, which use BM25 full-text search.

| Query | Result |
| --- | --- |
| pnpm | "User prefers pnpm over npm", score 1.0526 |
| what port does the API use | "The API runs on port 9200", score 2.9425 |
| which package manager | No match |
| deploy day | No match |

Keyword recall finds exact words fast and shows why it matched. It misses a question that uses other words. For those, ask for semantic or hybrid recall, or store the words you expect to search for.

## How to try it with XERJ

Use the Copy the prompt button and paste it into Claude Code. It installs XERJ, adds it as an MCP server for all your projects and checks recall in a new session. By hand:

```bash
curl -fsSL https://xerj.org/get | sh
claude mcp add --scope user xerj -- xerj mcp
```

The same node can also index your repo for [code search](/answers/code-search-mcp-for-claude-code) and your [past Claude Code sessions](/answers/search-claude-code-past-sessions). To compare XERJ with claude-mem and MemPalace, see [this comparison](/compare/xerj-vs-claude-mem-and-mempalace).

## Sources checked

- Claude Code memory docs, [code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory), checked 2026-10-30: file locations, load order, the 200 line and 25KB limits, /memory and the disable setting.
- XERJ recall results: local node, namespace demo, default mode, recorded 2026-10-30.
