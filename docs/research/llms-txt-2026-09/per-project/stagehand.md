# Stagehand (Browserbase)

Category: agent tooling (browser automation SDK)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.stagehand.dev/llms.txt> — HTTP 200
- Size: 26114 bytes, 189 lines
- Install position: Line 4, third bullet of the flat index: `- [Installation](https://docs.stagehand.dev/v4/first-steps/installation.md): Add Stagehand to an existing project.` — a link only; no install command anywhere in llms.txt itself. Repeated for v3 (line 46) and v2 (line 133).
- Tone: neutral / auto-generated (Mintlify). It is a plain docs index: one line per page, `[Title](url.md): description`. It makes no demands of the agent, contains no imperative text, no install command, no MCP config, no feedback ask. The only agent-directed framing lives in the page descriptions themselves (e.g. 'Give a Claude Code agent persistent Stagehand browser tools over MCP/stdio.').
- Section order: # Stagehand (no summary blockquote, no prose; straight into a flat link list: v4 first-steps → basics → configuration → add-ons → integrations (Claude Code, Codex, Eve, Deep Agents, CrewAI, Mastra, fx, Pi, Vercel AI SDK) → best practices → migrations → reference; then the same for v3 (incl. Browserbase MCP Server intro/setup/tools/configuration, Python/Java/Go/Ruby SDK + api-reference pages) and v2) → ## OpenAPI Specs (- [package](/package.json), - [openapi](/v3/openapi.json)) → ## Optional (Discord, GitHub, Changelog)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

```text
## Optional

- [Discord](https://discord.gg/stagehand)
- [GitHub](https://github.com/browserbase/stagehand)
- [Changelog](https://github.com/browserbase/stagehand/releases)
```

### Best passage for driving install and first use (verbatim)

```text
- [Quickstart](https://docs.stagehand.dev/v4/first-steps/quickstart.md): Build your first Stagehand automation with act, extract, and observe.
- [Installation](https://docs.stagehand.dev/v4/first-steps/installation.md): Add Stagehand to an existing project.
- [AI rules](https://docs.stagehand.dev/v4/first-steps/ai-rules.md): Give your AI coding assistant the rules it needs to write correct Stagehand v4 code.
- [Integrations](https://docs.stagehand.dev/v4/integrations/overview.md): Connect Claude Code, Codex, CrewAI, Deep Agents, Eve, Mastra, fx, Pi, or the Vercel AI SDK to a persistent Stagehand browser.
- [Claude Code](https://docs.stagehand.dev/v4/integrations/claude-code.md): Give a Claude Code agent persistent Stagehand browser tools over MCP/stdio.
```

## llms-full.txt

- URL: <https://docs.stagehand.dev/llms-full.txt> — HTTP 200
- Size: 898822 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every v4 page in llms.txt, each as `# Title` + `Source: <url>` + raw MDX (<Tabs>, <Steps>, <Card>, <Accordion> tags left in, ```bash theme={null} fences). Only v4 pages are included (v3/v2 pages listed in llms.txt are absent from llms-full). Page order appears alphabetical-by-directory (add-ons, basics, best-practices, configuration, first-steps, integrations, migrations, reference), not navigation order. The per-page '.md' endpoints (e.g. ai-rules.md) carry an extra injected header that llms-full.txt does not: '> ## Documentation Index / > Fetch the complete documentation index at: https://docs.stagehand.dev/llms.txt / > Use this file to discover all available pages before exploring further.' (0 occurrences in llms-full.txt). No feedback/contribution ask anywhere in llms-full.txt; the single community pointer is the intro's `<Card title="Join Discord" icon="discord" href="https://discord.gg/stagehand">Get help from the community</Card>` (line 16270).
- Install position: `# Installation` starts at line 15783 of 26973 (~59% of the way in); `# Quickstart` at line 16280; `# Introducing Stagehand` at 16074. Order is NOT reader-first: file opens with `# Fetch` (add-on), `# Search`, `# Act`, `# Extract`, `# Observe` ... the first-steps pages come after configuration. Install commands inside `# Installation` (verbatim): `pnpm add @browserbasehq/stagehand 'zod@~4.4.3'` / `# npm install @browserbasehq/stagehand 'zod@~4.4.3'` / `# yarn add ...` / `# bun add ...` / `pip install stagehand` / `# uv add stagehand` / `# poetry add stagehand` / `go get github.com/browserbase/stagehand/packages/sdk-go/v4@v4.0.0`. Quickstart (line 16280) is a 3-step `<Steps>`: 'Create a sample project' → 'Write the script' → 'Run it' with `export BROWSERBASE_API_KEY="bb_live_..."` then `pnpm dlx tsx index.ts`.

## Other agent-facing files

### AI rules page (v4) — HTTP 200

<https://docs.stagehand.dev/v4/first-steps/ai-rules.md>

1,146 lines / 36,738 bytes. Sections: # AI rules → ## Quickstart (two cards) → ## Using MCP servers (three JSON `mcpServers` configs: Context7 `{"command":"npx","args":["-y","@upstash/context7-mcp"]}`, DeepWiki `{"url":"https://mcp.deepwiki.com/mcp"}`, Stagehand docs `{"url":"https://docs.stagehand.dev/mcp"}`) → ## Editor rule files (copy-paste) (TypeScript and Python tabs, each a full ~500-line `# Stagehand Project` rules file for `.cursorrules`/`windsurfrules`/`claude.md`) → ## Security notes → ## Resources/references. This is the project's real agent-install surface; llms.txt merely links it.

```text
You're likely using AI to write code, and there's a **right and wrong way to do it.** This page collects the rules, configs, and copy-paste snippets that get your coding assistant writing correct Stagehand v4 code.
[...]
<Tip>
  **Prompting tip:**
  Explicitly ask your coding agent/assistant to use these MCP servers to fetch relevant information from the docs so they have better context and know how to write proper Stagehand code.

  ie. **"Use the stagehand-docs MCP to fetch the act/observe guidelines, then generate code that follows them. Prefer cached observe results."**
</Tip>

## Editor rule files (copy-paste)

Drop these in `.cursorrules`, `windsurfrules`, `claude.md`, or any agent rule framework:
```

### Claude Code integration page (v4) — HTTP 200

<https://docs.stagehand.dev/v4/integrations/claude-code.md>

99 lines. Prerequisites → 4-step Quickstart (clone+build, `export ANTHROPIC_API_KEY`, choose browser, run one task) → env-var table → 'Keep the browser session alive' → 'Connect a running Claude Code CLI' (project-scoped `.mcp.json` shipped in the package) → security Warning → link to source. Shell is assumed throughout (git clone, pnpm). Carries the injected 'Documentation Index' header pointing back to llms.txt.

````text
The package ships a project-scoped `.mcp.json` that mounts the same facade server in the Claude Code CLI. Its `args` path is relative to the package, so start the CLI from that directory:

```bash theme={null}
cd packages/integrations/claude-code
claude
```

Claude Code inherits your shell environment, so the exports above are the only configuration. For a headless one-shot run:

```bash theme={null}
claude -p "your instruction" --mcp-config .mcp.json --allowedTools "mcp__stagehand__run,mcp__stagehand__snapshot,mcp__stagehand__screenshot"
```
````

### Browserbase MCP Server Setup (v3) — HTTP 200

<https://docs.stagehand.dev/v3/integrations/mcp/setup.md>

327 lines. The one-click / one-command MCP install page: a Cursor deeplink card, a `claude mcp add` one-liner, hosted Streamable HTTP endpoint `https://mcp.browserbase.com/mcp`, per-client install tabs, then a '## Verify Installation' section with a first-prompt to try. Not linked from the v4 section of llms.txt (only via the v3 index rows).

````text
## Quick Installation

<Card title="Install with Cursor" icon="arrow-pointer" href="cursor://anysphere.cursor-deeplink/mcp/install?name=browserbase&config=eyJ1cmwiOiJodHRwczovL21jcC5icm93c2VyYmFzZS5jb20vbWNwIiwiaGVhZGVycyI6eyJBdXRob3JpemF0aW9uIjoiQmVhcmVyIFlPVVJfQlJPV1NFUkJBU0VfQVBJX0tFWSJ9fQ==">
  One-click installation directly in Cursor
</Card>

You can also add Browserbase MCP to Claude Code with a single command. If your client supports HTTP request headers, prefer `Authorization: Bearer YOUR_BROWSERBASE_API_KEY`; otherwise, use the legacy query-parameter fallback:

```bash theme={null}
claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"
```
[...]
## Verify Installation
[...]
    <Tip>
      Try: "Navigate to example.com and extract the main heading"
    </Tip>
````

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/browserbase/stagehand/main/AGENTS.md>

6 lines / 723 bytes. Contributor-facing only (stacked-PR git discipline); nothing about installing or using Stagehand; not an agent-onboarding file.

```text
<!--
Only a human may request changes to this file. Keep additions rare and limited to durable, repository-wide rules.
-->

- For stacked PRs, target each PR at its immediate predecessor; when a parent changes, merge it into its immediate child and resolve conflicts normally.
```

### CONTRIBUTING.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/browserbase/stagehand/main/CONTRIBUTING.md>

160 lines. Human-contributor guide: package map, `just install` / `just build`, checks. Feedback channel is Discord-first, not issues/PRs; no field-report or agent-report concept.

```text
Browserbase prioritizes reliability, extensibility, speed, and cost, in that order. Bug fixes and
small improvements are the best way to get started.

For anything larger, raise it in [Discord](https://discord.gg/stagehand) first. A quick
conversation is the best way to confirm the direction fits the roadmap before you invest time in
building it.
```

### browserbase/skills README (Claude Code plugin marketplace) — HTTP 200

<https://raw.githubusercontent.com/browserbase/skills/main/README.md>

92 lines. Separate repo that packages 18 SKILL.md skills as a Claude Code plugin. Install is `npx skills add browserbase/skills` or `/plugin marketplace add browserbase/skills` + `/plugin install browse@browserbase`, followed by six example prompts to type. Includes an 'agent-experience' skill that audits how agent-friendly a product/docs site is.

````text
## Installation

To install the skill to popular coding agents:

```bash
$ npx skills add browserbase/skills
```

### Claude Code

On Claude Code, to add the marketplace, simply run:

```bash
/plugin marketplace add browserbase/skills
```

Then install the plugin:

```bash
/plugin install browse@browserbase
```
[...]
## Usage

Once installed, you can ask Claude to browse or use the Browserbase CLI:
- *"Go to Hacker News, get the top post comments, and summarize them "*
- *"QA test http://localhost:3000 and fix any bugs you encounter"*
````

### llms-install.md — HTTP 404

<https://docs.stagehand.dev/llms-install.md>

does not exist

### CLAUDE.md (docs root and repo root) — HTTP 404

<https://raw.githubusercontent.com/browserbase/stagehand/main/CLAUDE.md>

does not exist at docs root (404) or repo root (404); the copy-paste `claude.md` content lives inside the AI rules page instead

### SKILL.md (docs root and repo root) — HTTP 404

<https://raw.githubusercontent.com/browserbase/stagehand/main/SKILL.md>

does not exist in the stagehand repo; skills live in the separate browserbase/skills repo

### .cursorrules (repo root) — HTTP 404

<https://raw.githubusercontent.com/browserbase/stagehand/main/.cursorrules>

does not exist; the rules file is only offered as copy-paste inside the AI rules page

## What they do better than XERJ

- A dedicated, llms.txt-indexed 'AI rules' page whose whole job is to get an agent configured: two entry cards (MCP servers / editor rules), three verbatim `mcpServers` JSON blocks, and a full copy-paste `claude.md`/`.cursorrules` file per language. XERJ scatters the equivalent across `xerj init`, mcp-tools.json and docs/agents/quickstart with no single page.
- One-click and one-command MCP install: a `cursor://anysphere.cursor-deeplink/mcp/install?...` card plus `claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?..."`, and a hosted Streamable-HTTP endpoint so a shell-less MCP-only client can attach with zero local process. XERJ has no hosted endpoint, no deeplink, and no `claude mcp add` one-liner in llms.txt (only `xerj init` and `xerj mcp`, both shell-required).
- Every per-page `.md` endpoint carries an injected three-line header ('> ## Documentation Index / > Fetch the complete documentation index at: https://docs.stagehand.dev/llms.txt / > Use this file to discover all available pages before exploring further.') so an agent that lands on any deep page is routed back to the index. XERJ's answer pages do not do this.
- An explicit example prompt the human should give the agent ('Use the stagehand-docs MCP to fetch the act/observe guidelines, then generate code that follows them. Prefer cached observe results.') and a 'Verify Installation' step with a concrete first prompt ('Try: "Navigate to example.com and extract the main heading"').
- A 'Get started in 60 seconds' promise and a 3-step Quickstart (create project / write script / run it) that ends in an observable result; install commands are shown in one fenced block per language with the alternates as comments (`pnpm add ...` / `# npm install ...` / `# yarn add ...` / `# bun add ...`).
- Per-agent integration pages (Claude Code, Codex, CrewAI, Mastra, fx, Pi, Deep Agents, Eve, Vercel AI SDK) each described in llms.txt with the same imperative template 'Give a <agent> agent persistent Stagehand browser tools over MCP/stdio.' — the agent can find its own runtime by name in the index.
- Skills distribution via a plugin marketplace (`npx skills add browserbase/skills`, `/plugin install browse@browserbase`) with six ready-to-type example prompts under '## Usage'.

## What XERJ does better

- Stagehand's llms.txt is a plain auto-generated Mintlify docs index with nothing about installation: no summary blockquote, no install command, no MCP config, no prose, no ask. XERJ's llms.txt has an inline 'Start here' with the actual `curl -fsSL https://xerj.org/get | sh` / `irm https://xerj.org/get.ps1 | iex` commands, the PATH consequence, and an ordered first-run sequence.
- XERJ has an inline Caveats section that corrects the most common agent misreading (lexical-not-neural default); Stagehand's llms.txt has no caveats at all and llms-full.txt buries the equivalent ('Stagehand never reads environment variables on your behalf') inside Notes.
- XERJ names the shell-less path explicitly ('Agent quickstart: the HTTP-only path for an agent with no shell') and states which operation is CLI-only; Stagehand's Claude Code page assumes git clone + pnpm and never addresses a shell-less agent (its hosted MCP endpoint does, but it is only on a v3 page).
- XERJ has an explicit contribution/feedback protocol for agents (field report, one-file CLA-exempt PR, `xerj feedback --open-pr`, what to do when sandboxed, 'a negative report is a good report'). Stagehand has zero feedback ask in llms.txt or llms-full.txt — only Discord/GitHub/Changelog links under '## Optional' and a 'Join Discord' card.
- XERJ's llms-full.txt is 84 KB / 1,235 lines and curated; Stagehand's is 899 KB / 26,973 lines of raw MDX with component tags, pages in directory order (Installation at line 15,783), and no v3/v2 content despite llms.txt listing it — far harder for an agent to consume in one read.
- XERJ ships machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) linked from llms.txt; Stagehand links only package.json and an OpenAPI spec.

## Adoptable ideas

- Add an 'AI rules'-style page and index it in llms.txt with Stagehand's exact framing, adapted: Stagehand's line is 'You're likely using AI to write code, and there's a **right and wrong way to do it.** This page collects the rules, configs, and copy-paste snippets that get your coding assistant writing correct Stagehand v4 code.' (https://docs.stagehand.dev/v4/first-steps/ai-rules.md). XERJ version would collect: the `.mcp.json` block `xerj init` writes, the 10-line skill it writes, and a copy-paste `CLAUDE.md`/`.cursorrules` block — so a shell-less persona can paste config instead of running `xerj init`.
- Publish a verbatim `mcpServers` JSON block in llms.txt for `xerj mcp` (stdio) modelled on Stagehand's `{"mcpServers":{"context7":{"command":"npx","args":["-y","@upstash/context7-mcp"]}}}` shape, plus a `claude mcp add` one-liner in the style of 'claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"' (https://docs.stagehand.dev/v3/integrations/mcp/setup.md). This directly targets the 'MCP server exists but was not shipped/documented' finding.
- Inject Stagehand's three-line index header on every XERJ answer/doc `.md` endpoint: '> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.stagehand.dev/llms.txt\n> Use this file to discover all available pages before exploring further.' (seen on https://docs.stagehand.dev/v4/first-steps/ai-rules.md and every .md page) — with the XERJ URL. Cheap, and it makes deep links self-routing for agents that arrive via search.
- Add a '## Verify Installation' step with a concrete first prompt, copying the pattern 'Try: "Navigate to example.com and extract the main heading"' (https://docs.stagehand.dev/v3/integrations/mcp/setup.md, 'Verify Installation'). For XERJ: 'Try: `xerj search "where is the config parsed"` and expect file:line + passage'. Same page also states 'Restart/refresh your MCP client app and verify tools are available.'
- Give the human a sentence to say to the agent, in Stagehand's exact form: '**Prompting tip:** Explicitly ask your coding agent/assistant to use these MCP servers to fetch relevant information from the docs ... ie. **"Use the stagehand-docs MCP to fetch the act/observe guidelines, then generate code that follows them. Prefer cached observe results."**' (ai-rules.md). XERJ equivalent: 'Use the xerj_search MCP tool before grepping; cite file:line.'
- Fix the field-report contradiction by NOT copying Stagehand here: Stagehand's llms.txt puts only Discord/GitHub/Changelog under '## Optional' and asks nothing of the agent, which is why 'Optional' reads as skippable. XERJ should rename its '## Optional' section (e.g. '## Your side of the deal') or move the field-report bullet into 'Start here' step 4 so the heading agrees with 'required, not optional'. (Inference from the contrast, not something Stagehand says.)
- Use Stagehand's one-block-per-platform install layout with alternates as comments — '```bash\npnpm add @browserbasehq/stagehand 'zod@~4.4.3'\n# npm install @browserbasehq/stagehand 'zod@~4.4.3'\n# yarn add ...\n# bun add ...\n```' (https://docs.stagehand.dev/v4/first-steps/installation.md) — for XERJ's `curl | sh` / manual verified-download / `irm` variants, so the manual verified-install path that unblocked the policy-constrained persona is visible in the same block as the one-liner rather than behind the Install docs link.
- Copy the per-agent index-row template 'Give a <Agent> agent persistent Stagehand browser tools over MCP/stdio.' (llms.txt rows for Claude Code, Codex, CrewAI, Mastra, fx, Pi, Deep Agents, Eve, Vercel AI SDK) as llms.txt rows for XERJ: 'Give a Claude Code agent XERJ search over MCP/stdio (`xerj init`)', 'Give a Cursor agent ...', 'Give a shell-less agent XERJ over plain HTTP', so each runtime finds its own page by name in the index.
- Consider a skills/plugin distribution channel like https://raw.githubusercontent.com/browserbase/skills/main/README.md: '$ npx skills add browserbase/skills' / '/plugin marketplace add browserbase/skills' / '/plugin install browse@browserbase', followed by a '## Usage' list of ready-to-type prompts ('"QA test http://localhost:3000 and fix any bugs you encounter"'). Its 'agent-experience' skill ('Audit how agent-friendly a product, SDK, or docs site is — drops Claude subagents at it with tiny prompts, captures their traces, and scores setup friction, speed, error recovery, and doc quality') is the same method as XERJ's 22-run FTX study and could be run against xerj.org.
