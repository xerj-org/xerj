# claude-context (zilliztech/claude-context)

Category: code search MCP server (embeddings + hosted vector database) — functional peer

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. This project publishes NO llms.txt: it is here because it is one of XERJ's nearest functional peers (a local code-search or code-intelligence tool with an MCP server), and the 2026-09-18 set had none. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://raw.githubusercontent.com/zilliztech/claude-context/master/llms.txt> — HTTP 404
- Size: 0 bytes, 0 lines
- Install position: absent — no llms.txt, llms-install.md or SKILL.md at the repository root (all 404). Install lives in the README's 'Quick Start'.
- Tone: n/a — there is no llms.txt. The README is the agent-install surface; see 'Other agent-facing files'.
- Section order: (no llms.txt is published)

### Install commands found

```sh
(README) claude mcp add claude-context -e OPENAI_API_KEY=… -e MILVUS_ADDRESS=… -e MILVUS_TOKEN=… -- npx @zilliz/claude-context-mcp@latest
(README, Codex TOML) command = "npx"  args = ["@zilliz/claude-context-mcp@latest"]
(README, every JSON client) "command": "npx", "args": ["@zilliz/claude-context-mcp@latest"]
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

````text
Use the command line interface to add the Claude Context MCP server:

```bash
claude mcp add claude-context \
  -e OPENAI_API_KEY=sk-your-openai-api-key \
  -e MILVUS_ADDRESS=your-zilliz-cloud-public-endpoint \
  -e MILVUS_TOKEN=your-zilliz-cloud-api-key \
  -- npx @zilliz/claude-context-mcp@latest
```
````

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
### Usage in Your Codebase

1. **Open Claude Code**

   ```
   cd your-project-directory
   claude
   ```

2. **Index your codebase**:

   ```
   Index this codebase
   ```

3. **Check indexing status**:

   ```
   Check the indexing status
   ```

4. **Start searching**:

   ```
   Find functions that handle user authentication
   ```

🎉 **That's it!** You now have semantic code search in Claude Code.
````

## llms-full.txt

- URL: <https://raw.githubusercontent.com/zilliztech/claude-context/master/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Neither file exists.
- Install position: absent

## Other agent-facing files

### README.md (the agent-install surface) — HTTP 200

<https://raw.githubusercontent.com/zilliztech/claude-context/master/README.md>

23301 bytes, 753 lines. Prerequisites (a vector-database account and an embedding API key) → one `claude mcp add` stdio line → 16 collapsed per-client blocks (Codex CLI, Gemini CLI, Qwen Code, Cursor, Void, Claude Desktop, Windsurf, VS Code, Cherry Studio, Cline, Augment, Roo Code, Zencoder, LangChain/LangGraph, other). First use is four plain-language prompts. An evaluation claim is stated with a link to its directory.

````text
<summary><strong>OpenAI Codex CLI</strong></summary>

Codex CLI uses TOML configuration files:

1. Create or edit the `~/.codex/config.toml` file.

2. Add the following configuration:

```toml
# IMPORTANT: the top-level key is `mcp_servers` rather than `mcpServers`.
[mcp_servers.claude-context]
command = "npx"
args = ["@zilliz/claude-context-mcp@latest"]
env = { "OPENAI_API_KEY" = "your-openai-api-key", "MILVUS_TOKEN" = "your-zilliz-cloud-api-key" }
# Optional: override the default 10s startup timeout
startup_timeout_ms = 20000
```

3. Save the file and restart Codex CLI to apply the changes.
````

### README.md — first use as prompts — HTTP 200

<https://raw.githubusercontent.com/zilliztech/claude-context/master/README.md>

The 'Usage in Your Codebase' section: the user types four sentences; indexing is a tool the agent calls (`index_codebase`), not a shell step.

```text
### Available Tools

#### 1. `index_codebase`

Index a codebase directory for hybrid search (BM25 + dense vector).

#### 2. `search_code`

Search the indexed codebase using natural language queries with hybrid search (BM25 + dense vector).

#### 3. `clear_index`

Clear the search index for a specific codebase.

#### 4. `get_indexing_status`

Get the current indexing status of a codebase. Shows progress percentage for actively indexing codebases and completion status for indexed codebases.

---
```

### README.md — evaluation claim (published by Zilliz, not run by us) — HTTP 200

<https://raw.githubusercontent.com/zilliztech/claude-context/master/README.md>

One paragraph and a chart; links an `evaluation/` directory.

```text
## 📊 Evaluation

Our controlled evaluation demonstrates that Claude Context MCP achieves ~40% token reduction under the condition of equivalent retrieval quality. This translates to significant cost and time savings in production environments. This also means that, under the constraint of limited token context length, using Claude Context yields better retrieval and answer results.
```

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/zilliztech/claude-context/master/AGENTS.md>

9 bytes: the text `CLAUDE.md` (a symlink rendered as its target). One source of truth for every harness.

```text
CLAUDE.md
```

### CLAUDE.md (contributor file) — HTTP 200

<https://raw.githubusercontent.com/zilliztech/claude-context/master/CLAUDE.md>

6226 bytes. Monorepo layout and commands for agents working ON the project. No user install or feedback content.

## What they do better than XERJ

- Indexing is an MCP tool. After registration the user types 'Index this codebase' and the agent calls `index_codebase`; then 'Check the indexing status' calls `get_indexing_status`. An MCP-only agent can therefore reach a first result with no shell. XERJ's `autoindex` has no MCP tool and no HTTP route, so the shell-less path always needs the operator.
- Sixteen per-client blocks, each with that client's file, key and steps; the Codex block states the footgun inline: '# IMPORTANT: the top-level key is `mcp_servers` rather than `mcpServers`.'
- First use is written as things to SAY, not commands to run.
- Registration is the install (`-- npx @zilliz/claude-context-mcp@latest`).

## What XERJ does better

- XERJ runs with no account, no API key and no network after install. claude-context's Quick Start begins with two sign-ups (a hosted vector database and an embedding API), so in that configuration code chunks are sent to both; its FAQ links a fully local setup.
- XERJ publishes llms.txt, llms-full.txt and tool schemas; claude-context publishes none.
- XERJ states its retrieval quality limits in the agent-facing file; the README's headline number links to a directory rather than stating n, corpus and date inline.
- XERJ indexes documents and data files as well as code.

## Adoptable ideas

- Expose indexing over MCP (proposed — not implemented): `xerj_index {path}` and `xerj_index_status`, with the same estimate-and-decision gate `autoindex` has. It is the single change that would make the MCP-only surface complete without an operator. It needs a security decision first: a tool that makes a node read arbitrary local paths is a capability the operator must opt in to.
- Write the first-use step of the MCP surface as sentences the user can say.
- Add the Codex TOML block with the key-name warning to XERJ's per-client table: `codex mcp add <name> -- <command>` is the one-liner (OpenAI's Codex MCP docs), and `[mcp_servers.xerj]` in `~/.codex/config.toml` is the file form.
