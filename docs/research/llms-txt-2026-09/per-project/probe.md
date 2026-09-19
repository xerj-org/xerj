# probe (probelabs/probe)

Category: local code search (Rust binary, MCP server, npm wrapper) — nearest functional peer

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. This project publishes NO llms.txt: it is here because it is one of XERJ's nearest functional peers (a local code-search or code-intelligence tool with an MCP server), and the 2026-09-18 set had none. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://raw.githubusercontent.com/probelabs/probe/main/llms.txt> — HTTP 404
- Size: 0 bytes, 0 lines
- Install position: absent — no llms.txt at the repository root (404) or at https://probelabs.com/llms.txt, https://probelabs.com/probe/llms.txt (both 404, HTML). No llms-install.md, SKILL.md or AGENTS.md at the root either (404). Install lives in the README.
- Tone: n/a — there is no llms.txt. The README is the agent-install surface; see 'Other agent-facing files'.
- Section order: (no llms.txt is published)

### Install commands found

```sh
(README, MCP) "command": "npx", "args": ["-y", "@probelabs/probe@latest", "agent", "--mcp"]
(README, MCP) "command": "npx", "args": ["-y", "@probelabs/probe@latest", "mcp"]
(README) npx -y @probelabs/probe search "authentication AND login" ./src
(README) npm install -g @probelabs/probe
(README) curl -fsSL https://raw.githubusercontent.com/probelabs/probe/main/install.sh | bash
(README) iwr -useb https://raw.githubusercontent.com/probelabs/probe/main/install.ps1 | iex
(README) cargo install --path .
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

````text
(from the README, '### Option 2: Raw Probe Tools via MCP':)
### Option 2: Raw Probe Tools via MCP

If you prefer direct access to search/query/extract tools without the agent layer:

```json
{
  "mcpServers": {
    "probe": {
      "command": "npx",
      "args": ["-y", "@probelabs/probe@latest", "mcp"]
    }
  }
}
```
````

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
## Quick Start

### Option 1: Probe Agent via MCP (Recommended)

Our built-in agent natively integrates with Claude Code, using its authentication—no extra API keys needed.

Add to `~/.claude/claude_desktop_config.json`:
```json
{
  "mcpServers": {
    "probe": {
      "command": "npx",
      "args": ["-y", "@probelabs/probe@latest", "agent", "--mcp"]
    }
  }
}
```

The Probe Agent is purpose-built to read and reason about code. It piggybacks on Claude Code's auth (or Codex auth), or works with any model via your own API key (e.g., `GOOGLE_API_KEY`).
````

## llms-full.txt

- URL: <https://probelabs.com/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Neither file exists.
- Install position: absent

## Other agent-facing files

### README.md (the agent-install surface) — HTTP 200

<https://raw.githubusercontent.com/probelabs/probe/main/README.md>

17112 bytes, 554 lines. 'Quick Start' is four options in this order: agent via MCP, raw tools via MCP, direct CLI, CLI agent. The first two are an `mcpServers` JSON block whose command is `npx`, so registering the server IS the install. A separate 'Installation' section (npm, curl, PowerShell, source) comes 350 lines later. The README gives one config path, `~/.claude/claude_desktop_config.json`, and no per-client variants or verify step.

````text
### The key insight: AI agents don't need embedding search

Embedding-based tools solve vocabulary mismatch -- finding "authentication" when the code says `verify_credentials`. But when an AI agent is the consumer, **the LLM already handles this**:

```
User: "find the authentication logic"
  -> LLM generates: probe search "verify_credentials OR authenticate OR login OR auth_handler"
  -> Probe returns complete AST blocks in milliseconds
```

The LLM translates intent into precise boolean queries. Probe gives it a powerful query language (`AND`, `OR`, `+required`, `-excluded`, `"exact phrases"`, `ext:rs`, `lang:python`) purpose-built for this. Combined with session dedup, the agent can run 3-4 rapid searches and cover more ground than a single embedding query -- faster, deterministic, and with zero setup cost.
````

### docs/probe-agent/protocols/mcp-integration.md — HTTP 200

<https://raw.githubusercontent.com/probelabs/probe/main/docs/probe-agent/protocols/mcp-integration.md>

20666 bytes. Tool parameter tables (every tool takes an absolute `path`), environment overrides including a custom binary path, and a Troubleshooting section.

```text
## Troubleshooting

### Common Issues

#### Binary Not Found

If you encounter a "Binary not found" error:

1. Check if the Probe binary exists in the expected location
2. Set the PROBE_PATH environment variable to the location of your Probe binary
3. Try reinstalling the @probelabs/probe@latest package

#### Permission Denied

If you encounter a "Permission denied" error:

1. Make sure the binary is executable (`chmod +x /path/to/probe`)
2. Check if your user has permission to execute the binary
3. Try running the MCP server with elevated privileges

#### Network Error During Binary Download

If you encounter a network error during binary download:

1. Check your internet connection
2. Verify that GitHub API is accessible from your network
3. Try downloading the binary manually and placing it in the bin directory

#### Tool Calls Timeout

If tool calls timeout:

1. Increase the timeout value in your AI editor's configuration
2. Try a simpler query that will execute faster
3. Limit the search scope to a smaller directory

#### Empty Search Results

If you get empty search results:

1. Check your query syntax
2. Try a simpler query
[...]
```

### CLAUDE.md (contributor file) — HTTP 200

<https://raw.githubusercontent.com/probelabs/probe/main/CLAUDE.md>

38832 bytes, 1119 lines. For agents working ON probe: testing rules, error handling, build notes. Its second paragraph tells the agent to dogfood the tool. No install or feedback content for users.

```text
**IMPORTANT**: ALWAYS use code-search-agent to navigate the codebase and ask questions about it.
```

### llms.txt / llms-install.md / SKILL.md / AGENTS.md at the repository root — HTTP 404

<https://raw.githubusercontent.com/probelabs/probe/main/llms-install.md>

All four 404.

## What they do better than XERJ

- Registration is the install. `npx -y @probelabs/probe@latest mcp` inside the `mcpServers` block downloads and runs the binary; a shell-less user pastes one JSON block into a client and is done. XERJ's equivalent needs three separate steps first (install, start a node, index).
- No server to start and no index to build before the first query: the tool searches files on demand. XERJ's first query is gated on a running node and a finished `autoindex`.
- The README states its retrieval position in one table and one paragraph, including what it does NOT use ('External dependencies: None').
- Every MCP tool takes an explicit absolute `path`, so which folder is searched is never implicit. XERJ's `xerj_search` requires an `index` name the agent has to discover.

## What XERJ does better

- XERJ publishes llms.txt, llms-full.txt, tool schemas in three shapes and question-shaped answer pages. probe publishes none of these; an agent has the README.
- XERJ indexes non-code formats (PDF, DOCX, SQLite, CSV, logs, SQL dumps) and keeps a persistent, queryable index with an HTTP API. probe is scoped to source trees.
- XERJ's installer verifies a SHA-256 and documents a no-`curl | sh` path. probe's README prints `curl … | bash` and `iwr … | iex` with no integrity statement.
- XERJ's docs name the config root key per client and warn about the desktop PATH problem; probe's README gives one path and one JSON shape.

## Adoptable ideas

- A package-runner wrapper (proposed — not implemented): an npm package whose `npx -y <pkg> mcp` downloads the verified release binary and execs `xerj mcp`. It removes the install step from the shell-less hand-off block. It does not remove the need for a running node, so it only helps if `xerj mcp` can also start or find one.
- Say in one table what XERJ's default retrieval is and is not (BM25 + feature-hashing vector; no embedding model, no network) — the same table is the lexical-by-default correction.
- Let `xerj_search` default `index` to `ax-*` (proposed — not implemented). RAN 2026-09-19: a call without `index` returns 'missing or empty required string field `index`', which is the first thing an MCP-only agent will hit.
