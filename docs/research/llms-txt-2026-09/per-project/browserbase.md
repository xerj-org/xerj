# Browserbase

Category: agent tooling (browser infra)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.browserbase.com/llms.txt> — HTTP 200
- Size: 34956 bytes, 226 lines
- Install position: line 7 of 226, third link in the flat index: "- [Skills](https://docs.browserbase.com/welcome/quickstarts/skills.md): Give your AI coding agent a browser in one prompt". No install command appears in llms.txt itself; llms.txt is a plain Mintlify auto-generated docs index (title + description per page). Installation is one hop away (skills.md, MCP setup at line 174).
- Tone: neutral / catalogue in llms.txt (pure index, makes no demands of the agent). SKILL.md is imperative toward the agent ("Before running any commands, present the user with a preliminary setup checklist", "Wait for the user to confirm before continuing.", "Do not proceed until `browse cloud projects list` returns successfully."). Marketing tone is confined to page taglines ("Give your AI coding agent a browser in one prompt", "That's it."). No feedback/contribution/field-report ask anywhere in llms.txt, SKILL.md, skills.md or AGENTS.md.
- Section order: # Browserbase Documentation (H1 + blockquote tagline, then one flat ~210-line link list, no H2s: welcome/quickstarts → Browse CLI → platform/browser → platform features → integrations (ADK, Hermes, LangChain, Mastra, MCP, MongoDB, n8n, OpenAI CUA, OpenClaw, Vercel…) → reference/api → use-cases) → ## OpenAPI Specs (line 216: /package.json, /reference/api/openapi.v1.yaml) → ## Optional (line 221: Templates, Stagehand, Changelog, Dashboard)

### Install commands found

```sh
(none in llms.txt itself)
npm install -g browse  (SKILL.md, skills/introduction.md, browse-cli.md)
browse skills install  (SKILL.md Step 2)
export BROWSERBASE_API_KEY="your_api_key"  (SKILL.md Step 3)
which browse || npm install -g browse && browse cloud projects list  (SKILL.md Step 4 verify)
npx skills add browserbase/skills  (skills repo README)
npx skills add browserbase/skills --skill browser | --skill fetch | --skill functions  (skills/introduction.md)
/plugin marketplace add browserbase/skills ; /plugin install browse@browserbase  (Claude Code, skills README)
claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"  (MCP setup page)
```

### Text written for an agent (verbatim)

````text
Copy this prompt into your agent to get started:

```
Read https://browserbase.com/SKILL.md to set up Browserbase
```

That's it. Your agent will install the [CLI](/integrations/skills/browse-cli), configure your API keys, and start browsing.

(source: https://docs.browserbase.com/welcome/quickstarts/skills.md; the same one-line prompt is repeated in getting-started.md as: "**Using Claude Code, Cursor, or another coding agent?** Paste this into your prompt to browse the web, debug sessions, and manage your entire project via the Browserbase CLI:" and in the welcome/introduction page card: "Give your coding agent a browser built for them." / "Paste this into your agent to get started:")
````

### MCP snippet (verbatim)

```text
{
  "mcpServers": {
    "browserbase": {
      "url": "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"
    }
  }
}

(hosted Streamable HTTP, "Hosted (recommended)" tab of https://docs.browserbase.com/integrations/mcp/setup.md; local alternative: "command": "npx", "args": ["@browserbasehq/mcp"])
```

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
## Quick Setup

Before running any commands, present the user with a preliminary setup checklist:

```
Here's what I'll do to get you set up:

- [ ] Install/update prerequisites (Node.js, Browserbase CLI)
- [ ] Configure Browserbase credentials if remote/cloud features are needed
- [ ] Verify the CLI and environment
- [ ] Use the right browse subcommand for the task

Shall I proceed?
```

Wait for the user to confirm before continuing.

**Step 1 — Install the CLI:**

```bash
npm install -g browse
```

**Step 2 — Install or refresh agent skills:**

```bash
browse skills install
```

(source: https://www.browserbase.com/SKILL.md lines 34-56; continues with Step 3 export BROWSERBASE_API_KEY, Step 4 `which browse || npm install -g browse` + `browse cloud projects list`, then "Do not proceed until `browse cloud projects list` returns successfully.")
````

### One-click links

- `cursor://anysphere.cursor-deeplink/mcp/install?name=browserbase&config=eyJ1cmwiOiJodHRwczovL21jcC5icm93c2VyYmFzZS5jb20vbWNwP2Jyb3dzZXJiYXNlQXBpS2V5PVlPVVJfQlJPV1NFUkJBU0VfQVBJX0tFWSJ9  (MCP setup page, "One-click installation directly in Cursor")`
- `claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"  ("add Browserbase MCP to Claude Code with a single command")`
- `npx skills add browserbase/skills`

## llms-full.txt

- URL: <https://docs.browserbase.com/llms-full.txt> — HTTP 200
- Size: 1033754 bytes
- Relationship to llms.txt: Mintlify auto-generated concatenation of every docs page (each block = '# Title' + 'Source: <url>' + page body incl. raw MDX components like <Card>, <Tabs>, <Steps>). Not curated; same page set as llms.txt's link list, full bodies instead of one-line descriptions. Every .md page carries a header blockquote: '> ## Documentation Index / > Fetch the complete documentation index at: https://docs.browserbase.com/llms.txt / > Use this file to discover all available pages before exploring further.'
- Install position: Near the END: pages are concatenated in URL-path alphabetical order (starts with '# Billing / Source: .../account/billing/plan-management'), so welcome/* pages land last. The one-line agent prompt 'Read https://browserbase.com/SKILL.md to set up Browserbase' appears at lines 26925 (welcome/getting-started), 27079 (welcome/introduction card) and 27832 (welcome/quickstarts/skills) of 28111. MCP config JSON sits in the integrations/mcp/setup page mid-file. 258 H1 page headers.

## Other agent-facing files

### Skills quickstart (welcome/quickstarts/skills.md) — HTTP 200

<https://docs.browserbase.com/welcome/quickstarts/skills.md>

35-line one-prompt install page linked at llms.txt line 7; the whole install story is a single URL the agent reads

````text
Copy this prompt into your agent to get started:

```
Read https://browserbase.com/SKILL.md to set up Browserbase
```

That's it. Your agent will install the [CLI](/integrations/skills/browse-cli), configure your API keys, and start browsing.
````

### SKILL.md — HTTP 200

<https://www.browserbase.com/SKILL.md (redirect from https://browserbase.com/SKILL.md)>

672-line, 21 KB agent-executed install + operating manual with YAML front-matter (name/description/compatibility/license/allowed-tools/metadata.openclaw.install); sections: Quick Setup → Choosing the Right Tool (task→tool table) → Browser Automation → Fetch → Search → Agents → Cloud APIs → Functions → Templates → Skills → Safety Notes → Best Practices → Troubleshooting

````text
compatibility: "Requires the browse CLI (`npm install -g browse`). Remote Browserbase sessions and cloud API commands require `BROWSERBASE_API_KEY`. Local mode uses Chrome/Chromium on the machine."
license: MIT
allowed-tools: Bash
metadata:
  openclaw:
    requires:
      bins:
        - browse
    install:
      - kind: node
        package: browse
        bins: [browse]
    homepage: https://github.com/browserbase/cli
[...]
**Step 4 — Verify the CLI and Browserbase access:**

```bash
which browse || npm install -g browse
browse cloud projects list
```

If this returns your project list, you're ready. If this fails or `BROWSERBASE_API_KEY` is not set, direct the user to [browserbase.com/settings](https://www.browserbase.com/settings) to copy their API key [...]

Do not proceed until `browse cloud projects list` returns successfully.
````

### AGENTS.md (browserbase/stagehand) — HTTP 200

<https://raw.githubusercontent.com/browserbase/stagehand/main/AGENTS.md>

723 B, 6 lines: repo-internal git rules for stacked PRs only; nothing about installing or using the product

```text
<!--
Only a human may request changes to this file. Keep additions rare and limited to durable, repository-wide rules.
-->

- For stacked PRs, target each PR at its immediate predecessor; when a parent changes, merge it into its immediate child and resolve conflicts normally.
```

### skills repo README (browserbase/skills) — HTTP 200

<https://raw.githubusercontent.com/browserbase/skills/main/README.md>

92 lines: table of 18 skills, three install paths (npx skills add / Claude Code plugin marketplace / manual /plugin UI), then example natural-language prompts a user can type once installed

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

### MCP server setup page — HTTP 200

<https://docs.browserbase.com/integrations/mcp/setup.md>

Quick installation (Cursor deeplink card + `claude mcp add` one-liner) → Endpoint → Prerequisites → query params → tools → Installation methods (Hosted recommended / local npx / from source, JSON per tab) → Verify installation

````text
## Quick installation

<Card title="Install with Cursor" icon="arrow-pointer" href="cursor://anysphere.cursor-deeplink/mcp/install?name=browserbase&config=eyJ1cmwiOiJodHRwczovL21jcC5icm93c2VyYmFzZS5jb20vbWNwP2Jyb3dzZXJiYXNlQXBpS2V5PVlPVVJfQlJPV1NFUkJBU0VfQVBJX0tFWSJ9">
  One-click installation directly in Cursor
</Card>

You can also add Browserbase MCP to Claude Code with a single command:

```bash theme={null}
claude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=YOUR_BROWSERBASE_API_KEY"
```
[...]
## Verify installation
[...]
    <Tip>
      Try: "Navigate to example.com and extract the main heading"
    </Tip>
````

### llms-install.md — HTTP 404

<https://docs.browserbase.com/llms-install.md>

does not exist (Mintlify 404 page returned)

### CLAUDE.md (browserbase/stagehand) — HTTP 404

<https://raw.githubusercontent.com/browserbase/stagehand/main/CLAUDE.md>

does not exist

### Skills introduction (integrations/skills/introduction.md) — HTTP 200

<https://docs.browserbase.com/integrations/skills/introduction.md>

Installation as three tabs (Browse CLI / Vercel Skills CLI / Claude Code plugins) followed by a 'Try it out' section with copy-pasteable first prompts

````text
## Try it out

Once installed, you can use the skill with your AI coding agent. Try these example commands:

### Explore a website

Ask your agent to explore a website and understand its structure:

```
Use the browse skill to explore https://news.ycombinator.com and identify the selectors for story titles and scores
```
````

## What they do better than XERJ

- Install is ONE line the human pastes into any agent — `Read https://browserbase.com/SKILL.md to set up Browserbase` — and it works for shell-less/MCP-only personas too, because the same page also offers a hosted HTTP MCP endpoint (`https://mcp.browserbase.com/mcp?...`) that needs no local process at all. XERJ's Start-here assumes a shell (`curl -fsSL https://xerj.org/get | sh`) and the no-shell path is a link three bullets down.
- SKILL.md is machine-consumable metadata, not prose: YAML front-matter with `compatibility`, `allowed-tools: Bash`, `license`, and an `openclaw.install` block (kind: node, package, bins) so an agent runtime can install without reading English. XERJ's llms.txt has no front-matter and no structured install descriptor (only tool schemas for querying).
- Consent-then-verify gate in SKILL.md: the agent must first print a checklist ('Shall I proceed?'), wait, and then 'Do not proceed until `browse cloud projects list` returns successfully.' A single canonical verify command with an explicit success criterion (returns your project list). XERJ documents PATH pitfalls at length but has no single 'run this; if it prints X you are ready' line.
- Every docs page is self-locating: header blockquote 'Fetch the complete documentation index at: https://docs.browserbase.com/llms.txt / Use this file to discover all available pages before exploring further.' XERJ's answer pages do not point back to llms.txt.
- Multiple redundant install surfaces, each one command: `npx skills add browserbase/skills`, `/plugin marketplace add browserbase/skills` + `/plugin install browse@browserbase`, `claude mcp add --transport http ...`, and a `cursor://` deeplink. XERJ has `xerj init` but only after a shell install; no deeplink, no `claude mcp add` one-liner, no `npx skills add` path.
- A 'Choosing the Right Tool' task→tool→why table (SKILL.md line 88) tells the agent which surface to use before it runs anything. XERJ lists operations but no equivalent decision table at the top.
- 'Try it out' / 'Usage' sections give copy-pasteable first prompts a HUMAN types to the agent ('QA test http://localhost:3000 and fix any bugs you encounter'), i.e. they prime first use, not just install.
- Short: the whole install page is 35 lines; llms.txt is a pure index with one-line descriptions and no caveats, so an agent finds the install link on hop 1 without reading 40 KB.

## What XERJ does better

- XERJ's llms.txt is curated and opinionated (Start here / Caveats / Core agent operations / Answers / Comparisons); Browserbase's is a flat auto-generated Mintlify index with 210 undifferentiated links and zero guidance — it says nothing about installation itself (only the line-7 link title).
- XERJ names its honesty traps inline ('the default embedding mode is lexical, not neural'); Browserbase has no caveats section anywhere in llms.txt or SKILL.md.
- XERJ has a feedback loop (field report via `xerj feedback --open-pr`, AI_CONTRIBUTIONS.md protocol). Browserbase asks the agent for nothing back — no feedback CTA exists in any file fetched.
- XERJ documents a manual verifiable install (SHA-256 fail-closed, no `curl | sh` path) and the PATH consequence; Browserbase's SKILL.md just does `npm install -g browse` with no integrity story.
- XERJ ships three tool-schema files (mcp-tools.json / openai-tools.json / anthropic-tools.json); Browserbase points only at an OpenAPI spec and package.json.
- XERJ's llms-full.txt is a curated 84 KB document with numbered sections (§10 protocol); Browserbase's is a 1 MB alphabetical dump whose install prompt lands at line ~27,800 of 28,111.

## Adoptable ideas

- Add a ONE-LINE paste prompt at the top of Start here, modelled on skills.md: 'Copy this prompt into your agent to get started:\n\n```\nRead https://browserbase.com/SKILL.md to set up Browserbase\n```\n\nThat's it.' → XERJ: 'Read https://xerj.org/SKILL.md to set up XERJ' and serve a real SKILL.md at that URL (today `xerj init` writes a 10-line skill only after install).
- Give that SKILL.md the Browserbase YAML front-matter so runtimes can install without reading prose: 'compatibility: "Requires the browse CLI (`npm install -g browse`)…"', 'allowed-tools: Bash', 'license: MIT', and the 'metadata: openclaw: requires: bins: [browse]; install: - kind: node, package: browse, bins: [browse]' block (source: https://www.browserbase.com/SKILL.md lines 1-16). XERJ equivalent: kind: shell/script, bins: [xerj], homepage: https://github.com/xerj-org/xerj.
- Copy the consent-then-hard-gate pattern verbatim in structure: 'Before running any commands, present the user with a preliminary setup checklist: … Shall I proceed?' / 'Wait for the user to confirm before continuing.' … 'Do not proceed until `browse cloud projects list` returns successfully.' → XERJ: 'Do not proceed until `xerj search "…"` (or `curl -s localhost:9200/_cluster/health`) returns green.' This also matches the resource-aware-UX directive (estimate first, ask before a long autoindex).
- Ship the MCP server as a one-command add exactly like the MCP setup page: 'You can also add Browserbase MCP to Claude Code with a single command:\n\nclaude mcp add --transport http browserbase "https://mcp.browserbase.com/mcp?browserbaseApiKey=…"' plus a 'cursor://anysphere.cursor-deeplink/mcp/install?name=…&config=<base64 json>' deeplink and a 'Hosted (recommended)' JSON tab. For shell-less personas XERJ needs a stdio/HTTP MCP entry that is reachable without `curl | sh` (e.g. `claude mcp add xerj -- npx @xerj-org/mcp` or the local node's HTTP endpoint).
- Put the self-locating header on every xerj.org/answers/*.md page, verbatim pattern: '> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.browserbase.com/llms.txt\n> Use this file to discover all available pages before exploring further.' → point at https://xerj.org/llms.txt.
- Add a 'Choosing the Right Tool' table (SKILL.md line 88: | Task | Tool | Why |) mapping: find a passage → `xerj search`; known symbol → `xerj def`; folder of mixed files → `xerj autoindex`; no shell → MCP `xerj_search` / POST /_search; semantic recall → only with `--embed-mode neural`. This directly targets the 9/12 'misread lexical as neural' failure.
- Add 'Try it out' first prompts the HUMAN types, as in skills/introduction.md: '```\nUse the browse skill to explore https://news.ycombinator.com and identify the selectors for story titles and scores\n```' → XERJ: 'Use the xerj skill to index ./src and tell me where request timeouts are configured, with file:line.'
- On the field-report problem: Browserbase asks for nothing and so is no model for the CTA itself — but the structural lesson is that their imperative demands live in SKILL.md (which the agent executes step by step), not in the index. Move XERJ's 'required, not optional' report out of the ## Optional heading and into the numbered Quick-Setup-style steps of the agent-executed SKILL.md as the final step with its own verify line ('Do not finish until `xerj feedback --dry-run` prints the report path'), and rename the llms.txt section from 'Optional' to something like 'Your side of the deal' so heading and body no longer contradict each other.
