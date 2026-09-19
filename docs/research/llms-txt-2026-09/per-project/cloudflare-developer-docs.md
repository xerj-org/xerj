# Cloudflare Developer Docs (Workers / Agents) — developers.cloudflare.com + cloudflare/agents, cloudflare/workers-sdk, cloudflare/skills, cloudflare/mcp, cloudflare/mcp-server-cloudflare

Category: AI infra / edge platform

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://developers.cloudflare.com/llms.txt> — HTTP 200
- Size: 16049 bytes, 138 lines
- Install position: ABSENT from the root llms.txt. It is a plain auto-generated hub index: 138 lines, every line is `- [Product](https://developers.cloudflare.com/<slug>/llms.txt): one-line blurb`. No command, no prompt, no MCP snippet. The only agent-install pointer is line 128 under '## Other': '- [Agent setup](https://developers.cloudflare.com/agent-setup/llms.txt): Install an agent of your choice, connect skills and MCP servers, and start deploying to Cloudflare — all from your editor or terminal.' Installation lives two hops away: agent-setup/llms.txt (21 lines, 2,900 B) -> agent-setup/<agent>/index.md (e.g. claude-code, 299 lines, 18,923 B) whose first H2 is '## Quick start'.
- Tone: Root llms.txt: neutral catalogue, zero demands of the agent (it SAYS 'recommended way to explore'; I INFER it is machine-generated from the site nav). Per-page .md header is mildly imperative toward the agent ('Fetch the complete documentation index at ... Use this file to discover all available pages before exploring further.'). Agent-setup pages are neutral-instructional, numbered 4-step Quick start (Install agent -> Launch in project -> Install plugin -> Try a prompt), each with a ready-to-paste prompt; light marketing only in the 'Build agents on Cloudflare' footer. No obligations, no 'required', no honesty caveats about capabilities.
- Section order: # Cloudflare Developer Documentation (1-line intro + blockquote: 'Each product below links to its own llms.txt ... recommended way to explore a specific product's content.') → ## Application performance → ## Application security → ## Cloudflare One → ## Consumer services → ## Core platform → ## Developer platform (contains Agents, Workers, Workers AI, Vectorize, ... each -> <product>/llms.txt) → ## Docs collections → ## Network security → ## Other (contains 'Agent setup', 'Docs for agents', 'Migration Guides', 'Use cases')

### Install commands found

```sh
curl -fsSL https://claude.ai/install.sh | bash   (agent-setup/claude-code step 1)
claude   (step 2, 'from the root of your project, where `wrangler.jsonc` lives')
/plugin marketplace add cloudflare/skills   (step 3, Claude Code slash command)
/plugin install cloudflare@cloudflare   (step 3)
/add-plugin cloudflare   (agent-setup/cursor step 2)
npm install -g @openai/codex   (agent-setup/codex step 1)
/plugins   (Codex step 3: 'search for and install **Cloudflare**')
codex plugin marketplace add cloudflare/skills   (cloudflare/skills README)
codex plugin add cloudflare@cloudflare   (cloudflare/skills README)
curl -fsSL https://opencode.ai/install | bash   (agent-setup/opencode step 1)
npx skills add https://github.com/cloudflare/skills   (agent-setup/opencode step 2; cloudflare/skills README)
claude mcp list   (Tips: 'Use `claude mcp list` to verify Cloudflare servers are connected')
claude mcp remove cloudflare   (Troubleshooting)
npx cf   (Wrangler section: 'The unified `cf` CLI is in technical preview ... Try it with `npx cf`')
curl -H "Accept: text/markdown" https://developers.cloudflare.com/agent-setup/   (markdown retrieval, agent-setup/llms.txt preamble)
```

### Text written for an agent (verbatim)

````text
[From https://developers.cloudflare.com/agent-setup/claude-code/index.md — header injected into EVERY .md page and every llms-full.txt:]
> Documentation Index  
> Fetch the complete documentation index at: https://developers.cloudflare.com/agent-setup/llms.txt  
> Use this file to discover all available pages before exploring further.

[Same page, '4. **Try a prompt**' — the first-use prompt handed to the agent:]
   ```txt
   Check my Workers deployment logs for errors and suggest fixes.
   ```

[Same page, '## Example prompts':]
Set up rate limiting and WAF rules to block abuse on my public API.
Build an image upload and transformation service using R2 and Cloudflare Images.
Configure Zero Trust access policies to protect my internal staging environment.
Add bot protection and rate limiting to my login and checkout endpoints.
Check my Workers deployment logs for errors and suggest fixes.

[From the skill description embedded in the page, an instruction written for the agent to execute:]
- nextjs-on-cloudflare ... For setup, migration, or deployment, install vinext's upstream skills with `npx skills add cloudflare/vinext` if missing, then read and follow the applicable skill and docs.
````

### MCP snippet (verbatim)

````text
[https://developers.cloudflare.com/agent-setup/visual-studio-code/index.md, Quick start step 2 'Create `.vscode/mcp.json` inside your workspace folder:']
   ```json
   {
     "servers": {
       "cloudflare-api": {
         "type": "http",
         "url": "https://mcp.cloudflare.com/mcp"
       }
     }
   }
   ```

   Visual Studio Code uses `servers` as the root key. Configurations copied from Cursor or Claude Desktop use `mcpServers` and will silently do nothing here.

[https://developers.cloudflare.com/agent-setup/opencode/index.md, step 3 'Add MCP servers to `.opencode.jsonc`']
   {
     "mcp": {
       "cloudflare": { "type": "remote", "url": "https://mcp.cloudflare.com/mcp", "enabled": true },
       "cloudflare-docs": { "type": "remote", "url": "https://docs.mcp.cloudflare.com/mcp", "enabled": true },
       "cloudflare-bindings": { "type": "remote", "url": "https://bindings.mcp.cloudflare.com/mcp", "enabled": true },
       "cloudflare-builds": { "type": "remote", "url": "https://builds.mcp.cloudflare.com/mcp", "enabled": true },
       "cloudflare-observability": { "type": "remote", "url": "https://observability.mcp.cloudflare.com/mcp", "enabled": true }
     }
   }

[https://raw.githubusercontent.com/cloudflare/mcp/main/README.md '### Option 1: OAuth (Recommended)']
Just connect to the MCP server URL - you'll be redirected to Cloudflare to authorize and select permissions.
{
  "mcpServers": {
    "cloudflare-api": {
      "type": "http",
      "url": "https://mcp.cloudflare.com/mcp"
    }
  }
}
````

### Feedback / contribution ask (verbatim)

```text
Root llms.txt: NONE. agent-setup pages: NONE beyond the site-wide footer 'Was this helpful?\n\nYesNo'. The only explicit ask is in the mcp-server-cloudflare README '## Need access to more Cloudflare tools?': "We're continuing to add more functionality to this remote MCP server repo. If you'd like to leave feedback, file a bug or provide a feature request, [please open an issue](https://github.com/cloudflare/mcp-server-cloudflare/issues/new/choose) on this repository". No field-report, no 'required' contribution language anywhere; AGENTS.md files talk only about PR mechanics for code contributors (workers-sdk: 'Use `.github/PULL_REQUEST_TEMPLATE.md` when preparing a pull request. The PR description requirements are enforced by `tools/deployments/validate-pr-description.ts`; treat that validator and the template as authoritative.').
```

### Best passage for driving install and first use (verbatim)

````text
[https://developers.cloudflare.com/agent-setup/claude-code/index.md]
## Quick start

1. **Install Claude Code**

   Install the Claude Code CLI. For Windows, Homebrew, WinGet, or npm, see the [Claude Code setup guide ↗](https://docs.anthropic.com/en/docs/claude-code/setup).

   ```bash
   curl -fsSL https://claude.ai/install.sh | bash
   ```

2. **Launch Claude Code in your project**

   Start Claude Code from the root of your project, where `wrangler.jsonc` lives (if it already exists).

   ```bash
   claude
   ```

3. **Install the Cloudflare plugin**

   In Claude Code, run these two slash commands. This installs Cloudflare Skills and registers the Cloudflare MCP servers.

   ```txt
   /plugin marketplace add cloudflare/skills
   /plugin install cloudflare@cloudflare
   ```

4. **Try a prompt**

   ```txt
   Check my Workers deployment logs for errors and suggest fixes.
   ```
````

### One-click links

- `NONE found. grep for cursor://, vscode:, 'claude mcp add' across root llms.txt, agent-setup/*/index.md, cloudflare/mcp README, mcp-server-cloudflare README, cloudflare/skills README returned zero hits. Closest equivalents are marketplace slash-commands (/plugin marketplace add cloudflare/skills; /add-plugin cloudflare) and a Cursor Marketplace URL: https://cursor.com/marketplace/cloudflare`

## llms-full.txt

- URL: <https://developers.cloudflare.com/llms-full.txt> — HTTP 200
- Size: 59403277 bytes
- Relationship to llms.txt: Concatenated, machine-generated: measured today 59,403,277 bytes / 1,075,066 lines (the task brief's 36.5MB/672,277 lines is stale — it has grown). It is the concatenation of every page's index.md (frontmatter '---' + description/title/image, then page body), in product order. NOT curated. docs-for-agents/index.md describes it as: '| [`/llms-full.txt`](https://developers.cloudflare.com/llms-full.txt) | Full content of all documentation in a single file, for offline indexing, bulk vectorization (converting content into numerical representations for similarity search), or large-context models |'. Per-product scoped variants exist: '/workers/llms-full.txt | Full content of all Workers documentation pages' — 'Replace `/workers/` with any product path.' Extra affordance documented there: 'The response includes `x-markdown-tokens` and `x-original-tokens` headers with estimated token counts for the Markdown document and original HTML document, useful for context window planning'.
- Install position: Absent as a section. The file starts with the alphabetically-first product (Argo Smart Routing) and has 6,975 '# ' page titles; installation appears only where individual product pages happen to contain it. Every page block starts with the same injected header ('> Documentation Index / Fetch the complete documentation index at: https://developers.cloudflare.com/<product>/llms.txt / Use this file to discover all available pages before exploring further.') and a 'Last updated ...|Copy as Markdown| [View as Markdown](...index.md)| [Agent setup](https://developers.cloudflare.com/agent-setup/)' line — so a link to the agent-setup hub is on every single page.

## Other agent-facing files

### agent-setup/llms.txt (hub of per-agent setup guides) — HTTP 200

<https://developers.cloudflare.com/agent-setup/llms.txt>

21-line index: one .md page per coding agent (Bionic, Claude Code, Codex, Command Code, Cursor, GitHub Copilot, OpenCode, VS Code, VS Code detailed walkthrough, Windsurf). Documents the markdown-retrieval trick.

```text
> Links below point directly to Markdown versions of each page. Any page can also be retrieved as Markdown by sending an `Accept: text/markdown` header to the page's URL without the `index.md` suffix (for example, `curl -H "Accept: text/markdown" https://developers.cloudflare.com/agent-setup/`).
```

### agent-setup/index.md (Pick your agent + compare table + concepts) — HTTP 200

<https://developers.cloudflare.com/agent-setup/index.md>

Persona router: filter by workflow (Terminal / IDE / Cloud / Extension), comparison table (Pricing / Model / Context / Open source), and a glossary that defines Skills vs MCP vs context types so a first-timer can choose a path.

```text
Every agent listed supports Skills and MCP.

## Understanding agents

Common types, concepts, and tradeoffs.

### Workflow

Where the agent runs changes how you interact with it.

Terminal

Runs in a shell. Best for automation, scripting, and CI pipelines.

IDE

Full code editor with AI first-class. Visual diffs, multi-file edits.

Cloud

Hosted infrastructure. Ideal for async, long-running work.

Extension

Plugs into an existing editor. Lightest install, keeps your setup.
```

### agent-setup/claude-code/index.md — HTTP 200

<https://developers.cloudflare.com/agent-setup/claude-code/index.md>

Per-agent page: Quick start (4 steps) -> Cloudflare platform access (Skills list, 18 MCP server URLs, Wrangler, Agent-friendly docs) -> Example prompts -> Tips -> FAQ -> Troubleshooting. Troubleshooting has a no-MCP fallback path.

```text
Getting outdated information about Cloudflare products

Enable the Cloudflare docs MCP server so the agent can fetch current documentation at runtime. If you prefer not to use the MCP server, point the agent directly at developers.cloudflare.com/llms.txt for a directory of every product, or developers.cloudflare.com/<product>/llms.txt for a product-specific index.
```

### docs-for-agents/index.md (how agents consume the docs) — HTTP 200

<https://developers.cloudflare.com/docs-for-agents/index.md>

Meta-page explaining the four ways to get markdown (Copy button, /index.md suffix, Accept header, llms endpoints), the token-count response headers, per-product llms-full, and the OpenAPI spec repo for agents.

```text
Every documentation page is available as Markdown using any of the following methods, powered by [Markdown for Agents](https://developers.cloudflare.com/fundamentals/reference/markdown-for-agents/).

### Copy from the current page

### Append `/index.md` to the URL

### Send an `Accept: text/markdown` header

The response includes `x-markdown-tokens` and `x-original-tokens` headers with estimated token counts for the Markdown document and original HTML document, useful for context window planning
```

### agents/model-context-protocol/index.md (MCP page) — HTTP 200

<https://developers.cloudflare.com/agents/model-context-protocol/index.md>

About BUILDING MCP servers on Workers, not about installing Cloudflare's MCP into an agent. Contains a 'Best Practices' block on tool design.

```text
- **Tool design**: Do not treat your MCP server as a wrapper around your full API schema. Instead, build tools that are optimized for specific user goals and reliable outcomes. Fewer, well-designed tools often outperform many granular ones, especially for agents with small context windows or tight latency budgets.
```

### cloudflare/mcp README (Code Mode MCP server) — HTTP 200

<https://raw.githubusercontent.com/cloudflare/mcp/main/README.md>

Install page for the recommended single MCP server: OAuth-vs-API-token, one JSON config block, a token-cost table justifying the design.

```text
| Approach                                    | Tools | Token cost | Context used (200K) |
| Raw OpenAPI spec in prompt                  | —     | ~2,000,000 | 977%                |
| Native MCP (full schemas)                   | 2,594 | 1,170,523  | 585%                |
| Native MCP (minimal — required params only) | 2,594 | 244,047    | 122%                |
| Code mode                                   | 3     | ~1,100     | 0.5%                |

## Get Started

MCP URL: `https://mcp.cloudflare.com/mcp`

### Option 1: OAuth (Recommended)

Just connect to the MCP server URL - you'll be redirected to Cloudflare to authorize and select permissions.
```

### cloudflare/mcp-server-cloudflare README (domain-specific MCP servers) — HTTP 200

<https://raw.githubusercontent.com/cloudflare/mcp-server-cloudflare/main/README.md>

Table of 17 remote MCP server URLs, 'Which server should you use?' decision guide, the only feedback ask found, and a context-overflow troubleshooting note.

```text
## Connect to an MCP server

Connect any MCP client with remote-server support directly to a URL in the table above. [Cloudflare AI Playground](https://playground.ai.cloudflare.com/) also accepts server URLs in its interface.
...
## Need access to more Cloudflare tools?

We're continuing to add more functionality to this remote MCP server repo. If you'd like to leave feedback, file a bug or provide a feature request, [please open an issue](https://github.com/cloudflare/mcp-server-cloudflare/issues/new/choose) on this repository
```

### cloudflare/skills README — HTTP 200

<https://raw.githubusercontent.com/cloudflare/skills/main/README.md>

One install recipe per agent (Codex, Claude Code, VS Code/Copilot, Cursor, npx skills, Clone/Copy with a per-agent skill-directory table), then a skill catalogue.

````text
## Installing

Use the native plugin where supported to install both Cloudflare guidance and the Cloudflare MCP server. Agents that only support the Agent Skills standard can install the skills separately.

### Claude Code

```
/plugin marketplace add cloudflare/skills
/plugin install cloudflare@cloudflare
```

### npx skills

```
npx skills add https://github.com/cloudflare/skills
```
````

### AGENTS.md on cloudflare/agents — HTTP 200

<https://raw.githubusercontent.com/cloudflare/agents/main/AGENTS.md>

Repo-contributor guide for coding agents (196 lines, 9,313 B): structure, nested AGENTS.md table, pnpm commands, code standards, Contributing/Changesets/PR process, 'Learned Workspace Facts' + 'Learned User Preferences' sections, and 'Boundaries'. Nothing about installing the product.

```text
## Boundaries

**Always:**

- Run `pnpm run check` before considering work done
- Use `import type` for type-only imports (enforced by `verbatimModuleSyntax`)
```

### AGENTS.md on cloudflare/workers-sdk — HTTP 200

<https://raw.githubusercontent.com/cloudflare/workers-sdk/main/AGENTS.md>

Repo-contributor guide (153 lines, 9,162 B): Start Here, Common Commands, Repository Map, conventions, PR rules enforced by a validator.

```text
## Start Here

- Use `pnpm`, not npm or yarn.
- Use the Node.js and pnpm versions declared in `package.json`.
- Install dependencies with `pnpm install`.
- Run commands from the workspace root unless package documentation says
  otherwise.
- Before changing a package, read its `AGENTS.md` if it has one.
- Do not edit generated files directly. Change their source or generator and
  regenerate them.
```

### llms-install.md / CLAUDE.md / SKILL.md at docs root; CLAUDE.md on cloudflare/agents — HTTP 404

<https://developers.cloudflare.com/llms-install.md>

Do not exist. Checked: developers.cloudflare.com/llms-install.md 404, /CLAUDE.md 404, /AGENTS.md 404, /SKILL.md 404, raw.githubusercontent.com/cloudflare/agents/main/CLAUDE.md 404. Skills live as SKILL.md files inside cloudflare/skills/<skill>/ (not fetched individually).

## What they do better than XERJ

- Persona routing by capability, not by OS: agent-setup/index.md filters 'Terminal / IDE / Cloud / Extension' and states 'Every agent listed supports Skills and MCP.' Each of 10 agents gets its own page with a Quick start in that agent's own syntax (/plugin ..., /add-plugin, .vscode/mcp.json with 'servers' root key, .opencode.jsonc). XERJ has one shell-centric 'Start here' and a single 'Agent quickstart' link for the no-shell case — exactly the persona the 22-run study shows failing.
- Every install path ends in a ready-to-paste first prompt ('4. **Try a prompt** ... Check my Workers deployment logs for errors and suggest fixes.') plus five '## Example prompts'. First-use is a copy action, not a decision.
- A zero-shell, zero-install fallback is documented in Troubleshooting: 'If you prefer not to use the MCP server, point the agent directly at developers.cloudflare.com/llms.txt ...'. There is always a degraded path that still works.
- Verification is a named command: 'Use `claude mcp list` to verify Cloudflare servers are connected.' and Troubleshooting gives the remove/re-add recipe. XERJ's llms.txt has no 'how do I know the MCP registration worked' step after `xerj init`.
- Every .md page and every llms-full chunk carries the same 3-line agent header ('Fetch the complete documentation index at ... Use this file to discover all available pages before exploring further.') and a footer link to '[Agent setup]'. Discovery is redundant on every hop; nothing depends on the agent landing on the root first.
- Token accounting is explicit and measured: the cloudflare/mcp README table ('Code mode | 3 | ~1,100 | 0.5%') and the `x-markdown-tokens` / `x-original-tokens` response headers. This is the honest-numbers style XERJ values, applied to the install decision itself.
- Config-format footguns are called out inline at the point of failure: 'Visual Studio Code uses `servers` as the root key. Configurations copied from Cursor or Claude Desktop use `mcpServers` and will silently do nothing here.'
- Scoped llms.txt / llms-full.txt per product plus the `Accept: text/markdown` header and `/index.md` suffix — three ways to get any page as markdown, so an agent never has to parse HTML.

## What XERJ does better

- XERJ's llms.txt actually contains the install commands and first-run sequence; Cloudflare's root llms.txt is a plain auto-generated product directory with nothing about installation — an agent needs 2-3 hops to reach a command.
- XERJ states honesty caveats up front ('## Caveats (read before you rely on a capability)', lexical-not-neural, exit-code semantics, the data-dir-inside-indexed-tree trap, the PATH consequence). Cloudflare's agent pages have no capability caveats at all.
- XERJ has an explicit contribution/feedback protocol with a one-command path (`xerj feedback --open-pr ...`, `--dry-run`, 'hand the printed report to your operator'). Cloudflare's only ask across all files is 'please open an issue' in one README; the docs pages end with a 'Was this helpful? YesNo' widget.
- XERJ documents the sandboxed / no-`gh` / policy-constrained case ('Sandboxed and unable to open a PR? Write the report out in full to your operator and ask them to file it'). Cloudflare assumes OAuth in a browser ('When prompted, complete the authorization flow in your browser') with only a CI bearer-token alternative.
- XERJ ships machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) generated from a real tools/list; Cloudflare links to an OpenAPI repo but the agent pages carry no schema.
- XERJ's llms-full.txt (84 KB) is curated and sectioned (e.g. '§10 the full run-it-for-a-human protocol'); Cloudflare's is a 59 MB unsorted concatenation no single context can hold.

## Adoptable ideas

- Add a per-client 'agent setup' spoke and index it from llms.txt exactly the way Cloudflare does: '- [Agent setup](https://developers.cloudflare.com/agent-setup/llms.txt): Install an agent of your choice, connect skills and MCP servers, and start deploying to Cloudflare — all from your editor or terminal.' For XERJ: xerj.org/agent-setup/llms.txt listing claude-code.md, cursor.md, codex.md, vscode.md, opencode.md, and an 'mcp-only-no-shell.md' page — the shell-less persona that fails today gets its own hop-1 page.
- Copy the 4-step Quick start shape verbatim as a template: '1. **Install <agent>** / 2. **Launch <agent> in your project** / 3. **Install the <product> plugin** / 4. **Try a prompt**' with a code block under each. XERJ equivalent: 1. `curl -fsSL https://xerj.org/get | sh` 2. `xerj --insecure --data-dir ~/xerj-data` (background) + `xerj autoindex .` 3. `xerj init` 4. a literal first prompt such as `Find where retries are implemented and cite file:line`.
- End every install path with a copy-paste first prompt and an '## Example prompts' list of 5 one-liners, as in claude-code/index.md ('Check my Workers deployment logs for errors and suggest fixes.'). XERJ's llms.txt has commands but no prompt the agent can hand its user.
- Add a named verification step after `xerj init`, mirroring 'Use `claude mcp list` to verify Cloudflare servers are connected.' and the Troubleshooting entry 'Run `claude mcp list` to verify the server is registered. Try removing and re-adding: `claude mcp remove cloudflare` then re-add it.' XERJ: 'Run `claude mcp list` and confirm `xerj` is listed; if not, `claude mcp add xerj -- xerj mcp`.'
- Publish a client-config snippet per client with the footgun called out inline, copying the VS Code wording pattern: 'Visual Studio Code uses `servers` as the root key. Configurations copied from Cursor or Claude Desktop use `mcpServers` and will silently do nothing here.' XERJ's `xerj init` writes .mcp.json (Claude Code shape); the VS Code `servers` shape and Cursor shape are currently undocumented.
- Inject a 3-line discovery header at the top of every xerj.org/*.md page and every llms-full section, verbatim pattern: '> Documentation Index  / > Fetch the complete documentation index at: https://xerj.org/llms.txt  / > Use this file to discover all available pages before exploring further.' — so an agent that lands on an answers/*.md page via search still finds the install page.
- Document the no-MCP, no-shell degraded path as a Troubleshooting entry the way Cloudflare does ('If you prefer not to use the MCP server, point the agent directly at developers.cloudflare.com/llms.txt ...'). XERJ: 'No shell and no MCP? Everything under Core agent operations is plain HTTP: POST http://localhost:9200/ax-*/_search with an ES query-DSL body.' — already true, but buried on line 31 as a link rather than positioned as the fallback.
- Expose token-cost numbers for the install decision, as cloudflare/mcp does ('Code mode | 3 | ~1,100 | 0.5%'): state the measured token size of xerj's ten-tool `tools/list` and of llms.txt, and consider serving `x-markdown-tokens`-style counts. Also offer scoped files (xerj.org/agents/llms.txt, xerj.org/autoindex/llms.txt) since Cloudflare's hub blockquote — 'Each product below links to its own llms.txt ... the recommended way to explore a specific product's content.' — is how they keep hop-1 short.
- On the field-report problem specifically: nothing here to copy — Cloudflare makes no ask of agents at all and ships no 'required' language. Inference: the model that gets adoption is 'install -> prompt -> verify' with the ask nowhere in the critical path; if XERJ keeps the report ask, it should be a numbered Quick-start step (with its own command block) rather than a paragraph plus a link under a heading named 'Optional'.
