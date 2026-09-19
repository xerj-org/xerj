# Qdrant

Category: vector database

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://qdrant.tech/llms.txt> — HTTP 200
- Size: 175811 bytes, 696 lines
- Install position: absent as content. The only mentions are link-index entries buried in ## Documentation: line 179 [Installation], line 180 [Local Quickstart], line 182 [Cloud Quickstart], line 200 [Agent Skills], line 201 [Qdrant MCP Server]. No install command appears anywhere in the file itself.
- Tone: neutral / catalogue. Docs-generator output (`# https://qdrant.tech/` as the H1, `## Overall Summary` boilerplate, every entry `- [Title](…/index.md): meta-description`). Makes no demands of the agent; contains no imperative sentence addressed to a reader at all.
- Section order: # https://qdrant.tech/ → ## Overall Summary → ## Documentation (lines 5-291, ~287 links) → ## Qdrant Academy (292-404) → ## Agenda (405) → ## Legal (408) → ## Qdrant Articles (415) → ## Qdrant Blog (502-684) → ## Qdrant Security: Compliance, Isolation, and Encryption Across Every Deployment (685) → ## RAG Use Case: Advanced Vector Search for AI Applications (688) → ## Vector Search Benchmarks (691-696)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
- [Installation](https://qdrant.tech/documentation/installation/index.md): Install Qdrant via Docker, Kubernetes, or binary releases — review CPU, memory, storage, and networking requirements for self-hosted deployments.
- [Local Quickstart](https://qdrant.tech/documentation/quickstart/index.md): Quickstart guide to running Qdrant locally with Docker, connecting an SDK, and building a first collection for semantic vector search.
- [Agent Skills](https://qdrant.tech/documentation/skills/index.md): Qdrant agent skills help AI coding assistants diagnose and tune vector search in production. Pass a skill URL from skills.qdrant.tech to your agent to get targeted guidance on scaling, search quality, performance, monitoring, and more.
- [Qdrant MCP Server](https://qdrant.tech/documentation/qdrant-mcp-server/index.md): Use the Qdrant MCP server to expose vector search as tools for AI assistants — power memory, retrieval, and context for agentic applications.
```

## llms-full.txt

- URL: <https://qdrant.tech/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: None — no llms-full.txt exists. Qdrant instead mirrors every doc page as `<page>/index.md` (each carrying a 3-line agent preamble) and runs a SEPARATE skills site with its own llms.txt at https://skills.qdrant.tech/llms.txt (200, 16,520 bytes, 43 lines) that is the real agent-onboarding layer.
- Install position: absent (file does not exist; also 404 at https://qdrant.tech/documentation/llms-full.txt and https://raw.githubusercontent.com/qdrant/qdrant/master/llms-full.txt)

## Other agent-facing files

### skills.qdrant.tech/llms.txt (second llms.txt, agent-skills catalogue) — HTTP 200

<https://skills.qdrant.tech/llms.txt>

Index of Agent-Skills-standard SKILL.md files; each description is a trigger list telling the agent WHEN to load it. This, not qdrant.tech/llms.txt, is where the agent-facing wording lives.

```text
- [qdrant-advisor](https://skills.qdrant.tech/meta/qdrant-advisor/SKILL.md): Diagnose, troubleshoot, and advise on any Qdrant deployment by loading the latest official Qdrant skills live from skills.qdrant.tech. Use this whenever someone raises a Qdrant problem or question — slow or degraded search, high or growing memory / OOM crashes, optimizer stuck or slow, […] Always prefer this skill over answering from memory: it pulls current, authoritative guidance and only the relevant context.
```

### documentation/skills/index.md (Agent Skills doc page, md mirror) — HTTP 200

<https://qdrant.tech/documentation/skills/index.md>

Explains the skills layer; the one-line install; the pass-a-URL-instead-of-installing alternative; the claude.ai-web-app caveat (a shell-less persona fix).

````text
You can also work with skills directly.
Pass the [skills.qdrant.tech](https://skills.qdrant.tech) URL to your agent and it will use it immediately, no installation required, which keeps your agent’s context focused on the problem at hand.
[…]
```bash
npx skills add qdrant/skills/meta/qdrant-advisor
```
[…]
> When you use the claude.ai web app, the Advisor can't fetch [skills.qdrant.tech](https://skills.qdrant.tech) on its own. You need to add `Use skills.qdrant.tech` to your prompt directly.
````

### Every docs index.md preamble (e.g. quickstart/index.md, installation/index.md) — HTTP 200

<https://qdrant.tech/documentation/quickstart/index.md>

Every markdown mirror page opens with the same 3 blockquote lines pointing an agent to the skills catalogue, a search endpoint, and llms.txt. Install commands on quickstart: `docker pull qdrant/qdrant` and `docker run -p 6333:6333 -p 6334:6334 -v "$(pwd)/qdrant_storage:/qdrant/storage:z" qdrant/qdrant`.

```text
> Explore Qdrant's agent skills catalog at https://skills.qdrant.tech/
> Search the documentation at https://skills.qdrant.tech/search?query=your+query+here
> Use this file to discover all available pages: https://qdrant.tech/llms.txt
```

### qdrant-advisor SKILL.md (meta-skill) — HTTP 200

<https://skills.qdrant.tech/meta/qdrant-advisor/SKILL.md>

A skill that ships no content, only a fetch-and-traverse protocol; written entirely in the imperative to the agent.

```text
Do not answer Qdrant questions from memory. Qdrant evolves quickly (new endpoints, metrics, defaults, and deployment patterns land often), and the authoritative, current guidance lives at `skills.qdrant.tech` as a hierarchy of agent skills. Your job is to **load the relevant skill context live, then ground your diagnosis in it** — loading only the branch that matches the problem, never the whole tree.

You are *consuming* these skills as context. You are **not** installing them and nothing needs to be installed.
```

### qdrant/skills README.md — HTTP 200

<https://raw.githubusercontent.com/qdrant/skills/main/README.md>

Per-agent install matrix (npx skills / Claude Code plugin marketplace / Cursor remote rule / clone-copy table with 5 agents' skill dirs), example prompts, and a 3-field bug-report ask.

````text
### Claude Code

Add the marketplace, then install all Qdrant skills:

```
/plugin marketplace add qdrant/skills
/plugin install qdrant@qdrant
```
[…]
## Getting Help

Found a bug or wrong advice in a skill? [Open an issue](https://github.com/qdrant/skills/issues/new) on GitHub and include:

- The skill name
- The prompt you gave your agent
- What the agent said vs what it should have said
````

### mcp-server-qdrant README.md — HTTP 200

<https://raw.githubusercontent.com/qdrant/mcp-server-qdrant/master/README.md>

MCP server install: uvx (zero-install), Smithery one-liner, Claude Desktop JSON, Cursor/Windsurf, Claude Code `claude mcp add` with verify step, VS Code one-click badges (insiders.vscode.dev/redirect/mcp/install?…). Contribution ask is a bare 'open an issue!'.

````text
1. Add the MCP server to Claude Code:

    ```shell
    # Add mcp-server-qdrant configured for code search
    claude mcp add code-search \
    -e QDRANT_URL="http://localhost:6333" \
    -e COLLECTION_NAME="code-repository" \
    -e EMBEDDING_MODEL="sentence-transformers/all-MiniLM-L6-v2" \
    -e TOOL_STORE_DESCRIPTION="Store code snippets with descriptions. […]" \
    -e TOOL_FIND_DESCRIPTION="Search for relevant code snippets using natural language. […]" \
    -- uvx mcp-server-qdrant
    ```

2. Verify the server was added:

    ```shell
    claude mcp list
    ```
[…]
npx @smithery/cli install mcp-server-qdrant --client claude
````

### documentation/qdrant-mcp-server/index.md (md mirror of the MCP doc page) — HTTP 200

<https://qdrant.tech/documentation/qdrant-mcp-server/index.md>

Effectively EMPTY: 254 bytes, only the 3-line preamble plus `# Qdrant MCP Server`. The page llms.txt line 201 points agents at has no body in markdown form (inferred: page is built from non-markdown source the mirror does not render). A gap, not a feature.

```text
> Use this file to discover all available pages: https://qdrant.tech/llms.txt# Qdrant MCP Server
```

### qdrant/qdrant AGENTS.md / CLAUDE.md / llms-install.md; mcp-server-qdrant AGENTS.md / CLAUDE.md / llms-install.md; qdrant.tech/documentation/mcp/index.md — HTTP 404

<https://raw.githubusercontent.com/qdrant/qdrant/master/AGENTS.md>

All 404. No AGENTS.md, CLAUDE.md, or llms-install.md in either repo.

## What they do better than XERJ

- Every markdown doc page carries the same 3-line agent preamble (skills catalogue / search endpoint / llms.txt) — an agent landing on ANY page via search finds the onboarding layer in hop 1; XERJ's answers/*.md pages have no such uniform header.
- A shell-less persona has an explicit path: 'Pass the skills.qdrant.tech URL to your agent and it will use it immediately, no installation required' plus the claude.ai-web-app caveat ('add `Use skills.qdrant.tech` to your prompt'). This directly addresses the failure mode XERJ's study found (MCP-only/no-terminal personas fail).
- Skill descriptions are written as trigger lists ('Use when someone asks "how to deploy Qdrant", "Docker vs Cloud", "local mode"…') so the agent knows WHEN to load a page, not just what it contains.
- MCP README gives per-client install with a VERIFY step (`claude mcp add … ` then `claude mcp list`), VS Code one-click install badges with the full config URL-encoded, a Smithery one-liner, and a zero-install `uvx` path — XERJ's llms.txt documents `xerj mcp` exists but gives no client-config snippet and no one-click link.
- Separate concerns: the docs index (llms.txt) is a pure catalogue; the agent-behaviour layer is a separate, small, imperative site (skills.qdrant.tech, 43 lines) with a live search endpoint. XERJ mixes catalogue, protocol, caveats and contribution demands into one 40 KB file.
- The bug-report ask is 3 concrete fields (skill name / prompt you gave / what the agent said vs should have said) — lower friction than XERJ's 25-line Markdown PR under a CLA-exempt path.
- A hosted search endpoint for agents (`https://skills.qdrant.tech/search?query=…`) returns the single best SKILL.md — XERJ tells agents to 'search this file' but offers no server-side search over its docs.

## What XERJ does better

- XERJ's llms.txt has an install command in the file at line 11 (`curl -fsSL https://xerj.org/get | sh`) and a numbered first-run sequence; Qdrant's llms.txt has zero install commands — an agent must follow two more hops to reach `docker pull qdrant/qdrant`.
- XERJ has a real llms-full.txt (1,235 lines) with an end-to-end protocol; Qdrant has none (404 at three locations) and its MCP doc page's markdown mirror is empty (254 bytes).
- XERJ puts honesty caveats inline ('the default embedding mode is lexical, not neural', PATH consequence, data-dir-outside-index trap); Qdrant's llms.txt carries no caveats at all.
- XERJ has a contribution/feedback mechanism baked into the CLI (`xerj feedback --open-pr …`, `--dry-run` fallback for sandboxes); Qdrant's ask is a plain GitHub issue link.
- XERJ ships tool-schema files in three shapes (mcp-tools.json / openai-tools.json / anthropic-tools.json) for agents that cannot run a process; Qdrant offers nothing equivalent in llms.txt.
- XERJ names the shell-less HTTP-only path ([Agent quickstart] … 'the HTTP-only path for an agent with no shell') even if the study shows it is not yet sufficient.

## Adoptable ideas

- Uniform 3-line agent preamble on every markdown page. Copy the pattern from https://qdrant.tech/documentation/quickstart/index.md verbatim: '> Explore Qdrant's agent skills catalog at https://skills.qdrant.tech/\n> Search the documentation at https://skills.qdrant.tech/search?query=your+query+here\n> Use this file to discover all available pages: https://qdrant.tech/llms.txt'. XERJ equivalent: prepend to every xerj.org/answers/*.md and docs/*.md: '> Install: curl -fsSL https://xerj.org/get | sh · Index: https://xerj.org/llms.txt · Full protocol: https://xerj.org/llms-full.txt'.
- Explicit no-shell path with the web-app caveat, from https://qdrant.tech/documentation/skills/index.md: 'Pass the skills.qdrant.tech URL to your agent and it will use it immediately, no installation required' and '> When you use the claude.ai web app, the Advisor can't fetch […] on its own. You need to add `Use skills.qdrant.tech` to your prompt directly.' XERJ: add a 'No shell? ' line at the top of ## Start here pointing to the HTTP-only quickstart and the MCP config snippet, and a one-line prompt suffix an operator can paste ('Use xerj.org/llms.txt').
- Trigger-list descriptions, from https://skills.qdrant.tech/llms.txt: 'Use when someone asks \'how to deploy Qdrant\', \'Docker vs Cloud\', \'local mode\', […] Also use when choosing between deployment types for a new project.' XERJ: rewrite ## Answers entries as 'Use when …' triggers instead of question titles.
- MCP install with a verify step and per-client blocks, from https://raw.githubusercontent.com/qdrant/mcp-server-qdrant/master/README.md: '1. Add the MCP server to Claude Code: claude mcp add code-search … -- uvx mcp-server-qdrant  2. Verify the server was added: claude mcp list'. XERJ: add to llms.txt a verbatim `claude mcp add xerj -- xerj mcp` + `claude mcp list` block, a Claude Desktop `mcpServers` JSON, and a Cursor block — the study says the MCP server 'was not shipped/documented'.
- One-click install badges, from the same README: '[![Install with UVX in VS Code](https://img.shields.io/badge/VS_Code-UVX-0098FF…)](https://insiders.vscode.dev/redirect/mcp/install?name=qdrant&config=%7B%22command%22%3A%22uvx%22…)'. XERJ: generate an insiders.vscode.dev/redirect/mcp/install?name=xerj&config=<urlencoded {"command":"xerj","args":["mcp"]}> link and list it under ## Start here.
- Three-field feedback ask, from https://raw.githubusercontent.com/qdrant/skills/main/README.md: 'Found a bug or wrong advice in a skill? Open an issue on GitHub and include: - The skill name - The prompt you gave your agent - What the agent said vs what it should have said'. XERJ: offer this as the zero-friction tier BELOW the PR field report (an issue with 3 bullet fields), and stop filing the 'required' report under a heading named '## Optional' — rename the section to '## Your side of the deal' or move the report into ## Start here step 4 only.
- Separate the catalogue from the protocol, as Qdrant separates qdrant.tech/llms.txt (696-line index, no demands) from skills.qdrant.tech/llms.txt (43 lines, all imperative triggers). XERJ: keep llms.txt as a short imperative agent protocol (install → run → verify → report) and move the ~150 answer/comparison links to a linked catalogue file, so the demand at line 7 is not diluted by 200 lines of links.
- Hosted search endpoint for agents, from the advisor SKILL.md: 'Fetch `https://skills.qdrant.tech/search?query=<your query>` […] It returns the single most relevant top-level skill's `SKILL.md`.' XERJ: expose https://xerj.org/search?query=… backed by a XERJ node indexing its own docs (dogfood) and advertise it in the preamble.
