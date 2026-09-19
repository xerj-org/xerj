# LlamaIndex (developers.llamaindex.ai; run-llama/llama_index, run-llama/llamaparse-agent-skills)

Category: agent framework / document parsing

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://developers.llamaindex.ai/llms.txt> — HTTP 200
- Size: 10908 bytes, 89 lines
- Install position: absent — the word 'install' appears only inside a link description on line 33: '[LlamaParse Platform Quickstart](https://developers.llamaindex.ai/llamaparse/): Install the SDK, get an API key, and run your first call against Parse, Extract, Classify, Split, or Index — all from one platform.' No install command anywhere in llms.txt. Installation lives one hop away, on the linked quickstart (which opens with a 'Using a coding agent?' callout) and on /for-agents/.
- Tone: neutral / reference. It makes no demands of the agent, gives no obligations, no 'required', no feedback ask. The only agent-directed instruction is descriptive: how to fetch raw Markdown and call the docs REST API. Everything else is a link index (auto-generated shape: title + URL + description, some entries without descriptions and with duplicated titles like two 'LlamaExtract Examples' and two 'Indexing').
- Section order: # LlamaIndex Documentation (title + one-line blockquote) → ## Accessing Documentation Programmatically → ## LlamaCloud → ## Python LlamaAgents & Workflows → ## Python Framework → ## For AI Agents

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## Accessing Documentation Programmatically

All documentation pages are available as raw Markdown by appending `index.md` to the page URL. For example, the page at `https://developers.llamaindex.ai/llamaparse/parse/getting_started/` has its Markdown source at `https://developers.llamaindex.ai/llamaparse/parse/getting_started/index.md`.

The site also exposes REST API endpoints for searching and browsing documentation:

- **Search**: `GET https://developers.llamaindex.ai/api/search?q=<query>&limit=10&section=<section>&full-content=true` — Full-text BM25 search across all docs. `section` and `full-content` are optional.
- **Grep**: `GET https://developers.llamaindex.ai/api/grep?q=<regex>&context=0&case-sensitive=false&max-results=100` — Regex pattern matching across documentation content.
- **Read**: `GET https://developers.llamaindex.ai/api/read?path=<doc-path>&startLine=0&endLine=500` — Retrieve content of a specific documentation page (e.g. `path=/llamaparse/parse/getting_started/`). Returns the first 500 lines by default. Use `startLine` and `endLine` to paginate through longer documents.
- **List**: `GET https://developers.llamaindex.ai/api/list?section=<section>&path=<path>&depth=2` — Browse the documentation tree structure. All parameters are optional.

All API endpoints return JSON with CORS enabled (`Access-Control-Allow-Origin: *`).

[...]

## For AI Agents
- [Using LlamaIndex with AI Agents](https://developers.llamaindex.ai/for-agents/): A map of the MCP servers, agent skills, plugins, and workflow nodes you can use to give an AI agent LlamaIndex capabilities.
- [MCP Documentation Search](https://developers.llamaindex.ai/for-agents/mcp/): Connect to our hosted MCP server to search LlamaIndex documentation
```

## llms-full.txt

- URL: <https://developers.llamaindex.ai/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: None — no llms-full.txt exists. Also tried https://docs.llamaindex.ai/llms.txt and /llms-full.txt: both 200 but redirect to https://developers.llamaindex.ai/python/framework/ and serve HTML (262,643 bytes), i.e. the old docs host is a stale directory entry, not a text file. Repo-root https://raw.githubusercontent.com/run-llama/llama_index/main/llms.txt: 404. Instead of a full-text dump, LlamaIndex exposes per-page raw Markdown (append index.md) plus the /api/search, /api/grep, /api/read, /api/list REST endpoints (verified: GET /api/search?q=install%20llama-cloud&limit=2 returned 200 JSON with "algorithm":"BM25").
- Install position: absent (file does not exist; 404 returns the site's HTML 404 page, 74,688 bytes)

## Other agent-facing files

### for-agents map page (/for-agents/index.md) — HTTP 200

<https://developers.llamaindex.ai/for-agents/index.md>

66-line routing page: a 'Pick your path' table by need, two hosted MCP servers, skills, plugins, n8n node, programmatic docs access. No install command on the page itself; each row links to the page with the command.

```text
## Pick your path

| What you want                                           | Use this                                                       |
| ------------------------------------------------------- | -------------------------------------------------------------- |
| My coding agent should parse and extract from documents | [Agent skills and plugins](#agent-skills-and-plugins)          |
| I want my agent to use the LlamaParse Platform remotely | [LlamaParse Platform MCP](/llamaparse/for-agents/mcp/index.md) |
| My agent should be able to search these docs            | [Documentation MCP](/for-agents/mcp/index.md)                  |
| I'm building a no-code document workflow                | [n8n node](/llamaparse/for-agents/n8n/index.md)                |
| I'm reading these docs programmatically                 | [Programmatic docs access](#programmatic-docs-access)          |
[...]
`https://developers.llamaindex.ai/mcp` gives an agent `search_docs`, `grep_docs`, and `read_doc` over this documentation site. Useful when you want an agent writing LlamaIndex code to look things up instead of guessing.
[...]
This documentation site is built to be read by agents, not just people:
```

### Docs MCP install page (/for-agents/mcp/index.md) — HTTP 200

<https://developers.llamaindex.ai/for-agents/mcp/index.md>

111 lines. Hosted docs-search MCP (search_docs / grep_docs / read_doc) with per-client install: Cursor one-click deeplink + mcp.json, Claude Code one-liner, Codex config.toml, and a LlamaIndex-agent Python example consuming the same MCP.

````text
### Cursor

You can [click to install to cursor directly](cursor://anysphere.cursor-deeplink/mcp/install?name=llama-index-docs\&config=eyJ1cmwiOiJodHRwczovL2RldmVsb3BlcnMubGxhbWFpbmRleC5haS9tY3AifQ%3D%3D) or add the following to your `mcp.json` configuration:

```
{
  "mcpServers": {
    "llama_index_docs": {
      "url": "https://developers.llamaindex.ai/mcp"
    }
  }
}
```

### Claude Code

Add the documentation search tools to your Claude Code agent with a single command:

```
claude mcp add llama-index-docs --transport http https://developers.llamaindex.ai/mcp
```

### OpenAI Codex
[...]
```
[mcp_servers.llama_index_docs]
url = "https://developers.llamaindex.ai/mcp"
```
````

### LlamaParse Platform MCP page (/llamaparse/for-agents/mcp/index.md) — HTTP 200

<https://developers.llamaindex.ai/llamaparse/for-agents/mcp/index.md>

310 lines, 18,483 bytes. The product MCP: OAuth vs API-key (headless) auth, one-click buttons for Cursor/VS Code/Codex/Claude Desktop, CLI one-liners for Claude Code and Codex, EU-region variant, then a tool-by-tool reference (getUserProjects, uploadFileByUrl, parseFile, parseWithLiteParse, estimateFileComplexity, classifyFile ...).

````text
**A LlamaCloud API key** works where a browser sign-in does not — headless agents, CI jobs, and clients with no OAuth support. Send it as the bearer token when you add the server:

```
claude mcp add --transport http llamaparse https://mcp.llamaindex.ai/mcp \
  --header "Authorization: Bearer $LLAMA_CLOUD_API_KEY"
```
[...]
Click the button to add the server in one click:

[Open in Cursor ](cursor://anysphere.cursor-deeplink/mcp/install?name=llamaparse\&config=eyJ1cmwiOiJodHRwczovL21jcC5sbGFtYWluZGV4LmFpL21jcCJ9)
[...]
[Open in VS Code ](vscode:mcp/install?%7B%22name%22%3A%22llamaparse%22%2C%22type%22%3A%22http%22%2C%22url%22%3A%22https%3A//mcp.llamaindex.ai/mcp%22%7D)
[...]
[Open connectors in Claude Desktop ](claude://claude.ai/settings/connectors)
[...]
[Open in Codex ](codex://settings)
[...]
codex mcp add llamaparse --url https://mcp.llamaindex.ai/mcp
````

### Skills and Plugins page (/llamaparse/for-agents/skills/index.md) — HTTP 200

<https://developers.llamaindex.ai/llamaparse/for-agents/skills/index.md>

74 lines. Two skills (llamaparse cloud, liteparse local/no-key) and four Claude Code/Codex plugins; install via npx skills add or the agent's own plugin marketplace slash command.

````text
```
npx skills add run-llama/llamaparse-agent-skills
```

To install a single skill, pass `--skill`:

```
npx skills add run-llama/llamaparse-agent-skills --skill llamaparse
```
[...]
Add the marketplace:

```
/plugin marketplace add run-llama/llamaparse-agent-plugins
```

Then enable a plugin with its slash command — for example `/liteparse:liteparse`, `/llamaparse:llamaparse`, or `/llamaparse-mcp:llamaparse-mcp`.
[...]
```
codex plugin marketplace add run-llama/llamaparse-agent-plugins
```
[...]
Tip

Use `liteparse` for fast, local parsing with no setup, and `llamaparse` (or `llamaparse-mcp`) when you need the LlamaParse Platform's advanced parsing and the rest of its document-processing tools.
````

### SKILL.md (llamaparse skill) — HTTP 200

<https://raw.githubusercontent.com/run-llama/llamaparse-agent-skills/main/skills/llamaparse/SKILL.md>

220 lines, 6,972 bytes. Agent-executed skill with YAML front-matter (name/description/compatibility/license), a scripted opening message the agent must say verbatim, then numbered steps (Step 0 install package, Step 1 produce TS script ...). Skills repo tree: skills/liteparse, skills/llamacloud-index, skills/llamaparse. Repo README (75 lines) lists each skill with 'Use with' + 'Pre-requisites' + the npx install.

````text
---
name: llamaparse
description: Use this skill when the user asks to parse the content of an unstructured file (PDF, PPTX, DOCX...)
compatibility: Needs a `LLAMA_CLOUD_API_KEY` defined within the environment and the `@llamaindex/llama-cloud@latest` typescript library installed.
license: MIT
---
[...]
## Initial Setup

When this skill is invoked, respond with:

```
I'm ready to use LlamaParse to parse files. Before we begin, please confirm that:

- `LLAMA_CLOUD_API_KEY` is set as environment variable within the current environment
- `@llamaindex/llama-cloud@latest` is installed and available within the current Node environment

If both of them are set, please provide:

1. One or more files to be parsed
2. Specific parsing options, such as tier, API version, custom prompt, processing options...
3. Any requests you might have regarding the parsed content of the file.

I will produce a Typescript script to run the parsing job and, once you approved its execution, I will report the results back to you based on your request.
```

Then wait for the user's input.

## Step 0 — Install `llama-cloud` (optional)

If the user does not have the `@llamaindex/llama-cloud` package installed, add it to the current environment by running:

```bash
npm install @llamaindex/llama-cloud@latest
```
````

### LlamaParse Platform Quickstart (/llamaparse/index.md) — the 'install' page llms.txt links to — HTTP 200

<https://developers.llamaindex.ai/llamaparse/index.md>

1,234 lines. Human quickstart, but line 10-12 is a coding-agent callout placed ABOVE the human install tabs (Python/TypeScript/Go/Java/CLI: pip install llama-cloud>=2.8, npm install @llamaindex/llama-cloud).

````text
Using a coding agent?

Give your AI agent access to these docs: `claude mcp add llama-index-docs --transport http https://developers.llamaindex.ai/mcp` — or supercharge your agent with LlamaParse [MCP tools and Skills](/for-agents/index.md).

## Install

- [Python](#tab-panel-28)
- [TypeScript](#tab-panel-29)
- [Go](#tab-panel-30)
- [Java](#tab-panel-31)
- [CLI](#tab-panel-32)

```
pip install llama-cloud>=2.8
```

```
npm install @llamaindex/llama-cloud
```
````

### llms-install.md — HTTP 404

<https://developers.llamaindex.ai/llms-install.md>

does not exist

### AGENTS.md (run-llama/llama_index main) — HTTP 404

<https://raw.githubusercontent.com/run-llama/llama_index/main/AGENTS.md>

does not exist

### CLAUDE.md (run-llama/llama_index main) — HTTP 404

<https://raw.githubusercontent.com/run-llama/llama_index/main/CLAUDE.md>

does not exist

### hosted docs MCP endpoint — HTTP 405

<https://developers.llamaindex.ai/mcp>

Live MCP HTTP endpoint (GET returns JSON-RPC 'Method not allowed.', i.e. it is a real streamable-HTTP MCP server, not a doc page).

```text
{"jsonrpc":"2.0","error":{"code":-32000,"message":"Method not allowed."},"id":null}
```

## What they do better than XERJ

- Shell-less agents are a first-class audience: llms.txt line 5-16 gives four GET endpoints (search/grep/read/list) plus 'append index.md' so an agent with only HTTP fetch can search, grep and read every doc; XERJ's llms.txt is a link index whose links resolve to HTML pages for the most part and whose only search story assumes `xerj` on a shell (the 'Agent quickstart' HTTP-only path is one link among 20 under Start here).
- The hosted docs MCP is shipped, documented and installable in one line per client (`claude mcp add llama-index-docs --transport http https://developers.llamaindex.ai/mcp`, Cursor deeplink, Codex toml) — exactly the piece the 22-run study found XERJ had built but not shipped/documented. XERJ's llms.txt mentions `xerj mcp` only inside the last bullet of the Optional section.
- Per-client one-click links: `cursor://anysphere.cursor-deeplink/mcp/install?...`, `vscode:mcp/install?...`, `claude://claude.ai/settings/connectors`, `codex://settings`, plus `claude mcp add` / `codex mcp add` one-liners. XERJ has none.
- Need-based routing table ('Pick your path': What you want | Use this) on /for-agents/ instead of a wall of prose; an agent with a persona (no shell / has shell / building pipeline) finds its row in seconds.
- Headless path is called out explicitly with the exact command: 'A LlamaCloud API key works where a browser sign-in does not — headless agents, CI jobs, and clients with no OAuth support' followed by the `--header "Authorization: Bearer $LLAMA_CLOUD_API_KEY"` variant.
- Skills are installable through the agent's own package manager (`npx skills add run-llama/llamaparse-agent-skills`, `/plugin marketplace add run-llama/llamaparse-agent-plugins`) and the SKILL.md scripts the agent's first utterance verbatim ('When this skill is invoked, respond with: ...'), including a preflight confirm of env var + package before doing anything.
- Brevity: 89 lines / 10.9 KB versus XERJ's 234 lines / 40 KB; the top-of-file blockquote is one sentence, and the agent-directed matter is the first section, not mixed into a 3-paragraph preamble.
- Human quickstart carries an agent callout ABOVE the install tabs ('Using a coding agent? Give your AI agent access to these docs: `claude mcp add ...`'), so humans copying the doc into an agent get the MCP path for free.

## What XERJ does better

- Install is actually in llms.txt: `curl -fsSL https://xerj.org/get | sh` / `irm https://xerj.org/get.ps1 | iex` are lines 11-12 under `## Start here`; LlamaIndex's llms.txt contains zero install commands (only a link description that says 'Install the SDK').
- An ordered first-run sequence (start node → autoindex → init → query) with the PATH trap and the data-dir-inside-indexed-folder trap called out; LlamaIndex offers no sequence, only links.
- Inline Caveats block (lexical-not-neural default, HNSW vs exact, fusion modes, `_source` size, RSS growth) with measured numbers; LlamaIndex's llms.txt makes no capability claims at all and therefore corrects none.
- Honesty/positioning guidance for agents summarizing the project; measured benchmark links with losses published. LlamaIndex has nothing comparable.
- A feedback/contribution mechanism exists at all (`xerj feedback --open-pr`, CLA-exempt one-file PR, sandboxed fallback via --dry-run). LlamaIndex's llms.txt, for-agents page, MCP pages and SKILL.md contain no feedback, issue, or contribution ask whatsoever — so LlamaIndex is not a model for fixing the low-PR-count problem; it simply does not ask.
- Tool schemas in three shapes (mcp-tools.json / openai-tools.json / anthropic-tools.json) generated from a real tools/list; LlamaIndex documents tools in prose only.
- Exit-code, progress-line and estimate-before-run guidance for agents operating on a human's laptop.

## Adoptable ideas

- Ship a docs REST API for shell-less agents and put it as the FIRST section of llms.txt, copying the LlamaIndex shape verbatim-style: '## Accessing Documentation Programmatically ... - **Search**: `GET https://developers.llamaindex.ai/api/search?q=<query>&limit=10&section=<section>&full-content=true` — Full-text BM25 search across all docs.' / '- **Read**: `GET .../api/read?path=<doc-path>&startLine=0&endLine=500`' / 'All API endpoints return JSON with CORS enabled (`Access-Control-Allow-Origin: *`).' XERJ can serve this from a hosted XERJ node over xerj.org's own docs (dogfood: `GET https://xerj.org/api/search?q=...`), which directly fixes the 'shell-less personas fail at every tier' finding and is a live demo of the product.
- Add a hosted docs MCP and a per-client install block modelled on https://developers.llamaindex.ai/for-agents/mcp/index.md: '### Claude Code — Add the documentation search tools to your Claude Code agent with a single command: `claude mcp add llama-index-docs --transport http https://developers.llamaindex.ai/mcp`', a Cursor `cursor://anysphere.cursor-deeplink/mcp/install?name=<name>&config=<base64 {"url":...}>` deeplink, a `vscode:mcp/install?{...}` link, and a Codex `[mcp_servers.<name>]\nurl = "..."` toml. XERJ equivalents: `claude mcp add xerj-docs --transport http https://xerj.org/mcp` (hosted docs) and, for the local node, document `xerj mcp` with `claude mcp add xerj -- xerj mcp` at the top of Start here, not in the last Optional bullet.
- Replace the prose preamble with a need-routing table copied from /for-agents/index.md: '## Pick your path | What you want | Use this |' with rows such as 'I have a shell and a folder to search → Start here (curl | sh)', 'I have no shell, only HTTP → Agent quickstart HTTP-only path + docs API', 'My client speaks MCP → `claude mcp add ...`', 'I am summarizing/classifying XERJ → Positioning + Caveats', 'I finished a session → File the field report'. This gives each persona from the 22-run study its own row on hop 1.
- Copy the headless-credentials pattern for the auth-on node: 'A LlamaCloud API key works where a browser sign-in does not — headless agents, CI jobs, and clients with no OAuth support. Send it as the bearer token when you add the server: `claude mcp add --transport http llamaparse https://mcp.llamaindex.ai/mcp --header "Authorization: Bearer $LLAMA_CLOUD_API_KEY"`' → XERJ: `claude mcp add --transport http xerj http://localhost:9200/mcp --header "Authorization: ApiKey $XERJ_ADMIN_KEY"` (if/when an HTTP MCP transport exists), right next to the `admin.key` sentence.
- Publish the `xerj init` skill as an installable skill/plugin the way run-llama does: '```\nnpx skills add run-llama/llamaparse-agent-skills\n```' and '```\n/plugin marketplace add run-llama/llamaparse-agent-plugins\n```' → `npx skills add xerj-org/xerj-agent-skills` and `/plugin marketplace add xerj-org/xerj-agent-plugins`. Inside SKILL.md, script the first utterance and preflight as LlamaParse does ('When this skill is invoked, respond with: ... Before we begin, please confirm that: - `LLAMA_CLOUD_API_KEY` is set ... Then wait for the user's input.') → XERJ: confirm node reachable at :9200 and `xerj` on PATH before indexing, and — for the field-report problem — make the LAST step of the skill the report: 'Before ending the session, run `xerj feedback --dry-run`, show it to the user, then `--open-pr`.' A skill step executes far more reliably than a doc sentence filed under 'Optional'.
- Put the agent callout above the human install, copying the Quickstart's wording: 'Using a coding agent?\n\nGive your AI agent access to these docs: `claude mcp add llama-index-docs --transport http https://developers.llamaindex.ai/mcp` — or supercharge your agent with LlamaParse [MCP tools and Skills](/for-agents/index.md).' → on https://xerj.org/docs/install and /docs/quickstart: 'Using a coding agent? `claude mcp add xerj-docs --transport http https://xerj.org/mcp` — or `xerj init` in your project.'
- Rename and reposition XERJ's feedback ask. LlamaIndex is evidence that the section heading matters more than the adjective: their site has no ask at all, and XERJ's own doc says 'required, not optional' then files the template under '## Optional'. Adopt the LlamaIndex convention of naming sections by what the reader wants (not by obligation): rename '## Optional' to '## Before you end the session — the field report' and move the `xerj feedback --open-pr` one-liner to be the LAST numbered step of Start here (currently it is a second item numbered '4.', a duplicated number that reads as a typo), keeping the sandboxed fallback line verbatim: 'Sandboxed / no `gh`? Run `--dry-run`, then hand the printed report to your operator.'
- Trim: LlamaIndex's whole llms.txt is 89 lines / 10.9 KB with a one-sentence blockquote. XERJ's blockquote is a 120-word paragraph followed by two more bold paragraphs before the first command. Move the reference-coding pitch and 'contribute back' out of the blockquote into their own routing-table rows so the first screen is: one sentence, the table, the two install lines.
