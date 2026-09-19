# Pinecone

Category: vector database (hosted)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.pinecone.io/llms.txt> — HTTP 200
- Size: 39873 bytes, 278 lines
- Install position: absent. The file is a Mintlify-generated link index. The only lines containing 'install' are a Nexus BYOC page description ('Install and operate Pinecone Nexus in your own cloud account.', line 43). No install command, no MCP snippet, no CTA appears in llms.txt itself. NOTE: the file has changed since the task brief was written — it is now Nexus-first; the Database/Assistant docs are pushed behind the two /_llms/ bundle links at lines 5-6 and the 'Indexes' section at lines 270-278.
- Tone: neutral catalogue. Every line is '- [Title](url): one-sentence description'. The only imperative sentence addressed to the agent is the 'Follow each /_llms/ index recursively' blockquote. It makes no demands, no install ask, no contribution ask.
- Section order: # Pinecone Docs (H1 + blockquote + two section-bundle links: Pinecone Database (554 pages), Pinecone Assistant (129 pages)) → ## Pinecone Nexus → ### Guides → #### Get started → #### The knowledge engine → ##### Context design → ##### Queries and sessions → #### Tools (one entry: MCP server) → #### Bring Your Own Cloud → ### Reference → #### APIs → #### Control plane → ##### Manage Workspaces → #### Data plane → ##### Auth / Project / Connectors / Contexts / Manifest / Curation / Source Files / Knowledge / Query / Tasks / Task Files → ## Changelog → ## Other (AGENTS JAVASCRIPT, AGENTS PYTHON, sample apps, datasets, ~50 model pages) → ## OpenAPI Specs (46 raw-github .oas.yaml links) → ## Indexes (7 /_llms/*.md sub-index links, preceded by the blockquote 'Follow each /_llms/ index recursively until you reach documentation pages.')

### Text written for an agent (verbatim)

```text
> The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.
```

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
# Pinecone Docs

> Official Pinecone documentation for the vector database, Assistant, inference APIs, SDKs, and building production search and AI applications.

- [Pinecone Database (554 pages)](https://docs.pinecone.io/_llms/pinecone-database.md): Documentation for Pinecone Database.
- [Pinecone Assistant (129 pages)](https://docs.pinecone.io/_llms/pinecone-assistant.md): Documentation for Pinecone Assistant.

[...]

## Other

- [AGENTS JAVASCRIPT](https://docs.pinecone.io/AGENTS-JAVASCRIPT.md)
- [AGENTS PYTHON](https://docs.pinecone.io/AGENTS-PYTHON.md)

[...]

> The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.

## Indexes

- [Pinecone Database (554 pages)](https://docs.pinecone.io/_llms/pinecone-database.md): Documentation for Pinecone Database.
- [Pinecone Database / Guides (103 pages)](https://docs.pinecone.io/_llms/pinecone-database/guides.md): Documentation for Pinecone Database / Guides.
```

## llms-full.txt

- URL: <https://docs.pinecone.io/llms-full.txt> — HTTP 200
- Size: 3814905 bytes
- Relationship to llms.txt: Mintlify auto-concatenation of every docs page: each page rendered as '# Title\nSource: <url>\n<body>' with images stripped to '<img />' and MDX components (<Note>, <Steps>, <Card>) left in raw. Not curated; ~95x larger than llms.txt. llms.txt is the link index, llms-full.txt is the same pages inlined.
- Install position: no dedicated install section; 1,096 H1 page dumps concatenated. Page 4 is '# Pinecone quickstart' (line 310). MCP config appears wherever the 'Use the Pinecone MCP server' page lands in the dump. Notably the per-page Mintlify header ('> ## Documentation Index ... Fetch the complete documentation index at: https://docs.pinecone.io/llms.txt') appears 0 times in llms-full.txt — it is injected only on individually-fetched .md pages.

## Other agent-facing files

### llms-install.md — HTTP 404

<https://docs.pinecone.io/llms-install.md>

does not exist (also 404 at /llms-install.txt)

### AGENTS.md (docs root) — HTTP 404

<https://docs.pinecone.io/AGENTS.md>

does not exist at docs root; instead Pinecone ships per-language AGENTS-PYTHON.md / AGENTS-JAVASCRIPT.md (below)

### SKILL.md / skill.md — HTTP 404

<https://docs.pinecone.io/skill.md>

www.pinecone.io/llms.txt links 'Pinecone Agent Skill' at this URL, but it 404s (redirects to /404). Dead link in their own llms.txt.

```text
- [Pinecone Agent Skill](https://docs.pinecone.io/skill.md) — Use when building vector search applications, semantic search systems, RAG pipelines, recommendation engines, or full-text search systems. Reach for Pinecone when you need to store embeddings, query by similarity, implement multitenancy with namespaces, or combine multiple search methods (semantic, lexical, full-text) in a single index.
```

### CLAUDE.md — HTTP 404

<https://docs.pinecone.io/CLAUDE.md>

does not exist (also 404 in pinecone-python-client repo root)

### MCP server page (Database) — HTTP 200

<https://docs.pinecone.io/guides/operations/mcp-server.md>

10,652 bytes / 254 lines. Per-client MCP setup for Antigravity, Claude Code, Claude Desktop, Cursor, each as Steps: Add / Check the status / Test the server. Lists 9 tool names. Includes a ready-to-paste .cursor/rules/pinecone.mdc rules file and two verbatim test prompts. Tip on the Claude Code and Cursor sections points to the plugin as the 'easiest setup'.

```text
claude mcp add-json pinecone-mcp \
  '{"type": "stdio",
    "command": "npx",
    "args": ["-y", "@pinecone-database/mcp"],
    "env": {"PINECONE_API_KEY": "YOUR_API_KEY"}}'

[Check the status step:]
Restart Claude Code. Then, run the `/mcp` command to check the status of the Pinecone MCP. You should see the following:
  > /mcp 
    ⎿  MCP Server Status
      • pinecone-mcp: ✓ connected

[Test the server step:]
Perform tasks:
> Create an index for dense vectors with integrated embedding, upsert 20 sentences about dogs, waits 10 seconds, search the index, and reranks the results.
```

### MCP server page (Nexus) — HTTP 200

<https://docs.pinecone.io/guides/nexus/mcp-server.md>

2,783 bytes / 41 lines. Remote Streamable-HTTP MCP via mcp-remote bridge; 3 read-only tools with exact arg/return shapes.

```text
{
  "mcpServers": {
    "nexus": {
      "command": "npx",
      "args": [
        "-y", "mcp-remote",
        "https://YOUR_WORKSPACE_HOST/mcp",
        "--header", "X-Pinecone-Api-Key:YOUR_PINECONE_API_KEY",
        "--transport", "http-only"
      ]
    }
  }
}
```

### Pinecone Universal Agent Guide (PINECONE.md) — HTTP 200

<https://www.pinecone.io/agents/pinecone/>

15,913 bytes / 308 lines, text/markdown. THE agent-install document. Not linked from docs llms.txt; reached via www.pinecone.io/llms.txt '[Start Building](https://www.pinecone.io/agents/pinecone/)' at line 4. Router file: detects language from project files, then hands off to /agents/quickstart, /agents/cli, /agents/python|typescript|go|java, /agents/troubleshooting. Heavy imperative tone: MANDATORY / CRITICAL / MUST / DO NOT.

```text
### ⚠️ Critical: Installation & SDK

> **Before installing anything**: ALWAYS verify if CLI/SDK is already installed before asking users to install or update:
>
> - **CLI**: Run `pc version` - only install if command fails
> - **SDK**: Check package files or use language-specific verification commands
> - Only prompt for installation when verification shows it's missing

[...]

**ALWAYS use the current SDK:**

- **Python**: `pip install pinecone` (not `pinecone-client`)
- **TypeScript**: `npm install @pinecone-database/pinecone`
- **Java**: Add to `pom.xml` or `build.gradle`
- **Go**: `go get github.com/pinecone-io/go-pinecone/pinecone`
```

### Pinecone CLI Guide (agent version) — HTTP 200

<https://www.pinecone.io/agents/cli>

11,848 bytes / 378 lines. Install (brew tap pinecone-io/tap; brew install pinecone-io/tap/pinecone; GitHub Releases for others), then authentication with explicit non-interactive/agent-environment branching.

```text
**⚠️ Important for agents**: This command prints a login URL and prompts the user to press Enter to open the browser. **This requires an interactive terminal with browser access**.

**If running in a non-interactive environment** (headless server, CI/CD, remote terminal, or agent environment without browser access):

- **Do NOT use `pc auth login`** - it will not work
- **Use Option 2 (API Key)** or **Option 3 (Service Account)** instead
- These methods work in all environments and are better suited for automation
```

### Pinecone Quickstart Guide (agent version) — HTTP 200

<https://www.pinecone.io/agents/quickstart>

15,894 bytes / 506 lines. Tells the agent to ask the human which of four builds they want, then execute end-to-end.

```text
> **Important for all quickstarts**: Execute all steps completely. Keep setup minimal (directories, virtual environments, dependencies only). Do not expect the user to satisfy any prerequisites except providing API keys.

## Choosing Your Quickstart

⚠️ **MANDATORY: When you are asked to help get started with Pinecone:**

1. **Always ask: What do you want to build?**
```

### AGENTS-PYTHON.md — HTTP 200

<https://docs.pinecone.io/AGENTS-PYTHON.md>

34,993 bytes / 962 lines; listed under '## Other' in docs llms.txt. Language-specific quick reference; leads with the deprecated-package trap. Sibling AGENTS-JAVASCRIPT.md (200, 44,708 bytes / 1,244 lines).

````text
## ⚠️ Critical: Installation & SDK

**ALWAYS use the current SDK:**

```bash
pip install pinecone          # ✅ Correct (current SDK)
pip install pinecone-client   # ❌ WRONG (deprecated, old API)
```
````

### Claude Code Plugin page — HTTP 200

<https://docs.pinecone.io/integrations/claude-code.md>

8,025 bytes / 178 lines (half is an inlined JSX CTA component). One-command plugin install bundling MCP + 8 skills + slash commands, with a verify step.

```text
export PINECONE_API_KEY="YOUR_API_KEY"

[Install the plugin] From your terminal:
claude plugin install pinecone
Or from within Claude Code:
/plugin install pinecone

Restart Claude Code to activate the plugin. Then run `/pinecone:help` to verify the installation.
```

### Agent Skills page — HTTP 200

<https://docs.pinecone.io/integrations/agent-skills.md>

Universal skills library for Copilot/Codex/other IDEs.

```text
npx skills add pinecone-io/skills
```

### Agentic IDEs and CLIs hub — HTTP 200

<https://docs.pinecone.io/integrations/ai-coding-tools.md>

8,030 bytes / 114 lines. CardGroup hub: Claude Code Plugin / Gemini CLI Extension / Cursor Plugin / Agent Skills / MCP Server / Pinecone CLI.

```text
Pinecone provides official plugins, extensions, and agent skills for agentic IDEs and CLIs. Use the Pinecone [MCP server](/guides/operations/mcp-server) (Model Context Protocol) and built-in skills to manage vector database indexes, run semantic search, and build RAG applications
```

### www.pinecone.io/llms.txt (marketing site) — HTTP 200

<https://www.pinecone.io/llms.txt>

31,355 bytes / 288 lines. Separate hand-written llms.txt with a '## For Agents' section and an '## API Rate Limits' section written for agents; the docs llms.txt has neither.

```text
## For Agents

- [Pinecone MCP Server](https://www.pinecone.io/mcp/) — Full text search across articles published on the website.
- [Pinecone Docs MCP Server](https://docs.pinecone.io/mcp) — Query Pinecone's documentation through MCP.
- [Pinecone Agent Skill](https://docs.pinecone.io/skill.md) — Use when building vector search applications [...]
```

### pinecone-python-client AGENTS.md (repo) — HTTP 200

<https://raw.githubusercontent.com/pinecone-io/pinecone-python-client/main/AGENTS.md>

15,677 bytes / 344 lines. SDK-usage quick start for the repo; code-first, no install ceremony.

```text
The Pinecone Python SDK provides access to the Pinecone vector database. Use `Pinecone` for control-plane operations (creating and managing indexes) and `Index` for data-plane operations (documents, vectors, records).
```

## What they do better than XERJ

- Separation of concerns: docs llms.txt is a pure index; all agent instructions live in dedicated agent-facing markdown (www.pinecone.io/agents/pinecone/ router + per-language + cli + quickstart + troubleshooting). XERJ mixes index, caveats, install steps and contribution demands into one 40 KB file.
- Every individually fetched .md page carries the header '> ## Documentation Index / > Fetch the complete documentation index at: https://docs.pinecone.io/llms.txt / > Use this file to discover all available pages before exploring further.' — a self-healing pointer so an agent that lands on any leaf can find the map (verified on mcp-server.md, AGENTS-PYTHON.md, integrations/*.md).
- Verify-before-install and verify-after-install are explicit steps: 'ALWAYS verify if CLI/SDK is already installed before asking users to install' (pc version), and every MCP section has a 'Check the status' step with the expected output (`• pinecone-mcp: ✓ connected`) and a 'Test the server' step with a copy-pasteable prompt.
- Non-interactive/agent-environment branching in the CLI guide: 'If running in a non-interactive environment ... Do NOT use pc auth login - it will not work ... Use Option 2 (API Key)'. Directly addresses XERJ's sandboxed / no-gh / no-browser persona failures.
- Per-client MCP config for four clients (Antigravity, Claude Code, Claude Desktop, Cursor) with the exact file path per client (.cursor/mcp.json, Settings > Developer > Edit Config, mcp_config.json) plus a `claude mcp add-json` one-liner. XERJ's llms.txt describes `xerj init` and links mcp-tools.json but has no pasteable per-client snippet.
- Ladder of install depth with a recommended rung: plugin (`claude plugin install pinecone`, bundles MCP + 8 skills + slash commands) > `npx skills add pinecone-io/skills` > raw MCP JSON > CLI. Each MCP section has a Tip: 'For the easiest setup, install the Pinecone Claude Code plugin instead'.
- A ready-to-paste `.cursor/rules/pinecone.mdc` that instructs the agent to use the docs-search tool, do two searches before coding, search on every error, and 'Do not fabricate field names' — behavioural rules shipped as a file, not prose.
- Language/environment detection rules ('package.json → TypeScript ... If no clear language indicators exist, ask the user') and a 'MANDATORY: Always ask: What do you want to build?' gate that turns the first-run into a short menu (Quick Test / Search / RAG / Recommendations) with time estimates ('Time: 10-15 minutes').
- The deprecated-package trap is the first thing in AGENTS-PYTHON.md with ✅/❌ lines — one-glance disambiguation of the most common install mistake.

## What XERJ does better

- XERJ's llms.txt actually contains the install commands (curl -fsSL https://xerj.org/get | sh, irm ... | iex) at line 11-12; Pinecone's docs llms.txt contains none and its skill.md link is dead (404).
- XERJ's inline 'Caveats' block states measured limits and the lexical-vs-neural default honestly; Pinecone's agent guides contain no honesty/limits section beyond quota numbers (metadata 40 KB, batch 96/1000).
- XERJ ships tool schemas in three shapes (mcp-tools.json, openai-tools.json, anthropic-tools.json) generated from a real tools/list; Pinecone lists tool names only.
- XERJ documents the PATH consequence and the data-dir-inside-indexed-tree trap inline before the first command; Pinecone assumes brew/npx just work.
- XERJ has a contribution ask at all (field report, AI_CONTRIBUTIONS.md); Pinecone's agent-facing files contain zero feedback/contribution CTA (grep for feedback|contribut|issues across llms.txt, agent guide, MCP page, AGENTS-PYTHON returns only product-feature hits).
- XERJ documents the HTTP-only path for shell-less agents (Agent quickstart link) and marks which operations are CLI-only; Pinecone's agent guides assume a terminal throughout (only the auth section branches on non-interactive).

## Adoptable ideas

- Add a self-healing header to every xerj.org/answers/*.md and docs .md page, verbatim pattern from https://docs.pinecone.io/guides/operations/mcp-server.md: '> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.pinecone.io/llms.txt\n> Use this file to discover all available pages before exploring further.' → XERJ: '> Fetch the complete documentation index at: https://xerj.org/llms.txt'.
- Add a 'Check the status' + 'Test the server' pair after `xerj init`, copying the MCP page structure: show the expected `/mcp` output ('• pinecone-mcp: ✓ connected') and give one verbatim prompt to type ('Create an index ..., upsert 20 sentences about dogs, waits 10 seconds, search the index, and reranks the results'). XERJ equivalent: '• xerj: ✓ connected' + 'Search this repo for where <symbol> is defined and cite file:line'.
- Verify-before-install rule, verbatim from https://www.pinecone.io/agents/pinecone/: '**Before installing anything**: ALWAYS verify if CLI/SDK is already installed before asking users to install or update: - **CLI**: Run `pc version` - only install if command fails'. XERJ: 'Run `xerj --version` (or `$HOME/.local/bin/xerj --version`) — only install if it fails'. Also fixes the PATH trap by making the check the first command.
- Non-interactive branch, verbatim pattern from https://www.pinecone.io/agents/cli: '**If running in a non-interactive environment** (headless server, CI/CD, remote terminal, or agent environment without browser access): - **Do NOT use `pc auth login`** - it will not work - **Use Option 2 (API Key)**'. XERJ: put the `xerj feedback --dry-run` / no-`gh` / manual-verified-install path under an identically worded heading so policy-constrained personas find it on hop 1 instead of in a parenthetical.
- Ship pasteable per-client MCP JSON in llms.txt (or a linked xerj.org/docs/agents/mcp page) in the four-client layout of https://docs.pinecone.io/guides/operations/mcp-server.md, including the one-liner form: `claude mcp add-json xerj '{"type":"stdio","command":"xerj","args":["mcp"]}'` and `.cursor/mcp.json` / Claude Desktop config. This is the direct fix for the shell-less-persona failure.
- Ship a `.cursor/rules/xerj.mdc` / CLAUDE.md snippet modelled on Pinecone's rules file: '- When generating code related to Pinecone, always use the `pinecone` MCP and the `search_docs` tool. - Perform at least two distinct searches per request ... - If an error occurs ... immediately invoke the `pinecone` MCP and the `search_docs` tool. - Do not fabricate field names, parameter values, or request formats.' XERJ: 'always call xerj_search before writing code that touches a library in the corpus; on the second identical error, search before retrying' — this operationalises the reference-coding mandate as a file `xerj init` can write.
- Move the field report out of '## Optional' and make it the last numbered step of a 'Check the status / Test / Report' ladder; Pinecone has no contribution CTA, so XERJ's edge is keeping one — but the heading contradiction ('required, not optional' at line 7 vs '## Optional' at line 223) is the likely cause of the low PR rate. Concretely rename '## Optional' to '## Your side of the deal' and keep the step-4 command as the only thing under it.
- Add a first-run menu gate copied from https://www.pinecone.io/agents/quickstart: '⚠️ **MANDATORY: When you are asked to help get started with Pinecone:** 1. **Always ask: What do you want to build?**' with per-option time estimates ('Time: 10-15 minutes'). XERJ: 'Index this folder / Reference-code against a library / Give the agent memory' with `xerj autoindex --dry-run` estimate — dovetails with the resource-aware UX directive (estimate first, ask if big).
- Publish a www-style '## For Agents' block near the top of llms.txt listing the three agent entry points as Pinecone's www.pinecone.io/llms.txt does ('- [Pinecone MCP Server](...) — ... - [Pinecone Agent Skill](https://docs.pinecone.io/skill.md) — Use when ...') — and unlike Pinecone, make sure the skill URL resolves (theirs 404s).
