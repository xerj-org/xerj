# Cline

Category: agent / editor extension / MCP marketplace

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.cline.bot/llms.txt> — HTTP 200
- Size: 18871 bytes, 116 lines
- Install position: line 6 of 116 (2nd bullet): '- [Installing Cline](https://docs.cline.bot/getting-started/installing-cline.md): Choose your installation path: IDE Extension, CLI, SDK, or Kanban' — a pointer only; no command in the file
- Tone: neutral catalogue. llms.txt makes no demands of the agent whatsoever — it is an auto-generated (Mintlify) docs index with title+one-line description per page. The per-page 3-line 'Documentation Index' header is the only imperative ('Fetch…', 'Use this file…'). The install page is human-oriented step-by-step (Ctrl/Cmd+Shift+X, click Install). No 'you must', no obligations, no honesty caveats.
- Section order: # Cline (H1) → > blockquote one-liner: 'AI-powered coding agent for complex work' → (no ## headings at all — 112 flat '- [Title](url.md): description' bullets; the implicit order by bullet position is: Overview, Installing Cline, billing/ClinePass/free models, Authorization, Local models, ~15 provider-config pages, Config, Desktop/IDE/TUI/CLI, CLI samples, ACP, Kanban, Tools, Rules/Skills/Plugins/MCP/Hooks/Scheduling/Connectors, core workflows, features, troubleshooting, deprecations, SDK (~25 pages), Enterprise (~25 pages), Cline API (8 pages))

### Install commands found

```sh
(none in llms.txt itself)
npm install -g cline  (installing-cline.md line 87; llms-full.txt line 10979)
cline auth  (installing-cline.md line 93)
cline / cline "your task"  (installing-cline.md lines 99-101)
npx kanban  (installing-cline.md line 119)
mkdir my-agent && cd my-agent && npm init -y  (installing-cline.md lines 133-134)
npm install @cline/sdk  (installing-cline.md line 140; llms-full.txt line 15090)
npx skills add cline/sdk-skill  (llms-full.txt line 15102, SDK overview)
cline mcp  (mcp-overview.md line 53, interactive MCP wizard)
cline config mcp / cline config mcp --json  (mcp-overview.md lines 147-148)
npm i -g cline  (cline/cline README.md line 51; llms-full.txt CLI Overview line ~16781)
```

### Text written for an agent (verbatim)

```text
> ## Documentation Index
> Fetch the complete documentation index at: https://docs.cline.bot/llms.txt
> Use this file to discover all available pages before exploring further.

(this 3-line header is prepended to every docs .md page, e.g. https://docs.cline.bot/getting-started/installing-cline.md lines 1-3 and mcp-overview.md lines 1-3 — it is the only text in the set addressed to an agent reader; llms.txt itself contains no agent-directed instruction)
```

### MCP snippet (verbatim)

```text
{
  "mcpServers": {
    "local-server": {
      "command": "node",
      "args": ["/path/to/server.js"],
      "env": {
        "API_KEY": "your_api_key"
      },
      "disabled": false,
      "autoApprove": []
    }
  }
}

(https://docs.cline.bot/mcp/mcp-overview.md lines 73-85; config file location line 30: '* **CLI:** `~/.cline/mcp.json`'. Remote variant lines 91-103 uses "type": "streamableHttp", "url", "headers".)
```

### Feedback / contribution ask (verbatim)

```text
(none in llms.txt or llms-full.txt. Nearest items: installing-cline.md lines 151-154 '## Need Help?\n\n* [Troubleshooting](/troubleshooting/networking-and-proxies)\n* [Discord community](https://discord.gg/cline)'; using-commands page in llms-full.txt line 3700 '| `/reportbug`     | Report a bug with diagnostic info |'; README.md line 231 'Start with the [Contributing Guide](CONTRIBUTING.md). Join our [Discord](https://discord.gg/cline) and head to the `#contributors` channel to connect with other contributors.')
```

### Best passage for driving install and first use (verbatim)

````text
## CLI

Use this if you want Cline in terminal workflows (interactive + automation).

<Steps>
  <Step title="Install Node.js">
    Install Node.js 20+ (22 recommended).
  </Step>

  <Step title="Install CLI">
    ```bash theme={"system"}
    npm install -g cline
    ```
  </Step>

  <Step title="Authenticate">
    ```bash theme={"system"}
    cline auth
    ```
  </Step>

  <Step title="Run Cline">
    ```bash theme={"system"}
    cline
    # or
    cline "your task"
    ```
  </Step>
</Steps>

(https://docs.cline.bot/getting-started/installing-cline.md lines 76-104)
````

### One-click links

- `(none — no cursor://, vscode:, claude mcp add, or npx … add install links in llms.txt, llms-full.txt or installing-cline.md; only conventional marketplace URLs: https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude-dev and https://plugins.jetbrains.com/plugin/28247-cline)`
- `npx skills add cline/sdk-skill  (closest thing to a one-liner agent install; llms-full.txt line 15102)`

## llms-full.txt

- URL: <https://docs.cline.bot/llms-full.txt> — HTTP 200
- Size: 675700 bytes
- Relationship to llms.txt: Mechanically generated concatenation of the same 112 pages the llms.txt indexes (112 '# Title\nSource: <url>' blocks; Mintlify markers like ```bash theme={"system"} and <Steps>/<Tab>/<Card> JSX are left in). Not curated; no preamble, no section map, no install-first ordering. llms.txt = titles+descriptions; llms-full.txt = full page bodies in a different (alphabetical) order.
- Install position: '# Installing Cline' begins at line 10896 of 17545 (~62% in). Pages are concatenated in ALPHABETICAL URL-path order (api/authentication first at line 1, then api/*, best-practices/*, cli/*, cline-overview at 3370, core-workflows/*, customization/*, enterprise-solutions/*, getting-started/* …, usage/cli-overview at 16774). First occurrence of 'npm install -g cline' is line 2048 inside a GitHub Actions YAML sample, not an install section.

## Other agent-facing files

### marketing llms.txt (cline.bot) — HTTP 200

<https://cline.bot/llms.txt>

Hand-curated 46-line marketing index with ## section headers (Product Overview, AI Workflows, Tools & Integrations, Documentation & Guides, Trust/Privacy, News, Use Cases, Other) and a '_Last-Updated: 2025-07-16_' footer; links mostly to blog posts. Stale (cites v3.18). Install is two pointers, no commands.

```text
- [Cline IDE Install Page](https://cline.bot/get-cline?ref=cline.ghost.io): Set up Cline in VS Code and other IDEs.
- [MCP Marketplace](https://cline.bot/mcp-marketplace): Discover and install MCP plugins to enhance AI development.
```

### Installing Cline (docs page as markdown) — HTTP 200

<https://docs.cline.bot/getting-started/installing-cline.md>

The actual install page (154 lines, 3,813 B). Path-chooser structure: IDE Extension / CLI / Kanban / SDK, each 'Use this if you want…' then <Steps>. IDE path is GUI clicks; CLI path is 3 commands; ends with 'Need Help?' (Troubleshooting + Discord).

```text
## Choose Your Install Path

* [IDE Extension](#ide-extension) — VS Code, Cursor, JetBrains, Windsurf, VSCodium, Antigravity
* [CLI](#cli) — terminal workflows
* [Kanban](#kanban) (preview) — easily manage multiple agents through a kanban board
* [SDK](#sdk) — build with `@cline/sdk`
```

### MCP overview — HTTP 200

<https://docs.cline.bot/mcp/mcp-overview.md>

How to add MCP servers to Cline (149 lines). Quick-start numbered list, config file location `~/.cline/mcp.json`, two verbatim JSON configs (STDIO and streamableHttp), `cline mcp` wizard table, security basics, troubleshooting table. Nothing about installing Cline itself via MCP.

```text
## Quick start

1. Open **MCP Servers** in Cline
2. Add a local server manually, or connect to a hosted remote server
3. Configure credentials/environment variables
4. Verify tools appear and test one tool call
```

### AGENTS.md (cline/cline) — HTTP 200

<https://raw.githubusercontent.com/cline/cline/main/AGENTS.md>

5,279 B, 32 lines. Purely a cloud-dev-VM operating manual for agents working ON the monorepo (Bun toolchain, build order, DISPLAY=:1, known flaky tests). Not a user/install document; contains no llms.txt pointer, no feedback ask.

```text
- An actual agent turn requires an **LLM provider credential**. With no credentials the default `cline` provider fails fast with an `Unauthorized` error and the interactive TUI shows a provider sign-in screen. Configure via `cline auth` or provider env vars (e.g. `ANTHROPIC_API_KEY`, `CLINE_API_KEY`, `OPENROUTER_API_KEY`); see `apps/cli/README.md`.
```

### MCP marketplace README (cline/mcp-marketplace) — HTTP 200

<https://raw.githubusercontent.com/cline/mcp-marketplace/main/README.md>

Submission rules for the one-click MCP marketplace. Defines the adoption bar as 'agent can set it up from README alone' and the optional llms-install.md convention. Submission is a GitHub ISSUE with a template, not a PR.

```text
3. Confirm that you have tested giving Cline just your `README.md` and/or the `llms-install.md` and watched him successfully setup the server. This will help prevent rejection in case we have trouble setting up your server using Cline.
```

### cline/cline README.md — HTTP 200

<https://raw.githubusercontent.com/cline/cline/main/README.md>

235 lines. Two-column table with the CLI install one-liner and marketplace buttons; 'Contributing' section points at CONTRIBUTING.md and Discord #contributors. No field-report or feedback obligation.

````text
### CLI

Run Cline in your terminal.
Interactive chat or fully headless 
for CI/CD and scripting.

```
npm i -g cline
```
````

### CLAUDE.md (cline/cline) — HTTP 404

<https://raw.githubusercontent.com/cline/cline/main/CLAUDE.md>

does not exist

### llms-install.md (cline/cline) — HTTP 404

<https://raw.githubusercontent.com/cline/cline/main/llms-install.md>

does not exist — the project that coined the convention does not ship one itself

### SKILL.md (cline/cline) — HTTP 404

<https://raw.githubusercontent.com/cline/cline/main/SKILL.md>

does not exist in the main repo (the SDK skill lives in a separate repo cline/sdk-skill, installed via `npx skills add cline/sdk-skill`)

## What they do better than XERJ

- Every docs page fetched as .md carries a 3-line agent header that closes the loop back to the index: '> ## Documentation Index / > Fetch the complete documentation index at: https://docs.cline.bot/llms.txt / > Use this file to discover all available pages before exploring further.' XERJ's answer/docs pages do not point back to llms.txt (INFERRED from the local landing copy; not verified against every xerj.org page).
- Install page is a persona/path chooser first ('Choose Your Install Path' → IDE Extension / CLI / Kanban / SDK, each with 'Use this if you want…'), so a reader with no terminal is routed to a GUI path in line 11 rather than discovering mid-page that the doc assumes a shell. XERJ's Start-here is one shell path with the no-shell route pushed to a link on line 31.
- Install path is 3 short commands with no prose between them (npm install -g cline / cline auth / cline). XERJ's Start-here puts ~1,500 chars of installer/checksum/PATH prose between the curl line and step 1.
- A one-command agent-context install exists: 'npx skills add cline/sdk-skill' with the sentence 'If you use a coding agent (Claude Code, Codex, Cline, etc.), install the [Cline SDK skill]… to give your agent context on the SDK's APIs' (llms-full.txt lines 15098-15104).
- Every .md page is individually fetchable at the same URL as the HTML page plus '.md', and llms.txt links to the .md form directly, so an agent never has to scrape HTML.
- MCP config is given as two complete copy-pasteable JSON blocks plus the exact file path (`~/.cline/mcp.json`), plus a non-interactive verification command (`cline config mcp --json`). XERJ's llms.txt mentions `xerj init` writes `.mcp.json` but shows no JSON and no verification command inline.
- Contribution asks are frictionless and low-ceremony: marketplace submission is a pre-templated GitHub ISSUE (issues/new?template=mcp-server-submission.yml), and README says only 'Join our Discord… #contributors'. No obligation language, so nothing reads as contradictory.

## What XERJ does better

- XERJ's llms.txt is a written document with a thesis, inline caveats and a Start-here run order; Cline's docs llms.txt is a plain auto-generated Mintlify docs index with nothing about installation beyond a one-line pointer — it makes zero demands and gives zero guidance to a first-time agent.
- XERJ has an actual install command in llms.txt line 11 (`curl -fsSL https://xerj.org/get | sh`) and a numbered first-run sequence; Cline's llms.txt has no command at all and llms-full.txt buries 'Installing Cline' at line 10,896 of 17,545 because pages are concatenated alphabetically.
- XERJ states honesty caveats inline ('the default embedding mode is lexical, not neural') and positioning rules; Cline has no equivalent 'where agents get this wrong' block anywhere in the set.
- XERJ ships machine-readable tool schemas (mcp-tools.json / openai-tools.json / anthropic-tools.json) linked from llms.txt; Cline's set has none.
- XERJ documents the no-shell path explicitly ([Agent quickstart]: 'the HTTP-only path for an agent with no shell') and the sandboxed-no-gh fallback for feedback ('Run --dry-run, then hand the printed report to your operator'); Cline's install page has no headless/no-GUI/no-shell alternative for the IDE path.
- XERJ has a defined contribution protocol with a verbatim one-command PR opener (`xerj feedback --open-pr …`); Cline has only Discord + a generic CONTRIBUTING link and a /reportbug slash command.
- XERJ's llms-full.txt is curated (numbered sections like §10) rather than a 675 KB alphabetical dump.

## Adoptable ideas

- Prepend a 3-line index header to every xerj.org answer/docs .md page, copying Cline's wording (https://docs.cline.bot/getting-started/installing-cline.md lines 1-3): '> ## Documentation Index\n> Fetch the complete documentation index at: https://xerj.org/llms.txt\n> Use this file to discover all available pages before exploring further.' — cheap, mechanical, and fixes the hop-2+ agents that land on an answer page first.
- Restructure '## Start here' as a path chooser in the first 5 lines, modelled on installing-cline.md lines 9-14 ('## Choose Your Install Path' with one bullet per persona and 'Use this if you want…' openers): e.g. '* Shell available — curl | sh, 3 commands · * No shell / MCP-only — HTTP-only quickstart (link) · * Policy forbids curl|sh — manual verified install (link)'. This directly targets the shell-less-persona failure and the policy-constrained persona that flipped to success once the manual path was documented.
- Collapse the shell install to a 3-command block with zero prose between the commands, as installing-cline.md lines 85-103 do ('npm install -g cline' / 'cline auth' / 'cline'). Move the checksum/PATH explanation below the block under its own heading; keep only the one-line PATH hint next to the command.
- Put a complete copy-pasteable MCP JSON block and the config-file path inline in llms.txt, matching mcp-overview.md lines 30 and 73-85 ('* **CLI:** `~/.cline/mcp.json`' + the {"mcpServers":{…}} block), plus a non-interactive verification command analogous to 'cline config mcp --json' (line 148). This ships the MCP server the study said 'exists but was not shipped/documented'.
- Adopt the marketplace's adoption bar as an internal release gate, quoting https://raw.githubusercontent.com/cline/mcp-marketplace/main/README.md line 34: 'Confirm that you have tested giving Cline just your `README.md` and/or the `llms-install.md` and watched him successfully setup the server.' — i.e. run the FTX harness with ONLY llms.txt as input before each llms.txt change lands.
- Ship an actual `llms-install.md` at the repo root (Cline coined the convention in README line 49 — 'you can include a `llms-install.md` file in your repository to provide additional guidance to agents like Cline. This can be particularly useful for complex setups requiring specific environment configurations or API keys' — but none of the probed repos, including cline/cline, has one). A short, imperative, shell-and-no-shell install file at that path is a free differentiator and is what Cline's own marketplace installer looks for.
- Offer a one-command agent-context install like 'npx skills add cline/sdk-skill' (llms-full.txt line 15102) with the same framing sentence: 'If you use a coding agent (Claude Code, Codex, Cline, etc.), install the … skill to give your agent context on …'. XERJ already writes a skill via `xerj init`; expose it as `npx skills add xerj-org/xerj-skill` (or equivalent) so agents without the binary can still load the skill.
- Fix the required/Optional contradiction by taking the tone lesson, not the wording: Cline never uses obligation language and its contribution ask is a pre-templated GitHub issue link (mcp-marketplace README line 29: 'Create a [new issue](https://github.com/cline/mcp-marketplace/issues/new?template=mcp-server-submission.yml)'). Either rename '## Optional' to '## Your side of the deal (required)' and move the field-report bullet up under Start here step 5, or drop 'required, not optional' — but do not keep both. Also add a zero-tooling fallback link of the Cline form: a GitHub issue template URL (issues/new?template=agent-field-report.yml) for agents that cannot run `gh` or `xerj feedback --open-pr`.
