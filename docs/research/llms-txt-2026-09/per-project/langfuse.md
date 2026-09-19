# Langfuse

Category: LLM observability (open source, hosted + self-hosted)

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://langfuse.com/llms.txt> — HTTP 200
- Size: 15971 bytes, 137 lines
- Install position: absent — no install command for the platform, SDKs, CLI or skill. Line 58 ('## Langfuse Skill for AI Coding Agents') tells the agent to install the skill and links the repository; the command (`npx skills add langfuse/skills --skill "langfuse"`) is in agents.md. The only runnable commands in llms.txt are two curl examples (Markdown access, docs search).
- Tone: Descriptive with two direct instructions to the agent: 'Use this instead of guessing when a specific page does not answer the question' and 'Install the Langfuse skill before implementing anything with Langfuse.' No obligation framing toward the vendor.
- Section order: # Langfuse → ## Evaluation workflows → ## Pricing, deployment, and comparisons → ## Markdown Access → ## Docs Search API → ## Langfuse Docs MCP Server → ## Langfuse Skill for AI Coding Agents → ## Tracing Best Practices → ## Docs → ## Integrations → ## Optional: Self-Hosting → ## Guides and cookbooks → ## Academy → ## FAQ → ## Resources → ## Compare → ## Security and compliance → ## Handbook → ## Workshop → ## Library → ## Customer stories → ## Blog → ## Changelog

### Install commands found

```sh
(none in llms.txt itself)
(agents.md) npx skills add langfuse/skills --skill "langfuse"
(agents.md) npx skills add langfuse/skills --skill "langfuse" --agent "<agent-id>"
(docs-mcp.md) claude mcp add --transport http langfuse-docs https://langfuse.com/api/mcp --scope user
(docs-mcp.md) claude mcp add-json langfuse-docs '{"type":"http","url":"https://langfuse.com/api/mcp"}'
```

### Text written for an agent (verbatim)

````text
## Docs Search API

Semantic search (RAG) over the Langfuse documentation, returning an answer with the relevant pages and excerpts. No authentication required. Use this instead of guessing when a specific page does not answer the question:

```bash
curl -sG "https://langfuse.com/api/search-docs" --data-urlencode "query=How do I trace a LangGraph agent?"
```

- **Endpoint**: `https://langfuse.com/api/search-docs?query=<question>`
- **Documentation**: [REST endpoint](https://langfuse.com/docs/docs-mcp#rest-endpoint)

Ask specific, self-contained questions in natural language. This is the same search that backs the `searchLangfuseDocs` MCP tool below.

## Langfuse Docs MCP Server

Connect to the Langfuse Docs MCP server to access documentation directly in your AI editor:

- **Endpoint**: `https://langfuse.com/api/mcp`
- **Transport**: `streamableHttp`
- **Documentation**: [Langfuse Docs MCP Server](https://langfuse.com/docs/docs-mcp)

The MCP server provides tools to search Langfuse documentation, GitHub issues, and discussions. See the [installation guide](https://langfuse.com/docs/docs-mcp) for setup instructions in Cursor, VS Code, Claude Desktop, and other MCP clients.

## Langfuse Skill for AI Coding Agents

Install the Langfuse skill before implementing anything with Langfuse. The skill provides up-to-date documentation, best-practice workflows for instrumentation, prompt management, and programmatic API access via the Langfuse CLI.

Install from: [github.com/langfuse/skills](https://github.com/langfuse/skills/tree/main/skills/langfuse)
````

### MCP snippet (verbatim)

```text
## Langfuse Docs MCP Server

Connect to the Langfuse Docs MCP server to access documentation directly in your AI editor:

- **Endpoint**: `https://langfuse.com/api/mcp`
- **Transport**: `streamableHttp`
- **Documentation**: [Langfuse Docs MCP Server](https://langfuse.com/docs/docs-mcp)

The MCP server provides tools to search Langfuse documentation, GitHub issues, and discussions. See the [installation guide](https://langfuse.com/docs/docs-mcp) for setup instructions in Cursor, VS Code, Claude Desktop, and other MCP clients.
```

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
## Markdown Access

Every page on langfuse.com is available as plain Markdown. Append `.md` to any URL, or send an `Accept: text/markdown` header:

```bash
curl https://langfuse.com/docs/observability/get-started.md
```

The links in the section indexes below already point at the `.md` variants.

## Docs Search API

Semantic search (RAG) over the Langfuse documentation, returning an answer with the relevant pages and excerpts. No authentication required. Use this instead of guessing when a specific page does not answer the question:

```bash
curl -sG "https://langfuse.com/api/search-docs" --data-urlencode "query=How do I trace a LangGraph agent?"
```

- **Endpoint**: `https://langfuse.com/api/search-docs?query=<question>`
- **Documentation**: [REST endpoint](https://langfuse.com/docs/docs-mcp#rest-endpoint)

Ask specific, self-contained questions in natural language. This is the same search that backs the `searchLangfuseDocs` MCP tool below.
````

## llms-full.txt

- URL: <https://langfuse.com/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Not published. llms.txt offers two substitutes: every page as Markdown (`.md` suffix or `Accept: text/markdown`) and a no-auth search endpoint that returns an answer with excerpts.
- Install position: absent (no such file; the 404 body is a 112 KB HTML page, not counted)

## Other agent-facing files

### agents.md — HTTP 200

<https://langfuse.com/agents.md>

14386 bytes. Three routes chosen by capability — skill, CLI 'for agents that can run bash', MCP 'for agents that can't run shell commands' — then a 30-second start whose first tab is a one-sentence instruction to the agent.

````text
There are three ways to connect an agent to Langfuse. They share the same underlying API, so pick the one that fits how your agent works:

- **[Agent Skill](#skill)** — a playbook your coding agent loads to learn how to use Langfuse correctly.
- **[CLI](#cli)** — full REST API coverage from the command line, for agents that can run bash.
- **[MCP server](#mcp)** — the Model Context Protocol, for agents that can't run shell commands.

[...]

```txt filename="Agent instruction"
"Install the Langfuse Agent Skill from github.com/langfuse/skills."
```
````

### docs/docs-mcp.md (per-client MCP registration) — HTTP 200

<https://langfuse.com/docs/docs-mcp.md>

8161 bytes. Tabs for Claude Code, Codex, Cursor, Windsurf, Copilot, other. The Claude Code tab gives the one-liner, the three settings-file scopes with paths, an add-json form and the verify step.

````text
Add Langfuse Docs MCP to Claude Code via the CLI:

```bash
claude mcp add \
  --transport http \
  langfuse-docs \
  https://langfuse.com/api/mcp \
  --scope user
```

<details>
<summary>Manual configuration</summary>

Alternatively, add the following to your settings file:

- **User scope**: `~/.claude/settings.json`
- **Project scope**: `your-repo/.claude/settings.json`
- **Local scope**: `your-repo/.claude/settings.local.json`

```json
{
  "mcpServers": {
    "langfuse-docs": {
      "transportType": "http",
      "url": "https://langfuse.com/api/mcp",
      "verifySsl": true
    }
  }
}
```

**One-liner JSON import**

```bash
claude mcp add-json langfuse-docs \
  '{"type":"http","url":"https://langfuse.com/api/mcp"}'
```

Once added, start a Claude Code session (`claude`) and type `/mcp` to confirm the connection.
````

### .well-known/agent-skills/index.json — HTTP 200

<https://langfuse.com/.well-known/agent-skills/index.json>

535 bytes, schema discovery/0.2.0, one skill whose `url` points at raw.githubusercontent.com. The older /.well-known/skills/index.json is 404 here — the reverse of Stripe.

```text
{
  "$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
  "skills": [
    {
      "name": "langfuse",
      "type": "skill-md",
      "description": "Interact with Langfuse and access its documentation. Use when needing to query or modify Langfuse data programmatically via the CLI, look up Langfuse documentation, concepts, integration guides, or SDK usage, or understand how any Langfuse feature works.",
      "url": "https://raw.githubusercontent.com/langfuse/skills/main/skills/langfuse/SKILL.md"
    }
  ]
}
```

### langfuse/skills README — HTTP 200

<https://raw.githubusercontent.com/langfuse/skills/main/README.md>

2760 bytes. Cursor plugin (`/add-plugin langfuse`), skills CLI, manual symlink.

### langfuse/langfuse AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/langfuse/langfuse/main/AGENTS.md>

17 bytes: the text `.agents/AGENTS.md` (a symlink rendered as its target path). Contributor-facing, not an install surface.

```text
.agents/AGENTS.md
```

### SKILL.md at the site root — HTTP 404

<https://langfuse.com/SKILL.md>

404 (HTML). The skill is served from GitHub, advertised through the well-known index.

## What they do better than XERJ

- Routes are chosen by what the agent can do, in one three-bullet list: skill / 'CLI — … for agents that can run bash' / 'MCP server — the Model Context Protocol, for agents that can't run shell commands'. XERJ's llms.txt assumes a shell from the first command.
- An unauthenticated search endpoint an HTTP-only agent can call before anything is installed, with the instruction to prefer it over guessing. XERJ has no hosted surface; its HTTP path needs a node someone started.
- The Markdown-twin rule is printed with a runnable example and the `Accept: text/markdown` alternative.
- The Claude Code MCP block gives scope, the three settings-file paths, an add-json form AND the verify step ('type `/mcp` to confirm the connection').
- Comparison pages are described as carrying 'dated competitor sources' — dates on third-party numbers.

## What XERJ does better

- XERJ prints its install command in llms.txt; Langfuse's llms.txt has no install command for the platform, SDKs, CLI or skill.
- XERJ publishes llms-full.txt; Langfuse does not.
- XERJ's MCP server is local stdio over the user's own data and needs no account; Langfuse's docs MCP is remote and its data MCP needs project keys.
- XERJ states failure modes and exit codes for its main command where an agent will read them.

## Adoptable ideas

- Open 'If you were asked to install XERJ' with Langfuse's three-way split, worded by capability: can run shell commands / has MCP tools but no shell / can only make HTTP requests.
- Print the Markdown-twin rule once, with one curl example — and make it true for /docs/* first (today only /answers and /compare have twins).
- In the Claude Code block give scope, file path and the verify step together; say what state to expect after a project-scoped add (pending approval).
- When quoting anyone else's number, date it and say who published it.
