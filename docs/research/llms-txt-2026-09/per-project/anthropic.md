# Anthropic — Claude Developer Platform (docs.anthropic.com → platform.claude.com)

Category: AI infra / API docs

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://platform.claude.com/llms.txt (fetched via https://docs.anthropic.com/llms.txt, 301-redirected)> — HTTP 200
- Size: 68036 bytes, 699 lines
- Install position: absent — no install command, no install prompt anywhere in the 699 lines. The word 'install' does not occur (grep -i install matches nothing). Installation exists only as links: '### CLI, SDKs, and libraries' (lines 270-288) links to a CLI quickstart .md page, and line 691-697 '### Other pages' links an 'API usage primer for Claude' .md page. It is a plain auto-generated docs index (per-language page counts + grouped link list) with nothing about installation.
- Tone: neutral inventory. Makes zero demands of the agent: no imperative sentences, no 'you must', no obligations. Opening line: 'This file provides an overview of the Anthropic API documentation and developer resources.' The only imperative-flavoured content is in the linked claude-api SKILL.md (repo file), which is strongly imperative toward the agent.
- Section order: # Anthropic Developer Documentation → ## Root URL → ## Available Languages on Website → ## English → ### Docs home → ### Messages → ### Managed Agents → ### Admin → ### Best practices → ### Models & pricing → ### CLI, SDKs, and libraries → ### API reference → ### Claude API skill → ### Release notes → ### API Reference → ### Other pages → (footer) For more comprehensive documentation, see llms-full.txt

### Install commands found

```sh
(none in llms.txt) — commands live in linked pages: `brew install anthropics/tap/ant` (cli/quickstart.md, get-started.md)
(linked page cli/quickstart.md) `go install github.com/anthropics/anthropic-cli/cmd/ant@latest`
(linked page cli/quickstart.md) `curl -fsSL "https://github.com/anthropics/anthropic-cli/releases/download/v${VERSION}/ant_${VERSION}_${OS}_${ARCH}.tar.gz" | sudo tar -xz -C /usr/local/bin ant`
(linked page get-started.md) `pip install anthropic`
(linked page get-started.md) `npm install @anthropic-ai/sdk`
(linked page claude-api-skill.md) `npx skills add https://github.com/anthropics/skills --skill claude-api`
(linked page claude-api-skill.md) `/plugin marketplace add anthropics/skills` then `/plugin install claude-api@anthropic-agent-skills`
(llms-full.txt, once) `ant auth login`
```

### Text written for an agent (verbatim)

````text
(llms.txt itself: none.) The only agent-addressed text is in the linked page https://platform.claude.com/docs/en/claude_api_primer.md (listed at llms.txt line 695 as '[API usage primer for Claude]'):

# API usage primer for Claude

> This guide is designed to give Claude the basics of using the Claude API. It gives explanation and examples of model IDs/the basic messages API, tool use, streaming, thinking, and nothing else.

## Models

```text wrap
Recommended default for most work, including complex agentic coding: Claude Opus 5: claude-opus-5
Step up for the hardest long-running agentic and research tasks, at 2x Claude Opus 5 pricing: Claude Fable 5.1: claude-fable-5-1
Previous Opus model: Claude Opus 4.8: claude-opus-4-8
Smart model: Claude Sonnet 5: claude-sonnet-5
For fast, cost-effective tasks: Claude Haiku 4.5: claude-haiku-4-5-20251001
```
````

### MCP snippet (verbatim)

```text
(none in llms.txt; llms-full.txt has 10 `mcpServers` occurrences but they are Messages-API request-body examples for the MCP connector feature, not install-config for a product MCP server)
```

### Feedback / contribution ask (verbatim)

```text
(none in llms.txt.) In llms-full.txt the only feedback asks are per-feature Google Forms, e.g. line 47465: `Share feedback on this feature through the [feedback form](https://forms.gle/MhcGFFwLxuwnWTkYA).`
```

### Best passage for driving install and first use (verbatim)

````text
(llms.txt has no install/first-use passage. Best passage from the linked page https://platform.claude.com/docs/en/agents-and-tools/agent-skills/claude-api-skill.md, section 'How to use the skill'):

### In Claude Code (bundled)

The skill ships with [Claude Code](https://code.claude.com/docs/en/overview) and requires no installation. When you ask Claude to help build something with the Claude API, or when your project already imports an Anthropic SDK, the skill activates automatically.

You can also invoke it directly:

```text wrap
/claude-api
```

### From the skills repository

The skill source is available in the [Anthropic skills repository](https://github.com/anthropics/skills). You can install it using the `npx` command:

```bash
npx skills add https://github.com/anthropics/skills --skill claude-api
```

Or install it as a [Claude Code plugin](https://code.claude.com/docs/en/plugins):

```text wrap
/plugin marketplace add anthropics/skills
/plugin install claude-api@anthropic-agent-skills
```
````

### One-click links

- `(none) — zero occurrences of cursor://, vscode:, or `claude mcp add` in llms.txt AND in the 35 MB llms-full.txt`

## llms-full.txt

- URL: <https://platform.claude.com/llms-full.txt (fetched via https://docs.anthropic.com/llms-full.txt, redirected)> — HTTP 200
- Size: 35109013 bytes
- Relationship to llms.txt: Machine-generated concatenation of all 629 English .md pages (629 `title:` frontmatter blocks counted), each page prefixed with a '---\ntitle:/url:/description:\n---' frontmatter block and raw MDX components (<CodeGroup>, <Tabs>, <Card>) left in. Same header/section skeleton as llms.txt ('Anthropic Developer Documentation - Full Content', 'Root URL', 'Available Languages'), then '# English Documentation - Full Content'. Not curated; 1,175,421 lines. This run measured 35,109,013 bytes via docs.anthropic.com (the task brief observed 29.8 MB earlier via the same host — size drifts with the doc build).
- Install position: CLI quickstart page (## Installation with brew/curl/go tabs) appears as one of 629 concatenated pages; no dedicated install section at the top. First 30 lines are the same language inventory as llms.txt, with 'English (en) - 629 pages - ✓ Full content included below'.

## Other agent-facing files

### claude_api_primer.md (API usage primer for Claude) — HTTP 200

<https://platform.claude.com/docs/en/claude_api_primer.md>

819-line page written explicitly FOR the model: model IDs, basic messages API, tool use, streaming, thinking 'and nothing else'. Linked from llms.txt line 695 under '### Other pages'. This is the closest thing Anthropic has to an agent-facing quickstart.

```text
> This guide is designed to give Claude the basics of using the Claude API. It gives explanation and examples of model IDs/the basic messages API, tool use, streaming, thinking, and nothing else.
```

### Claude API skill page — HTTP 200

<https://platform.claude.com/docs/en/agents-and-tools/agent-skills/claude-api-skill.md>

Docs page for the open-source `claude-api` Agent Skill: what it provides, when it auto-activates, three install paths (bundled / npx skills add / plugin marketplace), subcommands.

```text
**Automatic activation** occurs when:

* Your code imports an Anthropic SDK (`anthropic` for Python, `@anthropic-ai/sdk` for TypeScript/JavaScript)
* You ask Claude to help build, debug, or optimize something with the Claude API, an Anthropic SDK, or Managed Agents
```

### anthropics/skills claude-api/SKILL.md — HTTP 200

<https://raw.githubusercontent.com/anthropics/skills/main/skills/claude-api/SKILL.md>

572-line, 86 KB agent-executed skill. The frontmatter `description` is itself an imperative trigger/skip rule for the agent; body contains 'Output Requirement', 'Never guess SDK usage', an 'API Drift' table telling the model its training prior is stale, and a subcommand table.

```text
TRIGGER — read BEFORE opening the target file; don't skip because it "looks like a one-liner" — whenever: the prompt names Claude/Anthropic in any form (Claude, Anthropic, Fable, Opus, Sonnet, Haiku, `anthropic`, `@anthropic-ai`, `claude-*`, `us.anthropic.*`, `[1m]`); the user asks about an LLM (pricing/model choice/limits/caching) — never answer from memory; OR the task is LLM-shaped with provider unstated
```

### anthropics/skills mcp-builder/SKILL.md — HTTP 200

<https://raw.githubusercontent.com/anthropics/skills/main/skills/mcp-builder/SKILL.md>

236-line skill for BUILDING MCP servers (not installing one). Phase-structured workflow; tells the agent to fetch modelcontextprotocol.io pages with a `.md` suffix.

```text
Start with the sitemap to find relevant pages: `https://modelcontextprotocol.io/sitemap.xml`

Then fetch specific pages with `.md` suffix for markdown format (e.g., `https://modelcontextprotocol.io/specification/draft.md`).
```

### anthropics/skills README.md — HTTP 200

<https://raw.githubusercontent.com/anthropics/skills/main/README.md>

Repo README: what skills are, three install surfaces (Claude Code plugin marketplace, Claude.ai, API), a minimal SKILL.md template, and a first-use sentence.

```text
After installing the plugin, you can use the skill by just mentioning it. For instance, if you install the `document-skills` plugin from the marketplace, you can ask Claude Code to do something like: "Use the PDF skill to extract the form fields from `path/to/some-file.pdf`"
```

### CLI quickstart (ant) — HTTP 200

<https://platform.claude.com/docs/en/cli-sdks-libraries/cli/quickstart.md>

Install (brew / curl+tar / go install tabs) → `ant --version` check → `ant auth login` → first request with expected output shown.

````text
Check the installation:

```bash
ant --version
```
````

### get-started.md (Quickstart) — HTTP 200

<https://platform.claude.com/docs/en/get-started.md>

Per-language <Steps>: 'Set your API key' → 'Create a project and install the SDK' → first call. Repeated identically for CLI/Python/TS/C#/Go/Java/PHP/Ruby.

````text
Export your API key as an environment variable. The SDK reads `ANTHROPIC_API_KEY` automatically.

```bash
export ANTHROPIC_API_KEY="your-api-key-here"
```
````

### Agent Skills overview — HTTP 200

<https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview.md>

Explains progressive disclosure (metadata ~100 tokens always loaded; SKILL.md body <5k tokens on trigger) and that the `description` must say both what and when.

```text
The `description` is what Claude matches your request against when determining whether to trigger the Skill, so it must say both what the Skill does and when to use it. This lightweight approach means you can install many Skills without context penalty: until a Skill is triggered, only its name and description occupy context.
```

### llms-install.md — HTTP 404

<https://platform.claude.com/llms-install.md (and https://docs.anthropic.com/llms-install.md)>

does not exist (platform host returns a 404 HTML page; docs.anthropic.com returns 404)

### anthropics/claude-code AGENTS.md — HTTP 404

<https://raw.githubusercontent.com/anthropics/claude-code/main/AGENTS.md>

does not exist

### anthropics/claude-code CLAUDE.md — HTTP 404

<https://raw.githubusercontent.com/anthropics/claude-code/main/CLAUDE.md>

does not exist

### anthropics/skills AGENTS.md / CLAUDE.md — HTTP 404

<https://raw.githubusercontent.com/anthropics/skills/main/AGENTS.md (and CLAUDE.md)>

neither exists

## What they do better than XERJ

- Zero-obligation tone: the llms.txt asks nothing of the reader; every ask is moved into a triggerable skill whose frontmatter states precisely WHEN it applies ('TRIGGER — read BEFORE opening the target file … SKIP only when another provider is being worked on'). XERJ's llms.txt front-loads two demands in the first paragraph before the agent has installed anything.
- Shell-less install path is first-class: the claude-api skill offers three installs and the first one is 'ships with Claude Code and requires no installation'; the second is a one-liner `npx skills add …`; the third is a slash command typed inside the agent (`/plugin marketplace add anthropics/skills`). All three work without a Bash tool. XERJ's install is `curl | sh` then a running node — no route for the MCP-only persona your study says fails at every tier.
- A page written for the model, stated as such in the page's own description: 'This guide is designed to give Claude the basics … and nothing else.' The scoping sentence tells the agent what NOT to expect, which stops it wandering into 629 pages.
- Explicit 'your training prior may be stale' table in SKILL.md ('Warning: API Drift - Your Training Prior May Be Stale', stale-prior vs current-API columns). This is the mechanism that would have stopped 9/12 XERJ agents assuming default search is neural — a two-column 'what you probably assume / what is actually true' table.
- Verification step immediately after install (`ant --version`), then auth, then a first request with the expected JSON output shown inline, so the agent can diff its result against the doc.
- 'Never guess SDK usage … If WebFetch or repository access fails … do not keep retrying' — the skill pre-plans the failure mode (blocked network) and gives the fallback, instead of assuming the happy path.
- Stable, machine-fetchable per-page `.md` URLs for every docs page, plus the page inventory with counts at the top ('English (en) - 629 pages - Content included below'), so an agent can budget before fetching.

## What XERJ does better

- XERJ's llms.txt is an actual agent runbook (install → start node → autoindex → wire MCP → query → report) with numbered first-run steps; Anthropic's is a plain auto-generated docs index with no install prompt at all — the install command is three link-hops away.
- XERJ names concrete traps in-line ('PATH consequence — read this before the next command', 'Put the data dir OUTSIDE any folder you will autoindex'); Anthropic's llms.txt carries no operational caveats.
- XERJ has a '## Caveats (read before you rely on a capability)' section and honesty rules (lexical embedder, measured numbers); Anthropic's index has nothing comparable.
- XERJ ships tool-schema files (mcp-tools.json, anthropic-tools.json) generated from a real tools/list, discoverable from llms.txt; Anthropic's llms.txt links no MCP install/config for any product server.
- XERJ has a contribution/feedback protocol at all (field report, AI_CONTRIBUTIONS.md, CLA-exempt one-file PR); Anthropic's only feedback surface is per-feature Google Forms buried in llms-full.txt.
- XERJ's llms.txt is 40 KB and fits one context read; Anthropic's llms-full.txt is 35 MB and unusable as a single fetch.

## Adoptable ideas

- Ship the agent-facing obligations as a SKILL.md with a TRIGGER/SKIP frontmatter, modelled on https://raw.githubusercontent.com/anthropics/skills/main/skills/claude-api/SKILL.md — e.g. a `xerj` skill whose description reads 'TRIGGER — before you close a session in which you ran `xerj autoindex` or any `xerj_*` MCP tool: run `xerj feedback --dry-run` … SKIP when you never ran XERJ.' A skill fires at the right moment; a sentence in paragraph 1 of llms.txt is forgotten by the time the session ends. This directly addresses the missing-field-report problem: the ask is currently made before install and filed under '## Optional' (line 223), and neither placement is a trigger.
- Rename `## Optional` (llms.txt line 223) and move the 'Your side of the deal — the field report' entry (line 225) into a section whose heading matches the claim in line 7. Anthropic's structure lesson: the skill page's heading is literally 'When the skill activates'. Suggested heading: `## Before you end the session (required)`. Right now line 7 says 'this is required, not optional … and the template is under Optional' — the doc contradicts itself in one sentence.
- Add a no-shell install path modelled on the three-tier list in https://platform.claude.com/docs/en/agents-and-tools/agent-skills/claude-api-skill.md ('### In Claude Code (bundled) … requires no installation' / `npx skills add https://github.com/anthropics/skills --skill claude-api` / `/plugin marketplace add anthropics/skills` + `/plugin install claude-api@anthropic-agent-skills`). Concretely: publish `xerj init`'s 10-line skill + `.mcp.json` as an installable skill so an MCP-only persona can `npx skills add https://github.com/xerj-org/xerj --skill xerj` or `/plugin marketplace add xerj-org/xerj` without a terminal. Your study says shell-less personas fail at every tier; this is the path the largest AI-docs project uses for exactly that persona.
- Copy the 'Warning: API Drift - Your Training Prior May Be Stale' two-column table pattern from claude-api/SKILL.md ('| Area | Stale prior | Current API |') into XERJ's '## Caveats' as '| Area | What you probably assume | What XERJ actually does |' with row 1: 'Default search | neural/embedding | lexical feature-hashing (`--embed-mode neural` is opt-in)'. This targets the 9-of-12 misread directly.
- Add a one-line scoping sentence at the top of llms.txt in the style of https://platform.claude.com/docs/en/claude_api_primer.md: 'This guide is designed to give Claude the basics of using the Claude API. It gives explanation and examples of model IDs/the basic messages API, tool use, streaming, thinking, and nothing else.' — i.e. state what the page covers AND what it deliberately omits, so agents stop reading at the right point.
- Show expected output after the first command, as cli/quickstart.md does (`ant --version` then `ant messages create …` followed by a ```text Output``` block). XERJ's step 4 `xerj search "<plain-English text>"` should be followed by a short verbatim sample of what a successful result looks like, so an agent can self-verify without a human.
- Pre-plan the blocked-network/sandbox failure like claude-api/SKILL.md ('**If WebFetch or repository access fails** … do not keep retrying'). XERJ already has 'Sandboxed / no `gh`? Run `--dry-run`, then hand the printed report to your operator' — promote it to a bold, standalone rule with the same 'do not keep retrying' phrasing, and make `xerj feedback --dry-run` print the exact text the operator should paste.
- Adopt the page-inventory-with-counts header ('- English (en) - 629 pages - /docs - Content included below') for XERJ's `## Answers` / `## Docs` sections so an agent can see '58 answers, ~X KB' before deciding to fetch llms-full.txt.
