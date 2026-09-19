# Tavily

Category: ai-search

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.tavily.com/llms.txt> — HTTP 200
- Size: 18171 bytes, 212 lines
- Install position: Line 16, second H2 ('## Quick Start: CLI (fastest way to get started)'), immediately after the 7-line 'Start Here For Agents' router; MCP alternative is the third H2 at line 36. Installation is above every docs-index section. Not hand-written per-page links after line 70 (Mintlify-style index), but lines 1-68 are hand-authored prose.
- Tone: neutral-imperative, terse. Makes soft demands of the agent ('start with the canonical setup guide', 'Recommended defaults for agents: prefer search_depth="advanced"…') but no obligations, no 'required', no contribution ask. Marketing content is confined to one parenthetical ('1,000 credits/month, no credit card required'). llms.txt contains NO feedback/contribution ask at all (grep for feedback/contribut/report/issue matches nothing relevant); the '## Optional' section (line 208) holds Playground, Discord, Blog only.
- Section order: # Tavily Docs (blockquote summary + one-line 'Use Tavily to…') → ## Start Here For Agents → ## Quick Start: CLI (fastest way to get started) → ## Alternative: Remote MCP Server → ## Choose How To Connect → ## Choose A Capability → ## Getting Started → ## API Reference → ## Enterprise API → ## SDKs → ## Public repositories → ## Best Practices → ## Ecosystem → ## Integrations → ## Partnerships → ## Examples → ## Updates → ## OpenAPI Specs → ## Optional

### Install commands found

```sh
pip install tavily-cli
tvly login
tvly search "latest AI news"
tvly extract "https://example.com"
tvly crawl "https://docs.example.com" --depth 2
tvly research "compare React vs Svelte for production apps"
claude mcp add tavily-remote-mcp --transport http https://mcp.tavily.com/mcp/
pip install tavily-python (in SDKs index line)
npm i @tavily/core (in SDKs index line)
```

### Text written for an agent (verbatim)

```text
If you are an AI agent — or building one — start with the canonical setup guide:

- [Agents: Tavily setup & tool-choice guide](https://docs.tavily.com/agents.md): Choose how to connect (SDK/API, MCP, or CLI + Skills), choose the right capability (Search, Extract, Map, Crawl, Research), and apply recommended defaults.
- [Documentation MCP setup](https://docs.tavily.com/agents.md#documentation-mcp-server): Search and read Tavily's documentation. MCP endpoint: `https://docs.tavily.com/mcp` (Streamable HTTP).

Then use this file ([llms.txt](https://docs.tavily.com/llms.txt)) to find a specific page, [llms-full.txt](https://docs.tavily.com/llms-full.txt) for the full text of all docs, or append `.md` to any docs URL to fetch that single page as Markdown.
```

### MCP snippet (verbatim)

````text
Connect any MCP client to Tavily's remote server — no local install required:

```
https://mcp.tavily.com/mcp/?tavilyApiKey=<your-api-key>
```

OAuth 2.0 is supported, so no API key needs to be hardcoded. For example, in Claude Code:

```
claude mcp add tavily-remote-mcp --transport http https://mcp.tavily.com/mcp/
```
````

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
## Quick Start: CLI (fastest way to get started)

Install the Tavily CLI and log in — two commands:

```
pip install tavily-cli
tvly login
```

`tvly login` opens a browser for authentication. Then search, extract, crawl, and research from the terminal:

```
tvly search "latest AI news"
tvly extract "https://example.com"
tvly crawl "https://docs.example.com" --depth 2
tvly research "compare React vs Svelte for production apps"
```

Installing the CLI also installs Agent Skills for coding agents like Claude Code, Cursor, and Codex — they remind the agent to use Tavily for search, extract, crawl, and research. Get a free API key at https://app.tavily.com (1,000 credits/month, no credit card required).

## Alternative: Remote MCP Server

Connect any MCP client to Tavily's remote server — no local install required:
````

### One-click links

- `claude mcp add tavily-remote-mcp --transport http https://mcp.tavily.com/mcp/ (llms.txt line 47)`
- `None of cursor:// / vscode: / codex:// appear in llms.txt itself; they appear only in llms-full.txt (see llms_full_txt)`

## llms-full.txt

- URL: <https://docs.tavily.com/llms-full.txt> — HTTP 200
- Size: 856222 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every docs page (192 '# ' H1 blocks, each with 'Source: <url>' on line 2, raw MDX components like <CardGroup>/<Card>/<Icon> left in). Ordering: Agents page first, then Changelog, About, Agent Skills, pricing, API reference, best practices, enterprise, integrations…, Welcome page last. llms.txt is a separately hand-authored index (its first 68 lines of prose do not appear verbatim in llms-full). llms-full.txt is 47x larger than llms.txt; agents.md explicitly gates it: 'When you need broad context and have the budget.'
- Install position: Line 1 is the '# Agents' page (Source: https://docs.tavily.com/agents) — the file OPENS with the setup guide, so first install commands are at lines 45 (claude mcp add --transport http tavily-docs https://docs.tavily.com/mcp), 107 (claude mcp add tavily-remote-mcp …), 113 (pip install tavily-cli / tvly login). The '# Welcome' landing page with the one-click 'Read and execute tavily.com/agent-setup/SKILL.md' prompt is at line 17041 (near the end, ~17,659 lines total). MCP JSON config ('mcpServers') first appears at line 10714.

## Other agent-facing files

### agents.md (canonical agent setup guide; /AGENTS.md redirects here) — HTTP 200

<https://docs.tavily.com/agents.md>

167-line, 11,618 B hand-written router: which docs to fetch (table of llms.txt / agents.md / llms-full.txt / page .md URLs with 'When to fetch it'), how to connect (persona table incl. 'An autonomous agent with no credentials or API key | Keyless / MCP'), no-key paths (Keyless header 'X-Tavily-Access-Mode: keyless' and x402 crypto pay-per-request), capability chooser, recommended parameter defaults, docs MCP with feedback tool. Starts with a Mintlify blockquote pointing back to llms.txt.

````text
To connect to the documentation server, use the settings below. No Tavily API key or sign-in is required.

* **Server URL:** `https://docs.tavily.com/mcp`
* **Transport:** Streamable HTTP

For Claude Code:

```bash theme={null}
claude mcp add --transport http tavily-docs https://docs.tavily.com/mcp
```

After connecting, try asking: "Find the URL limit for Tavily Extract in the documentation."

The documentation server also provides a tool for submitting documentation feedback.
````

### SKILL.md (hosted agent-setup skill, hand-written YAML frontmatter, 536 lines, 26,130 B) — HTTP 200

<https://www.tavily.com/agent-setup/SKILL.md>

The executable install prompt. Landing page says 'Read and execute tavily.com/agent-setup/SKILL.md'. Defines a preflight->CLI->auth->skills->verification state machine, forbids making the human do terminal-doable steps, forbids printing keys, forbids backgrounding `tvly login`, requires per-layer verification with an exact 5-line readiness checklist, then an 'After Successful Setup: Turn Setup Into Useful Work' section that classifies context (mid-session / new session with prior knowledge / new session cold) and drives the agent into a real task. Install: `curl -fsSL https://cli.tavily.com/install.sh | bash`, `npx skills add tavily-ai/skills --skill '*' --agent claude-code --global --yes`, `claude plugin install tavily@claude-plugins-official`, keyless MCP `claude mcp add tavily-remote-mcp --transport http https://mcp.tavily.com/mcp/ --header "X-Tavily-Access-Mode: keyless"`.

````text
### 5. Verify every layer

Verify installation layers separately instead of treating one successful API call as proof that everything is ready.

CLI:

```bash
command -v tvly
tvly --version
```

Authentication:

```bash
tvly auth --json
```

Live Tavily request:

```bash
tvly search "Tavily Search API" --json
```

[...]

Only report **Tavily ready** when the applicable states are true:

```text
Tavily CLI installed
Authentication verified
Live Tavily Search verified
Agent Skills installed
Agent Skills active in current session
```

If skills are installed but the current client needs a restart/rescan, say **"installed; restart/rescan required"** rather than claiming they are already active.
````

### .well-known/skills/index.json — HTTP 200

<https://docs.tavily.com/.well-known/skills/index.json>

358 B Mintlify skill-discovery manifest listing one skill 'tavilyai' with files ['SKILL.md'] (which resolves to https://docs.tavily.com/skill.md).

```text
{"skills":[{"name":"tavilyai","description":"Use when building AI agents that need real-time web search, content extraction, site crawling, or comprehensive research synthesis. Reach for Tavily when an agent must retrieve current information, extract structured data from URLs, map site structure, or generate cited research reports.","files":["SKILL.md"]}]}
```

### skill.md (Mintlify auto-generated docs skill, 289 lines, 14,001 B) — HTTP 200

<https://docs.tavily.com/skill.md>

Usage skill (not an install skill): product summary with pricing, 'When to use' / 'Do not use Tavily for', endpoint/cost table, SDK init snippets. Frontmatter metadata 'mintlify-proj: tavilyai'. Distinct from the hand-written www.tavily.com SKILL.md.

```text
Do not use Tavily for: static content you already have, authentication-gated pages (Crawl cannot log in), or tasks that don't require live web data.
```

### llms-install.md — HTTP 404

<https://docs.tavily.com/llms-install.md>

Does not exist (Mintlify 404 page returned).

### CLAUDE.md — HTTP 404

<https://docs.tavily.com/CLAUDE.md>

Does not exist.

### docs MCP endpoint — HTTP 405

<https://docs.tavily.com/mcp>

Live Streamable-HTTP MCP endpoint; plain GET returns 405 (expected for an MCP POST endpoint), i.e. it exists. Per agents.md it exposes doc search/read plus a documentation-feedback tool.

```text
claude mcp add --transport http tavily-docs https://docs.tavily.com/mcp
```

### tavily-mcp README (GitHub) — HTTP 200

<https://raw.githubusercontent.com/tavily-ai/tavily-mcp/main/README.md>

235-line per-client install guide: remote URL with key-in-URL or Bearer header, DEFAULT_PARAMETERS header, Claude Code (two auth options, --scope user tip), Cursor one-click install badge, Claude Desktop, etc.

```text
[![Install MCP Server](https://cursor.com/deeplink/mcp-install-dark.svg)](https://cursor.com/en/install-mcp?name=tavily-remote-mcp&config=eyJjb21tYW5kIjoibnB4IC15IG1jcC1yZW1vdGUgaHR0cHM6Ly9tY3AudGF2aWx5LmNvbS9tY3AvP3RhdmlseUFwaUtleT08eW91ci1hcGkta2V5PiIsImVudiI6e319)

Click the ⬆️ Add to Cursor ⬆️ button, this will do most of the work for you but you will still need to edit the configuration to add your API-KEY.
```

## What they do better than XERJ

- Persona routing with a no-shell / no-credential row. agents.md table: 'An autonomous agent with no credentials or API key | **Keyless / MCP** | No key, no account — add a header for Search/Extract, or use the no-key remote MCP.' XERJ's llms.txt buries the shell-less path as one bullet ('Agent quickstart … the HTTP-only path for an agent with no shell') under Start here after a CLI-first 4-step list; the FTX study says shell-less personas fail at every tier.
- A separate, executable install prompt with one sentence to paste: 'Read and execute tavily.com/agent-setup/SKILL.md', with one-click deeplinks per client (claude-cli://open?q=…, codex://new?prompt=…, cursor://anysphere.cursor-deeplink/prompt?text=…). XERJ has no hosted SKILL.md / install prompt URL; install lives inline in llms.txt.
- Explicit verification contract: SKILL.md '### 5. Verify every layer' with a 5-line readiness checklist and the rule 'say "installed; restart/rescan required" rather than claiming they are already active'. XERJ's Start here has no 'you are done when…' state list.
- Feedback via an MCP tool instead of a PR: agents.md 'The documentation server also provides a tool for submitting documentation feedback.' Zero-friction; no gh, no CLA, no repo write. (INFER: this is why they need no 'required' language.)
- MCP shipped and documented three ways: hosted remote URL, `claude mcp add …`, JSON mcpServers block (llms-full line 10714), Cursor install badge. XERJ's MCP server exists but is documented only via `xerj init` (local, needs shell).
- Budget-aware doc tiering table in agents.md ('llms-full.txt | Full text of all docs | When you need broad context and have the budget'; 'append .md to any docs URL').
- Hard behavioural rules for the installing agent, e.g. 'Do not make the user perform a step that can be completed from the terminal.', 'Never ask the user to find an API key or manually add TAVILY_API_KEY to .env', 'Waiting for human consent is not an error condition.', 'Do not repeat setup instructions after successful verification.'
- Post-install handoff: SKILL.md '## After Successful Setup: Turn Setup Into Useful Work' classifies mid-session / new-session and pushes the agent into a real task with 'Ask at most one next-step question in each response.'
- Hand-written skill discovery manifest at /.well-known/skills/index.json plus /skill.md, so skill-aware clients auto-discover without reading llms.txt.

## What XERJ does better

- Install commands are in llms.txt line 11 (second line of Start here) — Tavily's llms.txt also puts install at line 16, roughly parity, but XERJ's is a single zero-account `curl -fsSL https://xerj.org/get | sh` with no login/key step; Tavily needs `tvly login` browser OAuth or an API key for anything beyond Search/Extract.
- Inline honesty caveats: XERJ's '## Caveats (read before you rely on a capability)' and the lexical-vs-neural warning are in the file body; Tavily's llms.txt has no caveats section and its only limits note is agents.md '## Availability' (2 sentences).
- Failure-mode guidance (PATH consequence, data-dir-inside-indexed-tree trap, exit codes 0/1/2/3/4, progress-line parsing, estimate-before-run for the human's laptop) — Tavily's llms.txt has none of this; only the SKILL.md carries Node-18 troubleshooting.
- Reproducibility framing: 'Everything on this page is reproducible against a running XERJ node' and measured numbers; Tavily's llms.txt quotes no measurements.
- Manual verifiable install path linked from Start here ('the manual verifiable install without curl | sh') — Tavily offers only `curl … | bash` or `pip install`; no checksum discussion anywhere in the fetched files.
- A contribution ask at all: Tavily asks agents for nothing back (only a docs-feedback MCP tool). XERJ's one-command `xerj feedback --open-pr` is a stronger mechanism than anything Tavily ships — the defect is placement/wording, not the mechanism.

## Adoptable ideas

- Publish a hosted, executable install skill at a stable URL and make the hero prompt one sentence, copying Tavily's landing pattern verbatim-shaped: 'Read and execute tavily.com/agent-setup/SKILL.md' → 'Read and execute xerj.org/agent-setup/SKILL.md'. Add the same three deeplinks Tavily uses on its Welcome page (llms-full.txt lines 17144/17154/17164): `claude-cli://open?q=Read%20and%20execute%20xerj.org%2Fagent-setup%2FSKILL.md`, `codex://new?prompt=Read%20and%20execute%20xerj.org%2Fagent-setup%2FSKILL.md`, `cursor://anysphere.cursor-deeplink/prompt?text=Read%20and%20execute%20xerj.org%2Fagent-setup%2FSKILL.md`.
- Fix the 'required … under Optional' contradiction by copying Tavily's SKILL.md verification-gate shape: define readiness as a state list and put the report inside it. Tavily: 'Only report **Tavily ready** when the applicable states are true: Tavily CLI installed / Authentication verified / Live Tavily Search verified / Agent Skills installed / Agent Skills active in current session'. XERJ equivalent: 'Only report XERJ ready when: binary on PATH / node answers /_cluster/health / autoindex exit 0 or 3 / one query answered / field report filed or --dry-run handed to operator'. Rename the llms.txt section from '## Optional' to something like '## Your side of the deal' or move the template link into Start here step 4.
- Add a persona routing table at the top of llms.txt modeled on agents.md '## 2. Choose how to connect' ('If you are… | Use | Why it fits | Start at'), with an explicit row for the persona the FTX study shows failing: 'An agent with no shell (MCP-only or HTTP-only) | HTTP / hosted MCP | … | https://xerj.org/docs/agents/quickstart'. Tavily's exact row: 'An autonomous agent with no credentials or API key | **Keyless / MCP** | No key, no account — add a header for Search/Extract, or use the no-key remote MCP.'
- Add the doc-tier table from agents.md '## 1. Agent-readable docs' (columns 'Resource | What it is | When to fetch it'; row 'llms-full.txt | Full text of all docs | When you need broad context and have the budget.') so agents stop pulling the 84 KB llms-full when the 40 KB index suffices.
- Ship and document the MCP server with all three forms Tavily uses: a `claude mcp add --transport … xerj …` one-liner (Tavily: 'claude mcp add tavily-remote-mcp --transport http https://mcp.tavily.com/mcp/'), a JSON `mcpServers` block (Tavily llms-full line 10714: {"mcpServers":{"tavily-remote-mcp":{"command":"npx -y mcp-remote https://mcp.tavily.com/mcp/?tavilyApiKey=<your-api-key>","env":{}}}}), and a Cursor install badge (`https://cursor.com/en/install-mcp?name=…&config=<base64>`). Also add `/.well-known/skills/index.json` ({"skills":[{"name":…,"description":…,"files":["SKILL.md"]}]}) so skill-aware clients discover XERJ.
- Lower feedback friction with an in-band channel, following 'The documentation server also provides a tool for submitting documentation feedback.' — expose a `xerj_feedback` MCP tool (and/or a POST endpoint on the node) that files the field report without gh/CLA/PR, and make `--open-pr` the upgrade path. This directly targets the user's low-PR-count concern: the study's shell-less personas cannot run `xerj feedback --open-pr` at all.
- Copy the agent-behaviour rules that reduce human-blocking, verbatim from SKILL.md: 'Do not make the user perform a step that can be completed from the terminal.', 'Surface a manual prerequisite only when automation is genuinely blocked. State only the blocker and the minimum action required.', 'Do not repeat setup instructions after successful verification.', 'Ask at most one next-step question in each response.' These belong in the XERJ SKILL.md, not llms.txt.
- Add a post-install handoff section modeled on SKILL.md '## After Successful Setup: Turn Setup Into Useful Work' ('Do not mechanically show every user the same command demo.' / classify Mid-session vs New session) — XERJ's llms.txt ends its Start here at the field report with no 'now go do the task you came for' step, which is where the field-report ask gets forgotten.
