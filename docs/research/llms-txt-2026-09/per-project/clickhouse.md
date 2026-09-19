# ClickHouse

Category: analytical database

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://clickhouse.com/llms.txt> — HTTP 200
- Size: 103444 bytes, 500 lines
- Install position: Lines 5-12, the FIRST H2 (## Get started for free) immediately after the one-paragraph description; install one-liner is on line 12. Nothing else in the file is install-related — lines 14-500 are a marketing/sitemap link index (Pages, Blog, Comparisons, Integrations).
- Tone: neutral-to-marketing. Makes NO demands of the agent: no imperatives, no 'you must', no required steps, no feedback ask. The install one-liner is stated as plain fact ('To install ClickHouse locally ... curl ... | sh'). After line 14 it is a link index whose descriptions are copied marketing taglines ('Fast at scale. Efficient by design.'). The only community line is 'Welcome to the ClickHouse Community ... Learn, contribute, connect' (line 54). No MCP install, no agent-directed text, no verification step in this file — MCP appears only as blog titles.
- Section order: # ClickHouse (blockquote one-paragraph description) → ## Get started for free → ## Pricing → ## Pages → ## Blog → ## Comparisons → ## Integrations → ## Optional → ### Resources (under Optional) → ### Videos (under Optional) → ### Legal (under Optional)

### Install commands found

```sh
curl https://clickhouse.com/ | sh
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## Get started for free

To start using ClickHouse Cloud, sign up here: https://console.clickhouse.cloud/signUp

A free trial is available with $300 in credits over 30 days.

To install ClickHouse locally (MacOS, Linux, FreeBSD):
curl https://clickhouse.com/ | sh

## Pricing

Full pricing in Markdown, incl. plans and per-region compute/storage rates: https://clickhouse.com/pricing.md
```

## llms-full.txt

- URL: <https://clickhouse.com/docs/llms-full.txt> — HTTP 200
- Size: 34655192 bytes
- Relationship to llms.txt: Not related to the marketing /llms.txt at all. It is the Mintlify auto-generated concatenation of every docs page (3,887 'Source: https://clickhouse.com/docs/...' page blocks, MDX components like <Steps>/<Tab>/<Frame> left in as tags). The docs root has its own separate index https://clickhouse.com/docs/llms.txt (200, 165 lines, 22,838 bytes) which is a pure auto-generated tree of _llms/<lang>/<section>.md bundle links ('Follow each /_llms/ index recursively') plus two OpenAPI specs — nothing about installation in that index. https://clickhouse.com/llms-full.txt at the site root is a 404 (HTML error page). Every docs .md page also carries a Mintlify banner: '> ## Documentation Index / > Fetch the documentation index at: https://clickhouse.com/docs/llms.txt / > Use this file to discover all available pages before exploring further. / > For broader context, fetch the full documentation at: https://clickhouse.com/docs/llms-full.txt (large file).'
- Install position: Install content is scattered by page, NOT front-loaded: the file opens with the VLDB 2024 paper ('# Architecture overview'). First 'curl https://clickhouse.com/ | sh' at line 4256 (clickhouse-local page). Installation section (get-started/setup) begins at line 20958; 'Quick install' page at line 21790; clickhousectl page at ~29540 ('curl https://clickhouse.com/cli | sh', 'clickhousectl skills --all'); MCP overview at 32993; MCP client setup tabs (claude mcp add / codex mcp add / Claude Desktop / Cursor / Windsurf JSON) around the Claude Desktop guide; docs-MCP server page at line 462379 ('claude mcp add --transport http clickhouse-docs https://clickhouse.com/docs/mcp --scope user' and 'npx skills add https://clickhouse.com'). An agent reading top-down hits ~21,000 lines of concepts before any install.

## Other agent-facing files

### docs llms.txt (auto-generated Mintlify index) — HTTP 200

<https://clickhouse.com/docs/llms.txt>

Pure docs index: 4 top-level bundles, per-language _llms bundles (ar/es/fr/ja/ko/pt-br/ru/zh), 2 OpenAPI specs. No installation content, no agent instructions beyond one recursion hint.

```text
> The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.
```

### llms-install.md — HTTP 404

<https://clickhouse.com/llms-install.md>

Does not exist (also 404 at https://clickhouse.com/docs/llms-install.md). The docs 404 body still carries the Mintlify banner pointing to /docs/llms.txt and /docs/llms-full.txt.

```text
> ## Documentation Index
> Fetch the documentation index at: https://clickhouse.com/docs/llms.txt
> Use this file to discover all available pages before exploring further.
> For broader context, fetch the full documentation at: https://clickhouse.com/docs/llms-full.txt (large file).
```

### AGENTS.md (core repo) — HTTP 200

<https://raw.githubusercontent.com/ClickHouse/ClickHouse/master/AGENTS.md>

17-byte pointer file; the whole content is a path. Contributor-facing only; nothing about installing or using the product.

```text
.claude/CLAUDE.md
```

### .claude/CLAUDE.md (core repo) — HTTP 200

<https://raw.githubusercontent.com/ClickHouse/ClickHouse/master/.claude/CLAUDE.md>

188-line contributor rulebook for coding agents working IN the ClickHouse repo (branching, doc anchors, wire-spec updates, CI report tools). Not an install/onboarding document.

```text
When working with a branch, do not use rebase or amend - add new commits instead.

Do not commit to the master branch. Create a new branch for every task.
```

### CLAUDE.md / SKILL.md at core repo root — HTTP 404

<https://raw.githubusercontent.com/ClickHouse/ClickHouse/master/CLAUDE.md>

Neither exists at the repo root (SKILL.md also 404). Skills live in the separate ClickHouse/agent-skills repo.

### Set up the ClickHouse MCP server (docs, MCP install page) — HTTP 200

<https://clickhouse.com/docs/guides/use-cases/ai-ml/MCP/claude-desktop.md>

Per-client tabs (Claude Code, Claude Desktop, Codex, ChatGPT, Cursor, Windsurf) with a copy-paste command or JSON each, a 'Verify the connection' prompt, and Troubleshooting. This is where the real agent install content lives — not in llms.txt.

```text
claude mcp add \
  --transport stdio \
  --env CLICKHOUSE_HOST=your-clickhouse-host \
  --env CLICKHOUSE_USER=your-clickhouse-user \
  --env CLICKHOUSE_PASSWORD=your-clickhouse-password \
  --scope user \
  mcp-clickhouse -- \
  uv run --with mcp-clickhouse --python 3.10 mcp-clickhouse

Run `claude mcp list` to verify the connection, or enter `/mcp` in Claude Code to inspect the server and its tools.
[...]
After the client reports that `mcp-clickhouse` is connected, ask it:

List the databases available in ClickHouse, then show me the tables in one of them.

The client might ask you to approve the first tool calls.
Review every request before granting access.
```

### Set up the ClickHouse documentation MCP server (docs, in llms-full.txt line 462379) — HTTP 200

<https://clickhouse.com/docs/resources/support-center/knowledge-base/setup-installation/set-up-clickhouse-documentation-mcp-server>

A remote, no-credentials MCP server for searching the DOCS (distinct from the data MCP). One-line install per client plus the skills installer.

```text
claude mcp add --transport http clickhouse-docs https://clickhouse.com/docs/mcp --scope user
[...]
## Add ClickHouse skills to your coding agent

Install the ClickHouse skills to give your agent expert ClickHouse knowledge and best practices:

npx skills add https://clickhouse.com
```

### Remote MCP in Cloud — HTTP 200

<https://clickhouse.com/docs/products/cloud/features/ai-ml/remote-mcp.md>

Managed Streamable-HTTP MCP endpoint with OAuth; all tools readOnlyHint. Zero-config path for shell-less agents.

```text
https://mcp.clickhouse.cloud/mcp
[...]
All tools exposed by the remote MCP server are **read-only**. Each tool is annotated with `readOnlyHint: true` in its MCP metadata.
```

### MCP agent-libraries index page — HTTP 200

<https://clickhouse.com/docs/guides/use-cases/ai-ml/MCP/ai-agent-libraries.md>

A 15-row table of 'How to build an AI Agent with <framework> and the ClickHouse MCP server' guides (Agno, Chainlit, Claude Agent SDK, CopilotKit, CrewAI, DSPy, LangChain, LlamaIndex, mcp-agent, Microsoft Agent Framework, OpenAI, PydanticAI, SlackBot, Streamlit, Upsonic). Pure link table.

```text
# Integrate AI agent libraries with ClickHouse MCP server

> Learn how to build an AI agent with DSPy and the ClickHouse MCP server
```

### mcp-clickhouse README — HTTP 200

<https://raw.githubusercontent.com/ClickHouse/mcp-clickhouse/main/README.md>

1,117-line server reference: tool contracts (run_query/list_databases/list_tables with response shapes), security/auth, Claude Desktop JSON configs (own server, public SQL Playground demo, chDB), env-var tables, middleware. No one-click links; only a glama.ai badge.

```text
Or, if you'd like to try it out with the [ClickHouse SQL Playground](https://sql.clickhouse.com/), you can use the following config:

{
  "mcpServers": {
    "mcp-clickhouse": {
      "command": "uv",
      "args": ["run", "--with", "mcp-clickhouse", "--python", "3.12", "mcp-clickhouse"],
      "env": {
        "CLICKHOUSE_HOST": "sql-clickhouse.clickhouse.com",
        "CLICKHOUSE_PORT": "8443",
        "CLICKHOUSE_USER": "demo",
        "CLICKHOUSE_PASSWORD": "",
        "CLICKHOUSE_SECURE": "true",
        "CLICKHOUSE_VERIFY": "true",
        "CLICKHOUSE_CONNECT_TIMEOUT": "30"
      }
    }
  }
}
```

### ClickHouse/agent-skills README + AGENTS.md + infra-clickhouse/SKILL.md — HTTP 200

<https://raw.githubusercontent.com/ClickHouse/agent-skills/main/README.md>

Official Agent Skills repo (agentskills.io spec). README leads with two install paths ('npx skills add clickhouse/agent-skills' and 'clickhousectl skills'), then per-skill 'For humans:' / 'For agents:' activation lines. infra-clickhouse/SKILL.md (200) is a decision-tree that self-installs the CLI and states exit codes for the agent.

```text
Check that `clickhousectl` is installed:

which clickhousectl

If not found, install it:

curl -fsSL https://clickhouse.com/cli | sh

This installs to `~/.local/bin/clickhousectl` (with a `chctl` alias). If the command is still not found, suggest `export PATH="$HOME/.local/bin:$PATH"` or a new terminal.

All commands accept `--json` for machine-readable output. Exit codes follow `gh` conventions: 0 success, 1 error, 2 cancelled, 4 auth required.
```

## What they do better than XERJ

- Install one-liner is literally the first thing after the description (line 12 of 500): '## Get started for free' -> 'curl https://clickhouse.com/ | sh'. XERJ's is on line 11 too, but wrapped in ~6 paragraphs of caveats (PATH consequence, data-dir trap) before the first run; ClickHouse spends zero words on caveats in llms.txt.
- Shell-less / MCP-only persona is served: a managed remote MCP (https://mcp.clickhouse.cloud/mcp, OAuth, readOnlyHint on every tool) and a docs MCP ('claude mcp add --transport http clickhouse-docs https://clickhouse.com/docs/mcp --scope user') that need no binary at all. XERJ's llms.txt has no MCP install snippet anywhere, only links to schema JSON files.
- Per-client copy-paste MCP install matrix on ONE page (Claude Code, Claude Desktop, Codex, ChatGPT, Cursor, Windsurf), each followed by a verification command ('claude mcp list', '/mcp') and a verification PROMPT ('List the databases available in ClickHouse, then show me the tables in one of them.').
- A zero-credential demo target the agent can hit immediately: the public SQL Playground config (CLICKHOUSE_HOST sql-clickhouse.clickhouse.com, user demo, empty password). XERJ has no hosted try-it target; every path requires installing and starting a node.
- Agent-native CLI ergonomics stated as contract: 'clickhousectl auto-detects coding-agent contexts (Claude Code, Cursor, Codex, Gemini CLI, Goose, Devin, and any tool that sets the standard AGENT env var) and emits JSON to stdout automatically without setting --json' and gh-style exit codes (0/1/2/4). XERJ documents exit codes but only behind an answers link.
- Skills distribution is a one-liner in two forms ('npx skills add clickhouse/agent-skills', 'clickhousectl skills --all'/'--detected-only'/'--agent claude --agent codex'), with each skill carrying an explicit 'For agents: The skill activates when...' trigger sentence. XERJ's 'xerj init' is buried as step 3 '(optional, one command)'.
- Skills self-heal the install path: infra-clickhouse/SKILL.md tells the agent to run 'which clickhousectl', install if missing, and what to do if PATH is wrong — the same PATH problem XERJ warns about, but framed as an executable procedure rather than a warning paragraph.
- Telemetry is transparent and agent-aware ('whether the CLI was invoked by an AI agent (and which agent) or in CI', DO_NOT_TRACK=1) — they get usage signal from agents WITHOUT asking agents to file anything.

## What XERJ does better

- XERJ's llms.txt is actually written for an agent ('This file is an index for you, the AI agent'); ClickHouse's marketing llms.txt is a sitemap with taglines and makes no demands of, and gives no instructions to, the reader — 486 of 500 lines are link lists.
- XERJ front-loads honest caveats (lexical-not-neural default, fusion modes, ingest scale) inline; ClickHouse has no caveat section anywhere in llms.txt or the docs index.
- XERJ has a numbered first-run sequence (start node -> autoindex -> init -> query) in the index itself; ClickHouse's install one-liner in llms.txt is not followed by any 'then run this' — the Steps live 21,000 lines into a 34.7 MB file.
- XERJ's llms-full.txt (84 KB) is curated and section-numbered (§3 operations, §10 protocol); ClickHouse's is a 34,655,192-byte / 570,402-line unfiltered dump opening with a VLDB paper — unusable as a single fetch for most agents.
- XERJ ships tool schemas in three shapes (mcp-tools.json / openai-tools.json / anthropic-tools.json) linked from the index; ClickHouse links only OpenAPI specs.
- XERJ has an explicit feedback/contribution mechanism (xerj feedback --open-pr, AI_CONTRIBUTIONS.md); ClickHouse's llms.txt has none at all (only 'Learn, contribute, connect' as a community tagline).
- XERJ has a documented manual verified-install path in the index; ClickHouse offers only 'curl | sh' in llms.txt with no checksum story.

## Adoptable ideas

- Add a per-client MCP install block to llms.txt (currently absent; the 22-run study's shell-less failures point here). Copy ClickHouse's shape from https://clickhouse.com/docs/guides/use-cases/ai-ml/MCP/claude-desktop.md: one command per client, then a verify command, then a verify PROMPT. Verbatim model: 'claude mcp add --transport http clickhouse-docs https://clickhouse.com/docs/mcp --scope user' / 'Run `claude mcp list` to verify the connection, or enter `/mcp` in Claude Code to inspect the server and its tools.' / 'After the client reports that `mcp-clickhouse` is connected, ask it: List the databases available in ClickHouse, then show me the tables in one of them.' For XERJ: `claude mcp add --transport stdio xerj -- xerj mcp` (or whatever `xerj init` writes to .mcp.json), then 'ask it: List the indices on the local XERJ node, then search one of them for <word>.'
- Give shell-less agents a zero-install target. ClickHouse's remote MCP (https://mcp.clickhouse.cloud/mcp) and the public SQL Playground config ('CLICKHOUSE_HOST': 'sql-clickhouse.clickhouse.com', 'CLICKHOUSE_USER': 'demo', 'CLICKHOUSE_PASSWORD': '') let an agent with no terminal succeed. XERJ equivalent: a read-only demo node (e.g. demo.xerj.org:9200 with a pre-indexed corpus) referenced in 'Start here' as 'No shell? Query the demo node: POST https://demo.xerj.org/docs/_search'.
- Adopt the 'For agents: The skill activates when ...' trigger sentence from https://raw.githubusercontent.com/ClickHouse/agent-skills/main/README.md ('**For agents:** The skill activates automatically when you work with ClickHouse—creating tables, writing queries, or designing data pipelines.') for the skill that `xerj init` writes, and publish it via 'npx skills add xerj-org/xerj' so `xerj init` is no longer the only path.
- Make the CLI announce its agent-awareness as a contract in llms.txt, copying the clickhousectl sentence: '`clickhousectl` auto-detects coding-agent contexts (Claude Code, Cursor, Codex, Gemini CLI, Goose, Devin, and any tool that sets the standard `AGENT` env var) and emits JSON to stdout automatically without setting `--json`.' and 'Exit codes follow the `gh` CLI conventions: 0 Success, 1 Error, 2 Cancelled, 4 Auth required'. XERJ already has exit codes 0/1/2/3/4 but only behind an answers link; inline them under 'Start here'.
- Turn the PATH warning into an executable self-heal, as infra-clickhouse/SKILL.md does: 'Check that `clickhousectl` is installed: which clickhousectl / If not found, install it: curl -fsSL https://clickhouse.com/cli | sh / ... If the command is still not found, suggest `export PATH="$HOME/.local/bin:$PATH"` or a new terminal.' XERJ: replace the 'PATH consequence — read this before the next command' paragraph with `which xerj || export PATH="$HOME/.local/bin:$PATH"` as step 0.
- Field-report problem: ClickHouse does not ask agents for reports at all — it collects agent-tagged telemetry instead ('whether the CLI was invoked by an AI agent (and which agent) or in CI', opt-out 'DO_NOT_TRACK=1', notice-before-send). INFERENCE, not stated by ClickHouse: XERJ's 'required' report sits under a heading literally named '## Optional' (llms.txt line 223 'Your side of the deal — the field report'); either rename that section (e.g. '## Your side of the deal (required)') and move the `xerj feedback --open-pr` line out of the numbered first-run list into its own H2 directly after 'Start here', or drop the 'required' claim and add an opt-out usage ping the way clickhousectl does. Do not keep both 'required' and 'Optional'.
- Keep the docs-page banner pattern Mintlify uses on every ClickHouse .md page, on every xerj.org/answers/*.md and /compare/*.md page: '> ## Documentation Index / > Fetch the documentation index at: https://clickhouse.com/docs/llms.txt / > Use this file to discover all available pages before exploring further.' — it makes any deep-linked page route the agent back to the index on hop 1.
- Do NOT copy: the 34.7 MB llms-full.txt (opens with a research paper, first install at line 21790) and the marketing llms.txt's 486 lines of taglines. XERJ's curated 84 KB full file is the better model; keep it.
